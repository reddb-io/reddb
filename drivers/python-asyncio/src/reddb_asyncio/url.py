"""Connection-string parser for the asyncio driver.

Mirrors the shape of ``drivers/js/src/url.js`` so URLs are portable
across the JS, Rust, and Python drivers. Returns a :class:`ParsedUri`
dataclass that ``connect()`` consumes.

Accepted schemes:

* ``red://[user[:pass]@]host[:port][?proto=<x>&...]`` — RedWire (default
  port ``5050``). Pass ``?proto=https`` etc. to switch transport, or
  use one of the explicit schemes below.
* ``reds://...`` — RedWire over TLS (default port ``5050``; mirrors
  the JS legacy alias).
* ``http://host:port`` / ``https://host:port`` — REST/HTTP transport
  (defaults: 8080 / 8443, same as JS).
* ``red:///absolute/path`` — embedded persistent (raise NotImplementedError;
  use the maturin ``reddb`` package).
* ``red://``, ``red://memory``, ``red://:memory``, ``red://:memory:`` —
  embedded in-memory (also raises NotImplementedError).

Accepted query knobs (parsed but mostly informative):

* ``auth=bearer|scram|oauth|anonymous`` — preferred auth method.
* ``sslmode=require|disable``
* ``timeout_ms=...``
* ``token=...`` — bearer/api-key.
* ``ca``, ``cert``, ``key`` — TLS file paths.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any
from urllib.parse import parse_qs, unquote, urlparse

from .errors import InvalidUri, UnsupportedScheme

__all__ = [
    "ParsedUri",
    "parse_uri",
    "default_port_for",
]


# Default ports — keep aligned with drivers/js/src/url.js.
_DEFAULT_PORTS: dict[str, int] = {
    "red": 5050,
    "reds": 5050,
    "redwire": 5050,
    "http": 8080,
    "https": 8443,
}


def default_port_for(kind: str) -> int:
    return _DEFAULT_PORTS.get(kind, 5050)


@dataclass
class ParsedUri:
    """Normalised connection-string information.

    Attributes:
        kind: ``"redwire"``, ``"redwire-tls"``, ``"http"``, ``"https"``,
            or ``"embedded"``.
        host: Hostname (None for embedded).
        port: TCP port.
        path: Filesystem path for ``embedded`` kind, otherwise ``None``.
        username, password: Credentials harvested from the authority.
        token: ``?token=`` query value.
        auth: ``?auth=`` value if explicitly chosen.
        sslmode: ``?sslmode=`` value.
        timeout_ms: ``?timeout_ms=`` value, parsed to int.
        ca, cert, key: TLS file paths.
        params: All other query parameters (single-string-valued).
        original_uri: Verbatim user input (for error messages).
    """

    kind: str
    host: str | None = None
    port: int | None = None
    path: str | None = None
    username: str | None = None
    password: str | None = None
    token: str | None = None
    auth: str | None = None
    sslmode: str | None = None
    timeout_ms: int | None = None
    ca: str | None = None
    cert: str | None = None
    key: str | None = None
    params: dict[str, str] = field(default_factory=dict)
    original_uri: str = ""


def _flatten_qs(qs: dict[str, list[str]]) -> dict[str, str]:
    """`urllib.parse.parse_qs` returns lists; we only ever take the first."""
    return {k: v[0] for k, v in qs.items() if v}


def parse_uri(uri: str) -> ParsedUri:
    """Parse a connection URI into a :class:`ParsedUri`.

    Raises:
        InvalidUri: when the URI is empty or malformed.
        UnsupportedScheme: when the scheme is not one we understand.
    """

    if not isinstance(uri, str) or not uri:
        raise InvalidUri("connection URI must be a non-empty string")

    # Embedded shortcuts that ``urllib`` cannot represent cleanly.
    if uri in ("red:", "red:/", "red://", "red://memory", "red://memory/", "red://:memory", "red://:memory:"):
        return ParsedUri(kind="embedded", original_uri=uri)
    if uri.startswith("red:///"):
        return ParsedUri(kind="embedded", path=uri[len("red://") :], original_uri=uri)

    # Allow Python's urllib to handle the rest.
    try:
        parsed = urlparse(uri)
    except ValueError as exc:  # pragma: no cover - urllib rarely raises here
        raise InvalidUri(f"failed to parse '{uri}': {exc}") from exc

    scheme = parsed.scheme.lower()
    if scheme not in {"red", "reds", "http", "https"}:
        raise UnsupportedScheme(
            f"unsupported scheme '{scheme}'. Use red://, reds://, http://, https://."
        )

    qs_raw = parse_qs(parsed.query, keep_blank_values=True)
    flat = _flatten_qs(qs_raw)
    proto_override = flat.get("proto", "").lower() or None

    # Embedded short-circuits when the host is missing.
    if not parsed.hostname and scheme == "red":
        if parsed.path and parsed.path not in ("", "/"):
            return ParsedUri(kind="embedded", path=parsed.path, original_uri=uri)
        return ParsedUri(kind="embedded", original_uri=uri)

    # Resolve final transport kind.
    if scheme in ("http", "https"):
        kind = scheme
    elif scheme == "reds":
        kind = "redwire-tls"
    elif proto_override:
        kind = _kind_from_proto(proto_override)
    else:
        # `red://...` defaults to RedWire plain TCP. ``sslmode=require``
        # bumps it to TLS, matching libpq behaviour callers expect.
        if flat.get("sslmode", "").lower() == "require":
            kind = "redwire-tls"
        else:
            kind = "redwire"

    port = parsed.port if parsed.port is not None else default_port_for(_scheme_for_default(kind))
    username = unquote(parsed.username) if parsed.username else None
    password = unquote(parsed.password) if parsed.password else None

    timeout_ms_raw = flat.get("timeout_ms")
    timeout_ms: int | None
    if timeout_ms_raw is not None:
        try:
            timeout_ms = int(timeout_ms_raw)
        except ValueError as exc:
            raise InvalidUri(f"timeout_ms must be an integer, got {timeout_ms_raw!r}") from exc
    else:
        timeout_ms = None

    auth_choice = flat.get("auth")
    if auth_choice is not None:
        auth_choice = auth_choice.lower()
        if auth_choice not in {"bearer", "scram", "oauth", "anonymous"}:
            raise InvalidUri(f"auth must be one of bearer/scram/oauth/anonymous, got {auth_choice!r}")

    return ParsedUri(
        kind=kind,
        host=parsed.hostname,
        port=port,
        path=parsed.path or None if parsed.path not in ("", "/") else None,
        username=username,
        password=password,
        token=flat.get("token"),
        auth=auth_choice,
        sslmode=flat.get("sslmode"),
        timeout_ms=timeout_ms,
        ca=flat.get("ca"),
        cert=flat.get("cert"),
        key=flat.get("key"),
        params=flat,
        original_uri=uri,
    )


def _kind_from_proto(proto: str) -> str:
    if proto in ("red", "redwire", "grpc"):
        return "redwire"
    if proto in ("reds", "redwires", "grpcs"):
        return "redwire-tls"
    if proto == "http":
        return "http"
    if proto == "https":
        return "https"
    raise UnsupportedScheme(
        f"unknown proto='{proto}'. Supported: red | reds | http | https"
    )


def _scheme_for_default(kind: str) -> str:
    if kind == "redwire":
        return "red"
    if kind == "redwire-tls":
        return "reds"
    return kind


# ---------------------------------------------------------------------------
# Helper for tests / introspection.
# ---------------------------------------------------------------------------


def to_dict(parsed: ParsedUri) -> dict[str, Any]:  # pragma: no cover - cosmetic
    return {k: v for k, v in parsed.__dict__.items() if v not in (None, {}, "")}
