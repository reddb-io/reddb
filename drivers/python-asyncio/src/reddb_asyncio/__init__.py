"""Pure-asyncio Python driver for RedDB.

Quickstart::

    import asyncio
    from reddb_asyncio import connect

    async def main():
        async with await connect("red://localhost:5050") as db:
            print(await db.query("SELECT 1"))

    asyncio.run(main())

The driver speaks both RedWire (binary TCP / TLS) and HTTP/HTTPS.
The transport is selected by the URI scheme.
"""

from .client import Reddb, connect
from .errors import (
    AuthRefused,
    CompressedButNoZstd,
    ConnectionClosed,
    EngineError,
    FrameDecompressFailed,
    FrameTooLarge,
    HttpError,
    InvalidUri,
    ProtocolError,
    RedDBError,
    UnknownBinaryTag,
    UnknownFlags,
    UnknownMethod,
    UnsupportedScheme,
)
from .http import HttpClient
from .redwire import (
    Flags,
    Frame,
    Kind,
    MAGIC,
    SUPPORTED_VERSION,
    RedwireClient,
    RedwireOptions,
    decode_frame,
    encode_frame,
    kind_name,
)
from .url import ParsedUri, default_port_for, parse_uri

__version__ = "0.1.0"

__all__ = [
    "__version__",
    # Top-level facade.
    "connect",
    "Reddb",
    # URL parsing.
    "ParsedUri",
    "parse_uri",
    "default_port_for",
    # Transports.
    "RedwireClient",
    "RedwireOptions",
    "HttpClient",
    # Wire constants + helpers.
    "MAGIC",
    "SUPPORTED_VERSION",
    "Kind",
    "Flags",
    "Frame",
    "encode_frame",
    "decode_frame",
    "kind_name",
    # Errors.
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
