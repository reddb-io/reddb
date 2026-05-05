# Workspace split — migration guide

PRD [#54][prd] split the single `reddb` crate into a Cargo
workspace so a thin `red_client` binary can ship without linking
the engine. This guide explains what moved, what stayed, and how
to update downstream code.

## TL;DR — nothing breaks

The umbrella `reddb` crate keeps every `pub` path it had before
the split. If your code does `use reddb::storage::…`,
`use reddb::runtime::…`, `use reddb::wire::redwire::…`, or
`use reddb::client::RedDBClient`, no change is required.

The split is structural: code physically lives in workspace
member crates, but the umbrella re-exports them.

## Crate layout

| Crate                       | Path                          | Role                                                |
|-----------------------------|-------------------------------|-----------------------------------------------------|
| `reddb`                     | repo root                     | Umbrella. Hosts the `red` binary, re-exports the rest |
| `reddb-server`              | `crates/reddb-server/`        | Engine, storage, runtime, replication, MCP, AI, server dispatch |
| `reddb-client`              | `crates/reddb-client/`        | Published Rust driver (embedded / gRPC / HTTP / RedWire), hosts the `red_client` binary, plus the workspace-internal `connector` module used by `red`'s REPL and `reddb-server`'s rpc_stdio |
| `reddb-client-connector`    | `crates/reddb-client-connector/` | Tiny tonic-only gRPC connector. Exists solely to break the `reddb-client[embedded] → reddb-server → reddb-client` path-dependency cycle. Re-exported as `reddb_client::connector::RedDBClient` for back-compat |
| `reddb-grpc-proto`          | `crates/reddb-grpc-proto/`    | Generated tonic protobuf stubs (server + client)    |
| `reddb-wire`                | `crates/reddb-wire/`          | Connection-string parser + RedWire frames           |

## What moved where

| Old path                                         | New canonical path                                  |
|--------------------------------------------------|-----------------------------------------------------|
| `reddb::storage`, `reddb::engine`, `reddb::runtime`, `reddb::replication`, `reddb::server`, `reddb::auth`, `reddb::mcp`, `reddb::ai`, `reddb::api`, `reddb::application`, `reddb::grpc`, `reddb::health`, `reddb::index`, `reddb::physical`, `reddb::regress`, `reddb::serde_json`, `reddb::sqlstate`, `reddb::telemetry`, `reddb::utils`, `reddb::wire`, `reddb::cli`, `reddb::service_cli` | `reddb-server` (re-exported by `reddb::*`)           |
| `reddb::client::RedDBClient`, `reddb::client::repl` | `reddb-client` (re-exported as `reddb::client`) |
| `reddb::grpc::proto::*` (the generated tonic types) | `reddb-grpc-proto` (re-exported as `reddb::grpc::proto`) |
| `reddb::wire::redwire::Frame`, `MessageKind`, `Flags`, `encode_frame`, `decode_frame`, `REDWIRE_MAGIC`, `MAX_KNOWN_MINOR_VERSION`, `DEFAULT_REDWIRE_PORT` | `reddb-wire::redwire::*` (re-exported via `reddb::wire::redwire::*` and `reddb::wire_proto::redwire::*`) |
| `reddb::wire_proto`                              | `reddb-wire` (new alias added during the split)     |

## Picking the right crate to depend on

- **Embed the engine in your own Rust process** → depend on
  `reddb-server` directly (or keep depending on `reddb` and pay
  for the bin path).
- **Talk to a running server from Rust** → depend on the
  published [`reddb-client`][drivers-rust] driver. As of the
  driver consolidation slice (issue #67) the crate lives at
  `crates/reddb-client/` instead of the previous `drivers/rust/`
  location, but the published name on crates.io is unchanged.
- **Parse connection strings or build alternative tooling on the
  RedWire protocol** → depend on `reddb-wire`.
- **Generate gRPC stubs in another language** → use the `.proto`
  source under `crates/reddb-grpc-proto/proto/`. The Rust stubs
  are in the `reddb-grpc-proto` crate.

## Notes on the umbrella

The `reddb` umbrella crate continues to publish to crates.io as
the engine artifact. As of issue #67 the published `reddb-client`
driver lives at `crates/reddb-client/` (no longer at
`drivers/rust/`); both `reddb-client` and the helper
`reddb-client-connector` ship as workspace members on crates.io
in lock-step with the engine version.

## Driver consolidation (issue #67)

The previously-separate `drivers/rust/` crate has been merged
into the workspace member at `crates/reddb-client/`. Most
downstream code keeps compiling — the published API
(`reddb_client::Reddb`, `reddb_client::JsonValue`,
`reddb_client::ClientError`, `reddb_client::connect::Target`,
the `embedded`/`grpc`/`http`/`redwire`/`redwire-tls` Cargo
features) is unchanged. A handful of intentional changes:

| Change                                  | Before                              | After                                          |
|-----------------------------------------|-------------------------------------|------------------------------------------------|
| Crate location                          | `drivers/rust/Cargo.toml`           | `crates/reddb-client/Cargo.toml`               |
| License                                 | MIT                                 | AGPL-3.0-only (matches the rest of the workspace) |
| `connect::parse` parser                 | local copy                          | thin shim over [`reddb-wire::parse`][rwp]      |
| `Target` variants                       | `Memory`, `File`, `Grpc`, `GrpcCluster`, `Http` | unchanged (the wire crate's `RedWire` variant is folded onto `Target::Grpc` for back-compat) |
| `embedded` feature engine dep           | `reddb` (umbrella)                  | `reddb-server` (workspace leaf, breaks a cycle) |
| `grpc.rs` JSON parsing                  | `reddb::json::Value`                | `serde_json::Value` (drops one engine coupling) |

[rwp]: ../../crates/reddb-wire/src/conn_string.rs

If your code reaches into the engine via `reddb::*` paths from
inside what used to be `drivers/rust`, switch those imports to
`reddb_server::*` (or keep using the `reddb` umbrella, which
re-exports the same paths).

## See also

- PRD [#54][prd]
- [Connection strings](../clients/connection-strings.md)
- [ADR 0001 — RedWire](../adr/0001-redwire-tcp-protocol.md)

[prd]: https://github.com/reddb-io/reddb/issues/54
[drivers-rust]: ../../crates/reddb-client
