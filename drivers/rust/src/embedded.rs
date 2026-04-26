//! Embedded backend — wraps the in-process RedDB engine.
//!
//! Compiled only when the `embedded` Cargo feature is enabled (default).
//! When `embedded` is off, this module does not exist and the
//! [`crate::Reddb`] enum will refuse to construct an embedded variant
//! at runtime with a clear `FEATURE_DISABLED` error.

use std::path::PathBuf;
use std::sync::Arc;

use reddb::api::RedDBOptions;
use reddb::runtime::RedDBRuntime;
use reddb::storage::query::unified::UnifiedRecord;
use reddb::storage::schema::Value as SchemaValue;

use crate::error::{ClientError, ErrorCode, Result};
use crate::types::{InsertResult, JsonValue, QueryResult, ValueOut};

/// In-process handle to a RedDB engine.
pub struct EmbeddedClient {
    runtime: Arc<RedDBRuntime>,
}

impl std::fmt::Debug for EmbeddedClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddedClient").finish_non_exhaustive()
    }
}

impl EmbeddedClient {
    /// Open a persistent database at `path`.
    pub fn open(path: PathBuf) -> Result<Self> {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(path))
            .map_err(|e| ClientError::new(ErrorCode::IoError, e.to_string()))?;
        Ok(Self {
            runtime: Arc::new(runtime),
        })
    }

    /// Open an ephemeral, tempfile-backed database. Equivalent to
    /// `connect("memory://")`.
    pub fn in_memory() -> Result<Self> {
        let runtime = RedDBRuntime::in_memory()
            .map_err(|e| ClientError::new(ErrorCode::IoError, e.to_string()))?;
        Ok(Self {
            runtime: Arc::new(runtime),
        })
    }

    pub fn query(&self, sql: &str) -> Result<QueryResult> {
        let qr = self
            .runtime
            .execute_query(sql)
            .map_err(|e| ClientError::new(ErrorCode::QueryError, e.to_string()))?;
        Ok(map_query_result(&qr))
    }

    pub fn insert(&self, collection: &str, payload: &JsonValue) -> Result<InsertResult> {
        let object = payload.as_object().ok_or_else(|| {
            ClientError::new(
                ErrorCode::QueryError,
                "insert payload must be a JSON object".to_string(),
            )
        })?;
        let sql = build_insert_sql(collection, object);
        let qr = self
            .runtime
            .execute_query(&sql)
            .map_err(|e| ClientError::new(ErrorCode::QueryError, e.to_string()))?;
        Ok(InsertResult {
            affected: qr.affected_rows,
            id: None,
        })
    }

    pub fn bulk_insert(&self, collection: &str, payloads: &[JsonValue]) -> Result<u64> {
        let mut total = 0u64;
        for payload in payloads {
            let object = payload.as_object().ok_or_else(|| {
                ClientError::new(
                    ErrorCode::QueryError,
                    "bulk_insert payloads must be JSON objects".to_string(),
                )
            })?;
            let sql = build_insert_sql(collection, object);
            let qr = self
                .runtime
                .execute_query(&sql)
                .map_err(|e| ClientError::new(ErrorCode::QueryError, e.to_string()))?;
            total += qr.affected_rows;
        }
        Ok(total)
    }

    pub fn delete(&self, collection: &str, id: &str) -> Result<u64> {
        let sql = format!("DELETE FROM {collection} WHERE _entity_id = {id}");
        let qr = self
            .runtime
            .execute_query(&sql)
            .map_err(|e| ClientError::new(ErrorCode::QueryError, e.to_string()))?;
        Ok(qr.affected_rows)
    }

    pub fn close(&self) -> Result<()> {
        self.runtime
            .checkpoint()
            .map_err(|e| ClientError::new(ErrorCode::IoError, e.to_string()))
    }

    pub fn version() -> &'static str {
        env!("CARGO_PKG_VERSION")
    }
}

fn build_insert_sql(collection: &str, object: &[(String, JsonValue)]) -> String {
    let mut cols = Vec::new();
    let mut vals = Vec::new();
    for (k, v) in object {
        cols.push(k.clone());
        vals.push(value_to_sql_literal(v));
    }
    format!(
        "INSERT INTO {collection} ({}) VALUES ({})",
        cols.join(", "),
        vals.join(", "),
    )
}

fn value_to_sql_literal(v: &JsonValue) -> String {
    match v {
        JsonValue::Null => "NULL".to_string(),
        JsonValue::Bool(b) => b.to_string(),
        JsonValue::Number(n) => {
            if n.fract() == 0.0 {
                format!("{}", *n as i64)
            } else {
                n.to_string()
            }
        }
        JsonValue::String(s) => format!("'{}'", s.replace('\'', "''")),
        JsonValue::Array(_) | JsonValue::Object(_) => {
            format!("'{}'", v.to_json_string().replace('\'', "''"))
        }
    }
}

fn map_query_result(qr: &reddb::runtime::RuntimeQueryResult) -> QueryResult {
    let columns: Vec<String> = qr
        .result
        .records
        .first()
        .map(|r| {
            let mut keys: Vec<String> = r.values.keys().map(|k| k.to_string()).collect();
            keys.sort();
            keys
        })
        .unwrap_or_default();

    let rows: Vec<Vec<(String, ValueOut)>> = qr
        .result
        .records
        .iter()
        .map(record_to_pairs)
        .collect();

    QueryResult {
        statement: qr.statement_type.to_string(),
        affected: qr.affected_rows,
        columns,
        rows,
    }
}

fn record_to_pairs(record: &UnifiedRecord) -> Vec<(String, ValueOut)> {
    let mut entries: Vec<(&str, &SchemaValue)> =
        record.values.iter().map(|(k, v)| (k.as_ref(), v)).collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    entries
        .into_iter()
        .map(|(k, v)| (k.to_string(), schema_value_to_value_out(v)))
        .collect()
}

fn schema_value_to_value_out(v: &SchemaValue) -> ValueOut {
    match v {
        SchemaValue::Null => ValueOut::Null,
        SchemaValue::Boolean(b) => ValueOut::Bool(*b),
        SchemaValue::Integer(n) => ValueOut::Integer(*n),
        SchemaValue::UnsignedInteger(n) => ValueOut::Integer(*n as i64),
        SchemaValue::Float(n) => ValueOut::Float(*n),
        SchemaValue::BigInt(n) => ValueOut::Integer(*n),
        SchemaValue::TimestampMs(n)
        | SchemaValue::Timestamp(n)
        | SchemaValue::Duration(n)
        | SchemaValue::Decimal(n) => ValueOut::Integer(*n),
        SchemaValue::Password(_) | SchemaValue::Secret(_) => ValueOut::String("***".to_string()),
        SchemaValue::Text(s) => ValueOut::String(s.to_string()),
        SchemaValue::Email(s)
        | SchemaValue::Url(s)
        | SchemaValue::NodeRef(s)
        | SchemaValue::EdgeRef(s) => ValueOut::String(s.clone()),
        other => ValueOut::String(format!("{other}")),
    }
}
