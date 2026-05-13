from typing import Any

from .client import KvClient as KvClient, Reddb as Reddb
from .errors import (
    AuthRefused as AuthRefused,
    CompressedButNoZstd as CompressedButNoZstd,
    ConnectionClosed as ConnectionClosed,
    EngineError as EngineError,
    FrameDecompressFailed as FrameDecompressFailed,
    FrameTooLarge as FrameTooLarge,
    HttpError as HttpError,
    InvalidUri as InvalidUri,
    ProtocolError as ProtocolError,
    RedDBError as RedDBError,
    UnknownBinaryTag as UnknownBinaryTag,
    UnknownFlags as UnknownFlags,
    UnknownMethod as UnknownMethod,
    UnsupportedScheme as UnsupportedScheme,
)
from .http import HttpClient as HttpClient
from .redwire import (
    FEATURE_PARAMS as FEATURE_PARAMS,
    FRAME_HEADER_SIZE as FRAME_HEADER_SIZE,
    MAGIC as MAGIC,
    MAX_FRAME_SIZE as MAX_FRAME_SIZE,
    SUPPORTED_VERSION as SUPPORTED_VERSION,
    Flags as Flags,
    Frame as Frame,
    Kind as Kind,
    RedwireClient as RedwireClient,
    RedwireOptions as RedwireOptions,
    ValueTag as ValueTag,
    decode_frame as decode_frame,
    encode_frame as encode_frame,
    encode_query_with_params as encode_query_with_params,
    kind_name as kind_name,
)
from .url import ParsedUri as ParsedUri, default_port_for as default_port_for, parse_uri as parse_uri

__version__: str

async def connect(uri: str, **opts: Any) -> Reddb: ...
