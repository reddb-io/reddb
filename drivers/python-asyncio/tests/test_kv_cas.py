from __future__ import annotations

import json
from typing import Any

import httpx
import pytest

from reddb_asyncio.client import KvClient, Reddb
from reddb_asyncio.http import HttpClient
from reddb_asyncio.redwire import RedwireClient


pytestmark = pytest.mark.asyncio


class RecordingTransport:
    def __init__(self) -> None:
        self.calls: list[tuple[str, str, Any, Any, Any]] = []

    async def kv_cas(
        self,
        collection: str,
        key: str,
        expected: Any,
        value: Any,
        ttl_ms: int | None = None,
    ) -> dict[str, Any]:
        self.calls.append((collection, key, expected, value, ttl_ms))
        return {"ok": True, "current": value}


async def test_kv_facade_exposes_cas_and_compare_and_set_alias() -> None:
    transport = RecordingTransport()
    kv = KvClient(transport)

    assert await kv.cas("app", 7, "old", "new", 250) == {"ok": True, "current": "new"}
    assert await kv.compare_and_set("app", "theme", None, {"mode": "dark"}) == {
        "ok": True,
        "current": {"mode": "dark"},
    }
    assert transport.calls == [
        ("app", "7", "old", "new", 250),
        ("app", "theme", None, {"mode": "dark"}, None),
    ]


async def test_redwire_kv_cas_uses_runtime_cas_sql() -> None:
    seen: list[str] = []

    async def query(sql: str) -> dict[str, Any]:
        seen.append(sql)
        return {"rows": [{"ok": True, "current": "new"}]}

    client = object.__new__(RedwireClient)
    client.query = query  # type: ignore[method-assign]

    assert await client.kv_cas("app", "theme", "old", "new", 500) == {
        "rows": [{"ok": True, "current": "new"}]
    }
    assert seen == ["CAS app.'theme' EXPECT 'old' SET 'new' EXPIRE 500 ms"]


async def test_http_kv_cas_posts_runtime_cas_query() -> None:
    seen: list[dict[str, Any]] = []

    async def handler(request: httpx.Request) -> httpx.Response:
        seen.append(json.loads(request.content.decode("utf-8")))
        return httpx.Response(200, json={"ok": True, "result": {"rows": [{"ok": True}]}})

    async_client = httpx.AsyncClient(
        base_url="http://testserver",
        transport=httpx.MockTransport(handler),
    )
    client = HttpClient(base_url="http://testserver", client=async_client)
    try:
        assert await client.kv_cas("app", "theme", "old", "new") == {
            "rows": [{"ok": True}]
        }
    finally:
        await client.close()
        await async_client.aclose()

    assert seen == [{"query": "CAS app.'theme' EXPECT 'old' SET 'new'"}]


async def test_reddb_facade_wires_kv_cas_transport() -> None:
    transport = RecordingTransport()
    db = Reddb(transport)

    assert await db.kv.cas("app", "feature", False, True) == {"ok": True, "current": True}
    assert transport.calls == [("app", "feature", False, True, None)]
