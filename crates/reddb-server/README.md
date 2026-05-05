# reddb-server

The server-side guts of RedDB: engine, storage, runtime,
replication, MCP, AI, and every protocol dispatcher (gRPC, HTTP,
RedWire, PG-wire). Re-exported by the umbrella `reddb` crate so
existing `use reddb::…` paths keep working after the workspace
split.

## Audience

Pick `reddb-server` when you need to **embed the engine** in a
Rust process — for example, a custom server, an integration test
that drives the engine in-memory, or a tool that performs
offline maintenance on a `.rdb` file.

If you're writing application code that just talks to a running
server, you want the published [`reddb-client`][drivers-rust]
driver instead. The thin `red_client` binary lives in
[`reddb-client-internal`](../reddb-client) (workspace).

## What's inside

Top-level modules (full list in `src/lib.rs`):

- `engine`, `storage`, `runtime` — the engine core
- `replication` — primary/replica plumbing
- `auth` — server-side auth (SCRAM, OAuth/JWT, mTLS, sessions)
- `mcp`, `ai` — Model Context Protocol bridge + AI features
- `server` — HTTP/REST handlers, OpenAPI surface
- `grpc` — gRPC service implementation
- `wire`  — RedWire dispatcher (frame layout/codec are owned by
  [`reddb-wire`](../reddb-wire) and re-exported here)
- `service_cli`, `service_router` — bind logic + per-port routing
- `rpc_stdio` — JSON-RPC stdio mode used by spawning drivers

## Features

- `default = []`
- `backend-s3`, `backend-turso`, `backend-d1` — alternative
  storage backends
- `otel` — OpenTelemetry tracing scaffolding (opt-in to keep the
  default dep tree small)

## References

- [Connection strings][conn-strings]
- [ADR 0001 — RedWire][adr-0001]
- [Disk format v1](../../docs/adr/0003-disk-format-v1.md)
- [Workspace migration guide](../../docs/migration/workspace-split.md)

[adr-0001]: ../../docs/adr/0001-redwire-tcp-protocol.md
[conn-strings]: ../../docs/clients/connection-strings.md
[drivers-rust]: ../../crates/reddb-client
