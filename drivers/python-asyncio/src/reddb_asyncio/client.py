"""Top-level :func:`connect` factory + transport-agnostic :class:`Reddb` facade.

Both transports (RedWire TCP/TLS and HTTP/HTTPS) expose the same
async surface: ``query``, ``insert``, ``bulk_insert``, ``get``,
``delete``, ``ping``, ``close``. :class:`Reddb` simply forwards calls
to whichever underlying client :func:`connect` chose for the given
URI.
"""

from __future__ import annotations

from typing import Any

from .documents import DocumentClient
from .errors import RedDBError, UnsupportedScheme
from .http import HttpClient
from .kv import (
    KvClient,
    kv_identifier as _kv_identifier,
    kv_key_segment as _kv_key_segment,
    kv_path as _kv_path,
    kv_tag_literal as _kv_tag_literal,
    kv_value_literal as _kv_value_literal,
)
from .params import normalize_params
from .queue import QueueClient
from .redwire import RedwireClient, RedwireOptions
from .url import ParsedUri, parse_uri

__all__ = ["connect", "Reddb", "KvClient", "DocumentClient", "QueueClient"]


async def connect(uri: str, **opts: Any) -> "Reddb":
    """Connect to a RedDB instance.

    The transport is picked from the URI scheme:

    * ``red://``   — RedWire TCP
    * ``reds://``  — RedWire TLS
    * ``http://``  — REST/HTTP
    * ``https://`` — REST/HTTPS

    Pass ``token=...`` (bearer/api-key), ``username``+``password``
    (SCRAM or HTTP login), or rely on URI-supplied credentials. The
    chosen auth method can be overridden with ``auth=...`` (one of
    ``anonymous``, ``bearer``, ``scram``, ``oauth``).

    Embedded URIs (``red://``, ``red:///path``, ``red://memory``) are
    not supported here — install the maturin ``reddb`` package for
    that.
    """

    parsed = parse_uri(uri)
    if parsed.kind == "embedded":
        raise NotImplementedError(
            "embedded mode is not supported by reddb-asyncio; install the "
            "maturin `reddb` package for in-process databases."
        )

    if parsed.kind in ("redwire", "redwire-tls"):
        return await _connect_redwire(parsed, **opts)
    if parsed.kind in ("http", "https"):
        return await _connect_http(parsed, **opts)
    raise UnsupportedScheme(f"unhandled URI kind: {parsed.kind!r}")


async def _connect_redwire(parsed: ParsedUri, **opts: Any) -> "Reddb":
    auth_choice = opts.get("auth") or parsed.auth or _default_auth_for(parsed, opts)
    rw_opts = RedwireOptions(
        host=parsed.host or "localhost",
        port=parsed.port or 5050,
        auth=auth_choice,
        username=opts.get("username") or parsed.username,
        password=opts.get("password") or parsed.password,
        token=opts.get("token") or parsed.token,
        jwt=opts.get("jwt"),
        client_name=opts.get("client_name", "reddb-asyncio/0.1"),
        use_tls=parsed.kind == "redwire-tls",
        ca=opts.get("ca") or parsed.ca,
        cert=opts.get("cert") or parsed.cert,
        key=opts.get("key") or parsed.key,
        timeout=opts.get("timeout") or (parsed.timeout_ms / 1000.0 if parsed.timeout_ms else 30.0),
    )
    client = await RedwireClient.connect(rw_opts)
    return Reddb(_RedwireAdapter(client))


async def _connect_http(parsed: ParsedUri, **opts: Any) -> "Reddb":
    base = f"{parsed.kind}://{parsed.host}:{parsed.port}"
    timeout = opts.get("timeout") or (
        parsed.timeout_ms / 1000.0 if parsed.timeout_ms else 30.0
    )
    verify = opts.get("verify", True)
    if parsed.ca:
        verify = parsed.ca
    client = HttpClient(
        base_url=base,
        token=opts.get("token") or parsed.token,
        timeout=timeout,
        verify=verify,
    )
    if (opts.get("username") or parsed.username) and (
        opts.get("password") or parsed.password
    ):
        await client.login(
            opts.get("username") or parsed.username or "",
            opts.get("password") or parsed.password or "",
        )
    return Reddb(client)


def _default_auth_for(parsed: ParsedUri, opts: dict[str, Any]) -> str:
    if opts.get("token") or parsed.token:
        return "bearer"
    if (opts.get("username") or parsed.username) and (
        opts.get("password") or parsed.password
    ):
        return "scram"
    return "anonymous"


# ---------------------------------------------------------------------------
# Adapter to give RedwireClient the same method names HttpClient already has.
# ---------------------------------------------------------------------------


class _RedwireAdapter:
    """Wraps :class:`RedwireClient` so :class:`Reddb` can speak to either."""

    def __init__(self, client: RedwireClient) -> None:
        self._client = client

    async def query(self, sql: str, params: list[Any] | None = None) -> dict[str, Any]:
        return await self._client.query(sql, params)

    async def execute(self, sql: str, params: list[Any] | None = None) -> dict[str, Any]:
        return await self.query(sql, params)

    async def insert(self, collection: str, payload: dict[str, Any]) -> dict[str, Any]:
        return await self._client.insert(collection, payload)

    async def bulk_insert(self, collection: str, payloads: list[dict[str, Any]]) -> dict[str, Any]:
        return await self._client.bulk_insert(collection, payloads)

    async def get(self, collection: str, doc_id: str) -> dict[str, Any]:
        return await self._client.get(collection, doc_id)

    async def delete(self, collection: str, doc_id: str) -> dict[str, Any]:
        return await self._client.delete(collection, doc_id)

    async def ping(self) -> dict[str, Any]:
        await self._client.ping()
        return {"ok": True}

    async def close(self) -> None:
        await self._client.close()

    @property
    def session(self) -> dict[str, Any]:
        return self._client.session


# ---------------------------------------------------------------------------
# Reddb facade.
# ---------------------------------------------------------------------------


class Reddb:
    """Transport-agnostic handle returned by :func:`connect`.

    All methods are async. Use :meth:`close` (or ``async with``) to
    release the underlying socket.
    """

    def __init__(self, transport: Any) -> None:
        self._t = transport
        self.kv = KvClient(transport)
        self.documents = DocumentClient(self)
        self.queue = QueueClient(transport)

    async def __aenter__(self) -> "Reddb":
        return self

    async def __aexit__(self, exc_type, exc, tb) -> None:
        await self.close()

    async def query(
        self,
        sql: str,
        params: list[Any] | tuple[Any, ...] | None = None,
    ) -> dict[str, Any]:
        normalized = normalize_params(params)
        return await self._t.query(sql, normalized if normalized else None)

    async def execute(
        self,
        sql: str,
        params: list[Any] | tuple[Any, ...] | None = None,
    ) -> dict[str, Any]:
        return await self.query(sql, params)

    async def insert(self, collection: str, payload: dict[str, Any]) -> dict[str, Any]:
        result = await self._t.insert(collection, payload)
        return _normalize_insert(result)

    async def bulk_insert(self, collection: str, payloads: list[dict[str, Any]]) -> dict[str, Any]:
        if not payloads:
            raise RedDBError(
                "bulk_insert requires at least one payload",
                code="INVALID_ARGUMENT",
            )
        result = await self._t.bulk_insert(collection, payloads)
        return _normalize_bulk_insert(result, len(payloads))

    async def exists(self, collection: str, rid: Any) -> dict[str, bool]:
        try:
            result = await self._t.get(collection, str(rid))
        except Exception as exc:
            code = getattr(exc, "code", "") or ""
            if "NOT_FOUND" in code or getattr(exc, "status", None) == 404:
                return {"exists": False}
            raise
        if isinstance(result, dict):
            entity = result.get("entity", result)
            return {"exists": entity is not None and entity != {}}
        return {"exists": result is not None}

    async def transaction(self, callback: Any) -> Any:
        if not callable(callback):
            raise RedDBError(
                "transaction(callback) requires a callable",
                code="INVALID_ARGUMENT",
            )
        await self.query("BEGIN")
        try:
            outcome = await callback(self)
            await self.query("COMMIT")
            return outcome
        except BaseException:
            try:
                await self.query("ROLLBACK")
            except Exception:
                pass
            raise

    async def get(self, collection: str, doc_id: str) -> dict[str, Any]:
        return await self._t.get(collection, doc_id)

    async def delete(self, collection: str, doc_id: str) -> dict[str, Any]:
        return await self._t.delete(collection, doc_id)

    async def ping(self) -> dict[str, Any]:
        return await self._t.ping()

    async def close(self) -> None:
        await self._t.close()

    @property
    def transport(self) -> Any:
        """Underlying transport handle (RedwireClient / HttpClient)."""
        return self._t


def _normalize_insert(result: Any) -> dict[str, Any]:
    if not isinstance(result, dict):
        raise RedDBError(
            "insert() expected a dict response with rid",
            code="INVALID_RESPONSE",
        )
    rid = result.get("rid") if result.get("rid") is not None else result.get("id")
    if rid is None:
        raise RedDBError(
            "insert() response missing rid (engine too old?)",
            code="INVALID_RESPONSE",
        )
    out = dict(result)
    out["rid"] = rid
    out.setdefault("id", rid)
    out.setdefault("affected", 1)
    return out


def _normalize_bulk_insert(result: Any, expected: int) -> dict[str, Any]:
    if not isinstance(result, dict):
        raise RedDBError(
            "bulk_insert() expected a dict response with rids",
            code="INVALID_RESPONSE",
        )
    rids = result.get("rids")
    if rids is None:
        rids = result.get("ids")
    if not isinstance(rids, list):
        raise RedDBError(
            "bulk_insert() response missing rids (engine too old?)",
            code="INVALID_RESPONSE",
        )
    if len(rids) != expected:
        raise RedDBError(
            f"bulk_insert() expected {expected} rids, got {len(rids)}",
            code="INVALID_RESPONSE",
        )
    out = dict(result)
    out["rids"] = list(rids)
    out["ids"] = list(rids)
    out.setdefault("affected", expected)
    return out
