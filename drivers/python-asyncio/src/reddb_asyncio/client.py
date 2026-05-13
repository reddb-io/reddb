"""Top-level :func:`connect` factory + transport-agnostic :class:`Reddb` facade.

Both transports (RedWire TCP/TLS and HTTP/HTTPS) expose the same
async surface: ``query``, ``insert``, ``bulk_insert``, ``get``,
``delete``, ``ping``, ``close``. :class:`Reddb` simply forwards calls
to whichever underlying client :func:`connect` chose for the given
URI.
"""

from __future__ import annotations

from typing import Any

from .errors import RedDBError, UnsupportedScheme
from .http import HttpClient
from .params import normalize_params
from .redwire import RedwireClient, RedwireOptions
from .url import ParsedUri, parse_uri

__all__ = ["connect", "Reddb"]


class KvClient:
    def __init__(self, transport: Any, collection: str = "kv_default") -> None:
        self._t = transport
        self.collection = collection

    def watch(self, key: str, **opts: Any):
        collection = opts.pop("collection", self.collection)
        if not hasattr(self._t, "kv_watch"):
            raise RedDBError("kv.watch requires the HTTP transport")
        return self._t.kv_watch(key, collection=collection, **opts)

    def watch_prefix(self, prefix: str, **opts: Any):
        collection = opts.pop("collection", self.collection)
        if not hasattr(self._t, "kv_watch_prefix"):
            raise RedDBError("kv.watch_prefix requires the HTTP transport")
        return self._t.kv_watch_prefix(prefix, collection=collection, **opts)

    async def put(self, key: str, value: Any, **opts: Any) -> dict[str, Any]:
        collection = opts.pop("collection", self.collection)
        tags = opts.pop("tags", None) or []
        expire_ms = opts.pop("expire_ms", None)
        expire = f" EXPIRE {int(expire_ms)} ms" if expire_ms is not None else ""
        tag_clause = (
            " TAGS [" + ", ".join(_kv_tag_literal(tag) for tag in tags) + "]" if tags else ""
        )
        sql = f"KV PUT {_kv_path(collection, key)} = {_kv_value_literal(value)}{expire}{tag_clause}"
        return await self._t.query(sql)

    async def invalidate_tags(self, tags: list[str], **opts: Any) -> int:
        collection = opts.pop("collection", self.collection)
        sql = (
            "INVALIDATE TAGS ["
            + ", ".join(_kv_tag_literal(tag) for tag in tags)
            + f"] FROM {_kv_identifier(collection)}"
        )
        result = await self._t.query(sql)
        rows = result.get("rows") if isinstance(result, dict) else None
        if rows:
            return int(rows[0].get("invalidated", 0))
        return int(result.get("affected", 0)) if isinstance(result, dict) else 0


def _kv_path(collection: str, key: str) -> str:
    return f"{_kv_identifier(collection)}.{_kv_identifier(key)}"


def _kv_identifier(value: Any) -> str:
    return "".join(ch if ch.isalnum() or ch == "_" else "_" for ch in str(value))


def _kv_value_literal(value: Any) -> str:
    if value is None:
        return "NULL"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    return "'" + str(value).replace("'", "''") + "'"


def _kv_tag_literal(value: Any) -> str:
    return "'" + str(value).replace("'", "''") + "'"


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
        return await self._t.insert(collection, payload)

    async def bulk_insert(self, collection: str, payloads: list[dict[str, Any]]) -> dict[str, Any]:
        return await self._t.bulk_insert(collection, payloads)

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
