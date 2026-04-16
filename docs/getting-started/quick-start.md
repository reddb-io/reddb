# Quick Start

This guide gets RedDB running, writes multiple data shapes, and shows how to query them back.

Before you start, keep one rule in mind: in RedDB, a `collection` is the named logical container, and
rows, documents, nodes, edges, vectors, KV entries, time-series points, and queue messages are the
entity shapes or model semantics stored in collections. `hosts`, `network`, and `embeddings` below are
collection names, not separate databases or folders above the models.

## 1. Start RedDB

Start RedDB with both remote APIs in one process:

```bash
mkdir -p ./data
red server \
  --path ./data/reddb.rdb \
  --grpc-bind 127.0.0.1:50051 \
  --http-bind 127.0.0.1:8080
```

If you want an ephemeral database for testing, omit `--path`.

## 2. Write a row

```bash
curl -X POST http://127.0.0.1:8080/collections/hosts/rows \
  -H 'content-type: application/json' \
  -d '{
    "fields": {
      "ip": "10.0.0.1",
      "os": "linux",
      "critical": true
    }
  }'
```

## 3. Write a graph node

```bash
curl -X POST http://127.0.0.1:8080/collections/network/nodes \
  -H 'content-type: application/json' \
  -d '{
    "label": "host-10.0.0.1",
    "node_type": "host",
    "properties": {
      "ip": "10.0.0.1",
      "environment": "prod"
    }
  }'
```

## 4. Write a vector

```bash
curl -X POST http://127.0.0.1:8080/collections/embeddings/vectors \
  -H 'content-type: application/json' \
  -d '{
    "dense": [0.12, 0.91, 0.44],
    "content": "prod linux host running ssh",
    "metadata": {
      "source": "inventory"
    }
  }'
```

## 5. Query with SQL-style syntax

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query":"SELECT * FROM hosts WHERE critical = true"}'
```

## 6. Query across models

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query":"FROM ANY ORDER BY _score DESC LIMIT 10"}'
```

`FROM ANY` is the shortest way to ask RedDB for a cross-model result set.

## 7. Check health

```bash
curl -s http://127.0.0.1:8080/health
curl -s http://127.0.0.1:8080/ready
curl -s http://127.0.0.1:8080/stats
```

## 8. Optional: connect over gRPC

Then connect with the CLI:

```bash
red connect 127.0.0.1:50051
```

## 9. Optional: use it embedded instead

If you want RedDB in-process, open the same kind of file directly from Rust:

```rust
use reddb::RedDB;
use reddb::storage::schema::Value;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = RedDB::open("./data/reddb.rdb")?;

    db.row("hosts", vec![
        ("ip", Value::Text("10.0.0.1".into())),
        ("critical", Value::Boolean(true)),
    ]).save()?;

    let results = db.query()
        .collection("hosts")
        .where_prop("critical", true)
        .execute()?;

    println!("matched {}", results.len());
    db.flush()?;
    Ok(())
}
```

## What next

- [Installation](/getting-started/installation.md)
- [Connect](/getting-started/connect.md)
- [HTTP API](/api/http.md)
- [Embedded API](/api/embedded.md)
