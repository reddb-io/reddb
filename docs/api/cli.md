# CLI Reference

The `red` CLI is the unified binary for running and interacting with RedDB.

## Usage

```bash
red <command> [args] [flags]
```

## Commands

| Command | Description |
|:--------|:------------|
| `server` | Start the database server (HTTP or gRPC) |
| `query` | Execute a query against a running server |
| `insert` | Insert an entity into a collection |
| `get` | Get an entity by ID |
| `delete` | Delete an entity by ID |
| `health` | Run a health check against a server |
| `replica` | Start as a read replica connected to a primary |
| `status` | Show replication status |
| `tick` | Run maintenance/reclaim tick operations |
| `mcp` | Start MCP server for AI agent integration |
| `auth` | Manage authentication (users, tokens, roles) |
| `connect` | Connect to a remote RedDB server (interactive REPL) |
| `version` | Show version information |

## Global Flags

| Flag | Short | Description |
|:-----|:------|:------------|
| `--help` | `-h` | Show help |
| `--json` | `-j` | Force JSON output |
| `--output FORMAT` | `-o` | Output format: `text`, `json`, `yaml` |
| `--verbose` | `-v` | Verbose output |
| `--no-color` | | Disable colored output |
| `--version` | | Show version |

## red server

Start the database server.

```bash
red server [--grpc|--http] [--path ./data/reddb.rdb] [--bind 127.0.0.1:50051]
```

| Flag | Short | Default | Description |
|:-----|:------|:--------|:------------|
| `--path` | `-d` | `./data/reddb.rdb` | Database file path (omit for in-memory) |
| `--bind` | `-b` | by transport | Bind address `host:port` |
| `--grpc` | | default | Serve gRPC API |
| `--http` | | | Serve HTTP API |
| `--role` | `-r` | `standalone` | Replication role: `standalone`, `primary`, `replica` |
| `--primary-addr` | | | Primary gRPC address (for replica mode) |
| `--read-only` | | | Open in read-only mode |
| `--no-create-if-missing` | | | Fail if database doesn't exist |
| `--vault` | | `false` | Enable encrypted auth vault |

Examples:

```bash
# HTTP server with persistent storage
red server --http --path ./data/reddb.rdb --bind 0.0.0.0:8080

# gRPC server (in-memory)
red server --grpc --bind 127.0.0.1:50051

# Primary mode with vault
red server --grpc --path ./data/primary.rdb --role primary --vault --bind 0.0.0.0:50051
```

## red query

Execute a query against a running server.

```bash
red query "SELECT * FROM users WHERE age > 21"
```

| Flag | Short | Default | Description |
|:-----|:------|:--------|:------------|
| `--bind` | `-b` | `0.0.0.0:6380` | Server address |

Examples:

```bash
red query "SELECT * FROM users" --bind 127.0.0.1:50051
red query "FROM ANY LIMIT 10"
red query "INSERT INTO users (name, age) VALUES ('Alice', 30)"
```

## red insert

Insert an entity into a collection.

```bash
red insert <collection> '<json>'
```

| Flag | Short | Default | Description |
|:-----|:------|:--------|:------------|
| `--bind` | `-b` | `0.0.0.0:6380` | Server address |

Example:

```bash
red insert users '{"name": "Alice", "age": 30}'
```

## red get

Retrieve an entity by ID.

```bash
red get <collection> <id>
```

## red delete

Delete an entity by ID.

```bash
red delete <collection> <id>
```

## red health

Check server health.

```bash
red health [--bind host:port] [--grpc|--http]
```

| Flag | Short | Description |
|:-----|:------|:------------|
| `--bind` | `-b` | Server address |
| `--grpc` | | Probe gRPC listener (default) |
| `--http` | | Probe HTTP listener |

## red tick

Run maintenance operations on a running server.

```bash
red tick [--bind 127.0.0.1:8080] [--operations maintenance,retention,checkpoint] [--dry-run]
```

| Flag | Short | Default | Description |
|:-----|:------|:--------|:------------|
| `--bind` | `-b` | `127.0.0.1:8080` | HTTP server address |
| `--operations` | | `maintenance,retention,checkpoint` | Comma-separated operations |
| `--dry-run` | | `false` | Validate operation plan without applying |

## red replica

Start as a read replica.

```bash
red replica --primary-addr http://primary:50051 [--path ./data/replica.rdb]
```

| Flag | Short | Default | Description |
|:-----|:------|:--------|:------------|
| `--primary-addr` | `-p` | | Primary gRPC address |
| `--path` | `-d` | `./data/reddb.rdb` | Local replica path |
| `--bind` | `-b` | by transport | Bind address |
| `--grpc` | | default | Serve gRPC |
| `--http` | | | Serve HTTP |
| `--vault` | | `false` | Enable vault |

## red mcp

Start the MCP server for AI agent integration.

```bash
red mcp [--path /data]
```

| Flag | Short | Default | Description |
|:-----|:------|:--------|:------------|
| `--path` | `-d` | (in-memory) | Data directory path |

## red auth

Manage authentication.

```bash
red auth <subcommand>
```

Subcommands:

```bash
red auth create-user alice --password secret --role admin
red auth create-api-key alice --name "ci-token" --role write
red auth list-users
red auth login alice --password secret
```

## red connect

Interactive REPL to a remote server.

```bash
red connect [--token <token>] [--query <sql>] <addr>
```

| Flag | Description |
|:-----|:------------|
| `--token` | Authentication token |
| `--query` | Execute a single query (non-interactive) |

Examples:

```bash
# Interactive REPL
red connect localhost:6380

# One-shot query
red connect --query "SELECT * FROM users" localhost:6380
```

## Examples

```bash
# Start server, insert data, and query
red server --http --path ./data/reddb.rdb --bind 127.0.0.1:8080 &
red insert users '{"name": "Alice", "age": 30}' --bind 127.0.0.1:8080
red query "SELECT * FROM users" --bind 127.0.0.1:8080
red health --http --bind 127.0.0.1:8080
red tick --bind 127.0.0.1:8080 --operations maintenance,retention,checkpoint
```
