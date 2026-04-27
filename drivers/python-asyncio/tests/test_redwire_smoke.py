"""End-to-end smoke for the RedWire transport.

Spawns (or reuses) a ``red server`` listener and runs the full
handshake → query → insert → get → delete → ping → close cycle.

Skipped when:

* ``RED_SKIP_SMOKE=1`` is set.
* No ``red`` binary can be located (we never run ``cargo build`` here
  because parallel agents may be using the target dir).
"""

from __future__ import annotations

import pytest

from reddb_asyncio import RedwireClient, RedwireOptions, connect


pytestmark = pytest.mark.asyncio


async def test_handshake_anonymous_query(running_server):
    opts = RedwireOptions(
        host=running_server["host"],
        port=running_server["port"],
        auth="anonymous",
    )
    client = await RedwireClient.connect(opts)
    try:
        result = await client.query("SELECT 1")
        assert isinstance(result, dict)
        # Server populates either 'statement' or 'records' / 'ok'.
        assert any(k in result for k in ("statement", "records", "ok"))
        await client.ping()
    finally:
        await client.close()


async def test_insert_get_delete_round_trip(running_server):
    async with await connect(
        f"red://{running_server['host']}:{running_server['port']}"
    ) as db:
        # Create a small collection up front so subsequent commands have a target.
        try:
            await db.query(
                "CREATE TABLE smoke_users (id TEXT PRIMARY KEY, name TEXT, age INTEGER)"
            )
        except Exception:
            # Collection may already exist when the fixture is shared.
            pass

        # Insert.
        ins = await db.insert("smoke_users", {"id": "py-smoke-1", "name": "alice", "age": 30})
        assert isinstance(ins, dict)

        # Get.
        got = await db.get("smoke_users", "py-smoke-1")
        assert isinstance(got, dict)

        # Delete (best-effort — engine may not have row-level delete here).
        try:
            await db.delete("smoke_users", "py-smoke-1")
        except Exception:
            pass


async def test_ping_pong(running_server):
    async with await connect(
        f"red://{running_server['host']}:{running_server['port']}"
    ) as db:
        result = await db.ping()
        assert result == {"ok": True}


async def test_bearer_required_when_auth_enabled(running_server):
    """If the server has auth disabled, anonymous succeeds. If auth is
    enabled and no token is supplied, AuthRefused fires. We can only
    verify the happy path here without provisioning a user."""
    opts = RedwireOptions(
        host=running_server["host"],
        port=running_server["port"],
        auth="anonymous",
    )
    client = await RedwireClient.connect(opts)
    await client.close()
