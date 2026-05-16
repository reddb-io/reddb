"""Queue helpers for the asyncio driver.

Mirrors the SDK Helper Spec ``queue.*`` surface: push/pop/peek/len/purge.
"""

from __future__ import annotations

import json as _json
from typing import Any

from .errors import RedDBError
from .sqlutil import sql_identifier


__all__ = ["QueueClient"]


class QueueClient:
    """Queue namespace bound to a single underlying transport."""

    def __init__(self, transport: Any) -> None:
        self._t = transport

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
        return "'" + value.replace("'", "''") + "'"
    return _json.dumps(value, separators=(",", ":"))


def _queue_payloads(result: Any) -> list[Any]:
    if not isinstance(result, dict):
        return []
    rows = result.get("rows")
    if not isinstance(rows, list):
        return []
    return [row.get("payload") for row in rows]
