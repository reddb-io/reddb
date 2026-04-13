# reddb-client

Official Rust client for [RedDB](https://github.com/forattini-dev/reddb). One
connection-string API, two backends (only one wired today).

## Install

```bash
cargo add reddb-client
```

## Quickstart

```rust
use reddb_client::{Reddb, JsonValue};

#[tokio::main]
async fn main() -> reddb_client::Result<()> {
    let db = Reddb::connect("memory://").await?;
    // or:  Reddb::connect("file:///var/lib/reddb/data.rdb").await?;

    db.insert(
        "users",
        &JsonValue::object([
            ("name", JsonValue::string("Alice")),
            ("age", JsonValue::number(30)),
        ]),
    )
    .await?;

    let result = db.query("SELECT * FROM users").await?;
    for row in &result.rows {
        for (col, value) in row {
            println!("{col} = {value}");
        }
    }

    db.close().await?;
    Ok(())
}
```

## Connection URIs

| URI                       | Backend                          | Status        |
|---------------------------|----------------------------------|---------------|
| `memory://`               | Ephemeral in-memory              | ✅            |
| `file:///absolute/path`   | Embedded engine on disk          | ✅            |
| `grpc://host:port`        | Remote tonic client              | ⚠ planned     |

`grpc://` returns `ErrorCode::FeatureDisabled` today. The remote client lands
in **PLAN_DRIVERS.md Phase 3.5** — see the repo root.

## Cargo features

```toml
[dependencies]
reddb-client = { version = "0.1", default-features = false, features = ["embedded"] }
```

| Feature    | Default | What it does                                                 |
|------------|---------|--------------------------------------------------------------|
| `embedded` | yes     | Compiles the full RedDB engine in-process. Best perf.        |
| `grpc`     | no      | Reserved for the upcoming remote client.                     |

## Errors

Every fallible call returns `reddb_client::Result<T>` with a `ClientError`:

```rust
use reddb_client::{ErrorCode, Reddb};

# async fn run() {
let err = Reddb::connect("mongodb://localhost").await.unwrap_err();
assert_eq!(err.code, ErrorCode::UnsupportedScheme);
println!("{err}");  // [UNSUPPORTED_SCHEME] unsupported URI scheme: 'mongodb'.
# }
```

Stable codes:

| code                  | meaning                                                    |
|-----------------------|------------------------------------------------------------|
| `UNSUPPORTED_SCHEME`  | URI scheme not recognized                                  |
| `INVALID_URI`         | URI malformed                                              |
| `IO_ERROR`            | Disk / file backend failure                                |
| `QUERY_ERROR`         | SQL parse or execution failure                             |
| `FEATURE_DISABLED`    | Caller hit a path gated behind a Cargo feature             |
| `CLIENT_CLOSED`       | Method called after `close()`                              |
| `INTERNAL_ERROR`      | Unexpected engine failure                                  |

These match the JSON-RPC error codes used by `red rpc --stdio` so tests across
language drivers compare 1:1.

## Limits

- The embedded backend pulls the entire RedDB engine into your binary. It is
  fast but not small — expect compile time and binary size in the megabytes.
- No pooling. The `Reddb` handle is `Send + Sync` via `Arc`, so feel free to
  `clone()` it across tasks.
- `query` returns the full result set in memory. Streaming is on the roadmap.

## Testing

```bash
cargo test -p reddb-client
```
