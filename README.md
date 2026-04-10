# RedDB

RedDB is a unified multi-model database engine for teams that do not want to split operational data, documents, graph relationships, vector embeddings, and key-value state across different systems.

It gives you one engine, one persistence layer, and one operational surface for:

- tables and rows
- JSON-like documents
- graph nodes and edges
- vector embeddings and similarity search
- key-value records

## What RedDB does

RedDB lets one application work with different data shapes in the same database file or server runtime.

Typical use cases:

- operational application state with SQL-style querying
- graph-aware products that also need regular tables
- semantic retrieval and vector search next to first-party data
- local-first or edge deployments that want an embedded database
- AI/agent workflows that need MCP, HTTP, gRPC, or in-process access

## How RedDB works

RedDB uses the same core engine across three practical modes:

| Mode | When to use it | How you access it |
|:-----|:---------------|:------------------|
| Embedded | Your app should own the database directly, like SQLite | Rust API (`RedDB` or `RedDBRuntime`) |
| Server | Multiple clients or services need to connect | HTTP or gRPC |
| Agent / tooling | You want CLI or MCP integration on top of the same engine | `red` CLI or MCP server |

That means the storage model stays the same whether you:

- open a local `.rdb` file inside your Rust process
- run `red server --grpc-bind 127.0.0.1:50051 --http-bind 127.0.0.1:8080`
- expose the same database to AI agents through MCP

## Install

### GitHub releases

The recommended install path is the release installer, which pulls the correct asset from GitHub Releases:

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

If you prefer manual installation, download the asset for your platform from GitHub Releases and place the `red` binary somewhere in your `PATH`.

Release page:

`https://github.com/forattini-dev/reddb/releases`

### npx

`reddb-cli` is also published as an npm package that installs and runs the real `red` binary for you.

Run RedDB through `npx`:

```bash
npx reddb-cli@latest version
```

Start an HTTP server through `npx`:

```bash
npx reddb-cli@latest server --http --path ./data/reddb.rdb --bind 127.0.0.1:8080
```

### Build from source

```bash
cargo build --release --bin red
./target/release/red version
```

## Run a server

### Local Dev

```bash
mkdir -p ./data
red server \
  --path ./data/reddb.rdb \
  --grpc-bind 127.0.0.1:50051 \
  --http-bind 127.0.0.1:8080
```

Create data:

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

Query it:

```bash
curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query":"SELECT * FROM hosts WHERE critical = true"}'
```

Health check:

```bash
curl -s http://127.0.0.1:8080/health
```

This is the recommended local setup because it gives you:

- HTTP for `curl`, browser tooling, and scripts
- gRPC for `red connect` and service-to-service clients

## Connect to RedDB

There are two main connection paths:

- HTTP clients call the REST endpoints directly.
- `red connect` opens a gRPC session to a running RedDB server.

### Connect over HTTP

```bash
curl -s http://127.0.0.1:8080/health

curl -X POST http://127.0.0.1:8080/query \
  -H 'content-type: application/json' \
  -d '{"query":"FROM ANY ORDER BY _score DESC LIMIT 10"}'
```

### Connect with the CLI REPL

Start a gRPC server first:

```bash
red server \
  --path ./data/reddb.rdb \
  --grpc-bind 127.0.0.1:50051 \
  --http-bind 127.0.0.1:8080
```

Then connect:

```bash
red connect 127.0.0.1:50051
```

One-shot query:

```bash
red connect --query "SELECT * FROM hosts" 127.0.0.1:50051
```

If auth is enabled:

```bash
red connect --token "$REDDB_TOKEN" 127.0.0.1:50051
```

## Embedded like SQLite

If you want RedDB inside your process, open the database directly from Rust and work against the same engine without a separate server.

### Fluent embedded API

```rust
use reddb::RedDB;
use reddb::storage::schema::Value;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = RedDB::open("./data/reddb.rdb")?;

    let _user_id = db.row("users", vec![
        ("name", Value::Text("Alice".into())),
        ("active", Value::Boolean(true)),
    ]).save()?;

    let _node_id = db.node("identity", "user")
        .node_type("account")
        .property("name", "Alice")
        .save()?;

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

### Embedded runtime with SQL-style queries

If you want embedded execution with the runtime/use-case layer, use `RedDBRuntime`. This is the closest path to using RedDB "like SQLite", but with the project's multi-model runtime.

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

## Documentation

- Docs home: [docs/README.md](docs/README.md)
- Installation: [docs/getting-started/installation.md](docs/getting-started/installation.md)
- Quick start: [docs/getting-started/quick-start.md](docs/getting-started/quick-start.md)
- Connection guide: [docs/getting-started/connect.md](docs/getting-started/connect.md)
- Embedded guide: [docs/api/embedded.md](docs/api/embedded.md)
- HTTP API: [docs/api/http.md](docs/api/http.md)
- CLI reference: [docs/api/cli.md](docs/api/cli.md)
