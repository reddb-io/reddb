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
| `reddb-client-internal`     | `crates/reddb-client/`        | Thin gRPC connector + REPL, hosts the `red_client` binary |
| `reddb-grpc-proto`          | `crates/reddb-grpc-proto/`    | Generated tonic protobuf stubs (server + client)    |
| `reddb-wire`                | `crates/reddb-wire/`          | Connection-string parser + RedWire frames           |

## What moved where

| Old path                                         | New canonical path                                  |
|--------------------------------------------------|-----------------------------------------------------|
| `reddb::storage`, `reddb::engine`, `reddb::runtime`, `reddb::replication`, `reddb::server`, `reddb::auth`, `reddb::mcp`, `reddb::ai`, `reddb::api`, `reddb::application`, `reddb::grpc`, `reddb::health`, `reddb::index`, `reddb::physical`, `reddb::regress`, `reddb::serde_json`, `reddb::sqlstate`, `reddb::telemetry`, `reddb::utils`, `reddb::wire`, `reddb::cli`, `reddb::service_cli` | `reddb-server` (re-exported by `reddb::*`)           |
| `reddb::client::RedDBClient`, `reddb::client::repl` | `reddb-client-internal` (re-exported as `reddb::client`) |
| `reddb::grpc::proto::*` (the generated tonic types) | `reddb-grpc-proto` (re-exported as `reddb::grpc::proto`) |
| `reddb::wire::redwire::Frame`, `MessageKind`, `Flags`, `encode_frame`, `decode_frame`, `REDWIRE_MAGIC`, `MAX_KNOWN_MINOR_VERSION`, `DEFAULT_REDWIRE_PORT` | `reddb-wire::redwire::*` (re-exported via `reddb::wire::redwire::*` and `reddb::wire_proto::redwire::*`) |
| `reddb::wire_proto`                              | `reddb-wire` (new alias added during the split)     |

## Picking the right crate to depend on

- **Embed the engine in your own Rust process** → depend on
  `reddb-server` directly (or keep depending on `reddb` and pay
  for the bin path).
- **Talk to a running server from Rust** → depend on the
  published [`reddb-client`][drivers-rust] driver under
  `drivers/rust`. **Not** `reddb-client-internal` — that one is
  `publish = false` and exists only for the workspace binaries.
- **Parse connection strings or build alternative tooling on the
  RedWire protocol** → depend on `reddb-wire`.
- **Generate gRPC stubs in another language** → use the `.proto`
  source under `crates/reddb-grpc-proto/proto/`. The Rust stubs
  are in the `reddb-grpc-proto` crate.

## Notes on the umbrella

The `reddb` crate continues to publish to crates.io as the
single user-facing artifact. The workspace members are
`publish = true` for `reddb-wire`, `reddb-grpc-proto`, and
`reddb-server`; `reddb-client-internal` is `publish = false` to
avoid the crates.io name collision with the standalone driver.

## See also

- PRD [#54][prd]
- [Connection strings](../clients/connection-strings.md)
- [ADR 0001 — RedWire](../adr/0001-redwire-tcp-protocol.md)

[prd]: https://github.com/reddb-io/reddb/issues/54
[drivers-rust]: ../../drivers/rust
