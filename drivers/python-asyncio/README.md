# reddb-asyncio

Pure-asyncio Python driver for [RedDB](https://github.com/forattini-dev/reddb).
Speaks **RedWire** (the binary TCP / TLS protocol) and the
**HTTP REST** API. No native build step, no PyO3 — just `pip install`.

If you want the embedded engine (PyO3 + maturin), use the sibling
package `reddb` from `drivers/python/`.

## Install

```bash
pip install reddb-asyncio          # core
pip install reddb-asyncio[zstd]    # + zstandard (frame compression)
```

Python 3.10+.

## Quickstart

```python
import asyncio
from reddb_asyncio import connect

async def main():
    async with await connect("red://localhost:5050") as db:
        print(await db.query("SELECT 1"))
        await db.insert("users", {"name": "alice", "age": 30})
        row = await db.get("users", "alice")
        print(row)
        await db.delete("users", "alice")

asyncio.run(main())
```

## URL formats

| URI                                       | Transport             | Default port |
| ----------------------------------------- | --------------------- | ------------ |
| `red://host[:port]`                       | RedWire (plain TCP)   | 5050         |
| `reds://host[:port]`                      | RedWire over TLS      | 5050         |
| `http://host[:port]`                      | REST                  | 8080         |
| `https://host[:port]`                     | REST over TLS         | 8443         |
| `red://user:pass@host`                    | RedWire + SCRAM/login | 5050         |
| `red://host?token=sk-abc`                 | RedWire + bearer      | 5050         |
| `reds://host?ca=/etc/ca.pem&cert=...`     | RedWire + mTLS        | 5050         |
| `red://`, `red:///path`, `red://:memory:` | Embedded — *raises `NotImplementedError`* (use the `reddb` PyO3 package). |

Recognised query knobs: `auth=bearer|scram|oauth|anonymous`,
`sslmode=require|disable`, `timeout_ms=30000`, `token=`, `ca=`,
`cert=`, `key=`, `proto=...`.

## Authentication methods

| `auth=`     | Hello methods sent       | Required input             |
| ----------- | ------------------------ | -------------------------- |
| `anonymous` | `["anonymous","bearer"]` | nothing                    |
| `bearer`    | `["bearer"]`             | `token=...`                |
| `scram`     | `["scram-sha-256"]`      | `username` + `password`    |
| `oauth`     | `["oauth-jwt"]`          | `jwt=...`                  |

When the URI carries `username:password@host`, the driver defaults
to `scram` for RedWire and to the HTTP `/auth/login` flow for HTTP.

## Public API

```python
from reddb_asyncio import (
    connect,         # factory: connect(uri, **opts) -> Reddb
    Reddb,           # transport-agnostic facade
    RedwireClient,   # raw RedWire client
    HttpClient,      # raw HTTP client
    parse_uri,       # URL parser
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

* `await db.query(sql)`
* `await db.insert(collection, payload)`
* `await db.bulk_insert(collection, payloads)`
* `await db.get(collection, id)`
* `await db.delete(collection, id)`
* `await db.ping()`
* `await db.close()`
* `async with await connect(uri) as db: ...`

## Tests

```bash
pip install -e '.[test]'
pytest tests -q
```

The smoke tests (`test_redwire_smoke.py`, `test_http_smoke.py`) spawn
the `red` server binary on an ephemeral port via `cargo run --release
--bin red -- server --bind 127.0.0.1:<port>`. Set `RED_SKIP_SMOKE=1`
to skip them, or `REDWIRE_TEST_HOST` / `REDWIRE_TEST_PORT` /
`REDDB_HTTP_URL` to point at a pre-running instance.

## Production deploy

When you're ready to point this driver at a production RedDB cluster:

- **Run RedDB with the encrypted vault** so auth state and
  `red.secret.*` values are protected at rest. See
  [`docs/security/vault.md`](../../docs/security/vault.md).
- **Use Docker secrets or your cloud secret manager** to inject the
  certificate — never bake it into an image. See
  [`docs/getting-started/docker.md`](../../docs/getting-started/docker.md).
- **Track every secret** the driver consumes (bearer tokens, mTLS
  cert + key, OAuth JWTs) in
  [`docs/operations/secrets.md`](../../docs/operations/secrets.md).
- **Use `reds://` (TLS)** or `red://...?tls=true` for any traffic
  crossing the network — never plain `red://` outside localhost.
- **TLS posture, mTLS, OAuth/JWT and reverse-proxy patterns** are
  covered in [`docs/security/transport-tls.md`](../../docs/security/transport-tls.md).
- See [Policies](../../docs/security/policies.md) for IAM-style authorization.
