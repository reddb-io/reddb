from __future__ import annotations

import httpx
import pytest

from reddb_asyncio.errors import HttpError
from reddb_asyncio.http import HttpClient, _parse_response


def test_default_http_client_enables_http2() -> None:
    client = HttpClient(base_url="https://example.test")
    pool = client._client._transport._pool  # type: ignore[attr-defined]
    assert pool._http2 is True


def test_structured_overload_is_machine_actionable() -> None:
    response = httpx.Response(
        503,
        headers={"retry-after": "5"},
        json={
            "error": "server at capacity",
            "error_code": "execution_queue_full",
            "detail": "engine execution queue is full",
            "retry_after_secs": 5,
        },
    )

    with pytest.raises(HttpError) as raised:
        _parse_response(response)

    assert raised.value.code == "execution_queue_full"
    assert raised.value.retry_after_secs == 5.0
    assert raised.value.details["detail"] == "engine execution queue is full"


def test_nested_principal_overload_code_is_preserved() -> None:
    response = httpx.Response(
        429,
        json={
            "error": {
                "code": "principal_inflight_exhausted",
                "message": "per-principal in-flight request cap exceeded",
            }
        },
    )

    with pytest.raises(HttpError) as raised:
        _parse_response(response)

    assert raised.value.code == "principal_inflight_exhausted"
