# Python driver (pure asyncio)

`reddb-asyncio` is a **pure-Python asyncio driver** for remote RedDB servers — no native build, no PyO3. It speaks RedWire (TCP / TLS) and HTTP REST. Use it when you want an async API and don't need to embed the engine.

- **Package:** [`reddb-asyncio`](https://pypi.org/project/reddb-asyncio/) on PyPI
- **Source:** [`drivers/python-asyncio/`](https://github.com/reddb-io/reddb/tree/main/drivers/python-asyncio)
- **Status:** Preview
- **Python:** 3.10+
- **Sibling:** for the embedded engine, use [`reddb`](./python.md) (PyO3).

## Install

```bash
pip install reddb-asyncio          # core
pip install reddb-asyncio[zstd]    # + zstandard (RedWire frame compression)
```

## Quickstart

```python
import asyncio
from reddb_asyncio import connect

async def main():
    async with await connect("http://localhost:8080") as db:
        rows = await db.query(
            "SELECT * FROM users WHERE name = $1",
            ["alice"],
        )
        print(rows)
        await db.insert("users", {"name": "alice", "age": 30})
        print(await db.get("users", "alice"))
        await db.delete("users", "alice")

asyncio.run(main())
```

## Connection URIs

| URI                                   | Transport             | Default port |
|---------------------------------------|-----------------------|:------------:|
| `red://host[:port]`                   | RedWire (plain TCP)   | 5050         |
| `reds://host[:port]`                  | RedWire over TLS      | 5050         |
| `http://host[:port]`                  | REST                  | 8080         |
| `https://host[:port]`                 | REST over TLS         | 8443         |
| `red://user:pass@host`                | RedWire + SCRAM/login | 5050         |
| `red://host?token=sk-abc`             | RedWire + bearer      | 5050         |
| `reds://host?ca=…&cert=…&key=…`       | RedWire + mTLS        | 5050         |

Recognised query knobs: `auth=bearer|scram|oauth|anonymous`, `sslmode=require|disable`, `timeout_ms=30000`, `token=`, `ca=`, `cert=`, `key=`, `proto=…`.

Embedded URIs (`red://`, `red:///path`, `red://:memory:`) raise `NotImplementedError` — use the [PyO3 `reddb` package](./python.md) for in-process mode.

## Authentication

| `auth=`     | Hello methods sent       | Required input          |
|-------------|--------------------------|-------------------------|
| `anonymous` | `["anonymous","bearer"]` | nothing                 |
| `bearer`    | `["bearer"]`             | `token=…`               |
| `scram`     | `["scram-sha-256"]`      | `username` + `password` |
| `oauth`     | `["oauth-jwt"]`          | `jwt=…`                 |

A URI carrying `username:password@host` defaults to `scram` over RedWire and to `/auth/login` over HTTP.

## Public API

```python
from reddb_asyncio import (
    connect, Reddb,
    RedwireClient, HttpClient,
    parse_uri,
    # frame primitives
    encode_frame, decode_frame, Frame, Kind, Flags,
    MAGIC, SUPPORTED_VERSION,
    # errors
    RedDBError, AuthRefused, ProtocolError, EngineError,
    ConnectionClosed, FrameTooLarge, UnknownFlags,
    CompressedButNoZstd, FrameDecompressFailed,
    UnknownBinaryTag, UnknownMethod, InvalidUri,
    UnsupportedScheme, HttpError,
)
```

`Reddb` exposes:

```python
await db.query(sql, params=None)
await db.insert(collection, payload)
await db.bulk_insert(collection, payloads)
await db.get(collection, rid)
await db.delete(collection, rid)
await db.ping()
await db.close()
async with await connect(uri) as db: ...
```

## Safe parameter binding

`await db.query(sql, params)` binds positional `$N` placeholders over HTTP and
RedWire. Use it for any user-supplied value — string concatenation is a
SQL-injection footgun. The cross-driver contract is tracked in
[ADR #352](https://github.com/reddb-io/reddb/issues/352).

```python
# Scalar params: int / text / null
rows = await db.query(
    "SELECT id, name FROM users WHERE id = $1 AND tenant = $2 AND deleted_at IS $3",
    [42, "acme", None],
)

# Vector param (HNSW / IVF similarity search)
hits = await db.query(
    "SEARCH SIMILAR $1 IN embeddings K 5",
    [[0.1, 0.2, 0.3]],
)
```

HTTP sends the `params` array directly to `/query`, where numeric arrays bind
as vectors and objects bind as JSON. RedWire routes non-empty params through
the binary `QueryWithParams` frame when the server advertises `FEATURE_PARAMS`;
older servers raise `PARAMS_UNSUPPORTED` instead of silently dropping params.

## Production checklist

- Use `reds://` (or `https://`) for any traffic crossing the network — never plain `red://` outside localhost.
- Run the server with the [encrypted vault](../../security/vault.md).
- See [Transport TLS](../../security/transport-tls.md) for mTLS / OAuth posture.
- Track credentials in [Secret Inventory](../../operations/secrets.md).

## Driver source

[`drivers/python-asyncio/README.md`](https://github.com/reddb-io/reddb/blob/main/drivers/python-asyncio/README.md) — full error table, test harness, smoke-test env vars.
