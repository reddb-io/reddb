# CLI Reference

The `red` CLI is the unified binary for running and interacting with RedDB.

## Usage

```bash
red <command> [args] [flags]
```

## Commands

| Command | Description |
|:--------|:------------|
| `server` | Start the database server (router, HTTP, gRPC, or wire) |
| `query` | Query command (currently placeholder; execution not wired) |
| `insert` | Insert command (currently placeholder; execution not wired) |
| `get` | Get command (currently placeholder; execution not wired) |
| `delete` | Delete command (currently placeholder; execution not wired) |
| `health` | Run a health check against a server |
| `doctor` | Probe `/metrics` + `/admin/status` against operator-tunable thresholds |
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

By default, `red server` without explicit transport flags starts the routed front-door on `127.0.0.1:5050`, which accepts HTTP, gRPC, and wire traffic on one address.

```bash
red server [--grpc] [--http] [--grpc-bind 127.0.0.1:50051] [--http-bind 127.0.0.1:8080] [--wire-bind 127.0.0.1:5051] [--path ./data/reddb.rdb]
```

| Flag | Short | Default | Description |
|:-----|:------|:--------|:------------|
| `--path` | `-d` | `./data/reddb.rdb` | Database file path (omit for in-memory) |
| `--bind` | `-b` | router `127.0.0.1:5050` | Routed front-door by default; also works as the legacy single-transport bind address when a transport is selected |
| `--grpc` | | | Enable gRPC API |
| `--http` | | | Enable HTTP API |
| `--grpc-bind` | | | Explicit gRPC bind address |
| `--http-bind` | | | Explicit HTTP bind address |
| `--wire-bind` | | | Explicit wire TCP bind address |
| `--wire-tls-bind` | | | Explicit wire TLS bind address |
| `--wire-tls-cert` | | | TLS certificate PEM for wire TLS |
| `--wire-tls-key` | | | TLS private key PEM for wire TLS |
| `--role` | `-r` | `standalone` | Replication role: `standalone`, `primary`, `replica` |
| `--primary-addr` | | | Primary gRPC address (for replica mode) |
| `--read-only` | | | Open in read-only mode |
| `--no-create-if-missing` | | | Fail if database doesn't exist |
| `--vault` | | `false` | Enable encrypted auth vault |

Examples:

```bash
# Default routed front-door for gRPC, HTTP and wire
red server --path ./data/reddb.rdb

# Local dev with both APIs
red server --path ./data/reddb.rdb --grpc-bind 127.0.0.1:50051 --http-bind 127.0.0.1:8080

# Wire-only server
red server --path ./data/reddb.rdb --wire-bind 127.0.0.1:5051

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
| `--bind` | `-b` | router `127.0.0.1:5050` | Routed front-door by default; also works as the legacy single-transport bind address when a transport is selected |
| `--grpc` | | | Enable gRPC API in the service |
| `--http` | | | Enable HTTP API in the service |
| `--grpc-bind` | | | Explicit gRPC bind address |
| `--http-bind` | | | Explicit HTTP bind address |

Examples:

```bash
sudo red service install \
  --binary /usr/local/bin/red \
  --path /var/lib/reddb/data.rdb \
  --bind 0.0.0.0:5050

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
| `--bind` | `-b` | Server address. Defaults to `127.0.0.1:5050` when no transport is selected |
| `--grpc` | | Probe gRPC listener |
| `--http` | | Probe HTTP listener |

## red doctor

Probe the running server's `/metrics` and `/admin/status` against
operator-tunable thresholds. Designed for CI gates, on-call runbooks,
and Kubernetes liveness wrappers.

```bash
red doctor --bind <host>:<port> [--token <admin-token>] [--json] \
  [--backup-age-warn-secs 600] [--backup-age-crit-secs 3600] \
  [--wal-lag-warn 1000] [--wal-lag-crit 10000] \
  [--replica-lag-warn 1000] [--replica-lag-crit 100000]
```

| Flag | Default | Description |
|:-----|:--------|:------------|
| `--bind` | `127.0.0.1:8080` | Server address (HTTP). |
| `--token` | `$RED_ADMIN_TOKEN` | Admin bearer token. |
| `--json` | off | Emit a structured report instead of human-readable text. |
| `--backup-age-warn-secs` | `600` | Warn when `reddb_backup_age_seconds` exceeds this. |
| `--backup-age-crit-secs` | `3600` | Critical when `reddb_backup_age_seconds` exceeds this. |
| `--wal-lag-warn` | `1000` | Warn on `reddb_wal_archive_lag_records`. |
| `--wal-lag-crit` | `10000` | Critical on `reddb_wal_archive_lag_records`. |
| `--replica-lag-warn` | `1000` | Warn on max `reddb_replica_lag_records`. |
| `--replica-lag-crit` | `100000` | Critical on max `reddb_replica_lag_records`. |

Exit codes are stable for automation:

| Code | Meaning |
|------|---------|
| `0` | Healthy. No checks fired. |
| `1` | At least one warn (backup older than warn threshold, read-only flag set, etc.). |
| `2` | At least one critical (lease lost, divergence, server unreachable, sha256 mismatch). |

Examples:

```bash
# Quick local probe
red doctor --bind 127.0.0.1:8080

# Kubernetes liveness probe
red doctor --bind 127.0.0.1:8080 --json | jq -e '.status == "ok"'

# Tighter backup SLA
red doctor --bind 127.0.0.1:8080 \
  --backup-age-warn-secs 120 --backup-age-crit-secs 600
```

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
red replica --primary-addr http://primary:50051 [--grpc-bind 127.0.0.1:50051] [--http-bind 127.0.0.1:8080] [--wire-bind 127.0.0.1:5051] [--path ./data/replica.rdb]
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
| `--wire-bind` | | | Explicit wire TCP bind address |
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
