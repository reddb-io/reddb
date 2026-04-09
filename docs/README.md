# RedDB

> One engine for tables, documents, graphs, vectors, and key-value data.

RedDB is a multi-model database for applications that need more than one data shape but do not want more than one database runtime.

It is designed for teams that want:

- rows and SQL-style queries
- documents and payload-oriented records
- graph relationships and analytics
- vector embeddings and semantic search
- embedded, server, and agent-facing access without changing engines

## What RedDB does

RedDB keeps different data models inside the same database core so you can work with operational records, linked entities, and semantic retrieval in one place.

Instead of pushing data between a SQL store, a graph store, and a vector store, RedDB keeps those concerns in one runtime and exposes them through:

- an embedded Rust API
- an HTTP server
- a gRPC server
- the `red` CLI
- an MCP server for agent integrations

## How RedDB works

The same core engine can be used in three ways:

| Mode | Best for | Access path |
|:-----|:---------|:------------|
| Embedded | local-first apps, CLIs, workers, edge binaries | `RedDB` or `RedDBRuntime` inside Rust |
| Server | shared databases and service-to-service traffic | HTTP or gRPC |
| Agent tooling | AI workflows and local operations | CLI or MCP |

This is the key idea behind the project: you do not switch products when your architecture changes. You switch how you expose the same engine.

## Install

### GitHub Releases

Use the release installer:

```bash
curl -fsSL https://raw.githubusercontent.com/forattini-dev/reddb/main/install.sh | bash
```

Pin a version:

```bash
curl -fsSL https://raw.githubusercontent.com/forattini-dev/reddb/main/install.sh | bash -s -- --version v0.1.2
```

Use the prerelease channel:

```bash
curl -fsSL https://raw.githubusercontent.com/forattini-dev/reddb/main/install.sh | bash -s -- --channel next
```

Manual assets:

`https://github.com/forattini-dev/reddb/releases`

### `npx`

Run the npm package directly:

```bash
npx reddb-cli@latest version
```

Start RedDB through `npx`:

```bash
npx reddb-cli@latest server --path ./data/reddb.rdb --grpc-bind 127.0.0.1:50051 --http-bind 127.0.0.1:8080
```

## First connection

### Local dev

```bash
red server --path ./data/reddb.rdb --grpc-bind 127.0.0.1:50051 --http-bind 127.0.0.1:8080
```

```bash
curl -s http://127.0.0.1:8080/health
```

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query":"SELECT * FROM hosts"}'
```

The same process also exposes gRPC for the CLI REPL:

```bash
red connect 127.0.0.1:50051
```

`red connect` is for gRPC servers. For HTTP servers, use the REST endpoints directly.

## Embedded like SQLite

If you want RedDB inside your process, use it directly from Rust.

Builder-first API:

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

Runtime/use-case API with SQL-style execution:

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
    Ok(())
}
```

## Start here

<div class="grid-3">
  <a href="#/getting-started/installation" class="card">
    <h4>Installation</h4>
    <p>Install from GitHub Releases, npx, or source.</p>
  </a>
  <a href="#/getting-started/connect" class="card">
    <h4>Connect</h4>
    <p>Choose HTTP, gRPC, CLI, or embedded access.</p>
  </a>
  <a href="#/api/embedded" class="card">
    <h4>Embedded</h4>
    <p>Run RedDB in-process, like SQLite, but multi-model.</p>
  </a>
</div>
