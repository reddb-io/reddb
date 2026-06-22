# Rust driver

The official Rust client is published on crates.io as **`reddb-io-client`**. One connection-string API across embedded, gRPC, HTTP, and RedWire transports, gated behind Cargo features so binaries only link the transports they use.

- **Crate:** [`reddb-io-client`](https://crates.io/crates/reddb-io-client)
- **Library import name:** `reddb_client` (unchanged across the 1.0 rename)
- **Source:** [`crates/reddb-client/`](https://github.com/reddb-io/reddb/tree/main/crates/reddb-client)
- **Status:** Stable

For the embedded-engine API (`use reddb::RedDB`, the fluent builder, etc.), see [Embedded (Rust API)](../../api/embedded.md). The page you're reading is about the **network-client** crate.

## Install

```toml
[dependencies]
reddb-io-client = { version = "1.0", features = ["grpc"] }
```

You must opt into a transport via a Cargo feature — none are enabled by default to keep dependency closure tight:

| Feature        | Transport(s) it enables                                    |
|----------------|------------------------------------------------------------|
| `embedded`     | In-process engine (pulls `reddb-io-server`).               |
| `grpc`         | Tonic-based gRPC client (`grpc://` / `grpcs://`).          |
| `http`         | HTTP / HTTPS REST.                                         |
| `redwire`      | RedWire TCP binary protocol.                               |
| `redwire-tls`  | `redwire` + TLS / mTLS.                                    |

You can enable any combination; `Reddb::connect(uri)` dispatches by URI scheme.

## Connect and query

```rust
use reddb_client::{Reddb, JsonValue, Value};

#[tokio::main]
async fn main() -> reddb_client::Result<()> {
    let db = Reddb::connect("grpc://localhost:55055").await?;

    db.insert(
        "users",
        &JsonValue::object([("name", JsonValue::string("Alice"))]),
    ).await?;

    let result = db
        .query_with(
            "SELECT * FROM users WHERE name = $1 LIMIT $2",
            &[Value::Text("Alice".into()), Value::Int64(10)],
        )
        .await?;
    println!("{} rows", result.rows.len());

    let _hits = db
        .query_with(
            "SEARCH SIMILAR $1 IN embeddings K $2",
            &[Value::Vector(vec![0.1, 0.2, 0.3]), Value::Int64(5)],
        )
        .await?;

    db.close().await?;
    Ok(())
}
```

## Authentication

```rust
use reddb_client::{Reddb, ConnectOptions};

let opts = ConnectOptions::builder()
    .token("sk-abc")                          // bearer
    // .basic_auth("alice", "hunter2")        // SCRAM (RedWire) / /auth/login (HTTP)
    // .jwt("<oauth-jwt>")                    // OAuth-JWT
    .build();

let db = Reddb::connect_with("red://reddb.example.com:5050", opts).await?;
```

Credentials can also ride in the URI: `red://user:pass@host`, `red://host?token=...`.

For the native RedWire transport, construct the RedWire client directly:

```rust
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

## Connection strings

| Scheme       | Transport                  | Default port | Required feature   |
|--------------|----------------------------|:------------:|--------------------|
| `red://`     | RedWire TCP                | 5050         | `redwire`          |
| `reds://`    | RedWire over TLS / mTLS    | 5050         | `redwire-tls`      |
| `grpc://`    | gRPC                       | 55055         | `grpc`             |
| `grpcs://`   | gRPC over TLS              | 55555         | `grpc`             |
| `http://`    | HTTP REST                  | 5000         | `http`             |
| `https://`   | HTTPS REST                 | 55555         | `http`             |
| `memory://`  | Embedded in-memory engine  | —            | `embedded`         |
| `file:///…`  | Embedded on-disk engine    | —            | `embedded`         |

Topology / multi-host (read-replicas + primary failover):

```rust
let db = Reddb::connect("grpc://primary:55055,replica1:55055,replica2:55055").await?;
```

The driver round-robins reads across replicas and pins writes to the primary. See [Connection Strings — gRPC cluster](../connection-strings.md#grpc-cluster-primary--read-replicas).

## What's in the crate

- `reddb_client::Reddb` — top-level facade, transport-agnostic.
- `reddb_client::JsonValue`, `ClientError`, `Result` — value + error types.
- `reddb_client::connect::Target` — parsed URI; share via `Target::parse(uri)?`.
- `reddb_client::connector::RedDBClient` — back-compat re-export of the tiny tonic-only connector (lives in `reddb-io-client-connector`).

Full API: [docs.rs/reddb-io-client](https://docs.rs/reddb-io-client).

## Safe parameter binding

`query_with(sql, &[params])` binds positional `$N` placeholders. Use it for any
user-supplied value — concatenation is a SQL-injection footgun. The
cross-driver contract is tracked in
[ADR #352](https://github.com/reddb-io/reddb/issues/352):

```rust
use reddb_client::{Reddb, Value};

# async fn run(db: Reddb) -> reddb_client::Result<()> {
// Scalar params: int / text / null
let rows = db
    .query_with(
        "SELECT id, name FROM users WHERE id = $1 AND tenant = $2 AND deleted_at IS $3",
        &[Value::Int64(42), Value::Text("acme".into()), Value::Null],
    )
    .await?;

// Vector param (HNSW / IVF similarity search):
let hits = db
    .query_with(
        "SEARCH SIMILAR $1 IN embeddings K $2",
        &[Value::Vector(vec![0.1, 0.2, 0.3]), Value::Int64(5)],
    )
    .await?;
# let _ = (rows, hits);
# Ok(())
# }
```

Native Rust → engine type mapping (see `reddb_client::params::IntoValue`):

| Rust | Engine value |
|------|--------------|
| `Option<T>::None` | Null |
| `bool` | Bool |
| `i8..i64`, `u8..u32` | Int |
| `Value::Int64(n)` | Int |
| `f32`, `f64` | Float |
| `&str`, `String` | Text |
| `Vec<u8>`, `&[u8]` | Bytes |
| `Vec<f32>`, `&[f32]`, `Value::Vector` | Vector |
| `serde_json::Value`, `JsonValue`, `Value::Json` | Json |
| `Value::Timestamp(seconds)` | Timestamp |
| `Value::Uuid(bytes)` | Uuid |

The embedded, HTTP, gRPC, and native RedWire transports carry params
end-to-end. Empty params are equivalent to `query(sql)` and stay on the legacy
no-params path.

## Embedded mode

If you want to embed the **engine** itself (not just the client crate's `embedded` feature), depend on the umbrella crate directly and use the fluent / runtime APIs documented at [Embedded (Rust API)](../../api/embedded.md):

```toml
[dependencies]
reddb-io = "1.0"
```

```rust
use reddb::RedDB;
let db = RedDB::open("./data.rdb")?;
```

## Production checklist

- Pin the crate version against the engine version — both ship in lock-step.
- Always use `reds://` / `grpcs://` / `https://` outside localhost.
- For mTLS, see [Transport TLS](../../security/transport-tls.md).
- Manage tokens / certs via [Secret Inventory](../../operations/secrets.md).
- See [Policies](../../security/policies.md) for IAM-style authorization.

## Driver source

[`crates/reddb-client/README.md`](https://github.com/reddb-io/reddb/blob/main/crates/reddb-client/README.md) — feature reference, error types, and topology details.
