# Workspace split — migration guide

PRD [#54][prd] split the single `reddb` crate into a Cargo
workspace so a thin `red_client` binary can ship without linking
the engine. This guide explains what moved, what stayed, and how
to update downstream code.

> [!IMPORTANT]
> **Crates.io rename (1.0).** Every published crate now lives under
> the `reddb-io-*` namespace on crates.io (previously the `reddb-*`
> family). Update your `Cargo.toml` `[dependencies]` keys
> accordingly. The Rust **library names** are unchanged
> (`use reddb::…`, `use reddb_client::…`, `use reddb_wire::…`,
> `use reddb_server::…`, `use reddb_grpc_proto::…`), so call sites
> keep compiling once the dependency key is updated.
>
> | Old crates.io name        | New crates.io name           |
> |---------------------------|------------------------------|
> | `reddb`                   | `reddb-io`                   |
> | `reddb-server`            | `reddb-io-server`            |
> | `reddb-client`            | `reddb-io-client`            |
> | `reddb-client-connector`  | `reddb-io-client-connector`  |
> | `reddb-grpc-proto`        | `reddb-io-grpc-proto`        |
> | `reddb-wire`              | `reddb-io-wire`              |
> | `reddb-file`              | `reddb-io-file`              |
> | `reddb-types`             | `reddb-io-types`             |
> | `reddb-rql`               | `reddb-io-rql`               |
> | `reddb-crypto`            | `reddb-io-crypto`            |

## TL;DR — nothing breaks

The umbrella `reddb-io` crate (lib name `reddb`) keeps every `pub`
path it had before the split. If your code does
`use reddb::storage::…`, `use reddb::runtime::…`,
`use reddb::wire::redwire::…`, or `use reddb::client::RedDBClient`,
no change is required.

The split is structural: code physically lives in workspace
member crates, but the umbrella re-exports them.

## Crate layout

| Crates.io name              | Lib import path        | Workspace path                | Role                                                |
|-----------------------------|------------------------|-------------------------------|-----------------------------------------------------|
| `reddb-io`                  | `reddb`                | repo root                     | Umbrella. Hosts the `red` binary, re-exports the rest |
| `reddb-io-server`           | `reddb_server`         | `crates/reddb-server/`        | Engine, storage, runtime, replication, MCP, AI, server dispatch |
| `reddb-io-client`           | `reddb_client`         | `crates/reddb-client/`        | Published Rust driver (embedded / gRPC / HTTP / RedWire), hosts the `red_client` binary, plus the workspace-internal `connector` module used by `red`'s REPL and the server's rpc_stdio |
| `reddb-io-client-connector` | `reddb_client_connector` | `crates/reddb-client-connector/` | Tiny tonic-only gRPC connector. Exists solely to break the `reddb-io-client[embedded] → reddb-io-server → reddb-io-client` path-dependency cycle. Re-exported as `reddb_client::connector::RedDBClient` for back-compat |
| `reddb-io-grpc-proto`       | `reddb_grpc_proto`     | `crates/reddb-grpc-proto/`    | Generated tonic protobuf stubs (server + client)    |
| `reddb-io-wire`             | `reddb_wire`           | `crates/reddb-wire/`          | Connection-string parser + RedWire frames           |
| `reddb-io-file`             | `reddb_file`           | `crates/reddb-file/`          | File artifact contracts: paths, manifests, WAL envelopes, snapshots, checkpoints, and recovery metadata |
| `reddb-io-types`            | `reddb_types`          | `crates/reddb-types/`         | Neutral logical type vocabulary shared by authority crates |
| `reddb-io-rql`              | `reddb_rql`            | `crates/reddb-rql/`           | RQL front-end and conformance corpus authority |
| `reddb-io-crypto`           | `reddb_crypto`         | `crates/reddb-crypto/`        | Cryptographic envelope and key parsing authority |

## What moved where

| Old path                                         | New canonical home                                  |
|--------------------------------------------------|-----------------------------------------------------|
| `reddb::storage`, `reddb::engine`, `reddb::runtime`, `reddb::replication`, `reddb::server`, `reddb::auth`, `reddb::mcp`, `reddb::ai`, `reddb::api`, `reddb::application`, `reddb::grpc`, `reddb::health`, `reddb::index`, `reddb::physical`, `reddb::regress`, `reddb::serde_json`, `reddb::sqlstate`, `reddb::telemetry`, `reddb::utils`, `reddb::wire`, `reddb::cli`, `reddb::service_cli` | `reddb-io-server` package, `reddb_server` import, re-exported by `reddb::*` |
| `reddb::client::RedDBClient`, `reddb::client::repl` | `reddb-io-client` package, `reddb_client` import, re-exported as `reddb::client` |
| `reddb::grpc::proto::*` (the generated tonic types) | `reddb-io-grpc-proto` package, `reddb_grpc_proto` import, re-exported as `reddb::grpc::proto` |
| `reddb::wire::redwire::Frame`, `MessageKind`, `Flags`, `encode_frame`, `decode_frame`, `REDWIRE_MAGIC`, `MAX_KNOWN_MINOR_VERSION`, `DEFAULT_REDWIRE_PORT` | `reddb-io-wire` package, `reddb_wire::redwire::*` import, re-exported via `reddb::wire::redwire::*` and `reddb::wire_proto::redwire::*` |
| `reddb::wire_proto`                              | `reddb-io-wire` package, `reddb_wire` import, new alias added during the split |

## Picking the right crate to depend on

- **Embed the engine in your own Rust process** → depend on
  `reddb-io-server` directly (or keep depending on `reddb-io` and
  pay for the bin path).
- **Talk to a running server from Rust** → depend on the
  published [`reddb-io-client`][drivers-rust] driver. As of the
  driver consolidation slice (issue #67) the crate lives at
  `crates/reddb-client/` instead of the previous `drivers/rust/`
  location.
- **Parse connection strings or build alternative tooling on the
  RedWire protocol** → depend on `reddb-io-wire`.
- **Generate gRPC stubs in another language** → use the `.proto`
  source under `crates/reddb-grpc-proto/proto/`. The Rust stubs
  are in the `reddb-io-grpc-proto` crate.

## Notes on the umbrella

The `reddb-io` umbrella crate (formerly published as `reddb`)
continues to publish to crates.io as the engine artifact. As of
issue #67 the published `reddb-io-client` driver lives at
`crates/reddb-client/` (no longer at `drivers/rust/`); both
`reddb-io-client` and the helper `reddb-io-client-connector` ship
as workspace members on crates.io in lock-step with the engine
version.

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
| `connect::parse` parser                 | local copy                          | thin shim over [`reddb_wire::parse`][rwp]      |
| `Target` variants                       | `Memory`, `File`, `Grpc`, `GrpcCluster`, `Http` | unchanged (the wire crate's `RedWire` variant is folded onto `Target::Grpc` for back-compat) |
| `embedded` feature engine dep           | `reddb` (umbrella)                  | `reddb-io-server` (workspace leaf, breaks a cycle) |
| `grpc.rs` JSON parsing                  | `reddb::json::Value`                | `serde_json::Value` (drops one engine coupling) |

[rwp]: ../../crates/reddb-wire/src/conn_string.rs

If your code reaches into the engine via `reddb::*` paths from
inside what used to be `drivers/rust`, switch those imports to
`reddb_server::*` (or keep using the `reddb-io` umbrella — lib
name `reddb` — which re-exports the same paths).

## See also

- PRD [#54][prd]
- [Connection strings](../clients/connection-strings.md)
- [Monorepo structure](../dev/monorepo-structure.md)
- [ADR 0001 — RedWire](../../.red/adr/0001-redwire-tcp-protocol.md)

[prd]: https://github.com/reddb-io/reddb/issues/54
[drivers-rust]: ../../crates/reddb-client
