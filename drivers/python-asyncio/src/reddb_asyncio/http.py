"""HTTP/HTTPS transport for the asyncio driver.

The RedDB HTTP server exposes a REST surface that mirrors the
RedWire data-plane (see ``src/server/routing.rs``). This module
keeps the same public method names as :class:`RedwireClient` so the
top-level ``Reddb`` facade can route either way without callers
caring.

The primary endpoint mapping matches what the JS driver speaks
(``drivers/js/src/http.js``):

==========================  =================================================
Method                      Endpoint
==========================  =================================================
``query``                   ``POST /query`` (body ``{"query": sql}``)
``insert``                  ``POST /collections/{name}/rows``
``bulk_insert``             ``POST /collections/{name}/bulk/rows``
``scan``                    ``GET  /collections/{name}/scan?limit=...``
``get``                     ``GET  /collections/{name}/{id}``
``delete``                  ``DELETE /collections/{name}/{id}``
``ping`` / health           ``GET  /health``
``login``                   ``POST /auth/login``
==========================  =================================================
"""

from __future__ import annotations

from typing import Any
from urllib.parse import quote

import httpx

from .errors import EngineError, HttpError

DEFAULT_TIMEOUT_SECS: float = 30.0


class HttpClient:
    """Thin async HTTP client matching the RedWire surface.

    ``base_url`` should NOT include a trailing slash; we strip it just
    in case. Pass ``token`` to attach ``Authorization: Bearer ...`` to
    every request.
    """

    def __init__(
        self,
        *,
        base_url: str,
        token: str | None = None,
        timeout: float = DEFAULT_TIMEOUT_SECS,
        verify: bool | str = True,
        client: httpx.AsyncClient | None = None,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self._token = token
        self._owns_client = client is None
        if client is None:
            client = httpx.AsyncClient(
                base_url=self.base_url,
                timeout=timeout,
                verify=verify,
                http2=False,
                headers={"accept": "application/json"},
            )
        self._client = client

    @property
    def token(self) -> str | None:
        return self._token

    def set_token(self, token: str | None) -> None:
        self._token = token

    async def close(self) -> None:
        if self._owns_client:
            await self._client.aclose()

    # ------------------------------------------------------------------ auth

    async def login(self, username: str, password: str) -> dict[str, Any]:
        """POST /auth/login → returns the parsed response. Stores the token
        on the client so subsequent calls carry it automatically.
        """
        body = await self._post_json("/auth/login", {"username": username, "password": password})
        token = body.get("token") if isinstance(body, dict) else None
        if isinstance(token, str):
            self.set_token(token)
        return body if isinstance(body, dict) else {"raw": body}

    # ------------------------------------------------------------------ ops

    async def query(self, sql: str) -> dict[str, Any]:
        return await self._post_json("/query", {"query": sql})

    async def insert(self, collection: str, payload: dict[str, Any]) -> dict[str, Any]:
        return await self._post_json(f"/collections/{quote(collection)}/rows", payload)

    async def bulk_insert(self, collection: str, payloads: list[dict[str, Any]]) -> dict[str, Any]:
        return await self._post_json(
            f"/collections/{quote(collection)}/bulk/rows", {"rows": payloads}
        )

    async def get(self, collection: str, doc_id: str) -> dict[str, Any]:
        url = f"/collections/{quote(collection)}/{quote(str(doc_id))}"
        return await self._request("GET", url)

    async def delete(self, collection: str, doc_id: str) -> dict[str, Any]:
        url = f"/collections/{quote(collection)}/{quote(str(doc_id))}"
        return await self._request("DELETE", url)

    async def scan(self, collection: str, **params: Any) -> dict[str, Any]:
        url = f"/collections/{quote(collection)}/scan"
        return await self._request("GET", url, params=params or None)

    async def ping(self) -> dict[str, Any]:
        return await self._request("GET", "/health")

    async def health(self) -> dict[str, Any]:  # alias
        return await self.ping()

    # ----------------------------------------------------------------- low-

    def _headers(self) -> dict[str, str]:
        h: dict[str, str] = {}
        if self._token:
            h["authorization"] = f"Bearer {self._token}"
        return h

    async def _request(
        self,
        method: str,
        path: str,
        *,
        json_body: Any | None = None,
        params: dict[str, Any] | None = None,
    ) -> Any:
        try:
            response = await self._client.request(
                method,
                path,
                json=json_body,
                params=params,
                headers=self._headers(),
            )
        except httpx.HTTPError as exc:  # pragma: no cover - network
            raise HttpError(str(exc), status=0, body="") from exc
        return _parse_response(response)

    async def _post_json(self, path: str, body: Any) -> Any:
        headers = {**self._headers(), "content-type": "application/json"}
        try:
            response = await self._client.post(path, json=body, headers=headers)
        except httpx.HTTPError as exc:  # pragma: no cover - network
            raise HttpError(str(exc), status=0, body="") from exc
        return _parse_response(response)


def _parse_response(response: httpx.Response) -> Any:
    text = response.text
    parsed: Any = None
    if text:
        try:
            parsed = response.json()
        except Exception:
            parsed = {"raw": text}
    if response.is_error:
        code = (
            parsed.get("error_code") if isinstance(parsed, dict) else None
        ) or f"HTTP_{response.status_code}"
        message = (
            (parsed.get("error") if isinstance(parsed, dict) else None)
            or (parsed.get("message") if isinstance(parsed, dict) else None)
            or f"request failed with status {response.status_code}"
        )
        raise HttpError(message, status=response.status_code, body=text)
    if isinstance(parsed, dict) and "ok" in parsed:
        if parsed["ok"] is False:
            raise EngineError(
                parsed.get("error") or "RPC error",
                code=parsed.get("error_code", "RPC_ERROR"),
                payload=parsed,
            )
        return parsed.get("result", parsed)
    return parsed


__all__ = ["HttpClient", "DEFAULT_TIMEOUT_SECS"]
