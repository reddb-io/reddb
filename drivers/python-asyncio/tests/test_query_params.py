from __future__ import annotations

import httpx
import pytest

from reddb_asyncio import HttpClient, Reddb
from reddb_asyncio.client import _RedwireAdapter
from reddb_asyncio.redwire import Kind, ValueTag, encode_query_with_params


def test_redwire_query_with_params_codec_encodes_common_values() -> None:
    sql = "SELECT $1, $2, $3, $4"
    sql_bytes = sql.encode()
    payload = encode_query_with_params(sql, [None, True, 42, "Ada"])
    pos = 4 + len(sql_bytes)
    assert payload[:4] == len(sql_bytes).to_bytes(4, "little")
    assert payload[4:pos] == sql_bytes
    assert payload[pos:pos + 4] == (4).to_bytes(4, "little")
    pos += 4
    assert payload[pos] == ValueTag.Null
    assert payload[pos + 1:pos + 3] == bytes([ValueTag.Bool, 1])
    assert payload[pos + 3] == ValueTag.Int
    assert payload[pos + 12] == ValueTag.Text


def test_redwire_query_with_params_kind_is_pinned() -> None:
    assert Kind.QueryWithParams == 0x28


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


async def test_redwire_facade_forwards_non_empty_params() -> None:
    class FakeRedwireTransport:
        async def query(self, sql: str, params=None) -> dict[str, object]:
            return {"query": sql, "params": params}

    db = Reddb(_RedwireAdapter(FakeRedwireTransport()))

    assert await db.query("SELECT * FROM users WHERE id = $1", (1,)) == {
        "query": "SELECT * FROM users WHERE id = $1",
        "params": [1],
    }


async def test_empty_params_keep_legacy_query_path() -> None:
    class FakeRedwireTransport:
        async def query(self, sql: str, params=None) -> dict[str, object]:
            return {"query": sql, "params": params}

    db = Reddb(_RedwireAdapter(FakeRedwireTransport()))

    assert await db.query("SELECT 1", []) == {"query": "SELECT 1", "params": None}


async def test_http_query_serializes_bytes_and_datetime_params() -> None:
    import datetime

    async def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == "/query"
        assert request.method == "POST"
        assert request.read() == (
            b'{"query":"SELECT * FROM events WHERE payload = $1 AND at = $2",'
            b'"params":[{"$bytes":"AAEC"},{"$ts":1704164645}]}'
        )
        return httpx.Response(200, json={"rows": []})

    client = HttpClient(
        base_url="http://red.local",
        client=httpx.AsyncClient(
            base_url="http://red.local",
            transport=httpx.MockTransport(handler),
        ),
    )

    await client.query(
        "SELECT * FROM events WHERE payload = $1 AND at = $2",
        [
            b"\x00\x01\x02",
            datetime.datetime(2024, 1, 2, 3, 4, 5, tzinfo=datetime.timezone.utc),
        ],
    )
    await client.close()


async def test_execute_alias_and_call_site_type_error() -> None:
    class FakeTransport:
        async def query(self, sql: str, params=None) -> dict[str, object]:
            return {"query": sql, "params": params}

    db = Reddb(FakeTransport())

    assert await db.execute("SELECT $1", params=(42,)) == {
        "query": "SELECT $1",
        "params": [42],
    }
    with pytest.raises(TypeError):
        await db.query("SELECT $1", params={1, 2})
