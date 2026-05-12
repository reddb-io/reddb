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
use reddb_client::{Reddb, JsonValue};

#[tokio::main]
async fn main() -> reddb_client::Result<()> {
    let db = Reddb::connect("grpc://localhost:5055").await?;

    db.insert(
        "users",
        &JsonValue::object([("name", JsonValue::string("Alice"))]),
    ).await?;

    let result = db.query("SELECT * FROM users LIMIT 10").await?;
    println!("{} rows", result.rows.len());

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

## Connection strings

| Scheme       | Transport                  | Default port | Required feature   |
|--------------|----------------------------|:------------:|--------------------|
| `red://`     | RedWire TCP                | 5050         | `redwire`          |
| `reds://`    | RedWire over TLS / mTLS    | 5050         | `redwire-tls`      |
| `grpc://`    | gRPC                       | 5055         | `grpc`             |
| `grpcs://`   | gRPC over TLS              | 5056         | `grpc`             |
| `http://`    | HTTP REST                  | 8080         | `http`             |
| `https://`   | HTTPS REST                 | 8443         | `http`             |
| `memory://`  | Embedded in-memory engine  | —            | `embedded`         |
| `file:///…`  | Embedded on-disk engine    | —            | `embedded`         |

Topology / multi-host (read-replicas + primary failover):

```rust
let db = Reddb::connect("grpc://primary:5055,replica1:5055,replica2:5055").await?;
```

The driver round-robins reads across replicas and pins writes to the primary. See [Connection Strings — gRPC cluster](../connection-strings.md#grpc-cluster-primary--read-replicas).

## What's in the crate

- `reddb_client::Reddb` — top-level facade, transport-agnostic.
- `reddb_client::JsonValue`, `ClientError`, `Result` — value + error types.
- `reddb_client::connect::Target` — parsed URI; share via `Target::parse(uri)?`.
- `reddb_client::connector::RedDBClient` — back-compat re-export of the tiny tonic-only connector (lives in `reddb-io-client-connector`).

Full API: [docs.rs/reddb-io-client](https://docs.rs/reddb-io-client).

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
