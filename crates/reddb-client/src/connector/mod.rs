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

/// Encode public client bind values for the dependency-light gRPC connector.
pub fn params_to_grpc_values(params: &[crate::Value]) -> Vec<reddb_grpc_proto::QueryValue> {
    use reddb_grpc_proto::query_value::Kind;
    use reddb_grpc_proto::{QueryNull, QueryValue, QueryVector};

    params
        .iter()
        .cloned()
        .map(|value| {
            let kind = match value {
                crate::Value::Null => Kind::NullValue(QueryNull {}),
                crate::Value::Bool(value) => Kind::BoolValue(value),
                crate::Value::Int(value) => Kind::IntValue(value),
                crate::Value::Float(value) => Kind::FloatValue(value),
                crate::Value::Text(value) => Kind::TextValue(value),
                crate::Value::Bytes(value) => Kind::BytesValue(value),
                crate::Value::Vector(values) => Kind::VectorValue(QueryVector { values }),
                crate::Value::Json(value) => Kind::JsonValue(value.to_json_string()),
                crate::Value::Timestamp(value) => Kind::TimestampValue(value),
                crate::Value::Uuid(value) => Kind::UuidValue(value.to_vec()),
            };
            QueryValue { kind: Some(kind) }
        })
        .collect()
}
