//! gRPC backend — placeholder.
//!
//! Wiring the tonic client requires bundling and compiling
//! `proto/reddb.proto`, plus matching the message/response shapes for
//! every method this client exposes. That is the next sub-task in
//! PLAN_DRIVERS.md (Phase 3.5). Until then we expose the same surface
//! so the public API does not flip-flop, and every call returns a
//! crisp `FEATURE_DISABLED`-style error.

use crate::error::{ClientError, ErrorCode, Result};
use crate::types::{InsertResult, JsonValue, QueryResult};

#[derive(Debug)]
pub struct GrpcClient {
    endpoint: String,
}

impl GrpcClient {
    pub async fn connect(endpoint: String) -> Result<Self> {
        Ok(Self { endpoint })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub async fn query(&self, _sql: &str) -> Result<QueryResult> {
        Err(unimplemented_err("query"))
    }

    pub async fn insert(&self, _collection: &str, _payload: &JsonValue) -> Result<InsertResult> {
        Err(unimplemented_err("insert"))
    }

    pub async fn bulk_insert(&self, _collection: &str, _payloads: &[JsonValue]) -> Result<u64> {
        Err(unimplemented_err("bulk_insert"))
    }

    pub async fn delete(&self, _collection: &str, _id: &str) -> Result<u64> {
        Err(unimplemented_err("delete"))
    }

    pub async fn close(&self) -> Result<()> {
        Ok(())
    }
}

fn unimplemented_err(method: &str) -> ClientError {
    ClientError::new(
        ErrorCode::FeatureDisabled,
        format!(
            "grpc::{method} is not implemented yet. Tracking: PLAN_DRIVERS.md Phase 3.5. \
             For now, use file:// or memory://."
        ),
    )
}
