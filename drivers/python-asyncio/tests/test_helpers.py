"""Unit tests for the rich SDK helpers (SDK Helper Spec v0.1).

These tests do not need a running server — they exercise SQL / path
generation and dispatch through a stub transport.
"""

from __future__ import annotations

from typing import Any

import pytest

from reddb_asyncio import RedDBError, Reddb
from reddb_asyncio.kv import (
    KvClient,
    kv_key_segment,
    kv_path,
    kv_value_literal,
)
from reddb_asyncio.queue import QueueClient
from reddb_asyncio.documents import DocumentClient


class FakeTransport:
    """Records every SQL/RPC call and returns scripted responses."""

    def __init__(self, replies: list[Any] | None = None) -> None:
        self.calls: list[tuple[str, tuple[Any, ...], dict[str, Any]]] = []
        self.replies = list(replies or [])

    async def query(self, sql: str, params: Any | None = None) -> Any:
        self.calls.append(("query", (sql,), {"params": params}))
        return self._pop({"rows": [], "affected": 0})

    async def insert(self, collection: str, payload: dict[str, Any]) -> Any:
        self.calls.append(("insert", (collection, payload), {}))
        return self._pop({"rid": "rid-1", "affected": 1})

    async def bulk_insert(self, collection: str, payloads: list[Any]) -> Any:
        self.calls.append(("bulk_insert", (collection, payloads), {}))
        return self._pop({"rids": [f"rid-{i}" for i in range(len(payloads))], "affected": len(payloads)})

    async def get(self, collection: str, rid: str) -> Any:
        self.calls.append(("get", (collection, rid), {}))
        return self._pop({"entity": {"rid": rid}})

    async def delete(self, collection: str, rid: str) -> Any:
        self.calls.append(("delete", (collection, rid), {}))
        return self._pop({"affected": 1})

    async def ping(self) -> Any:
        return {"ok": True}

    async def close(self) -> None:  # pragma: no cover
        pass

    def _pop(self, default: Any) -> Any:
        if self.replies:
            return self.replies.pop(0)
        return default


# ---------------------------------------------------------------- KV helpers


def test_kv_path_quotes_namespaced_keys_without_rewriting():
    assert kv_path("kv_default", "corpus:version") == "kv_default.'corpus:version'"


def test_kv_path_preserves_dots_and_slashes_in_keys():
    assert kv_path("kv_default", "a/b.c") == "kv_default.'a/b.c'"


def test_kv_key_segment_keeps_alnum_unquoted():
    assert kv_key_segment("hansel") == "hansel"
    assert kv_key_segment("with space") == "'with space'"
    assert kv_key_segment("o'reilly") == "'o''reilly'"


def test_kv_value_literal_json_for_objects():
    assert kv_value_literal({"a": 1}) == "'{\"a\":1}'"
    assert kv_value_literal(None) == "NULL"
    assert kv_value_literal(True) == "true"
    assert kv_value_literal(42) == "42"
    assert kv_value_literal("hi") == "'hi'"


async def test_kv_set_emits_exact_key_path():
    transport = FakeTransport()
    kv = KvClient(transport)
    await kv.set("characters:hansel", "ok")
    sql = transport.calls[0][1][0]
    assert "kv_default.'characters:hansel'" in sql
    assert "= 'ok'" in sql


async def test_kv_exists_uses_get_result():
    transport = FakeTransport(replies=[{"rows": [{"value": "v"}]}, {"rows": []}])
    kv = KvClient(transport)
    assert await kv.exists("k") == {"exists": True}
    assert await kv.exists("k2") == {"exists": False}


async def test_kv_list_filters_by_prefix_without_rewriting_keys():
    transport = FakeTransport(replies=[{"rows": [
        {"key": "a:1", "value": 1},
        {"key": "b:1", "value": 2},
        {"key": "a:2", "value": 3},
    ]}])
    kv = KvClient(transport)
    result = await kv.list(prefix="a:")
    assert [row["key"] for row in result["items"]] == ["a:1", "a:2"]


async def test_kv_list_rejects_invalid_limit():
    transport = FakeTransport()
    kv = KvClient(transport)
    with pytest.raises(RedDBError):
        await kv.list(limit=0)


# ----------------------------------------------------------- Queue helpers


async def test_queue_push_emits_priority_and_payload():
    transport = FakeTransport()
    queue = QueueClient(transport)
    await queue.push("jobs", {"id": 1}, priority=5)
    sql = transport.calls[0][1][0]
    assert sql.startswith("QUEUE PUSH jobs ")
    assert "PRIORITY 5" in sql
    assert '{"id":1}' in sql


async def test_queue_len_returns_int():
    transport = FakeTransport(replies=[{"rows": [{"len": 3}]}])
    queue = QueueClient(transport)
    assert await queue.len("jobs") == 3


async def test_queue_pop_returns_payload_list():
    transport = FakeTransport(replies=[
        {"rows": [{"payload": "a"}, {"payload": "b"}]}
    ])
    queue = QueueClient(transport)
    assert await queue.pop("jobs", 2) == ["a", "b"]


async def test_queue_rejects_invalid_count():
    queue = QueueClient(FakeTransport())
    with pytest.raises(RedDBError):
        await queue.pop("jobs", -1)


async def test_queue_rejects_invalid_identifier():
    queue = QueueClient(FakeTransport())
    with pytest.raises(RedDBError):
        await queue.push("bad-name!", "x")


async def test_queue_read_wait_builds_sql_and_returns_payloads():
    transport = FakeTransport(replies=[
        {"rows": []},
        {"rows": [{"payload": "x"}]},
        {"rows": []},
    ])
    queue = QueueClient(transport)

    # Timeout returns empty list — same shape as an empty pop.
    assert await queue.read_wait("jobs", "worker1", wait_ms=0) == []
    # Available message returns payloads.
    assert await queue.read_wait("jobs", "worker1", wait_ms=30_000) == ["x"]
    # GROUP + COUNT both flow into the SQL.
    await queue.read_wait("jobs", "worker1", wait_ms=5000, group="g", count=4)

    assert [c[1][0] for c in transport.calls] == [
        "QUEUE READ jobs CONSUMER worker1 WAIT 0ms",
        "QUEUE READ jobs CONSUMER worker1 WAIT 30000ms",
        "QUEUE READ jobs GROUP g CONSUMER worker1 COUNT 4 WAIT 5000ms",
    ]


async def test_queue_read_wait_requires_non_negative_wait_ms():
    queue = QueueClient(FakeTransport())
    with pytest.raises(RedDBError):
        await queue.read_wait("jobs", "w", wait_ms=-1)
    with pytest.raises(RedDBError):
        await queue.read_wait("jobs", "w", wait_ms=1.5)  # type: ignore[arg-type]


# ---------------------------------------------------------- Document helpers


async def test_documents_insert_returns_rid_envelope():
    transport = FakeTransport(replies=[
        # CREATE DOCUMENT (succeeds, no rows)
        {"rows": [], "affected": 0},
        # INSERT RETURNING *
        {"rows": [{"rid": "doc-1", "body": {"name": "alice"}}], "affected": 1},
    ])
    db = Reddb(transport)
    result = await db.documents.insert("people", {"name": "alice"})
    assert result == {
        "affected": 1,
        "rid": "doc-1",
        "item": {"rid": "doc-1", "body": {"name": "alice"}},
    }
    # ADR 0067 (#1709): the emitted INSERT uses the inline JSON literal form
    # with no (body) column list and no quoted-string body coercion.
    insert_sql = next(
        call[1][0] for call in transport.calls
        if call[0] == "query" and call[1][0].startswith("INSERT")
    )
    assert insert_sql == (
        'INSERT INTO people DOCUMENT VALUES ({"name":"alice"}) RETURNING *'
    )
    assert "(body)" not in insert_sql
    assert "('{" not in insert_sql


async def test_documents_get_raises_not_found_on_missing_entity():
    transport = FakeTransport(replies=[{"entity": None}])
    db = Reddb(transport)
    with pytest.raises(RedDBError) as exc:
        await db.documents.get("people", "doc-1")
    assert exc.value.code == "NOT_FOUND"


async def test_documents_patch_rejects_json_pointer_paths():
    db = Reddb(FakeTransport())
    with pytest.raises(RedDBError) as exc:
        await db.documents.patch("people", "doc-1", {"a/b": 1})
    assert exc.value.code == "INVALID_ARGUMENT"


async def test_documents_list_orders_by_rid_when_unspecified():
    transport = FakeTransport(replies=[{"rows": [{"rid": "a"}, {"rid": "b"}]}])
    db = Reddb(transport)
    result = await db.documents.list("people")
    assert [item["rid"] for item in result["items"]] == ["a", "b"]
    sql = transport.calls[0][1][0]
    assert "ORDER BY rid ASC" in sql


# -------------------------------------------------------- Reddb facade


async def test_insert_normalizes_rid_envelope():
    transport = FakeTransport(replies=[{"id": "rid-1"}])
    db = Reddb(transport)
    result = await db.insert("c", {"x": 1})
    assert result["rid"] == "rid-1"
    assert result["id"] == "rid-1"
    assert result["affected"] == 1


async def test_bulk_insert_normalizes_rids_envelope():
    transport = FakeTransport(replies=[{"ids": ["a", "b"]}])
    db = Reddb(transport)
    result = await db.bulk_insert("c", [{"x": 1}, {"y": 2}])
    assert result["rids"] == ["a", "b"]
    assert result["ids"] == ["a", "b"]
    assert result["affected"] == 2


async def test_bulk_insert_rejects_empty():
    db = Reddb(FakeTransport())
    with pytest.raises(RedDBError):
        await db.bulk_insert("c", [])


async def test_transaction_commits_on_success():
    transport = FakeTransport()
    db = Reddb(transport)

    async def cb(handle):
        await handle.query("SELECT 1")
        return "ok"

    assert await db.transaction(cb) == "ok"
    sqls = [c[1][0] for c in transport.calls]
    assert sqls[0] == "BEGIN"
    assert sqls[-1] == "COMMIT"


async def test_transaction_rolls_back_on_error():
    transport = FakeTransport()
    db = Reddb(transport)

    async def cb(handle):
        raise RuntimeError("boom")

    with pytest.raises(RuntimeError):
        await db.transaction(cb)
    sqls = [c[1][0] for c in transport.calls]
    assert "ROLLBACK" in sqls
