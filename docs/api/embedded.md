# Embedded (Rust API)

RedDB can be used as an embedded database library directly in your Rust application. No server process, no network hop -- just direct function calls.

## Add to Cargo.toml

```toml
[dependencies]
reddb = { version = "0.1", features = ["query-vector", "query-graph"] }
```

## Create a Database

```rust
use reddb::{RedDB, Value};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // In-memory database
    let db = RedDB::new();

    // Or persistent file-backed
    // let db = RedDB::open("./data/reddb.rdb")?;

    Ok(())
}
```

## Table Rows

### Insert

```rust
let user_id = db.row("users", vec![
    ("name", Value::Text("Alice".into())),
    ("email", Value::Text("alice@example.com".into())),
    ("age", Value::Integer(30)),
    ("active", Value::Boolean(true)),
]).save()?;

println!("Created user with ID: {user_id}");
```

### Query

```rust
let results = db.query("SELECT * FROM users WHERE age > 21 ORDER BY name")?;

for record in &results.records {
    println!("{:?}", record);
}
```

### Update

```rust
db.query("UPDATE users SET age = 31 WHERE name = 'Alice'")?;
```

### Delete

```rust
db.query("DELETE FROM users WHERE name = 'Alice'")?;
```

## Graph Nodes and Edges

### Create Nodes

```rust
let alice = db.node("social", "alice")
    .node_type("person")
    .property("name", "Alice Johnson")
    .property("department", "engineering")
    .save()?;

let bob = db.node("social", "bob")
    .node_type("person")
    .property("name", "Bob Smith")
    .property("department", "product")
    .save()?;
```

### Create Edges

```rust
let edge_id = db.edge("social", "REPORTS_TO")
    .from(alice)
    .to(bob)
    .weight(1.0)
    .property("since", "2023-06-01")
    .save()?;
```

### Graph Queries

```rust
// Shortest path
let path = db.query(
    "PATH FROM alice TO charlie ALGORITHM dijkstra"
)?;

// Graph pattern matching
let matches = db.query(
    "MATCH (a:person)-[r:REPORTS_TO]->(b:person) RETURN a.name, b.name"
)?;
```

## Vector Embeddings

### Insert

```rust
let vec_id = db.vector("embeddings")
    .dense(vec![0.12, 0.91, 0.44, 0.33, 0.67])
    .content("host 10.0.0.1 running nginx on port 443")
    .metadata("source", "network-scan")
    .save()?;
```

### Similarity Search

```rust
let results = db.similar("embeddings", &[0.15, 0.89, 0.40, 0.30, 0.70])
    .k(5)
    .min_score(0.7)
    .execute()?;

for result in &results {
    println!("ID: {}, Score: {:.3}, Content: {}", result.id, result.score, result.content);
}
```

## Documents

```rust
let doc_id = db.document("events")
    .body(json!({
        "event_type": "login",
        "user_id": "u_abc123",
        "timestamp": "2024-01-15T10:30:00Z"
    }))
    .metadata("source", "auth-service")
    .save()?;
```

## Key-Value

```rust
// Set
db.kv_set("config", "max_retries", Value::Integer(5))?;

// Get
let val = db.kv_get("config", "max_retries")?;
println!("max_retries = {:?}", val);
```

## Universal Query

```rust
let results = db.query("FROM ANY ORDER BY _score DESC LIMIT 20")?;

for record in &results.records {
    println!("[{}] {} ({}): score={:.3}",
        record.collection,
        record.entity_id,
        record.kind,
        record.score,
    );
}
```

## Error Handling

All operations return `Result` types:

```rust
match db.row("users", vec![("name", Value::Text("Alice".into()))]).save() {
    Ok(id) => println!("Created entity {id}"),
    Err(e) => eprintln!("Failed to create entity: {e}"),
}
```

## Complete Example

```rust
use reddb::{RedDB, Value};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = RedDB::new();

    // Store operational data as a row
    let host_id = db.row("hosts", vec![
        ("ip", Value::Text("10.0.0.1".into())),
        ("os", Value::Text("linux".into())),
        ("critical", Value::Boolean(true)),
    ]).save()?;

    // Link it as a graph node
    let node_id = db.node("network", "host-10.0.0.1")
        .node_type("host")
        .property("ip", "10.0.0.1")
        .save()?;

    // Attach a vector embedding
    let vec_id = db.vector("embeddings")
        .dense(vec![0.12, 0.91, 0.44])
        .content("host 10.0.0.1 running ssh")
        .save()?;

    // Query across all data shapes
    let results = db.query("FROM ANY ORDER BY _score DESC LIMIT 10")?;
    println!("Found {} entities across all models", results.record_count);

    // Structured query
    let hosts = db.query("SELECT * FROM hosts WHERE critical = true")?;
    println!("Critical hosts: {}", hosts.record_count);

    Ok(())
}
```

> [!TIP]
> The embedded API has the lowest possible latency since there is no network serialization. Use it when RedDB runs inside your application process.
