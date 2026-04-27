"""Exception taxonomy for the pure-asyncio RedDB driver.

All errors derive from :class:`RedDBError` so callers can catch the
whole family with one ``except``. Subclasses tag specific failure
modes that downstream code might want to react to (auth retry, frame
limits, missing zstd, etc.).
"""

from __future__ import annotations

from typing import Any


class RedDBError(Exception):
    """Base class for every error raised by ``reddb_asyncio``."""

    code: str = "REDDB_ERROR"

    def __init__(self, message: str, *, code: str | None = None, payload: Any = None) -> None:
        super().__init__(message)
        if code is not None:
            self.code = code
        self.payload = payload

    def __repr__(self) -> str:  # pragma: no cover - cosmetic
        return f"{self.__class__.__name__}({self.code!r}, {super().__str__()!r})"


class ProtocolError(RedDBError):
    code = "PROTOCOL"


class AuthRefused(RedDBError):
    code = "AUTH_REFUSED"


class EngineError(RedDBError):
    code = "ENGINE"


class ConnectionClosed(RedDBError):
    code = "CONNECTION_CLOSED"


class FrameTooLarge(ProtocolError):
    code = "FRAME_TOO_LARGE"


class UnknownFlags(ProtocolError):
    code = "FRAME_UNKNOWN_FLAGS"


class CompressedButNoZstd(ProtocolError):
    code = "COMPRESSED_BUT_NO_ZSTD"


class FrameDecompressFailed(ProtocolError):
    code = "FRAME_DECOMPRESS_FAILED"


class UnknownBinaryTag(ProtocolError):
    code = "UNKNOWN_BINARY_TAG"


class UnknownMethod(RedDBError):
    code = "UNKNOWN_METHOD"


class InvalidUri(RedDBError):
    code = "INVALID_URI"


class UnsupportedScheme(RedDBError):
    code = "UNSUPPORTED_SCHEME"


class HttpError(RedDBError):
    """Raised when the HTTP transport gets a non-2xx response."""

    code = "HTTP_ERROR"

    def __init__(self, message: str, *, status: int, body: str = "") -> None:
        super().__init__(message, payload={"status": status, "body": body})
        self.status = status
        self.body = body


__all__ = [
    "RedDBError",
    "ProtocolError",
    "AuthRefused",
    "EngineError",
    "ConnectionClosed",
    "FrameTooLarge",
    "UnknownFlags",
    "CompressedButNoZstd",
    "FrameDecompressFailed",
    "UnknownBinaryTag",
    "UnknownMethod",
    "InvalidUri",
    "UnsupportedScheme",
    "HttpError",
]
