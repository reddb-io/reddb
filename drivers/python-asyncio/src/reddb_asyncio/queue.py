"""Queue helpers for the asyncio driver.

Mirrors the SDK Helper Spec ``queue.*`` surface: push/pop/peek/len/purge.
"""

from __future__ import annotations

import json as _json
import asyncio
import contextlib
import inspect
import random
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

    async def nack(
        self, delivery_id: str, *, delay: str | None = None
    ) -> dict[str, Any]:
        delay_clause = _retry_delay(delay)
        return await self._queue._t.query(
            f"QUEUE NACK {self.name} WITH delivery_id = "
            f"{_queue_string_literal(_delivery_id(delivery_id))}{delay_clause}"
        )

    async def extend(self, delivery_id: str) -> dict[str, Any]:
        return await self._queue._t.query(
            f"QUEUE EXTEND {self.name} WITH delivery_id = "
            f"{_queue_string_literal(_delivery_id(delivery_id))}"
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
        concurrency: int = 1,
        receive_backoff_initial_ms: int = 250,
        receive_backoff_max_ms: int = 30_000,
        receive_backoff_multiplier: float = 2.0,
        receive_backoff_jitter: bool = True,
        lease_heartbeat_ms: int | None = None,
        handler_timeout_ms: int | None = None,
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
        if (
            not isinstance(concurrency, int)
            or isinstance(concurrency, bool)
            or not 1 <= concurrency <= 128
        ):
            raise TypeError(
                "queue consumer concurrency must be an integer between 1 and 128"
            )
        self.concurrency = concurrency
        self._backoff_initial = _non_negative_int(
            receive_backoff_initial_ms, "receive_backoff_initial_ms"
        )
        self._backoff_max = _non_negative_int(
            receive_backoff_max_ms, "receive_backoff_max_ms"
        )
        if self._backoff_max < self._backoff_initial:
            raise TypeError(
                "receive_backoff_max_ms must be >= receive_backoff_initial_ms"
            )
        if (
            not isinstance(receive_backoff_multiplier, (int, float))
            or receive_backoff_multiplier < 1
        ):
            raise TypeError("receive_backoff_multiplier must be >= 1")
        if not isinstance(receive_backoff_jitter, bool):
            raise TypeError("receive_backoff_jitter must be a boolean")
        self._backoff_multiplier = float(receive_backoff_multiplier)
        self._backoff_jitter = receive_backoff_jitter
        self.lease_heartbeat_ms = _optional_positive_int(
            lease_heartbeat_ms, "lease_heartbeat_ms"
        )
        self.handler_timeout_ms = _optional_positive_int(
            handler_timeout_ms, "handler_timeout_ms"
        )
        self._state = "idle"
        self._task: asyncio.Task[None] | None = None
        self._lanes: list[asyncio.Task[None]] = []
        self._handlers: set[asyncio.Task[None]] = set()
        self._resume = asyncio.Event()
        self._resume.set()
        self._stopping = asyncio.Event()
        self._in_flight = 0

    @property
    def is_running(self) -> bool:
        return self._state in {"running", "paused"}

    @property
    def status(self) -> dict[str, Any]:
        return {
            "state": self._state,
            "is_running": self.is_running,
            "is_paused": self._state == "paused",
            "in_flight": self._in_flight,
            "concurrency": self.concurrency,
        }

    def start(self) -> "QueueConsumer":
        if self._state in {"running", "paused", "stopping"}:
            return self
        self._state = "running"
        self._stopping.clear()
        self._resume.set()
        self._task = asyncio.create_task(self._run())
        return self

    async def stop(self) -> None:
        if self._state in {"idle", "stopped", "failed"}:
            return
        self._state = "stopping"
        self._stopping.set()
        self._resume.set()
        current = asyncio.current_task()
        if (
            self._task is not None
            and current is not self._task
            and current not in self._lanes
            and current not in self._handlers
        ):
            await self._task

    def pause(self) -> "QueueConsumer":
        if self._state == "running":
            self._state = "paused"
            self._resume.clear()
        return self

    def resume(self) -> "QueueConsumer":
        if self._state == "paused":
            self._state = "running"
            self._resume.set()
        return self

    async def wait(self) -> None:
        if self._task is not None:
            await self._task

    async def _run(self) -> None:
        try:
            self._lanes = [
                asyncio.create_task(self._run_lane()) for _ in range(self.concurrency)
            ]
            await asyncio.gather(*self._lanes)
            self._state = "stopped"
        except Exception:
            self._stopping.set()
            self._resume.set()
            for lane in self._lanes:
                if not lane.done():
                    lane.cancel()
            await asyncio.gather(*self._lanes, return_exceptions=True)
            self._state = "failed"
            raise
        finally:
            self._lanes = []

    async def _run_lane(self) -> None:
        receive_failures = 0
        while not self._stopping.is_set():
            await self._resume.wait()
            if self._stopping.is_set():
                return
            try:
                rows = await self.queue.read_wait_entries(
                    self.consumer_name, wait_ms=self.wait_ms, group=self.group
                )
            except Exception as error:
                if self._stopping.is_set():
                    return
                await self._report_error(error, None)
                receive_failures += 1
                await self._backoff(receive_failures)
                continue
            receive_failures = 0
            if not rows:
                continue
            await self._process_message(rows[0])

    async def _process_message(self, message: dict[str, Any]) -> None:
        delivery_id = _delivery_id(message.get("delivery_id"))
        self._in_flight += 1
        completed = asyncio.Event()
        handler = asyncio.create_task(self._invoke_handler(message))
        self._handlers.add(handler)
        heartbeat = (
            asyncio.create_task(self._heartbeat(delivery_id, completed))
            if self.lease_heartbeat_ms is not None
            else None
        )
        timeout = (
            asyncio.create_task(asyncio.sleep(self.handler_timeout_ms / 1000))
            if self.handler_timeout_ms is not None
            else None
        )
        watched = {handler}
        if heartbeat is not None:
            watched.add(heartbeat)
        if timeout is not None:
            watched.add(timeout)
        try:
            done, _ = await asyncio.wait(watched, return_when=asyncio.FIRST_COMPLETED)
            if handler in done:
                completed.set()
                if timeout is not None:
                    timeout.cancel()
                if heartbeat is not None:
                    try:
                        await heartbeat
                    except Exception as error:
                        await self._report_error(error, message)
                        return
                error = handler.exception()
                if error is not None:
                    await self.queue.nack(delivery_id, delay=self.retry_delay)
                    await self._report_error(error, message)
                    return
                try:
                    await self.queue.ack(delivery_id)
                except Exception as error:
                    await self._report_error(error, message)
                return

            if heartbeat is not None and heartbeat in done:
                lease_error = heartbeat.exception()
                if lease_error is not None:
                    handler.cancel()
                    with contextlib.suppress(asyncio.CancelledError):
                        await handler
                    if timeout is not None:
                        timeout.cancel()
                    await self._report_error(lease_error, message)
                    return

            handler.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await handler
            completed.set()
            if heartbeat is not None:
                await heartbeat
            error = RedDBError(
                f"queue handler exceeded {self.handler_timeout_ms}ms",
                code="QUEUE_HANDLER_TIMEOUT",
            )
            await self.queue.nack(delivery_id, delay=self.retry_delay)
            await self._report_error(error, message)
        finally:
            self._handlers.discard(handler)
            completed.set()
            for task in (heartbeat, timeout):
                if task is not None and not task.done():
                    task.cancel()
            self._in_flight -= 1

    async def _invoke_handler(self, message: dict[str, Any]) -> None:
        result = self.handle_message(message)
        if inspect.isawaitable(result):
            await result

    async def _heartbeat(self, delivery_id: str, completed: asyncio.Event) -> None:
        assert self.lease_heartbeat_ms is not None
        while not completed.is_set():
            try:
                await asyncio.wait_for(
                    completed.wait(), timeout=self.lease_heartbeat_ms / 1000
                )
                return
            except asyncio.TimeoutError:
                await self.queue.extend(delivery_id)

    async def _backoff(self, failures: int) -> None:
        delay_ms = min(
            self._backoff_max,
            self._backoff_initial * (self._backoff_multiplier ** max(0, failures - 1)),
        )
        if self._backoff_jitter:
            delay_ms = random.uniform(0, delay_ms)
        if delay_ms <= 0:
            return
        try:
            await asyncio.wait_for(self._stopping.wait(), timeout=delay_ms / 1000)
        except asyncio.TimeoutError:
            pass

    async def _report_error(
        self, error: Exception, message: dict[str, Any] | None
    ) -> None:
        if self.on_error is None:
            raise error
        result = self.on_error(error, message)
        if inspect.isawaitable(result):
            await result


def _non_negative_int(value: Any, name: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < 0:
        raise TypeError(f"queue consumer {name} must be a non-negative integer")
    return value


def _optional_positive_int(value: Any, name: str) -> int | None:
    if value is None:
        return None
    if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
        raise TypeError(f"queue consumer {name} must be a positive integer")
    return value


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
        raise RedDBError(
            "queue delivery is missing delivery_id", code="INVALID_RESPONSE"
        )
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
