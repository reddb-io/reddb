# Embedded Mode

Embedded mode runs RedDB inside your Rust process, without a separate server. Operationally, this is the closest RedDB gets to the "SQLite model": your application opens the database file directly and calls the API in-process.

## When to choose embedded mode

- you want zero network hops
- your app owns the database lifecycle
- you are building a CLI, desktop app, worker, or edge service
- you want simple local deployment with one process

## Example

```rust
use reddb::RedDB;
use reddb::storage::schema::Value;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = RedDB::open("./data/reddb.rdb")?;

    db.row("users", vec![
        ("name", Value::Text("Alice".into())),
        ("active", Value::Boolean(true)),
    ]).save()?;

    let results = db.query()
        .collection("users")
        .where_prop("active", true)
        .limit(10)
        .execute()?;

    println!("matched {}", results.len());
    db.flush()?;
    Ok(())
}
```

## Characteristics

| Property | Value |
|:---------|:------|
| Process model | In-process |
| Latency profile | No network serialization |
| Persistence | In-memory or file-backed |
| Operational shape | Application-owned lifecycle |

## Maintenance

Embedded mode does not have a long-running RedDB server process to trigger maintenance on your behalf. Call it from your application where appropriate:

```rust
use reddb::RedDB;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = RedDB::open("./data/reddb.rdb")?;
    db.enforce_retention_policy()?;
    db.flush()?;
    Ok(())
}
```

For the full API surface, see [Embedded (Rust)](/api/embedded.md).
