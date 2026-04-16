//! gRPC backend — wraps `reddb::client::RedDBClient` under the `grpc`
//! Cargo feature.
//!
//! Design note: today both `embedded` and `grpc` pull the entire engine
//! crate as a dep, because `reddb::client::RedDBClient` lives inside
//! the engine. A truly thin client (proto + tonic only, no engine
//! code) is tracked in PLAN_DRIVERS.md and will live in this module
//! without breaking the public API.
//!
//! All methods are genuinely async — they `.await` directly on tonic
//! futures. Callers must be in a tokio runtime (any runtime actually,
//! as long as tonic's transport stack is happy there). This crate
//! does not spin up its own runtime.

use tokio::sync::Mutex;

use reddb::client::RedDBClient;
use reddb::json::Value as JsonWireValue;

use crate::error::{ClientError, ErrorCode, Result};
use crate::types::{InsertResult, JsonValue, QueryResult, ValueOut};

/// Async handle to a remote RedDB server over gRPC.
pub struct GrpcClient {
    endpoint: String,
    inner: Mutex<RedDBClient>,
}

impl std::fmt::Debug for GrpcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrpcClient")
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

impl GrpcClient {
    pub async fn connect(endpoint: String) -> Result<Self> {
        let inner = RedDBClient::connect(&endpoint, None).await.map_err(|e| {
            ClientError::new(ErrorCode::IoError, format!("connect {endpoint}: {e}"))
        })?;
        Ok(Self {
            endpoint,
            inner: Mutex::new(inner),
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub async fn query(&self, sql: &str) -> Result<QueryResult> {
        let reply = {
            let mut guard = self.inner.lock().await;
            guard
                .query_reply(sql)
                .await
                .map_err(|e| ClientError::new(ErrorCode::QueryError, e.to_string()))?
        };
        parse_query_json(&reply.result_json)
    }

    pub async fn insert(&self, collection: &str, payload: &JsonValue) -> Result<InsertResult> {
        if payload.as_object().is_none() {
            return Err(ClientError::new(
                ErrorCode::QueryError,
                "insert payload must be a JSON object".to_string(),
            ));
        }
        let json_payload = payload.to_json_string();
        let reply = {
            let mut guard = self.inner.lock().await;
            guard
                .create_row_entity(collection, &json_payload)
                .await
                .map_err(|e| ClientError::new(ErrorCode::QueryError, e.to_string()))?
        };
        Ok(InsertResult {
            affected: 1,
            id: Some(reply.id.to_string()),
        })
    }

    pub async fn bulk_insert(&self, collection: &str, payloads: &[JsonValue]) -> Result<u64> {
        let mut encoded = Vec::with_capacity(payloads.len());
        for payload in payloads {
            if payload.as_object().is_none() {
                return Err(ClientError::new(
                    ErrorCode::QueryError,
                    "bulk_insert payloads must be JSON objects".to_string(),
                ));
            }
            encoded.push(payload.to_json_string());
        }
        let reply = {
            let mut guard = self.inner.lock().await;
            guard
                .bulk_create_rows(collection, encoded)
                .await
                .map_err(|e| ClientError::new(ErrorCode::QueryError, e.to_string()))?
        };
        Ok(reply.count)
    }

    pub async fn delete(&self, collection: &str, id: &str) -> Result<u64> {
        let id = id.parse::<u64>().map_err(|_| {
            ClientError::new(
                ErrorCode::InvalidUri,
                "id must be a numeric string".to_string(),
            )
        })?;
        {
            let mut guard = self.inner.lock().await;
            guard
                .delete_entity(collection, id)
                .await
                .map_err(|e| ClientError::new(ErrorCode::QueryError, e.to_string()))?
        };
        Ok(1)
    }

    pub async fn close(&self) -> Result<()> {
        // The tonic channel closes when `inner` drops.
        Ok(())
    }
}

fn parse_query_json(s: &str) -> Result<QueryResult> {
    let parsed = reddb::json::from_str::<JsonWireValue>(s)
        .map_err(|e| ClientError::new(ErrorCode::QueryError, format!("bad server JSON: {e}")))?;
    let statement = parsed
        .get("statement")
        .and_then(JsonWireValue::as_str)
        .unwrap_or("select")
        .to_string();
    let affected = parsed
        .get("affected")
        .and_then(JsonWireValue::as_f64)
        .unwrap_or(0.0) as u64;
    let columns = parsed
        .get("columns")
        .and_then(JsonWireValue::as_array)
        .map(|cols| {
            cols.iter()
                .filter_map(|col| col.as_str().map(ToString::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let rows = parsed
        .get("rows")
        .or_else(|| parsed.get("records"))
        .and_then(JsonWireValue::as_array)
        .map(|rows| rows.iter().map(parse_row_value).collect())
        .unwrap_or_default();
    Ok(QueryResult {
        statement,
        affected,
        columns,
        rows,
    })
}

fn parse_row_value(value: &JsonWireValue) -> Vec<(String, ValueOut)> {
    value
        .as_object()
        .map(|row| {
            row.iter()
                .map(|(key, value)| (key.clone(), parse_scalar(value)))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_scalar(value: &JsonWireValue) -> ValueOut {
    match value {
        JsonWireValue::Null => ValueOut::Null,
        JsonWireValue::Bool(b) => ValueOut::Bool(*b),
        JsonWireValue::Number(n) => {
            if n.fract() == 0.0 {
                ValueOut::Integer(*n as i64)
            } else {
                ValueOut::Float(*n)
            }
        }
        JsonWireValue::String(s) => ValueOut::String(s.clone()),
        other => ValueOut::String(other.to_string_compact()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_query_json_extracts_rows_and_columns() {
        let input = r#"{"statement":"select","affected":0,"columns":["id","name"],"rows":[{"id":1,"name":"Alice"},{"id":2,"name":"Bob"}]}"#;
        let qr = parse_query_json(input).unwrap();
        assert_eq!(qr.statement, "select");
        assert_eq!(qr.affected, 0);
        assert_eq!(qr.columns, vec!["id".to_string(), "name".to_string()]);
        assert_eq!(qr.rows.len(), 2);
        assert_eq!(qr.rows[0][0].0, "id");
        assert!(matches!(qr.rows[0][0].1, ValueOut::Integer(1)));
        assert_eq!(qr.rows[1][1].0, "name");
        assert!(matches!(&qr.rows[1][1].1, ValueOut::String(s) if s == "Bob"));
    }

    #[test]
    fn parse_query_json_handles_empty_rows() {
        let input = r#"{"statement":"select","affected":0,"columns":[],"rows":[]}"#;
        let qr = parse_query_json(input).unwrap();
        assert!(qr.rows.is_empty());
        assert!(qr.columns.is_empty());
    }

    #[test]
    fn parse_query_json_tolerates_missing_fields() {
        // If server omits fields we fall back to empty defaults.
        let qr = parse_query_json("{}").unwrap();
        assert_eq!(qr.affected, 0);
        assert!(qr.rows.is_empty());
    }
}
