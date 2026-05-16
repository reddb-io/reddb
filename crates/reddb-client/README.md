# reddb-io-client

Official Rust client for [RedDB](https://github.com/reddb-io/reddb).
One connection-string API across embedded, gRPC, HTTP, and
RedWire transports. Also hosts the `red_client` binary and the
workspace-internal connector used by `red`'s REPL and
`reddb-server`'s rpc_stdio mode.

## Quickstart (library)

```toml
[dependencies]
reddb-io-client = "1.2"
```

```rust,no_run
use reddb_client::{Reddb, JsonValue};

# async fn run() -> reddb_client::Result<()> {
let db = Reddb::connect("memory://").await?;
db.insert("users", &JsonValue::object([("name", JsonValue::string("Alice"))])).await?;
let result = db.query("SELECT * FROM users").await?;
println!("{} rows", result.rows.len());
db.close().await?;
# Ok(())
# }
```

### Rich helpers

The Rust client exposes the same helper families as the other SDKs while
keeping the low-level `query` API available.

```rust,no_run
use reddb_client::{JsonValue, ListOptions, Reddb};

# async fn run() -> reddb_client::Result<()> {
let db = Reddb::connect("memory://").await?;

let doc = db
    .documents()
    .insert(
        "events",
        &JsonValue::object([
            ("event_type", JsonValue::string("login")),
            ("attempts", JsonValue::number(1.0)),
        ]),
    )
    .await?;
let _same_doc = db.documents().get("events", &doc.rid).await?;
let _recent = db
    .documents()
    .list("events", ListOptions::new().filter("event_type = 'login'").limit(10))
    .await?;

let kv = db.kv_collection("settings");
kv.set("characters:hansel", JsonValue::string("trail")).await?;
let _value = kv.get("characters:hansel").await?;

let queue = db.queue();
queue.create("jobs").await?;
queue.push("jobs", &JsonValue::object([("kind", JsonValue::string("email"))])).await?;
let _jobs = queue.pop("jobs").await?;
# Ok(())
# }
```

### Parameterized queries

`query_with(sql, &[params])` binds positional `$N` placeholders. SQL
literals never carry untrusted values — vector / int / text params
travel through the same engine binder the prepared-statement path uses,
so injection-by-string is structurally impossible.

```rust,no_run
use reddb_client::{Reddb, Value};

# async fn run() -> reddb_client::Result<()> {
let db = Reddb::connect("memory://").await?;

// Scalars use IntoValue conversions:
let rows = db
    .query_with(
        "SELECT * FROM users WHERE id = $1 AND name = $2",
        &[1i64.into(), "Alice".into()],
    )
    .await?;

// Vector params route through `Value::Vector` (no string-formatting):
let hits = db
    .query_with(
        "SEARCH SIMILAR $1 COLLECTION embeddings LIMIT $2",
        &[Value::Vector(vec![0.1, 0.2, 0.3]), Value::Int64(5)],
    )
    .await?;
# let _ = (rows, hits);
# Ok(())
# }
```

The native RedWire client uses the same parameter API when the crate is built
with `--features redwire`:

```rust,no_run
use reddb_client::redwire::{Auth, ConnectOptions, RedWireClient};
use reddb_client::Value;

# async fn run() -> reddb_client::Result<()> {
let mut client = RedWireClient::connect(
    ConnectOptions::new("127.0.0.1", 5050).with_auth(Auth::Anonymous),
)
.await?;

let rows = client
    .query_with("SELECT $1", &[Value::Int64(42)])
    .await?;
# let _ = rows;
# Ok(())
# }
```

Native Rust → engine `Value` mapping:

| Rust                  | Engine variant          |
|-----------------------|-------------------------|
| `bool`                | `Boolean`               |
| `i8..i64` / `u8..u32` | `Integer` (i64)         |
| `f32` / `f64`         | `Float` (f64)           |
| `&str` / `String`     | `Text`                  |
| `Vec<u8>` / `&[u8]`   | `Blob`                  |
| `Vec<f32>` / `&[f32]` | `Vector`                |
| `Option<T>` (None)    | `Null`                  |
| `serde_json::Value`   | `Json`                  |
| `Value::Int64(n)` | `Int` (i64 compatibility constructor) |
| `Value::Timestamp(s)` | `Timestamp` (seconds)   |
| `Value::Uuid(b)`      | `Uuid` (16 raw bytes)   |

Today the `embedded`, `http`, and Rust `grpc` transports carry parameters
end-to-end. The native Rust RedWire client also carries parameters end-to-end
with `QueryWithParams` and depends on the server advertising `FEATURE_PARAMS`.

## Cargo features

- `embedded` (default) — pulls the engine in-process for
  `memory://` / `file:///` URIs.
- `grpc` — async tonic client for `grpc://` / `red://`.
- `http` — REST client over reqwest+rustls for `http://` / `https://`.
- `redwire` — native RedWire TCP client (no engine).
- `redwire-tls` — adds TLS / mTLS to the RedWire transport.

Disable defaults to drop the engine: `default-features = false`.

## `red_client` binary

The crate also hosts the `red_client` binary (built with
`cargo build -p reddb-io-client --bin red_client --no-default-features`).
It is the thin remote-only client used by ops tooling — no
engine, no embedded backend, just transports:

| Scheme              | Status     |
|---------------------|------------|
| `red://host[:port]` | gRPC, default port 5050 |
| `reds://host[:port]`| TODO (TLS not yet wired in the bin) |
| `grpc://host[:port]`| gRPC, default port 5055 |
| `http://host[:port]`| REST |
| `memory:` / `file://` | rejected (exit 2, points to `red`) |

`red_client` is guarded by [`SIZE_BUDGET`](./SIZE_BUDGET) (stripped
release bytes); CI runs `./scripts/check-red-client-size.sh` on
every PR to catch accidental engine re-linkage.

## Module layout

- `crate::Reddb` / `JsonValue` / `ClientError` — published
  high-level API.
- `crate::connect::{Target, parse}` — back-compat shim over
  [`reddb-wire`'s][rw] connection-string parser.
- `crate::connector::{RedDBClient, repl, http, redwire}` —
  workspace-internal connector consumed by the `red` REPL,
  `red_client` bin, and `reddb-server`'s rpc_stdio mode. The
  gRPC connector type itself lives in the
  [`reddb-client-connector`](../reddb-client-connector) sibling
  crate to break a path-dependency cycle.

## References

- [Connection strings][conn-strings]
- [ADR 0001 — RedWire][adr-0001]
- [Workspace migration guide](../../docs/migration/workspace-split.md)

[adr-0001]: ../../docs/adr/0001-redwire-tcp-protocol.md
[conn-strings]: ../../docs/clients/connection-strings.md
[rw]: ../reddb-wire
[sdk]: ../../docs/clients/sdk-compatibility.md
