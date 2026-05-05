//! RedDB umbrella crate.
//!
//! This crate is the published face of RedDB: it hosts the `red`
//! binary and re-exports the workspace members so existing
//! `use reddb::…` import paths keep resolving after the workspace
//! split (PRD #54). The actual code lives in the four sibling
//! crates:
//!
//!   - `reddb-wire`           — connection-string parser + RedWire frames
//!   - `reddb-grpc-proto`     — generated tonic protobuf stubs
//!   - `reddb-server`         — engine, storage, runtime, replication, MCP, AI, server dispatch
//!   - `reddb-client`         — gRPC client + REPL used by the bins, plus the published high-level driver

pub use reddb_server::*;

/// Connection-string parser and RedWire frame vocabulary.
///
/// Re-exported under the legacy path so existing call sites can
/// reach the parser without depending on `reddb-wire` directly.
pub use reddb_wire as wire_proto;

/// gRPC client + REPL used by the `red` and `red_client` binaries.
/// Exposed under the legacy path so existing `reddb::client::…`
/// imports keep resolving.
pub use reddb_client as client;
