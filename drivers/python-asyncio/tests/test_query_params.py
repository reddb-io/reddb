from __future__ import annotations

import httpx
import pytest

from reddb_asyncio import HttpClient, RedDBError, Reddb
from reddb_asyncio.client import _RedwireAdapter


pytestmark = pytest.mark.asyncio


async def test_http_query_sends_params_array() -> None:
    async def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == "/query"
        assert request.method == "POST"
        assert request.read() == (
            b'{"query":"SELECT * FROM users WHERE id = $1 AND name = $2",'
            b'"params":[1,"Alice"]}'
        )
        return httpx.Response(200, json={"rows": [{"id": 1, "name": "Alice"}]})

    client = HttpClient(
        base_url="http://red.local",
        client=httpx.AsyncClient(
            base_url="http://red.local",
            transport=httpx.MockTransport(handler),
        ),
    )

    result = await client.query(
        "SELECT * FROM users WHERE id = $1 AND name = $2",
        [1, "Alice"],
    )

    assert result["rows"] == [{"id": 1, "name": "Alice"}]
    await client.close()


async def test_redwire_facade_rejects_non_empty_params() -> None:
    class FakeRedwireTransport:
        async def query(self, sql: str) -> dict[str, object]:
            return {"query": sql}

    db = Reddb(_RedwireAdapter(FakeRedwireTransport()))

    with pytest.raises(RedDBError) as exc:
        await db.query("SELECT * FROM users WHERE id = $1", [1])

    assert exc.value.code == "PARAMS_UNSUPPORTED"


async def test_empty_params_keep_legacy_query_path() -> None:
    class FakeRedwireTransport:
        async def query(self, sql: str) -> dict[str, object]:
            return {"query": sql}

    db = Reddb(_RedwireAdapter(FakeRedwireTransport()))

    assert await db.query("SELECT 1", []) == {"query": "SELECT 1"}
