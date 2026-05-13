from dataclasses import dataclass
from typing import Any

MAGIC: int
SUPPORTED_VERSION: int
FRAME_HEADER_SIZE: int
MAX_FRAME_SIZE: int
KNOWN_FLAGS: int
FEATURE_PARAMS: int
DEFAULT_TIMEOUT_SECS: float

class Kind:
    Query: int
    Result: int
    Error: int
    BulkInsert: int
    BulkOk: int
    BulkInsertBinary: int
    QueryBinary: int
    BulkInsertPrevalidated: int
    Hello: int
    HelloAck: int
    AuthRequest: int
    AuthResponse: int
    AuthOk: int
    AuthFail: int
    Bye: int
    Ping: int
    Pong: int
    Get: int
    Delete: int
    DeleteOk: int
    QueryWithParams: int

class ValueTag:
    Null: int
    Bool: int
    Int: int
    Float: int
    Text: int
    Bytes: int
    Vector: int
    Json: int
    Timestamp: int
    Uuid: int

class Flags:
    COMPRESSED: int
    MORE_FRAMES: int

@dataclass
class Frame:
    kind: int
    correlation_id: int
    payload: bytes = b""
    flags: int = 0
    stream_id: int = 0

@dataclass
class RedwireOptions:
    host: str
    port: int
    auth: str = "anonymous"
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

class RedwireClient:
    def __init__(self, reader: Any, writer: Any, opts: RedwireOptions, session: dict[str, Any]) -> None: ...
    @property
    def session(self) -> dict[str, Any]: ...
    def supports_params(self) -> bool: ...
    @classmethod
    async def connect(cls, opts: RedwireOptions) -> RedwireClient: ...
    async def query(
        self,
        sql: str,
        params: list[Any] | tuple[Any, ...] | None = None,
    ) -> dict[str, Any]: ...
    async def execute(
        self,
        sql: str,
        params: list[Any] | tuple[Any, ...] | None = None,
    ) -> dict[str, Any]: ...
    async def insert(self, collection: str, payload: dict[str, Any]) -> dict[str, Any]: ...
    async def bulk_insert(self, collection: str, payloads: list[dict[str, Any]]) -> dict[str, Any]: ...
    async def get(self, collection: str, doc_id: str) -> dict[str, Any]: ...
    async def delete(self, collection: str, doc_id: str) -> dict[str, Any]: ...
    async def ping(self) -> None: ...
    async def close(self) -> None: ...

def kind_name(value: int) -> str: ...
def encode_frame(frame: Frame) -> bytes: ...
def decode_frame(buf: bytes) -> Frame: ...
def encode_query_with_params(sql: str, params: list[Any]) -> bytes: ...
