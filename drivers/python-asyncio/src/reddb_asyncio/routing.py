"""Protocol-independent discovery and routing (reddb-wire owns the contract)."""
from __future__ import annotations

import asyncio
import re
import struct
import time
from typing import Any
from urllib.parse import urlsplit, urlunsplit

import httpx

from .errors import RedDBError, ProtocolError

FEATURE_TOPOLOGY = 2
TOPOLOGY_KIND = 0x32
TOPOLOGY_METADATA_KEY = "x-reddb-topology-transport"


def decode_topology(data: bytes) -> dict[str, Any] | None:
    if len(data) < 5:
        raise ProtocolError("truncated topology")
    size = struct.unpack_from("<I", data, 1)[0]
    if size > len(data) - 5:
        raise ProtocolError("truncated topology body")
    if data[0] != 1:
        return None
    data = data[5:5 + size]
    offset = 0

    def take(count: int) -> bytes:
        nonlocal offset
        if count > len(data) - offset:
            raise ProtocolError("truncated topology field")
        value = data[offset:offset + count]
        offset += count
        return value

    def integer(size: int) -> int:
        return int.from_bytes(take(size), "little")

    def string() -> str:
        size = integer(4)
        if size > 4096:
            raise ProtocolError("topology string too long")
        try:
            return take(size).decode("utf8")
        except UnicodeDecodeError as error:
            raise ProtocolError("invalid topology UTF-8") from error

    epoch = str(integer(8))
    primary = {"addr": string(), "region": string()}
    count = integer(4)
    if count > 256:
        raise ProtocolError("too many topology replicas")
    replicas = [
        {"addr": string(), "region": string(), "healthy": integer(1) != 0,
         "lag_ms": integer(4), "last_applied_lsn": str(integer(8)),
         "rebootstrapping": False}
        for _ in range(count)
    ]
    if len(data) - offset >= count:
        for replica in replicas:
            replica["rebootstrapping"] = integer(1) != 0
    return {"version": 1, "epoch": epoch, "primary": primary, "replicas": replicas}


def endpoint(address: Any, protocol: str) -> str | None:
    if not isinstance(address, str) or not address or len(address) > 4096:
        return None
    try:
        parsed = urlsplit(address if "://" in address else f"{protocol}://{address}")
        if (parsed.scheme != protocol or not parsed.hostname or parsed.username
                or parsed.password or parsed.query or parsed.fragment):
            return None
        _ = parsed.port
        return urlunsplit(parsed).rstrip("/")
    except ValueError:
        return None


def sql_tokens(sql: str) -> list[str] | None:
    if len(sql) > 1_048_576:
        return None
    tokens: list[str] = []
    index = 0
    while index < len(sql):
        if sql[index].isspace():
            index += 1
        elif sql.startswith("--", index):
            end = sql.find("\n", index + 2)
            index = len(sql) if end < 0 else end + 1
        elif sql.startswith("/*", index):
            depth = 1
            index += 2
            while index < len(sql) and depth:
                if sql.startswith("/*", index):
                    depth += 1
                    index += 2
                elif sql.startswith("*/", index):
                    depth -= 1
                    index += 2
                else:
                    index += 1
                if depth > 64:
                    return None
            if depth:
                return None
        elif sql[index] in "'\"\x60":
            quote = sql[index]
            index += 1
            while index < len(sql):
                char = sql[index]
                index += 1
                if char == "\\":
                    return None
                if char == quote:
                    if index < len(sql) and sql[index] == quote:
                        index += 1
                    else:
                        break
            else:
                return None
            tokens.append("#value" if quote == "'" else "#identifier")
        else:
            if ord(sql[index]) > 127:
                return None
            word = re.match(r"[A-Za-z_][A-Za-z_0-9]*", sql[index:])
            token = word[0] if word else sql[index]
            tokens.append(token.upper())
            index += len(token)
    if tokens and tokens[-1] == ";":
        tokens.pop()
    return tokens


def sql_policy(sql: str) -> tuple[str, bool]:
    tokens = sql_tokens(sql)
    if not tokens or ";" in tokens:
        return "session", False
    first = tokens[0]
    if first == "BEGIN" or tokens[:2] == ["START", "TRANSACTION"]:
        return "begin", False
    if first in ("COMMIT", "END", "ROLLBACK"):
        if first == "ROLLBACK" and "TO" in tokens:
            return "primary", False
        return ("begin" if "CHAIN" in tokens and "NO" not in tokens else "end"), False
    if first in ("SET", "RESET", "USE", "PREPARE", "DEALLOCATE", "DECLARE", "LISTEN"):
        return "session", False
    forbidden = {"INSERT", "UPDATE", "DELETE", "CREATE", "ALTER", "DROP", "INTO",
                 "FOR", "LOCK", "CALL", "EXECUTE", "SET", "QUEUE", "COPY", "[", "]"}
    functions = {"COUNT", "SUM", "MIN", "MAX", "AVG", "LOWER", "UPPER", "LENGTH",
                 "COALESCE", "ABS", "ROUND", "CAST", "EXISTS", "IN", "AS", "FROM",
                 "WHERE", "AND", "OR", "NOT", "SELECT"}
    read = first == "SELECT" and not forbidden.intersection(tokens)
    read = read and all(not (index + 1 < len(tokens) and tokens[index + 1] == "("
                            and (token == "#identifier" or re.match(r"^[A-Z_]", token))
                            and token not in functions)
                        for index, token in enumerate(tokens))
    return "primary", read


def network_error(error: BaseException) -> bool:
    return getattr(error, "status", None) == 0 or isinstance(error, (OSError, TimeoutError, httpx.TransportError)) or getattr(
        error, "code", "") in {"NETWORK", "CONNECTION_CLOSED", "IO_ERROR"}


class RoutingClient:
    def __init__(self, seed: str, client: Any, dial: Any, protocol: str,
                 force_primary: bool = False) -> None:
        self.seed = self.primary = seed
        self.protocol = protocol
        self.clients = {seed: client}
        self.dial = dial
        self.replicas: list[dict[str, Any]] = []
        self.refresh_at = 0.0
        self.next_replica = 0
        self.pinned: str | None = None
        self.session = False
        self.broken = False
        self.closed = False
        self.force_primary = force_primary
        self.lock = asyncio.Lock()
        self.streams: set[Any] = set()

    async def initialize(self) -> "RoutingClient":
        await self.refresh()
        return self

    async def _endpoint(self, address: str) -> Any:
        if address not in self.clients:
            client = await asyncio.wait_for(self.dial(address), 5)
            try:
                await asyncio.wait_for(client.ping(), 5)
            except BaseException:
                await client.close()
                raise
            self.clients[address] = client
        return self.clients[address]

    async def refresh(self) -> None:
        await self._refresh_membership()

    async def _refresh_membership(self) -> None:
        if self.pinned or self.closed:
            return
        self.refresh_at = time.monotonic() + 30
        deadline = time.monotonic() + 5
        candidates = list(dict.fromkeys([self.primary, self.seed] + [r["addr"] for r in self.replicas]))
        for address in candidates:
            if time.monotonic() >= deadline:
                break
            control = None
            try:
                control = await asyncio.wait_for(self.dial(address), max(0.001, deadline - time.monotonic()))
                topology = await asyncio.wait_for(control.topology(self.protocol), max(0.001, deadline - time.monotonic()))
                if not topology or topology.get("version") != 1:
                    continue
                primary = endpoint(topology.get("primary", {}).get("addr"), self.protocol)
                if not primary:
                    continue
                if primary not in candidates and len(candidates) < 258:
                    candidates.append(primary)
                await asyncio.wait_for(self._endpoint(primary), max(0.001, deadline - time.monotonic()))
                replicas = []
                for replica in topology.get("replicas", [])[:256]:
                    if time.monotonic() >= deadline:
                        break
                    target = endpoint(replica.get("addr"), self.protocol)
                    if not target or target == primary or not replica.get("healthy") or replica.get("rebootstrapping"):
                        continue
                    try:
                        await asyncio.wait_for(self._endpoint(target), max(0.001, deadline - time.monotonic()))
                        replicas.append({**replica, "addr": target})
                    except Exception:
                        continue
                self.primary, self.replicas = primary, replicas
                keep = {self.seed, primary, *(r["addr"] for r in replicas)}
                for retired in set(self.clients) - keep:
                    await self.clients.pop(retired).close()
                if primary == address:
                    return
            except Exception as error:
                if getattr(error, "code", "") in {"AUTH_REFUSED", "HTTP_401"} or getattr(error, "status", None) in {401, 403}:
                    raise
            finally:
                if control is not None:
                    await control.close()

    async def query_with_options(self, sql: str, options: dict[str, Any],
                                 params: Any = None) -> Any:
        return await self._call("query", sql, params, options=options)

    async def query(self, sql: str, params: Any = None) -> Any:
        return await self._call("query", sql, params)

    async def execute(self, sql: str, params: Any = None) -> Any:
        return await self.query(sql, params)

    async def _call(self, method: str, *args: Any, options: Any = None) -> Any:
        async with self.lock:
            if self.closed or self.broken:
                raise RedDBError("connection is closed", code="CONNECTION_CLOSED")
            if not self.pinned and time.monotonic() >= self.refresh_at:
                await self.refresh()
            if options is not None and (not isinstance(options, dict)
                    or options.get("consistency") not in {"strong", "local", "bounded_staleness"}
                    or options.get("consistency") == "bounded_staleness"
                    and (not isinstance(options.get("max_lag_ms"), (int, float))
                         or not 0 <= options["max_lag_ms"] < float("inf"))):
                raise RedDBError("unsupported read consistency", code="CONSISTENCY_UNSUPPORTED")
            state, safe = sql_policy(args[0]) if method == "query" else ("primary", False)
            read = method == "get" or safe and options is not None
            replicas = [] if self.force_primary or options and options["consistency"] == "strong" else self.replicas
            if options and options["consistency"] == "bounded_staleness":
                replicas = [r for r in replicas if r.get("lag_ms", float("inf")) <= options["max_lag_ms"]]
            address = self.pinned or self.primary
            if not self.pinned and read and replicas:
                address = replicas[self.next_replica % len(replicas)]["addr"]
                self.next_replica += 1
            if state in {"begin", "session"}:
                self.pinned = address
                self.session |= state == "session"
            try:
                client = await self._endpoint(address)
                result = await getattr(client, method)(*args)
                if state == "end" and not self.session:
                    self.pinned = None
                return result
            except BaseException as error:
                interrupted = isinstance(error, asyncio.CancelledError)
                if not interrupted and not network_error(error):
                    raise
                self.refresh_at = 0
                failed = self.clients.pop(address, None)
                if failed:
                    await failed.close()
                if self.pinned:
                    self.broken = True
                if self.pinned or interrupted or not read:
                    raise
                await self.refresh()
                return await getattr(await self._endpoint(self.primary), method)(*args)

    async def insert(self, *args: Any) -> Any:
        return await self._call("insert", *args)

    async def bulk_insert(self, *args: Any) -> Any:
        return await self._call("bulk_insert", *args)

    async def get(self, *args: Any) -> Any:
        return await self._call("get", *args)

    async def delete(self, *args: Any) -> Any:
        return await self._call("delete", *args)

    async def ping(self) -> Any:
        return await self._call("ping")

    def kv_watch(self, *args: Any, **options: Any) -> Any:
        return self._watch("kv_watch", *args, **options)

    def kv_watch_prefix(self, *args: Any, **options: Any) -> Any:
        return self._watch("kv_watch_prefix", *args, **options)

    async def _watch(self, method: str, *args: Any, **options: Any) -> Any:
        if self.protocol not in {"http", "https"}:
            raise RedDBError("KV watch requires HTTP", code="UNSUPPORTED_TRANSPORT")
        async with self.lock:
            if self.closed or self.broken:
                raise RedDBError("connection is closed", code="CONNECTION_CLOSED")
            if not self.pinned and time.monotonic() >= self.refresh_at:
                await self.refresh()
            stream = await asyncio.wait_for(self.dial(self.pinned or self.primary), 5)
            self.streams.add(stream)
        try:
            method_impl = getattr(stream, method, None)
            if method_impl is None:
                raise RedDBError("watch is unavailable", code="UNSUPPORTED_TRANSPORT")
            async for event in method_impl(*args, **options):
                yield event
        except BaseException as error:
            if network_error(error):
                self.refresh_at = 0
            raise
        finally:
            self.streams.discard(stream)
            await stream.close()

    async def close(self) -> None:
        async with self.lock:
            self.closed = True
            clients, self.clients = self.clients, {}
            streams, self.streams = self.streams, set()
            await asyncio.gather(*(client.close() for client in [*clients.values(), *streams]))
