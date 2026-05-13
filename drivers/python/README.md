# reddb (Python)

Official Python driver for [RedDB](https://github.com/reddb-io/reddb).
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

## Parameterized queries

`db.query(sql, *params)` binds positional `$N` placeholders without
splicing values into the SQL string — the engine evaluates a parsed
expression with typed values, so quoting bugs and SQL injection are
not on the menu.

```python
import datetime, uuid
import reddb

with reddb.connect("memory://") as db:
    db.query("CREATE TABLE users (id INT, name TEXT, age INT)")
    db.insert("users", {"id": 1, "name": "Alice", "age": 30})
    db.insert("users", {"id": 2, "name": "Bob",   "age": 25})

    # variadic positional form
    rows = db.query("SELECT * FROM users WHERE id = $1", 2)["rows"]

    # keyword form — both produce the same wire call
    rows = db.query("SELECT * FROM users WHERE id = $1", params=[2])["rows"]

    # vector parameter for SEARCH SIMILAR
    db.query(
        "SEARCH SIMILAR $1 IN embeddings LIMIT $2",
        [0.1, 0.2, 0.3], 10,
    )

    # bytes / timestamp / uuid are first-class
    db.query("SELECT * FROM blobs WHERE blob = $1", b"\\x00\\x01\\x02")
    db.query("SELECT * FROM events WHERE at = $1", datetime.datetime(2026, 5, 12))
    db.query("SELECT * FROM users WHERE uid = $1",
             uuid.UUID("12345678-1234-5678-1234-567812345678"))
```

### Native type mapping

| Python                                | Engine `Value`     |
|---------------------------------------|--------------------|
| `None`                                | `Null`             |
| `bool`                                | `Boolean`          |
| `int`  (`-2^63 .. 2^63-1`)            | `Integer`          |
| `int`  (`2^63 .. 2^64-1`)             | `UnsignedInteger`  |
| `float`                               | `Float`            |
| `str`                                 | `Text`             |
| `bytes`, `bytearray`                  | `Blob`             |
| `list[float|int]`                     | `Vector` (f32)     |
| `datetime.datetime`                   | `Timestamp` (sec)  |
| `uuid.UUID`                           | `Uuid` (16 bytes)  |
| `dict[str, scalar]`                   | `Json`             |

Anything else raises `ValueError("[INVALID_PARAMS] ...")` so a typo
(`tuple`, `set`, unsupported numpy scalar) fails loud instead of
silently coercing.

> Parameterized queries currently require the embedded backend
> (`memory://` or `file://`). Over `grpc://` they raise
> `[PARAMS_UNSUPPORTED] ...` until the gRPC server advertises
> `FEATURE_PARAMS` (tracked alongside the Rust client work in #364).

## Connection URIs

| URI                       | Backend                          | Status        |
|---------------------------|----------------------------------|---------------|
| `memory://`               | Ephemeral in-memory engine       | ✅            |
| `file:///absolute/path`   | Embedded engine on disk          | ✅            |
| `grpc://host:port`        | Remote gRPC server               | ✅            |

The high-level `reddb.connect("grpc://host:port")` API speaks the
official tonic-backed gRPC client and is the recommended path for
remote connections. The low-level `legacy_grpc_connect` API below is
kept around for power users who need direct access to the
generated-protobuf `Connection` class.

## API

### High-level (recommended)

```python
import reddb

db = reddb.connect(uri: str) -> RedDb

db.query(sql: str, *params)                      -> {"statement", "affected", "columns", "rows"}
db.query(sql: str, params=[...])                 -> {"statement", "affected", "columns", "rows"}
db.execute(sql: str, *params)                    -> {"statement", "affected", "columns", "rows"}
db.execute(sql: str, params=[...])               -> {"statement", "affected", "columns", "rows"}
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
