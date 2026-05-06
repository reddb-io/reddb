//! Embedded backend — wraps the in-process RedDB engine.
//!
//! Compiled only when the `embedded` Cargo feature is enabled (default).
//! When `embedded` is off, this module does not exist and the
//! [`crate::Reddb`] enum will refuse to construct an embedded variant
//! at runtime with a clear `FEATURE_DISABLED` error.

use std::path::PathBuf;
use std::sync::Arc;

use reddb_server::RuntimeEntityPort;
use reddb_server::api::RedDBOptions;
use reddb_server::runtime::RedDBRuntime;
use reddb_server::storage::query::unified::UnifiedRecord;
use reddb_server::storage::schema::Value as SchemaValue;

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

    /// Single-row insert. Routes through the same
    /// `runtime.create_rows_batch_columnar` port that
    /// [`Self::bulk_insert`] uses (#110), passing a one-row batch.
    /// Skips `build_insert_sql` + `execute_query`, so this hot
    /// autocommit path pays zero SQL build / lex / parse / plan cost
    /// when the collection carries no contract.
    pub fn insert(&self, collection: &str, payload: &JsonValue) -> Result<InsertResult> {
        let object = payload.as_object().ok_or_else(|| {
            ClientError::new(
                ErrorCode::QueryError,
                "insert payload must be a JSON object".to_string(),
            )
        })?;
        let column_names: Vec<String> = object.iter().map(|(k, _)| k.clone()).collect();
        let row: Vec<SchemaValue> =
            object.iter().map(|(_, v)| json_value_to_schema_value(v)).collect();
        let count = self
            .runtime
            .create_rows_batch_columnar(
                collection.to_string(),
                Arc::new(column_names),
                vec![row],
            )
            .map_err(|e| ClientError::new(ErrorCode::QueryError, e.to_string()))?;
        Ok(InsertResult {
            affected: count as u64,
            id: None,
        })
    }

    /// Routes through `runtime.create_rows_batch_columnar`, which fast-paths
    /// to the prevalidated columnar kernel when the collection carries no
    /// contract — same shape `MSG_BULK_INSERT_BINARY` already uses on the
    /// wire path. Result: one WAL append per batch instead of one per row,
    /// no per-row SQL build / lex / parse / plan, and no per-row `(String,
    /// Value)` tuple materialisation when the collection is contract-free.
    ///
    /// Heterogeneous payloads (rows with differing key sets) fall back to
    /// the per-row `execute_query` path so existing semantics are preserved
    /// for callers that mix shapes.
    pub fn bulk_insert(&self, collection: &str, payloads: &[JsonValue]) -> Result<u64> {
        if payloads.is_empty() {
            return Ok(0);
        }

        // Validate every payload is a JSON object up-front. Mirrors the
        // old loop's error contract.
        let objects: Vec<&[(String, JsonValue)]> = payloads
            .iter()
            .map(|p| {
                p.as_object().ok_or_else(|| {
                    ClientError::new(
                        ErrorCode::QueryError,
                        "bulk_insert payloads must be JSON objects".to_string(),
                    )
                })
            })
            .collect::<Result<_>>()?;

        // Columnar fast path requires a uniform schema (same column names in
        // the same order across every row). When that holds we pay zero
        // per-row SQL build / lex / parse cost. When it doesn't we fall back
        // to the per-row loop so heterogeneous workloads stay correct.
        if uniform_schema(&objects) {
            let column_names: Vec<String> =
                objects[0].iter().map(|(k, _)| k.clone()).collect();
            let ncols = column_names.len();
            let mut rows: Vec<Vec<SchemaValue>> = Vec::with_capacity(objects.len());
            for obj in &objects {
                let mut values = Vec::with_capacity(ncols);
                for (_, v) in obj.iter() {
                    values.push(json_value_to_schema_value(v));
                }
                rows.push(values);
            }
            let count = self
                .runtime
                .create_rows_batch_columnar(
                    collection.to_string(),
                    Arc::new(column_names),
                    rows,
                )
                .map_err(|e| ClientError::new(ErrorCode::QueryError, e.to_string()))?;
            return Ok(count as u64);
        }

        // Fallback: heterogeneous shapes. Per-row `execute_query` retains
        // the old behaviour for mixed-key payloads.
        let mut total = 0u64;
        for object in &objects {
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

/// True when every row carries the same column names in the same order.
/// The columnar `create_rows_batch_columnar` port requires one shared
/// `Arc<Vec<String>>` schema, so heterogeneous payloads have to fall
/// back to the per-row path.
fn uniform_schema(objects: &[&[(String, JsonValue)]]) -> bool {
    let Some((first, rest)) = objects.split_first() else {
        return true;
    };
    let ncols = first.len();
    for row in rest {
        if row.len() != ncols {
            return false;
        }
        for ((k1, _), (k2, _)) in first.iter().zip(row.iter()) {
            if k1 != k2 {
                return false;
            }
        }
    }
    true
}

/// Best-effort `JsonValue` → `SchemaValue` coercion for the columnar
/// fast path. The contract-free branch in `create_rows_batch_columnar`
/// stores values without column-type normalisation, so the choice here
/// only affects what the row reads back as. The mapping mirrors what
/// `value_to_sql_literal` would have produced through the SQL parser:
/// integers stay integers, fractional numbers stay floats, arrays /
/// objects are JSON-encoded into a `Text` value (the parser would have
/// quoted them too, so the on-disk shape matches).
fn json_value_to_schema_value(v: &JsonValue) -> SchemaValue {
    match v {
        JsonValue::Null => SchemaValue::Null,
        JsonValue::Bool(b) => SchemaValue::Boolean(*b),
        JsonValue::Number(n) => {
            if n.is_finite() && n.fract() == 0.0 && *n >= i64::MIN as f64 && *n <= i64::MAX as f64
            {
                SchemaValue::Integer(*n as i64)
            } else {
                SchemaValue::Float(*n)
            }
        }
        JsonValue::String(s) => SchemaValue::Text(std::sync::Arc::from(s.as_str())),
        JsonValue::Array(_) | JsonValue::Object(_) => {
            SchemaValue::Text(std::sync::Arc::from(v.to_json_string()))
        }
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

fn map_query_result(qr: &reddb_server::runtime::RuntimeQueryResult) -> QueryResult {
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
