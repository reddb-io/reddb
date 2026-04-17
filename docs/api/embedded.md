# Embedded (Rust API)

Embedded mode means RedDB runs inside your Rust process, without a separate server. Think of it as the RedDB equivalent of using SQLite in-process, except the engine is built for multiple data models instead of only relational tables.

There are two practical embedded APIs:

- `RedDB` for fluent, builder-style access
- `RedDBRuntime` for runtime/use-case access, including SQL-style query execution

## 1. Fluent embedded API with `RedDB`

### Add the dependency

```toml
[dependencies]
reddb = "0.1"
```

### Open a database

```rust
use reddb::RedDB;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = RedDB::open("./data/reddb.rdb")?;
    db.flush()?;
    Ok(())
}
```

Use `RedDB::new()` if you want an in-memory database.

### Create rows, nodes, vectors, documents, and KV

```rust
use reddb::RedDB;
use reddb::storage::schema::Value;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = RedDB::open("./data/reddb.rdb")?;

    let user_id = db.row("users", vec![
        ("name", Value::Text("Alice".into())),
        ("age", Value::Integer(30)),
        ("active", Value::Boolean(true)),
    ]).save()?;

    let host_id = db.node("network", "host")
        .node_type("machine")
        .property("hostname", "web-01")
        .property("owner", "platform")
        .save()?;

    let vector_id = db.vector("embeddings")
        .dense(vec![0.12, 0.91, 0.44])
        .content("web-01 runs nginx and ssh")
        .metadata("source", "inventory")
        .save()?;

    let doc_id = db.doc("events")
        .field("kind", "login")
        .field("user", "alice")
        .field("success", true)
        .save()?;

    let cfg_id = db.kv("config", "theme", Value::Text("dark".into()))
        .metadata("updated_by", "admin")
        .save()?;

    println!("{user_id} {host_id} {vector_id} {doc_id} {cfg_id}");
    db.flush()?;
    Ok(())
}
```

### Query with the fluent builder

```rust
use reddb::RedDB;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = RedDB::open("./data/reddb.rdb")?;

    let results = db.query()
        .collection("users")
        .where_prop("active", true)
        .limit(10)
        .execute()?;

    println!("matched {}", results.len());
    Ok(())
}
```

### Vector similarity search

```rust
use reddb::RedDB;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = RedDB::open("./data/reddb.rdb")?;

    let matches = db.similar("embeddings", &[0.12, 0.91, 0.44], 5);
    println!("matches {}", matches.len());
    Ok(())
}
```

## 2. Embedded runtime with `RedDBRuntime`

If you want the application/use-case layer in-process, use `RedDBRuntime`. This is the better fit when you want SQL-style execution while still keeping everything embedded.

### Open a persistent runtime

```rust
use reddb::{RedDBOptions, RedDBRuntime};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _rt = RedDBRuntime::with_options(
        RedDBOptions::persistent("./data/reddb.rdb")
    )?;
    Ok(())
}
```

### Write with use-cases and query with SQL-style syntax

```rust
use reddb::application::{CreateRowInput, ExecuteQueryInput};
use reddb::storage::schema::Value;
use reddb::{EntityUseCases, QueryUseCases, RedDBOptions, RedDBRuntime};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let rt = RedDBRuntime::with_options(
        RedDBOptions::persistent("./data/reddb.rdb")
    )?;

    EntityUseCases::new(&rt).create_row(CreateRowInput {
        collection: "users".into(),
        fields: vec![
            ("name".into(), Value::Text("Alice".into())),
            ("age".into(), Value::Integer(30)),
        ],
        metadata: vec![],
        node_links: vec![],
        vector_links: vec![],
    })?;

    let result = QueryUseCases::new(&rt).execute(ExecuteQueryInput {
        query: "SELECT * FROM users".into(),
    })?;

    println!("rows = {}", result.result.records.len());
    rt.checkpoint()?;
    Ok(())
}
```

### In-memory runtime

```rust
use reddb::RedDBRuntime;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _rt = RedDBRuntime::in_memory()?;
    Ok(())
}
```

## Retention and maintenance

If you run embedded, there is no always-on background process calling maintenance for you. Run it explicitly at points that make sense for your application:

```rust
use reddb::RedDB;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = RedDB::open("./data/reddb.rdb")?;
    db.enforce_retention_policy()?;
    db.flush()?;
    Ok(())
}
```

## When embedded mode is the right choice

- desktop applications
- local CLIs
- workers and batch jobs
- edge binaries
- services that want zero network hops between app code and storage

If you need remote clients, shared access from multiple services, or a CLI REPL from another process, use server mode instead.

## Logging — optional `tracing` initialisation

RedDB emits structured logs through `tracing` but **never installs a
subscriber itself** when used as a library. Your app owns the
subscriber so RedDB's logs can share pipelines with the rest of your
code.

### Let RedDB configure it for you

`reddb::telemetry::init(cfg)` sets up stderr + optional file rotation
using the same helpers the `red` binary uses:

```rust
use reddb::telemetry::{init, LogFormat, TelemetryConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Keep the guard alive for the whole process lifetime —
    // dropping it flushes pending file writes.
    let _telemetry_guard = init(TelemetryConfig {
        log_dir: Some("./logs".into()),
        file_prefix: "myapp.log".into(),
        level_filter: "info,reddb=debug".into(),
        format: LogFormat::Pretty,
        rotation_keep_days: 7,
        service_name: "myapp",
    });

    let db = reddb::RedDB::open("./data.rdb")?;
    // ...
    Ok(())
}
```

### Use your own subscriber

If your application already runs its own `tracing-subscriber::fmt` or
`tracing-opentelemetry` pipeline, just don't call
`reddb::telemetry::init`. Every `tracing::info!` / `warn!` inside
RedDB gets routed through whichever subscriber you registered.

`reddb::telemetry::init` is idempotent — if a subscriber is already
installed, it returns `None` without panicking.

### Fields to filter on

RedDB stamps the following fields on spans it creates:

| Span | Fields |
|------|--------|
| `query` | `conn_id`, `tenant`, `query_len` |
| `conn` | `transport`, `peer` |
| `listener` | `transport`, `bind` |

Use `tracing-subscriber::EnvFilter` to silence or elevate per-target:

```text
RUST_LOG=warn,reddb::wire=debug,reddb::runtime=info
```
