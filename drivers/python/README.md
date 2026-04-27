# reddb (Python)

Official Python driver for [RedDB](https://github.com/forattini-dev/reddb).
Compiled Rust under the hood (via [pyo3](https://pyo3.rs)) — `import reddb`
gives you the engine in-process, no server, no subprocess, no extra deps.

## Install

```bash
pip install reddb
```

(After the first PyPI release. For now: see "Building from source" below.)

## Quickstart

```python
import reddb

db = reddb.connect("memory://")
# or:  reddb.connect("file:///var/lib/reddb/data.rdb")

db.insert("users", {"name": "Alice", "age": 30})
db.bulk_insert("users", [{"name": "Bob"}, {"name": "Carol"}])

result = db.query("SELECT * FROM users")
for row in result["rows"]:
    print(row)

db.close()
```

You can also use it as a context manager:

```python
with reddb.connect("memory://") as db:
    db.insert("users", {"name": "Alice"})
    print(db.query("SELECT * FROM users"))
```

## Connection URIs

| URI                       | Backend                          | Status        |
|---------------------------|----------------------------------|---------------|
| `memory://`               | Ephemeral in-memory engine       | ✅            |
| `file:///absolute/path`   | Embedded engine on disk          | ✅            |
| `grpc://host:port`        | Remote gRPC server               | ⚠ planned     |

For now, `grpc://` raises a clear `FEATURE_DISABLED` error from
`reddb.connect()`. If you need a remote gRPC connection today, use the
low-level `reddb.legacy_grpc_connect("host:port")` API which exposes a thinner
`Connection` class.

## API

### High-level (recommended)

```python
import reddb

db = reddb.connect(uri: str) -> RedDb

db.query(sql: str)                              -> {"statement", "affected", "columns", "rows"}
db.insert(collection: str, payload: dict)        -> {"affected"}
db.bulk_insert(collection: str, payloads: list[dict]) -> {"affected"}
db.delete(collection: str, id: str)              -> {"affected"}
db.health()                                      -> {"ok": True, "version": str}
db.version()                                     -> {"version": str, "protocol": "1.0"}
db.close()                                       -> None
```

`payload` values must be `None`, `bool`, `int`, `float` or `str`. Nested
dicts/lists are not yet supported as field values — wrap them in a JSON string
if you need to round-trip them.

### Low-level (advanced)

The original gRPC and raw-wire clients are still exported for power users:

```python
import reddb

# gRPC against a remote server (returns reddb.Connection)
conn = reddb.legacy_grpc_connect("127.0.0.1:50051")
conn.query("SELECT * FROM users")
conn.close()

# Raw TCP wire protocol (fastest, returns reddb.WireConnection)
wc = reddb.wire_connect("127.0.0.1:5050")
```

These mirror the `Connection` and `WireConnection` classes that shipped before
the unified API.

## Errors

The high-level API raises `ValueError` with a message of the form
`[CODE] message`. Stable codes (mirroring the JSON-RPC stdio protocol):

| code                  | meaning                                                    |
|-----------------------|------------------------------------------------------------|
| `INVALID_URI`         | URI malformed                                              |
| `UNSUPPORTED_SCHEME`  | Scheme not recognized                                      |
| `INVALID_PARAMS`      | A method argument has the wrong type                       |
| `QUERY_ERROR`         | SQL parse or execution failure                             |
| `IO_ERROR`            | Disk / file backend failure                                |
| `FEATURE_DISABLED`    | Caller hit a path gated behind a Cargo feature             |
| `CLIENT_CLOSED`       | Method called after `close()`                              |

## Building from source

You need a Rust toolchain and [maturin](https://www.maturin.rs).

```bash
python3 -m venv .venv
source .venv/bin/activate
pip install maturin
cd drivers/python
maturin develop --release
python -c "import reddb; print(reddb.connect('memory://').version())"
```

## Limits

- The wheel statically links the entire RedDB engine. Expect ~10 MB+ per arch.
- No async API yet — every call is blocking (releases the GIL during query
  execution so other Python threads can run).
- No connection pooling. The handle is `Send + Sync` on the Rust side; in
  Python you can share a `RedDb` between threads.
- `query` returns the full result set as a Python list. Streaming is on the
  roadmap.

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
