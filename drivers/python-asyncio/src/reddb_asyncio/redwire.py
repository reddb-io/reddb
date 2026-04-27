"""Pure-asyncio RedWire client.

Speaks the binary protocol defined in
``docs/adr/0001-redwire-tcp-protocol.md`` directly. No threads, no
spawning, no engine code — just :mod:`asyncio` streams.

Public surface:

* :class:`RedwireClient`  — connect-then-talk handle. Supports
  ``query``, ``insert``, ``bulk_insert``, ``get``, ``delete``,
  ``ping``, and ``close``.
* :data:`MAGIC`, :data:`SUPPORTED_VERSION` — protocol constants.
* :data:`Kind`, :data:`Flags` — wire enums.
"""

from __future__ import annotations

import asyncio
import json
import ssl
import struct
from dataclasses import dataclass
from typing import Any

from .errors import (
    AuthRefused,
    CompressedButNoZstd,
    ConnectionClosed,
    EngineError,
    FrameDecompressFailed,
    FrameTooLarge,
    ProtocolError,
    UnknownFlags,
)
from . import scram as scram_lib

# ---------------------------------------------------------------------------
# Protocol constants — keep in sync with src/wire/redwire/{frame,mod}.rs.
# ---------------------------------------------------------------------------

MAGIC: int = 0xFE
SUPPORTED_VERSION: int = 0x01
FRAME_HEADER_SIZE: int = 16
MAX_FRAME_SIZE: int = 16 * 1024 * 1024
KNOWN_FLAGS: int = 0b0000_0011

DEFAULT_TIMEOUT_SECS: float = 30.0


class Kind:
    """Frame kind discriminator. Numeric values are wire-stable."""

    Query = 0x01
    Result = 0x02
    Error = 0x03
    BulkInsert = 0x04
    BulkOk = 0x05
    BulkInsertBinary = 0x06
    QueryBinary = 0x07
    BulkInsertPrevalidated = 0x08
    Hello = 0x10
    HelloAck = 0x11
    AuthRequest = 0x12
    AuthResponse = 0x13
    AuthOk = 0x14
    AuthFail = 0x15
    Bye = 0x16
    Ping = 0x17
    Pong = 0x18
    Get = 0x19
    Delete = 0x1A
    DeleteOk = 0x1B


_KIND_NAMES = {v: k for k, v in vars(Kind).items() if isinstance(v, int)}


def kind_name(value: int) -> str:
    return _KIND_NAMES.get(value, f"0x{value:02x}")


class Flags:
    COMPRESSED = 0b0000_0001
    MORE_FRAMES = 0b0000_0010


# ---------------------------------------------------------------------------
# Frame dataclass + codec.
# ---------------------------------------------------------------------------


@dataclass
class Frame:
    kind: int
    correlation_id: int
    payload: bytes = b""
    flags: int = 0
    stream_id: int = 0


def _zstd_module():
    """Return the ``zstandard`` module if importable, else ``None``."""
    try:
        import zstandard  # type: ignore
    except ImportError:  # pragma: no cover - exercised only without dep
        return None
    return zstandard


def encode_frame(frame: Frame) -> bytes:
    """Serialise a frame.

    When ``Flags.COMPRESSED`` is set the payload is zstd-compressed
    in place. Falls back to plaintext (and clears the flag) if
    ``zstandard`` is not installed.
    """
    payload = frame.payload
    flags = frame.flags & KNOWN_FLAGS
    if flags & Flags.COMPRESSED:
        zstd = _zstd_module()
        if zstd is None:
            flags &= ~Flags.COMPRESSED
        else:
            try:
                cctx = zstd.ZstdCompressor(level=1)
                payload = cctx.compress(frame.payload)
            except Exception:
                # Drop the flag rather than fail outright.
                flags &= ~Flags.COMPRESSED
                payload = frame.payload
    length = FRAME_HEADER_SIZE + len(payload)
    if length > MAX_FRAME_SIZE:
        raise FrameTooLarge(f"frame {length} > {MAX_FRAME_SIZE}")
    header = struct.pack(
        "<IBBHQ",
        length,
        frame.kind,
        flags,
        frame.stream_id,
        frame.correlation_id,
    )
    return header + payload


def decode_frame(buf: bytes) -> Frame:
    """Decode a single complete frame (assumes ``buf`` is exactly one frame)."""
    if len(buf) < FRAME_HEADER_SIZE:
        raise ProtocolError(f"frame header truncated: {len(buf)} bytes")
    length, kind, flags, stream_id, correlation_id = struct.unpack(
        "<IBBHQ", buf[:FRAME_HEADER_SIZE]
    )
    if length < FRAME_HEADER_SIZE or length > MAX_FRAME_SIZE:
        raise ProtocolError(f"invalid frame length {length}")
    if len(buf) < length:
        raise ProtocolError(
            f"payload truncated: expected {length} bytes, got {len(buf)}"
        )
    if flags & ~KNOWN_FLAGS:
        raise UnknownFlags(f"flags=0x{flags:02x}")
    payload = buf[FRAME_HEADER_SIZE:length]
    if flags & Flags.COMPRESSED:
        zstd = _zstd_module()
        if zstd is None:
            raise CompressedButNoZstd(
                "frame has COMPRESSED flag but `zstandard` is not installed"
            )
        try:
            payload = zstd.ZstdDecompressor().decompress(payload)
        except Exception as exc:
            raise FrameDecompressFailed(str(exc)) from exc
    return Frame(
        kind=kind,
        correlation_id=correlation_id,
        payload=bytes(payload),
        flags=flags,
        stream_id=stream_id,
    )


# ---------------------------------------------------------------------------
# Connection options.
# ---------------------------------------------------------------------------


@dataclass
class RedwireOptions:
    host: str
    port: int
    auth: str = "anonymous"  # one of: anonymous, bearer, scram, oauth
    username: str | None = None
    password: str | None = None
    token: str | None = None
    jwt: str | None = None
    client_name: str = "reddb-asyncio/0.1"
    use_tls: bool = False
    ca: str | None = None
    cert: str | None = None
    key: str | None = None
    timeout: float = DEFAULT_TIMEOUT_SECS


# ---------------------------------------------------------------------------
# Client.
# ---------------------------------------------------------------------------


class RedwireClient:
    """Asynchronous RedWire client.

    Use :meth:`connect` to establish a session — it does the magic
    handshake, picks an auth method advertised by the server, runs
    any challenge/response, and leaves the socket open for queries.

    Methods are 1:1 with the wire kinds (:meth:`query`, :meth:`insert`,
    etc.). All of them are coroutines that round-trip a single frame.
    """

    def __init__(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter, opts: RedwireOptions, session: dict[str, Any]) -> None:
        self._reader = reader
        self._writer = writer
        self._opts = opts
        self._session = session
        self._next_corr = 4  # reserved 1..3 for handshake frames
        self._lock = asyncio.Lock()
        self._closed = False

    @property
    def session(self) -> dict[str, Any]:
        """Server-supplied session info from ``AuthOk`` (sub, roles, ...)."""
        return self._session

    @classmethod
    async def connect(cls, opts: RedwireOptions) -> "RedwireClient":
        """Open a TCP/TLS socket, run the handshake, return a client."""
        reader, writer = await _open_socket(opts)
        try:
            session = await _perform_handshake(reader, writer, opts)
        except BaseException:
            writer.close()
            try:
                await writer.wait_closed()
            except Exception:
                pass
            raise
        return cls(reader, writer, opts, session)

    # -- public ops ---------------------------------------------------------

    async def query(self, sql: str) -> dict[str, Any]:
        resp = await self._round_trip(
            Frame(kind=Kind.Query, correlation_id=self._corr(), payload=sql.encode("utf-8"))
        )
        return _expect_json(resp, ok_kinds=(Kind.Result,))

    async def insert(self, collection: str, payload: dict[str, Any]) -> dict[str, Any]:
        body = {"collection": collection, "payload": payload}
        resp = await self._round_trip(
            Frame(kind=Kind.BulkInsert, correlation_id=self._corr(), payload=_json_bytes(body))
        )
        return _expect_json(resp, ok_kinds=(Kind.BulkOk,))

    async def bulk_insert(self, collection: str, payloads: list[dict[str, Any]]) -> dict[str, Any]:
        body = {"collection": collection, "payloads": payloads}
        resp = await self._round_trip(
            Frame(kind=Kind.BulkInsert, correlation_id=self._corr(), payload=_json_bytes(body))
        )
        return _expect_json(resp, ok_kinds=(Kind.BulkOk,))

    async def get(self, collection: str, doc_id: str) -> dict[str, Any]:
        body = {"collection": collection, "id": str(doc_id)}
        resp = await self._round_trip(
            Frame(kind=Kind.Get, correlation_id=self._corr(), payload=_json_bytes(body))
        )
        return _expect_json(resp, ok_kinds=(Kind.Result,))

    async def delete(self, collection: str, doc_id: str) -> dict[str, Any]:
        body = {"collection": collection, "id": str(doc_id)}
        resp = await self._round_trip(
            Frame(kind=Kind.Delete, correlation_id=self._corr(), payload=_json_bytes(body))
        )
        return _expect_json(resp, ok_kinds=(Kind.DeleteOk,))

    async def ping(self) -> None:
        resp = await self._round_trip(
            Frame(kind=Kind.Ping, correlation_id=self._corr())
        )
        if resp.kind != Kind.Pong:
            raise ProtocolError(f"expected Pong, got {kind_name(resp.kind)}")

    async def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        try:
            corr = self._corr()
            await _write_frame(
                self._writer, Frame(kind=Kind.Bye, correlation_id=corr), self._opts.timeout
            )
        except Exception:
            pass
        try:
            self._writer.close()
            await self._writer.wait_closed()
        except Exception:
            pass

    # -- internals ----------------------------------------------------------

    def _corr(self) -> int:
        c = self._next_corr
        self._next_corr = (self._next_corr + 1) & 0xFFFF_FFFF_FFFF_FFFF
        return c

    async def _round_trip(self, frame: Frame) -> Frame:
        if self._closed:
            raise ConnectionClosed("client is closed")
        async with self._lock:
            await _write_frame(self._writer, frame, self._opts.timeout)
            return await _read_frame(self._reader, self._opts.timeout)


# ---------------------------------------------------------------------------
# Socket + frame I/O helpers.
# ---------------------------------------------------------------------------


async def _open_socket(
    opts: RedwireOptions,
) -> tuple[asyncio.StreamReader, asyncio.StreamWriter]:
    if opts.use_tls:
        ctx = _make_tls_context(opts)
        reader, writer = await asyncio.wait_for(
            asyncio.open_connection(
                host=opts.host, port=opts.port, ssl=ctx, server_hostname=opts.host
            ),
            timeout=opts.timeout,
        )
    else:
        reader, writer = await asyncio.wait_for(
            asyncio.open_connection(host=opts.host, port=opts.port),
            timeout=opts.timeout,
        )
    sock = writer.get_extra_info("socket")
    if sock is not None:
        try:
            import socket as _socket

            sock.setsockopt(_socket.IPPROTO_TCP, _socket.TCP_NODELAY, 1)
        except Exception:
            pass
    return reader, writer


def _make_tls_context(opts: RedwireOptions) -> ssl.SSLContext:
    ctx = ssl.create_default_context()
    if opts.ca:
        ctx.load_verify_locations(cafile=opts.ca)
    if opts.cert and opts.key:
        ctx.load_cert_chain(certfile=opts.cert, keyfile=opts.key)
    try:
        ctx.set_alpn_protocols(["redwire/1"])
    except NotImplementedError:  # pragma: no cover - rare
        pass
    return ctx


async def _write_frame(writer: asyncio.StreamWriter, frame: Frame, timeout: float) -> None:
    data = encode_frame(frame)
    writer.write(data)
    try:
        await asyncio.wait_for(writer.drain(), timeout=timeout)
    except asyncio.TimeoutError as exc:
        raise ProtocolError("write timed out") from exc


async def _read_exact(reader: asyncio.StreamReader, n: int, timeout: float) -> bytes:
    try:
        data = await asyncio.wait_for(reader.readexactly(n), timeout=timeout)
    except asyncio.IncompleteReadError as exc:
        raise ConnectionClosed(
            f"connection closed after {len(exc.partial)}/{n} bytes"
        ) from exc
    except asyncio.TimeoutError as exc:
        raise ProtocolError(f"read of {n} bytes timed out") from exc
    return data


async def _read_frame(reader: asyncio.StreamReader, timeout: float) -> Frame:
    header = await _read_exact(reader, FRAME_HEADER_SIZE, timeout)
    length = struct.unpack_from("<I", header, 0)[0]
    if length < FRAME_HEADER_SIZE or length > MAX_FRAME_SIZE:
        raise ProtocolError(f"server sent frame with length {length}")
    body = await _read_exact(reader, length - FRAME_HEADER_SIZE, timeout)
    return decode_frame(header + body)


# ---------------------------------------------------------------------------
# Handshake.
# ---------------------------------------------------------------------------


async def _perform_handshake(
    reader: asyncio.StreamReader,
    writer: asyncio.StreamWriter,
    opts: RedwireOptions,
) -> dict[str, Any]:
    # 1) magic + minor version.
    writer.write(bytes([MAGIC, SUPPORTED_VERSION]))
    await asyncio.wait_for(writer.drain(), timeout=opts.timeout)

    # 2) Hello.
    methods = _hello_methods(opts.auth)
    hello_payload = _json_bytes(
        {
            "versions": [SUPPORTED_VERSION],
            "auth_methods": methods,
            "features": 0,
            "client_name": opts.client_name,
        }
    )
    await _write_frame(
        writer, Frame(kind=Kind.Hello, correlation_id=1, payload=hello_payload), opts.timeout
    )

    # 3) HelloAck.
    ack = await _read_frame(reader, opts.timeout)
    if ack.kind == Kind.AuthFail:
        raise AuthRefused(_reason(ack.payload, "AuthFail at HelloAck"))
    if ack.kind != Kind.HelloAck:
        raise ProtocolError(f"expected HelloAck, got {kind_name(ack.kind)}")
    ack_obj = _json_loads(ack.payload)
    chosen = ack_obj.get("auth")
    if not isinstance(chosen, str):
        raise ProtocolError("HelloAck missing 'auth' string")

    # 4) AuthResponse — branch on chosen method.
    if chosen == "anonymous":
        await _write_frame(
            writer, Frame(kind=Kind.AuthResponse, correlation_id=2, payload=b""), opts.timeout
        )
    elif chosen == "bearer":
        if not opts.token:
            raise AuthRefused("server picked bearer but no token was supplied")
        await _write_frame(
            writer,
            Frame(
                kind=Kind.AuthResponse,
                correlation_id=2,
                payload=_json_bytes({"token": opts.token}),
            ),
            opts.timeout,
        )
    elif chosen == "oauth-jwt":
        if not opts.jwt:
            raise AuthRefused("server picked oauth-jwt but no JWT was supplied")
        await _write_frame(
            writer,
            Frame(
                kind=Kind.AuthResponse,
                correlation_id=2,
                payload=_json_bytes({"jwt": opts.jwt}),
            ),
            opts.timeout,
        )
    elif chosen == "scram-sha-256":
        return await _perform_scram(reader, writer, opts)
    else:
        raise ProtocolError(f"server picked unsupported auth method: {chosen}")

    # 5) AuthOk / AuthFail.
    final = await _read_frame(reader, opts.timeout)
    if final.kind == Kind.AuthFail:
        raise AuthRefused(_reason(final.payload, "auth refused"))
    if final.kind != Kind.AuthOk:
        raise ProtocolError(f"expected AuthOk, got {kind_name(final.kind)}")
    return _json_loads(final.payload) or {}


async def _perform_scram(
    reader: asyncio.StreamReader,
    writer: asyncio.StreamWriter,
    opts: RedwireOptions,
) -> dict[str, Any]:
    if not opts.username or opts.password is None:
        raise AuthRefused("scram-sha-256 requires username + password")
    client_nonce = scram_lib.make_client_nonce()
    client_first, client_first_bare = scram_lib.build_client_first(opts.username, client_nonce)

    # client-first
    await _write_frame(
        writer,
        Frame(kind=Kind.AuthResponse, correlation_id=2, payload=client_first.encode("utf-8")),
        opts.timeout,
    )

    # server-first
    sf_frame = await _read_frame(reader, opts.timeout)
    if sf_frame.kind == Kind.AuthFail:
        raise AuthRefused(_reason(sf_frame.payload, "scram: AuthFail at server-first"))
    if sf_frame.kind != Kind.AuthRequest:
        raise ProtocolError(
            f"expected AuthRequest(server-first), got {kind_name(sf_frame.kind)}"
        )
    server_first = sf_frame.payload.decode("utf-8")
    combined_nonce, salt, iters = scram_lib.parse_server_first(server_first, client_nonce)

    # client-final
    client_final_no_proof = f"c=biws,r={combined_nonce}"
    am = scram_lib.auth_message(client_first_bare, server_first, client_final_no_proof)
    proof = scram_lib.client_proof(opts.password.encode("utf-8"), salt, iters, am)
    client_final = f"{client_final_no_proof},p={scram_lib.b64encode(proof)}"
    await _write_frame(
        writer,
        Frame(kind=Kind.AuthResponse, correlation_id=3, payload=client_final.encode("utf-8")),
        opts.timeout,
    )

    final = await _read_frame(reader, opts.timeout)
    if final.kind == Kind.AuthFail:
        raise AuthRefused(_reason(final.payload, "scram: AuthFail"))
    if final.kind != Kind.AuthOk:
        raise ProtocolError(f"expected AuthOk, got {kind_name(final.kind)}")
    session = _json_loads(final.payload) or {}

    # Verify the server signature (key field name is 'v' per build_scram_auth_ok).
    sig_b64 = session.get("v") or session.get("server_signature")
    if sig_b64 and not scram_lib.verify_server_signature(
        opts.password.encode("utf-8"), salt, iters, am, scram_lib.b64decode(sig_b64)
    ):
        raise AuthRefused("scram: server signature verification failed")
    return session


def _hello_methods(auth: str) -> list[str]:
    auth = auth.lower()
    if auth == "bearer":
        return ["bearer"]
    if auth in ("scram", "scram-sha-256"):
        return ["scram-sha-256"]
    if auth in ("oauth", "oauth-jwt"):
        return ["oauth-jwt"]
    if auth == "anonymous":
        return ["anonymous", "bearer"]
    raise ProtocolError(f"unknown auth selector: {auth!r}")


# ---------------------------------------------------------------------------
# Misc helpers.
# ---------------------------------------------------------------------------


def _json_bytes(obj: Any) -> bytes:
    return json.dumps(obj, separators=(",", ":")).encode("utf-8")


def _json_loads(buf: bytes) -> dict[str, Any]:
    if not buf:
        return {}
    try:
        v = json.loads(buf.decode("utf-8"))
    except Exception:
        return {}
    return v if isinstance(v, dict) else {}


def _reason(payload: bytes, fallback: str) -> str:
    obj = _json_loads(payload)
    reason = obj.get("reason")
    return reason if isinstance(reason, str) else fallback


def _expect_json(frame: Frame, *, ok_kinds: tuple[int, ...]) -> dict[str, Any]:
    if frame.kind in ok_kinds:
        return _json_loads(frame.payload)
    if frame.kind == Kind.Error:
        raise EngineError(frame.payload.decode("utf-8", errors="replace"))
    raise ProtocolError(
        f"expected one of {[kind_name(k) for k in ok_kinds]}, got {kind_name(frame.kind)}"
    )


__all__ = [
    "MAGIC",
    "SUPPORTED_VERSION",
    "FRAME_HEADER_SIZE",
    "MAX_FRAME_SIZE",
    "Kind",
    "Flags",
    "Frame",
    "RedwireOptions",
    "RedwireClient",
    "encode_frame",
    "decode_frame",
    "kind_name",
]
