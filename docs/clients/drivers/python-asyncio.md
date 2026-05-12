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
    async with await connect("red://localhost:5050") as db:
        print(await db.query("SELECT 1"))
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
await db.query(sql)
await db.insert(collection, payload)
await db.bulk_insert(collection, payloads)
await db.get(collection, id)
await db.delete(collection, id)
await db.ping()
await db.close()
async with await connect(uri) as db: ...
```

## Production checklist

- Use `reds://` (or `https://`) for any traffic crossing the network — never plain `red://` outside localhost.
- Run the server with the [encrypted vault](../../security/vault.md).
- See [Transport TLS](../../security/transport-tls.md) for mTLS / OAuth posture.
- Track credentials in [Secret Inventory](../../operations/secrets.md).

## Driver source

[`drivers/python-asyncio/README.md`](https://github.com/reddb-io/reddb/blob/main/drivers/python-asyncio/README.md) — full error table, test harness, smoke-test env vars.
