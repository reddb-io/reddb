from __future__ import annotations

import httpx
import pytest

from reddb_asyncio import HttpClient, Reddb

pytestmark = pytest.mark.asyncio


async def test_kv_watch_uses_canonical_sse_endpoint_and_yields_events():
    seen: list[httpx.Request] = []

    async def handler(request: httpx.Request) -> httpx.Response:
        seen.append(request)
        assert request.headers["accept"] == "text/event-stream"
        assert request.url.path == "/collections/app/kv/theme/watch"
        return httpx.Response(
            200,
            headers={"content-type": "text/event-stream"},
            text=(
                ": ready\n\n"
                'event: put\n'
                'data: {"collection":"app","key":"theme","value":"dark"}\n\n'
                "data: plain-text\n\n"
            ),
        )

    client = HttpClient(
        base_url="http://testserver",
        client=httpx.AsyncClient(
            base_url="http://testserver",
            transport=httpx.MockTransport(handler),
        ),
    )
    db = Reddb(client)
    try:
        events = [event async for event in db.kv.watch("app", "theme")]
    finally:
        await db.close()

    assert len(seen) == 1
    assert events == [
        {"collection": "app", "key": "theme", "value": "dark"},
        "plain-text",
    ]
