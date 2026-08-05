//! Deprecated compatibility shim for the authenticated gRPC client.
//!
//! New code should import [`reddb_grpc_proto::RedDBClient`] and the generated
//! reply types from `reddb-grpc-proto` directly.

#![deprecated(
    since = "1.23.2",
    note = "use reddb_grpc_proto::RedDBClient and generated reply types; this shim will be removed in the next major release"
)]

pub use reddb_grpc_proto::{
    BulkEntityReply as BulkCreateStatus, EntityReply as CreatedEntity,
    HealthReply as HealthStatus, OperationReply as OperationStatus, QueryReply as QueryResponse,
    RedDBClient,
};
