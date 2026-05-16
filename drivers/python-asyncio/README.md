# reddb-asyncio

Pure-asyncio Python driver for [RedDB](https://github.com/reddb-io/reddb).
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
    async with await connect("http://localhost:8080") as db:
        print(await db.query("SELECT * FROM users WHERE name = $1", ["alice"]))
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

`Reddb` exposes the generic surface:

* `await db.query(sql, params=None)`
* `await db.execute(sql, params=None)`
* `await db.insert(collection, payload)` → `{affected, rid, id}` (`id` is a legacy alias for `rid`)
* `await db.bulk_insert(collection, payloads)` → `{affected, rids, ids}`
* `await db.exists(collection, rid)` → `{exists}`
* `await db.get(collection, rid)`
* `await db.delete(collection, rid)`
* `await db.transaction(callback)` (single connection — nested transactions are not supported)
* `await db.ping()`
* `await db.close()`
* `async with await connect(uri) as db: ...`

### Rich helper namespaces

The driver implements the [SDK Helper Spec v0.1](../../docs/clients/sdk-helper-spec.md):

```python
async with await connect("http://localhost:8080") as db:
    # Documents
    inserted = await db.documents.insert("people", {"name": "alice", "age": 30})
    doc = await db.documents.get("people", inserted["rid"])
    page = await db.documents.list("people", limit=20)
    await db.documents.patch("people", inserted["rid"], {"age": 31})
    await db.documents.delete("people", inserted["rid"])

    # KV (exact keys, namespaced keys round-trip unchanged)
    await db.kv.set("characters:hansel", {"role": "hero"})
    value = await db.kv.get("characters:hansel")
    presence = await db.kv.exists("characters:hansel")
    await db.kv.delete("characters:hansel")

    # Queue (FIFO)
    await db.queue.push("jobs", {"id": 1})
    next_item = await db.queue.peek("jobs")
    popped = await db.queue.pop("jobs")
    length = await db.queue.len("jobs")
```

| Helper namespace | Methods | Notes |
| ---------------- | ------- | ----- |
| `db.documents`   | `insert`, `get`, `list`, `patch`, `delete` | Patch applies top-level fields; JSON-pointer paths rejected. |
| `db.kv`          | `set`, `put`, `get`, `get_many`, `exists`, `delete`, `list`, `invalidate_tags`, `watch`, `watch_prefix` | Keys are exact strings — `:`, `/`, `.`, and Unicode are preserved. `watch`/`watch_prefix` require the HTTP transport. |
| `db.queue`       | `push`, `pop`, `peek`, `len`, `purge` | FIFO ordering; `pop`/`peek` accept an optional count. |
| `db.transaction` | `transaction(async callback)` | Auto `BEGIN`/`COMMIT`; rolls back on raise. Nested transactions are not supported. |

Probabilistic helpers (HLL/Bloom/CMS) are not yet implemented in this driver
— call them through `db.query` with the matching `RED HLL ...`, `RED BLOOM
...`, `RED CMS ...` statements until they ship.

#### Embedded URIs

`red://`, `red:///path`, and `red://:memory:` raise `NotImplementedError` here
— install the PyO3 `reddb` package from `drivers/python/` for in-process
embedded databases.

## Safe parameter binding

`await db.query(sql, params)` and `await db.execute(sql, params)` bind
positional `$N` placeholders over HTTP and RedWire:

```python
rows = await db.query(
    "SELECT id, name FROM users WHERE id = $1 AND tenant = $2",
    [42, "acme"],
)

hits = await db.query(
    "SEARCH SIMILAR $1 COLLECTION embeddings LIMIT 5",
    [[0.1, 0.2, 0.3]],
)
```

Parameters may be lists or tuples. Native Python values map to RedDB wire
values: `None`, `bool`, `int`, `float`, `str`, `bytes`, `bytearray`,
`memoryview`, `datetime.datetime`, and numeric vectors. Unsupported values raise
`TypeError` before a request is sent.

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
