//! Internal connector + REPL used by the `red` and `red_client`
//! binaries (and by `reddb-server`'s rpc_stdio mode).
//!
//! This module is deliberately kept dependency-light. The
//! [`RedDBClient`] gRPC connector itself lives beside the generated
//! stubs in [`reddb-grpc-proto`] so both client and server consumers
//! share the wire reply types directly.
//! The [`http`], [`redwire`], and [`repl`] helpers below are
//! consumed by the `red_client` bin only and stay here because
//! nothing else in the workspace pulls them in.
//!
//! [`reddb-grpc-proto`]: ../../../reddb_grpc_proto/index.html

pub mod http;
pub mod redwire;
pub mod repl;

// Re-export the connector types from their canonical crate so the
// existing `reddb_client::{RedDBClient, repl, …}` import paths
// keep resolving.
pub use reddb_grpc_proto::{
    BulkEntityReply as BulkCreateStatus, EntityReply as CreatedEntity, HealthReply as HealthStatus,
    OperationReply as OperationStatus, QueryReply as QueryResponse, RedDBClient,
};
