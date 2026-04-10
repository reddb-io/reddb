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
| `query` | Query command (currently placeholder; execution not wired) |
| `insert` | Insert command (currently placeholder; execution not wired) |
| `get` | Get command (currently placeholder; execution not wired) |
| `delete` | Delete command (currently placeholder; execution not wired) |
| `health` | Run a health check against a server |
| `replica` | Start as a read replica connected to a primary |
| `status` | Replication status command (currently placeholder) |
| `tick` | Run maintenance/reclaim tick operations |
| `service` | Install or inspect a systemd service |
| `mcp` | Start MCP server for AI agent integration |
| `auth` | Authentication commands (bootstrap implemented) |
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
red server [--grpc] [--http] [--grpc-bind 127.0.0.1:50051] [--http-bind 127.0.0.1:8080] [--path ./data/reddb.rdb]
```

| Flag | Short | Default | Description |
|:-----|:------|:--------|:------------|
| `--path` | `-d` | `./data/reddb.rdb` | Database file path (omit for in-memory) |
| `--bind` | `-b` | gRPC `127.0.0.1:50051` | Legacy single-transport bind address |
| `--grpc` | | | Enable gRPC API |
| `--http` | | | Enable HTTP API |
| `--grpc-bind` | | | Explicit gRPC bind address |
| `--http-bind` | | | Explicit HTTP bind address |
| `--role` | `-r` | `standalone` | Replication role: `standalone`, `primary`, `replica` |
| `--primary-addr` | | | Primary gRPC address (for replica mode) |
| `--read-only` | | | Open in read-only mode |
| `--no-create-if-missing` | | | Fail if database doesn't exist |
| `--vault` | | `false` | Enable encrypted auth vault |

Examples:

```bash
# Local dev with both APIs
red server --path ./data/reddb.rdb --grpc-bind 127.0.0.1:50051 --http-bind 127.0.0.1:8080

# HTTP-only server
red server --http --bind 0.0.0.0:8080

# Primary mode with vault
red server --path ./data/primary.rdb --role primary --vault --grpc-bind 0.0.0.0:50051 --http-bind 0.0.0.0:8080
```

## red service

Install or inspect a systemd unit.

```bash
red service <install|print-unit> [--binary /usr/local/bin/red] [--grpc-bind 0.0.0.0:50051] [--http-bind 0.0.0.0:8080] [--path /var/lib/reddb/data.rdb]
```

| Flag | Short | Default | Description |
|:-----|:------|:--------|:------------|
| `--binary` | | `/usr/local/bin/red` | Path to the `red` binary |
| `--service-name` | | `reddb` | systemd unit name |
| `--user` | | `reddb` | Service user |
| `--group` | | `reddb` | Service group |
| `--path` | `-d` | `/var/lib/reddb/data.rdb` | Persistent database file path |
| `--bind` | `-b` | gRPC `127.0.0.1:50051` | Legacy single-transport bind address |
| `--grpc` | | | Enable gRPC API in the service |
| `--http` | | | Enable HTTP API in the service |
| `--grpc-bind` | | | Explicit gRPC bind address |
| `--http-bind` | | | Explicit HTTP bind address |

Examples:

```bash
sudo red service install \
  --binary /usr/local/bin/red \
  --path /var/lib/reddb/data.rdb \
  --grpc-bind 0.0.0.0:50051 \
  --http-bind 0.0.0.0:8080

red service print-unit \
  --path /var/lib/reddb/data.rdb \
  --grpc-bind 0.0.0.0:50051 \
  --http-bind 0.0.0.0:8080
```

## red query

Query command scaffold.

> [!WARNING]
> `red query` is not wired to runtime execution yet. For actual query execution, use `red connect --query "<sql>" <grpc-addr>` or `POST /query` over HTTP.

```bash
red query "SELECT * FROM users WHERE age > 21"
```

| Flag | Short | Default | Description |
|:-----|:------|:--------|:------------|
| `--bind` | `-b` | `0.0.0.0:6380` | Reserved for future runtime wiring |

Examples:

```bash
red connect --query "SELECT * FROM users" 127.0.0.1:50051
curl -X POST http://127.0.0.1:8080/query -H 'content-type: application/json' -d '{"query":"FROM ANY LIMIT 10"}'
```

## red insert

Insert command scaffold.

> [!WARNING]
> `red insert` is not wired yet. For inserts today, use HTTP `POST /collections/{name}/rows` or the gRPC `CreateRow` RPC.

```bash
red insert <collection> '<json>'
```

| Flag | Short | Default | Description |
|:-----|:------|:--------|:------------|
| `--bind` | `-b` | `0.0.0.0:6380` | Reserved for future runtime wiring |

Example:

```bash
curl -X POST http://127.0.0.1:8080/collections/users/rows \
  -H 'content-type: application/json' \
  -d '{"fields":{"name":"Alice","age":30}}'
```

## red get

Get command scaffold (`not yet wired`).

```bash
red get <collection> <id>
```

## red delete

Delete command scaffold (`not yet wired`).

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
red replica --primary-addr http://primary:50051 [--grpc-bind 127.0.0.1:50051] [--http-bind 127.0.0.1:8080] [--path ./data/replica.rdb]
```

| Flag | Short | Default | Description |
|:-----|:------|:--------|:------------|
| `--primary-addr` | `-p` | | Primary gRPC address |
| `--path` | `-d` | `./data/reddb.rdb` | Local replica path |
| `--bind` | `-b` | gRPC `127.0.0.1:50051` | Legacy single-transport bind address |
| `--grpc` | | | Enable gRPC |
| `--http` | | | Enable HTTP |
| `--grpc-bind` | | | Explicit gRPC bind address |
| `--http-bind` | | | Explicit HTTP bind address |
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

Implemented today:

```bash
red auth bootstrap --password s3cret!
```

`create-user`, `list-users`, and `login` are listed in help output but are not wired in the current CLI command dispatcher.

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
red connect 127.0.0.1:50051

# One-shot query
red connect --query "SELECT * FROM users" 127.0.0.1:50051
```

## Examples

```bash
# Start server, insert data, and query
red server --path ./data/reddb.rdb --grpc-bind 127.0.0.1:50051 --http-bind 127.0.0.1:8080 &
curl -X POST http://127.0.0.1:8080/collections/users/rows -H 'content-type: application/json' -d '{"fields":{"name":"Alice","age":30}}'
red connect --query "SELECT * FROM users" 127.0.0.1:50051
red health --http --bind 127.0.0.1:8080
red tick --bind 127.0.0.1:8080 --operations maintenance,retention,checkpoint
```
