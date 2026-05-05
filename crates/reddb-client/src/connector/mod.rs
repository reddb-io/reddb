//! Internal connector + REPL used by the `red` and `red_client`
//! binaries (and by `reddb-server`'s rpc_stdio mode).
//!
//! This module is deliberately kept dependency-light. The
//! [`RedDBClient`] gRPC connector itself lives in the sibling
//! [`reddb-client-connector`] crate so that `reddb-server` can
//! depend on it without forming a circular path dependency
//! through `reddb-client[embedded]` → `reddb` → `reddb-server`.
//! The [`http`], [`redwire`], and [`repl`] helpers below are
//! consumed by the `red_client` bin only and stay here because
//! nothing else in the workspace pulls them in.
//!
//! [`reddb-client-connector`]: ../../../reddb_client_connector/index.html

pub mod http;
pub mod redwire;
pub mod repl;

// Re-export the connector types from the dedicated crate so the
// existing `reddb_client::{RedDBClient, repl, …}` import paths
// keep resolving.
pub use reddb_client_connector::{
    BulkCreateStatus, CreatedEntity, HealthStatus, OperationStatus, QueryResponse, RedDBClient,
};
