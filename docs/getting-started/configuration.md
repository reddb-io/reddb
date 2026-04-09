# Configuration

RedDB is configured through CLI flags and environment variables. There are no configuration files by default -- the binary is self-contained.

## Server Flags

```bash
red server [flags]
```

| Flag | Short | Description | Default |
|:-----|:------|:------------|:--------|
| `--path` | `-d` | Persistent database file path (omit for in-memory) | `./data/reddb.rdb` |
| `--bind` | `-b` | Bind address `host:port` | `127.0.0.1:50051` (gRPC) or `127.0.0.1:8080` (HTTP) |
| `--grpc` | | Serve the gRPC API (default transport) | `true` |
| `--http` | | Serve the HTTP API | `false` |
| `--role` | `-r` | Replication role: `standalone`, `primary`, `replica` | `standalone` |
| `--primary-addr` | | Primary gRPC address for replica mode | |
| `--read-only` | | Open database in read-only mode | `false` |
| `--no-create-if-missing` | | Fail if database file doesn't exist | `false` |
| `--vault` | | Enable encrypted auth vault | `false` |

## Storage Modes

### In-Memory

Omit `--path` to run entirely in RAM. Useful for development and testing:

```bash
red server --http --bind 127.0.0.1:8080
```

### Persistent (File-Backed)

Specify `--path` to persist data to disk with WAL-based durability:

```bash
red server --http --path ./data/reddb.rdb --bind 127.0.0.1:8080
```

### Read-Only

Open an existing database without writes:

```bash
red server --http --path ./data/reddb.rdb --read-only --bind 127.0.0.1:8080
```

## Deployment Profiles

RedDB supports three deployment profiles, each with different operational characteristics:

| Profile | Use Case | Characteristics |
|:--------|:---------|:----------------|
| **Embedded** | Library usage inside a Rust process | Direct function calls, no network, lowest latency |
| **Server** | Standalone database server | HTTP/gRPC transport, connection pool, full operational surface |
| **Serverless** | Edge/function workloads | Attach/warmup/reclaim lifecycle, ephemeral connections |

Query the active profile:

```bash
curl http://127.0.0.1:8080/deployment/profiles?profile=server
```

## Replication

### Primary Mode

```bash
red server --grpc --path ./data/primary.rdb --role primary --bind 0.0.0.0:50051
```

### Replica Mode

```bash
red replica \
  --primary-addr http://primary-host:50051 \
  --path ./data/replica.rdb \
  --http --bind 0.0.0.0:8080
```

Check replication status:

```bash
red status --bind primary-host:50051
```

## Global CLI Flags

These flags work with any `red` command:

| Flag | Short | Description |
|:-----|:------|:------------|
| `--help` | `-h` | Show help for the command |
| `--json` | `-j` | Force JSON output |
| `--output FORMAT` | `-o` | Output format: `text`, `json`, or `yaml` |
| `--verbose` | `-v` | Verbose output |
| `--no-color` | | Disable colored output |
| `--version` | | Show version |

## Feature Flags (Compile-Time)

When using RedDB as a Rust crate, you control features at compile time:

```toml
[dependencies]
reddb = { version = "0.1", features = ["query-vector", "query-graph", "encryption"] }
```

| Feature | What It Enables |
|:--------|:----------------|
| `query-vector` | Vector similarity search in the query engine |
| `query-graph` | Graph traversal and analytics in the query engine |
| `query-fulltext` | Full-text search indexing and queries |
| `encryption` | AES-256-GCM encryption at rest |
| `backend-s3` | S3-compatible remote storage backend |
| `backend-turso` | Turso (libSQL) remote backend |
| `backend-d1` | Cloudflare D1 remote backend |

> [!NOTE]
> By default, no optional features are enabled. The base crate provides table and document storage with the core query engine.
