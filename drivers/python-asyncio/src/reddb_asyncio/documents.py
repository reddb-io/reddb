"""Document helpers for the asyncio driver.

Mirrors the SDK Helper Spec ``documents.*`` surface used by the JS
driver. All helpers run over the same transport as :class:`Reddb`, so
both RedWire and HTTP work transparently.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from .errors import RedDBError
from .sqlutil import (
    sql_identifier,
    sql_identifier_path,
    sql_json_inline_literal,
    sql_value_literal,
)

if TYPE_CHECKING:
    from .client import Reddb


class DocumentClient:
    """Document namespace bound to a :class:`Reddb` instance."""

    def __init__(self, db: "Reddb") -> None:
        self._db = db

    async def insert(self, collection: str, document: dict[str, Any]) -> dict[str, Any]:
        _ensure_object(document, "documents.insert document")
        await self._ensure_collection(collection)
        sql = (
            f"INSERT INTO {sql_identifier_path(collection)} DOCUMENT "
            f"VALUES ({sql_json_inline_literal(document)}) RETURNING *"
        )
        result = await self._db.query(sql)
        item = _first_row(result)
        if not item or item.get("rid") is None:
            raise RedDBError(
                "documents.insert expected one returned item with rid",
                code="INVALID_RESPONSE",
            )
        return {
            "affected": result.get("affected", 1),
            "rid": item["rid"],
            "item": item,
        }

    async def get(self, collection: str, rid: Any) -> dict[str, Any]:
        result = await self._db.get(collection, str(rid))
        entity = result.get("entity") if isinstance(result, dict) else None
        if not entity:
            raise RedDBError(
                f"document {rid!s} was not found",
                code="NOT_FOUND",
            )
        return entity

    async def list(self, collection: str, **options: Any) -> dict[str, Any]:
        limit = _normalize_limit(options.get("limit"))
        order_by = options.get("order_by") or options.get("orderBy") or "rid ASC"
        where_clause = options.get("filter")
        where = f" WHERE {where_clause}" if where_clause else ""
        sql = (
            f"SELECT * FROM {sql_identifier_path(collection)}{where} "
            f"ORDER BY {order_by} LIMIT {limit}"
        )
        result = await self._db.query(sql)
        return {"items": _rows(result)}

    async def patch(
        self, collection: str, rid: Any, patch: dict[str, Any]
    ) -> dict[str, Any]:
        _ensure_object(patch, "documents.patch patch")
        if not patch:
            return await self.get(collection, rid)
        for field in patch.keys():
            if "/" in field:
                raise RedDBError(
                    "documents.patch currently accepts top-level document fields",
                    code="INVALID_ARGUMENT",
                )
        assignments = ", ".join(
            f"{sql_identifier(field)} = {sql_value_literal(value)}"
            for field, value in patch.items()
        )
        sql = (
            f"UPDATE {sql_identifier_path(collection)} SET {assignments} "
            f"WHERE rid = $1 RETURNING *"
        )
        result = await self._db.query(sql, [rid])
        item = _first_row(result)
        if not item:
            raise RedDBError(
                f"document {rid!s} was not found",
                code="NOT_FOUND",
            )
        return item

    async def delete(self, collection: str, rid: Any) -> dict[str, Any]:
        result = await self._db.delete(collection, str(rid))
        affected = (
            result.get("affected") if isinstance(result, dict) else None
        )
        return {"affected": int(affected or 0)}

    async def _ensure_collection(self, collection: str) -> None:
        try:
            await self._db.query(f"CREATE DOCUMENT {sql_identifier_path(collection)}")
        except Exception as exc:
            message = str(exc)
            if "already exists" not in message:
                raise


def _ensure_object(value: Any, label: str) -> None:
    if not isinstance(value, dict):
        raise RedDBError(f"{label} must be an object", code="INVALID_ARGUMENT")


def _normalize_limit(value: Any) -> int:
    if value is None:
        return 100
    if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
        raise RedDBError(
            "limit must be a positive integer", code="INVALID_ARGUMENT"
        )
    return value


def _rows(result: Any) -> list[dict[str, Any]]:
    if not isinstance(result, dict):
        return []
    rows = result.get("rows")
    return list(rows) if isinstance(rows, list) else []


def _first_row(result: Any) -> dict[str, Any] | None:
    rows = _rows(result)
    return rows[0] if rows else None


__all__ = ["DocumentClient"]
