"""Queue helpers for the asyncio driver.

Mirrors the SDK Helper Spec ``queue.*`` surface: push/pop/peek/len/purge.
"""

from __future__ import annotations

import json as _json
import asyncio
import re
from typing import Any

from .errors import RedDBError
from .sqlutil import sql_identifier


__all__ = ["QueueClient", "ScopedQueueClient", "QueueConsumer"]


class QueueClient:
    """Queue namespace bound to a single underlying transport."""

    def __init__(self, transport: Any) -> None:
        self._t = transport

    def __call__(self, queue: str) -> "ScopedQueueClient":
        return self.scope(queue)

    def scope(self, queue: str) -> "ScopedQueueClient":
        return ScopedQueueClient(self, sql_identifier(queue))

    async def push(self, queue: str, value: Any, **opts: Any) -> dict[str, Any]:
        priority = opts.pop("priority", None)
        priority_clause = (
            f" PRIORITY {_queue_priority(priority)}" if priority is not None else ""
        )
        sql = (
            f"QUEUE PUSH {sql_identifier(queue)} "
            f"{_queue_value_literal(value)}{priority_clause}"
        )
        return await self._t.query(sql)

    async def pop(self, queue: str, count: int | None = None) -> list[Any]:
        sql = f"QUEUE POP {sql_identifier(queue)}{_queue_count(count)}"
        result = await self._t.query(sql)
        return _queue_payloads(result)

    async def peek(self, queue: str, count: int | None = None) -> list[Any]:
        sql = f"QUEUE PEEK {sql_identifier(queue)}{_queue_count(count)}"
        result = await self._t.query(sql)
        return _queue_payloads(result)

    async def len(self, queue: str) -> int:
        result = await self._t.query(f"QUEUE LEN {sql_identifier(queue)}")
        rows = result.get("rows") if isinstance(result, dict) else None
        if rows:
            return int(rows[0].get("len", 0))
        return 0

    async def purge(self, queue: str) -> dict[str, Any]:
        return await self._t.query(f"QUEUE PURGE {sql_identifier(queue)}")

    async def read_wait(
        self,
        queue: str,
        consumer: str,
        *,
        wait_ms: int,
        group: str | None = None,
        count: int | None = None,
    ) -> list[Any]:
        """Live ``QUEUE READ … WAIT <ms>`` helper (PRD #718 / #725).

        Blocks until a message is available for ``consumer`` on ``queue``
        (optionally scoped to ``group``), the ``wait_ms`` budget elapses,
        or the server cancels. Timeout returns an empty list — same shape
        as a non-waiting empty pop, never raises. ``wait_ms`` is required;
        there is no infinite-wait default. Cancellation and cap rejection
        surface as ``RedDBError`` via the transport.
        """
        rows = await self.read_wait_entries(
            queue, consumer, wait_ms=wait_ms, group=group, count=count
        )
        return [row.get("payload") for row in rows]

    async def read_wait_entries(
        self,
        queue: str,
        consumer: str,
        *,
        wait_ms: int,
        group: str | None = None,
        count: int | None = None,
    ) -> list[dict[str, Any]]:
        """Wait without discarding the server-issued delivery handle."""
        sql = _build_read_wait_sql(
            queue, consumer, wait_ms=wait_ms, group=group, count=count
        )
        result = await self._t.query(sql)
        rows = result.get("rows") if isinstance(result, dict) else None
        return [row for row in rows or [] if isinstance(row, dict)]


class ScopedQueueClient:
    def __init__(self, queue: QueueClient, name: str) -> None:
        self._queue = queue
        self.name = name

    async def push(self, value: Any, **opts: Any) -> dict[str, Any]:
        return await self._queue.push(self.name, value, **opts)

    async def read_wait_entries(
        self, consumer: str, *, wait_ms: int, group: str | None = None
    ) -> list[dict[str, Any]]:
        return await self._queue.read_wait_entries(
            self.name, consumer, wait_ms=wait_ms, group=group, count=1
        )

    async def ack(self, delivery_id: str) -> dict[str, Any]:
        return await self._queue._t.query(
            f"QUEUE ACK {self.name} WITH delivery_id = {_queue_string_literal(_delivery_id(delivery_id))}"
        )

    async def nack(self, delivery_id: str, *, delay: str | None = None) -> dict[str, Any]:
        delay_clause = _retry_delay(delay)
        return await self._queue._t.query(
            f"QUEUE NACK {self.name} WITH delivery_id = "
            f"{_queue_string_literal(_delivery_id(delivery_id))}{delay_clause}"
        )

    def consumer(self, **options: Any) -> "QueueConsumer":
        return QueueConsumer(self, **options)


class QueueConsumer:
    def __init__(
        self,
        queue: ScopedQueueClient,
        *,
        consumer: str,
        handle_message: Any,
        group: str | None = None,
        wait_ms: int = 30_000,
        retry_delay: str | None = None,
        on_error: Any = None,
    ) -> None:
        if not callable(handle_message):
            raise TypeError("queue consumer requires handle_message")
        self.queue = queue
        self.consumer_name = sql_identifier(consumer)
        self.group = sql_identifier(group) if group is not None else None
        if not isinstance(wait_ms, int) or isinstance(wait_ms, bool) or wait_ms < 0:
            raise TypeError("queue consumer wait_ms must be a non-negative integer")
        _retry_delay(retry_delay)
        self.wait_ms = wait_ms
        self.retry_delay = retry_delay
        self.handle_message = handle_message
        self.on_error = on_error
        self._running = False
        self._task: asyncio.Task[None] | None = None

    @property
    def is_running(self) -> bool:
        return self._running

    def start(self) -> "QueueConsumer":
        if self._running:
            return self
        self._running = True
        self._task = asyncio.create_task(self._run())
        return self

    async def stop(self) -> None:
        self._running = False
        if self._task is not None and self._task is not asyncio.current_task():
            await self._task

    async def wait(self) -> None:
        if self._task is not None:
            await self._task

    async def _run(self) -> None:
        try:
            while self._running:
                rows = await self.queue.read_wait_entries(
                    self.consumer_name, wait_ms=self.wait_ms, group=self.group
                )
                if not rows:
                    continue
                message = rows[0]
                delivery_id = _delivery_id(message.get("delivery_id"))
                try:
                    result = self.handle_message(message)
                    if asyncio.iscoroutine(result):
                        await result
                except Exception as error:
                    await self.queue.nack(delivery_id, delay=self.retry_delay)
                    if self.on_error is None:
                        raise
                    result = self.on_error(error, message)
                    if asyncio.iscoroutine(result):
                        await result
                    continue
                try:
                    await self.queue.ack(delivery_id)
                except Exception as error:
                    if self.on_error is None:
                        raise
                    result = self.on_error(error, message)
                    if asyncio.iscoroutine(result):
                        await result
        finally:
            self._running = False


def _build_read_wait_sql(
    queue: str,
    consumer: str,
    *,
    wait_ms: int,
    group: str | None,
    count: int | None,
) -> str:
    if not isinstance(wait_ms, int) or isinstance(wait_ms, bool) or wait_ms < 0:
        raise RedDBError(
            "queue read_wait requires a non-negative integer wait_ms (no infinite wait)",
            code="INVALID_ARGUMENT",
        )
    q = sql_identifier(queue)
    c = sql_identifier(consumer)
    g = f" GROUP {sql_identifier(group)}" if group is not None else ""
    n = _queue_count(count) if count is not None else ""
    return f"QUEUE READ {q}{g} CONSUMER {c}{n} WAIT {wait_ms}ms"


def _queue_count(count: Any) -> str:
    if count is None:
        return ""
    if not isinstance(count, int) or isinstance(count, bool) or count < 0:
        raise RedDBError(
            "queue count must be a non-negative integer",
            code="INVALID_ARGUMENT",
        )
    return f" COUNT {count}"


def _queue_priority(priority: Any) -> str:
    if not isinstance(priority, int) or isinstance(priority, bool):
        raise RedDBError(
            "queue priority must be an integer",
            code="INVALID_ARGUMENT",
        )
    return str(priority)


def _queue_value_literal(value: Any) -> str:
    if value is None:
        return "NULL"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    if isinstance(value, str):
        return "'" + value.replace("\\", "\\\\").replace("'", "''") + "'"
    return _json.dumps(value, separators=(",", ":"))


def _queue_string_literal(value: Any) -> str:
    return "'" + str(value).replace("\\", "\\\\").replace("'", "''") + "'"


def _delivery_id(value: Any) -> str:
    delivery_id = "" if value is None else str(value)
    if not delivery_id:
        raise RedDBError("queue delivery is missing delivery_id", code="INVALID_RESPONSE")
    return delivery_id


_RETRY_DELAY = re.compile(
    r"^(?:\d+(?:\.\d+)?)(?:ms|s|sec|secs|m|min|mins|h|hr|hrs|d|day|days)?$"
)


def _retry_delay(value: str | None) -> str:
    if value is None:
        return ""
    if _RETRY_DELAY.fullmatch(value) is None:
        raise RedDBError("invalid queue retry delay", code="INVALID_ARGUMENT")
    return f" WITH DELAY {value}"


def _queue_payloads(result: Any) -> list[Any]:
    if not isinstance(result, dict):
        return []
    rows = result.get("rows")
    if not isinstance(rows, list):
        return []
    return [row.get("payload") for row in rows]
