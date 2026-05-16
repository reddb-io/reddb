"""KV helpers for the asyncio driver.

Implements the SDK Helper Spec ``kv.*`` surface — exact keys, namespaced
keys (``corpus:version``), object and scalar round-tripping.
"""

from __future__ import annotations

from typing import Any

from .errors import RedDBError
from .sqlutil import sql_identifier


__all__ = [
    "KvClient",
    "kv_path",
    "kv_identifier",
    "kv_key_segment",
    "kv_value_literal",
    "kv_tag_literal",
]


class KvClient:
    """KV namespace bound to a single underlying transport."""

    def __init__(self, transport: Any, collection: str = "kv_default") -> None:
        self._t = transport
        self.collection = collection

    # ------------------------------------------------------------------ spec

    async def set(self, key: str, value: Any, **opts: Any) -> dict[str, Any]:
        return await self.put(key, value, **opts)

    async def put(self, key: str, value: Any, **opts: Any) -> dict[str, Any]:
        collection = opts.pop("collection", self.collection)
        tags = opts.pop("tags", None) or []
        expire_ms = opts.pop("expire_ms", None)
        expire = f" EXPIRE {int(expire_ms)} ms" if expire_ms is not None else ""
        tag_clause = (
            " TAGS [" + ", ".join(kv_tag_literal(tag) for tag in tags) + "]"
            if tags
            else ""
        )
        sql = (
            f"KV PUT {kv_path(collection, key)} = {kv_value_literal(value)}"
            f"{expire}{tag_clause}"
        )
        return await self._t.query(sql)

    async def get(self, key: str, **opts: Any) -> Any:
        collection = opts.pop("collection", self.collection)
        result = await self._t.query(f"KV GET {kv_path(collection, key)}")
        rows = _rows(result)
        if not rows:
            return None
        return rows[0].get("value")

    async def get_many(self, keys: list[str], **opts: Any) -> list[Any]:
        return [await self.get(key, **opts) for key in keys]

    async def exists(self, key: str, **opts: Any) -> dict[str, bool]:
        value = await self.get(key, **opts)
        return {"exists": value is not None}

    async def delete(self, key: str, **opts: Any) -> dict[str, int]:
        collection = opts.pop("collection", self.collection)
        result = await self._t.query(f"KV DELETE {kv_path(collection, key)}")
        affected = 0
        if isinstance(result, dict):
            affected = int(
                result.get("affected")
                or result.get("affected_rows")
                or 0
            )
        return {"affected": affected}

    async def list(self, **opts: Any) -> dict[str, list[dict[str, Any]]]:
        collection = opts.pop("collection", self.collection)
        limit = opts.pop("limit", 100)
        if not isinstance(limit, int) or isinstance(limit, bool) or limit <= 0:
            raise RedDBError(
                "kv.list limit must be a positive integer",
                code="INVALID_ARGUMENT",
            )
        prefix = opts.pop("prefix", None)
        sql = (
            f"SELECT key, value FROM {sql_identifier(collection)} "
            f"ORDER BY key ASC LIMIT {int(limit)}"
        )
        result = await self._t.query(sql)
        rows = _rows(result)
        if prefix is not None and str(prefix) != "":
            prefix_str = str(prefix)
            rows = [r for r in rows if str(r.get("key", "")).startswith(prefix_str)]
        return {"items": rows}

    async def invalidate_tags(self, tags: list[str], **opts: Any) -> int:
        collection = opts.pop("collection", self.collection)
        sql = (
            "INVALIDATE TAGS ["
            + ", ".join(kv_tag_literal(tag) for tag in tags)
            + f"] FROM {sql_identifier(collection)}"
        )
        result = await self._t.query(sql)
        rows = _rows(result)
        if rows:
            return int(rows[0].get("invalidated", 0))
        return int(result.get("affected", 0)) if isinstance(result, dict) else 0

    # ----------------------------------------------------------------- watch

    def watch(self, key: str, **opts: Any):
        collection = opts.pop("collection", self.collection)
        if not hasattr(self._t, "kv_watch"):
            raise RedDBError(
                "kv.watch requires the HTTP transport",
                code="UNSUPPORTED_TRANSPORT",
            )
        return self._t.kv_watch(key, collection=collection, **opts)

    def watch_prefix(self, prefix: str, **opts: Any):
        collection = opts.pop("collection", self.collection)
        if not hasattr(self._t, "kv_watch_prefix"):
            raise RedDBError(
                "kv.watch_prefix requires the HTTP transport",
                code="UNSUPPORTED_TRANSPORT",
            )
        return self._t.kv_watch_prefix(prefix, collection=collection, **opts)


# ---------------------------------------------------------------------------
# Pure helpers (unit-testable without a running server)
# ---------------------------------------------------------------------------


def kv_path(collection: str, key: str) -> str:
    return f"{kv_identifier(collection)}.{kv_key_segment(key)}"


def kv_identifier(value: Any) -> str:
    ident = str(value)
    bad = [ch for ch in ident if not (ch.isalnum() or ch == "_")]
    if bad:
        raise RedDBError(
            f'invalid KV collection "{ident}": character "{bad[0]}" is not supported',
            code="INVALID_KV_KEY",
        )
    return ident


def kv_key_segment(value: Any) -> str:
    key = str(value)
    if key and all(ch.isalnum() or ch == "_" for ch in key):
        return key
    return "'" + key.replace("'", "''") + "'"


def kv_value_literal(value: Any) -> str:
    if value is None:
        return "NULL"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    if isinstance(value, (dict, list, tuple)):
        import json as _json

        return "'" + _json.dumps(value, separators=(",", ":")).replace("'", "''") + "'"
    return "'" + str(value).replace("'", "''") + "'"


def kv_tag_literal(value: Any) -> str:
    return "'" + str(value).replace("'", "''") + "'"


def _rows(result: Any) -> list[dict[str, Any]]:
    if not isinstance(result, dict):
        return []
    rows = result.get("rows")
    return list(rows) if isinstance(rows, list) else []
