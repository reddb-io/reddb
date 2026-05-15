# Python driver (embedded, PyO3)

The `reddb` PyPI package ships the **engine compiled into a Python wheel** via [pyo3](https://pyo3.rs). `import reddb` gives you the database in-process — no server, no subprocess. The same package also speaks gRPC for remote use.

- **Package:** [`reddb`](https://pypi.org/project/reddb/) on PyPI
- **Source:** [`drivers/python/`](https://github.com/reddb-io/reddb/tree/main/drivers/python)
- **Status:** Preview
- **Sibling:** for a pure-asyncio, native-build-free driver, use [`reddb-asyncio`](./python-asyncio.md).

## Install

```bash
pip install reddb
```

The wheel statically links the engine (~10 MB+ per arch).

## Quickstart

```python
import reddb

db = reddb.connect("memory://")
# or:  reddb.connect("file:///var/lib/reddb/data.rdb")
# or:  reddb.connect("grpc://localhost:50051")

db.insert("users", {"name": "Alice", "age": 30})
db.bulk_insert("users", [{"name": "Bob"}, {"name": "Carol"}])

result = db.query("SELECT * FROM users WHERE age >= $1", 30)
for row in result["rows"]:
    print(row)

db.close()
```

Context-manager form:

```python
with reddb.connect("memory://") as db:
    db.insert("users", {"name": "Alice"})
    print(db.query("SELECT * FROM users"))
```

## Connection URIs

| URI                     | Backend                    |
|-------------------------|----------------------------|
| `memory://`             | Ephemeral in-memory engine |
| `file:///absolute/path` | Embedded engine on disk    |
| `grpc://host:port`      | Remote gRPC server         |

## API surface

```python
db.query(sql: str)                                # → {"statement", "affected", "columns", "rows"}
db.insert(collection, payload: dict)              # → {"affected"}
db.bulk_insert(collection, payloads: list[dict])  # → {"affected"}
db.delete(collection, rid: str)                   # → {"affected"}
db.health()                                       # → {"ok": True, "version": str}
db.version()                                      # → {"version": str, "protocol": "1.0"}
db.close()
```

`payload` field values must be `None | bool | int | float | str`. Wrap nested structures in a JSON string for now.

## Safe parameter binding

`db.query` accepts positional `$N` bind values either as variadic args or via
the `params=` keyword. Use it for any user-supplied value — concatenation is a
SQL-injection footgun. The cross-driver contract is tracked in
[ADR #352](https://github.com/reddb-io/reddb/issues/352):

```python
# Scalar params: int / text / null
rows = db.query(
    "SELECT id, name FROM users WHERE id = $1 AND tenant = $2 AND deleted_at IS $3",
    42, "acme", None,
)["rows"]

# Vector param (HNSW / IVF similarity search):
rows = db.query(
    "SEARCH SIMILAR $1 IN embeddings K 5",
    [0.1, 0.2, 0.3],
)["rows"]

# `params=` kwarg form is byte-equivalent — useful when composing dynamic
# bind lists:
binds = [42, "acme", None]
rows = db.query(
    "SELECT id FROM users WHERE id = $1 AND tenant = $2 AND deleted_at IS $3",
    params=binds,
)["rows"]
```

Native Python → engine type mapping (see `py_to_param_value` in
`drivers/python/src/high_level.rs`):

| Python                                | Engine            |
|---------------------------------------|-------------------|
| `None`                                | Null              |
| `bool`                                | Bool              |
| `int`                                 | Int (i64; u64 above `i64::MAX`) |
| `float`                               | Float (f64)       |
| `str`                                 | Text              |
| `bytes`, `bytearray`                  | Bytes             |
| `list[int \| float]`                  | Vector (f32)      |
| `datetime.datetime`                   | Timestamp (secs)  |
| `uuid.UUID`                           | Uuid              |
| `dict[str, ...]`                      | Json (canonical)  |

Mixed-type lists (e.g. `[1, "two"]`) raise `INVALID_PARAMS` — vectors must be
all-numeric. `bool` is checked before `int` (since `bool` is an `int` subclass
in Python) so `True` ships as Bool, not `1`.

Embedded backends (`memory://`, `file://`) route params through the engine's
`query_with_params` entry point. The gRPC backend currently raises
`PARAMS_UNSUPPORTED` — the gRPC server has not yet advertised `FEATURE_PARAMS`
(tracked by #359); use `memory://` or `file://` until that lands.
`db.query(sql)` with no params stays on the legacy single-arg path — byte-
identical to pre-params releases.

## Errors

Raised as `ValueError("[CODE] message")`. Stable codes:

| Code                 | Meaning                                         |
|----------------------|-------------------------------------------------|
| `INVALID_URI`        | URI malformed                                   |
| `UNSUPPORTED_SCHEME` | Scheme not recognised                           |
| `INVALID_PARAMS`     | Wrong argument type                             |
| `QUERY_ERROR`        | SQL parse or execution failure                  |
| `IO_ERROR`           | Disk / file backend failure                     |
| `FEATURE_DISABLED`   | Path gated behind a Cargo feature              |
| `CLIENT_CLOSED`      | Method called after `close()`                   |
| `PARAMS_UNSUPPORTED` | Bind values sent to a backend that lacks `FEATURE_PARAMS` (e.g. gRPC today) |

## Build from source

If a prebuilt wheel isn't available for your platform:

```bash
python3 -m venv .venv
source .venv/bin/activate
pip install maturin
cd drivers/python
maturin develop --release
python -c "import reddb; print(reddb.connect('memory://').version())"
```

Requires a working Rust toolchain.

## Limits

- Blocking API only (GIL is released during query execution).
- `query` returns the full result set — no streaming yet.
- No connection pooling. `RedDb` is `Send + Sync` on the Rust side; share between threads.

## Production checklist

- Run the server with the [encrypted vault](../../security/vault.md).
- Track every credential in [Secret Inventory](../../operations/secrets.md).
- See [Transport TLS](../../security/transport-tls.md) for mTLS / OAuth.

## Driver source

[`drivers/python/README.md`](https://github.com/reddb-io/reddb/blob/main/drivers/python/README.md) — low-level `legacy_grpc_connect` / `wire_connect`, build details, error glossary.
