# Embedded Mode

In embedded mode, RedDB runs as a Rust library inside your application process. No separate server, no network hop.

## Setup

```toml
[dependencies]
reddb = { version = "0.1", features = ["query-vector", "query-graph"] }
```

## Usage

```rust
use reddb::RedDB;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // In-memory
    let db = RedDB::new();

    // Or file-backed
    // let db = RedDB::open("./data/reddb.rdb")?;

    // Use directly
    let id = db.row("users", vec![
        ("name", reddb::Value::Text("Alice".into())),
    ]).save()?;

    let results = db.query("SELECT * FROM users")?;
    println!("{} users found", results.record_count);

    Ok(())
}
```

## Characteristics

| Property | Value |
|:---------|:------|
| Latency | Nanoseconds (no network) |
| Concurrency | Thread-safe with connection pool |
| Persistence | Optional (file-backed or in-memory) |
| Deployment | Single binary, no external processes |

## When to Use

- CLI tools that need local storage
- Desktop applications with built-in database
- Microservices with co-located data
- Testing and development
- Edge/IoT devices

See [Embedded Rust API](/api/embedded.md) for the complete API reference.
