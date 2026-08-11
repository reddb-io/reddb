//! Embedded backend — wraps the in-process RedDB engine.
//!
//! Compiled only when the `embedded` Cargo feature is enabled (default).
//! When `embedded` is off, this module does not exist and the
//! [`crate::Reddb`] facade will refuse to construct an embedded adapter
//! at runtime with a clear `FEATURE_DISABLED` error.

use std::path::PathBuf;
use std::sync::Arc;

use reddb_server::api::RedDBOptions;
use reddb_server::runtime::RedDBRuntime;
use reddb_server::storage::query::unified::UnifiedRecord;
use reddb_types::Value as SchemaValue;

use crate::error::{ClientError, ErrorCode, Result};
use crate::params::IntoParams;
use crate::types::{BulkInsertResult, InsertResult, JsonValue, QueryResult, ValueOut};

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
        Self::open_with_options(RedDBOptions::persistent(path))
    }

    /// Open with explicit engine options (storage profile, packaging,
    /// durability). Callers that resolve deployment profiles — like the
    /// `red` CLI — build the options themselves; `open` keeps the
    /// plain-default path.
    pub fn open_with_options(options: RedDBOptions) -> Result<Self> {
        let runtime = RedDBRuntime::with_options(options)
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
        Ok(query_result_from_runtime(&qr))
    }

    /// Parameterized embedded query — see [`crate::Reddb::query_with`].
    /// Empty `params` short-circuits to the legacy `execute_query` fast
    /// path so the parameter-less hot path pays zero overhead.
    pub fn query_with<P: IntoParams>(&self, sql: &str, params: P) -> Result<QueryResult> {
        let params = params.into_params();
        if params.is_empty() {
            return self.query(sql);
        }
        let binds: Vec<SchemaValue> = params
            .iter()
            .cloned()
            .map(crate::params::Value::into_schema_value)
            .collect();
        // Bind inside the runtime's statement frame (#2183) so the
        // parameterized statement keeps the same snapshot isolation,
        // `AS OF` resolution, and intent locks as textual SQL.
        let qr = self
            .runtime
            .execute_query_with_params(sql, &binds)
            .map_err(|e| ClientError::new(ErrorCode::QueryError, e.to_string()))?;
        Ok(query_result_from_runtime(&qr))
    }

    /// Parameterized execution for INSERT / UPDATE / DELETE. RedDB uses the
    /// same result envelope for DML as for SELECT, so this forwards to
    /// [`Self::query_with`].
    pub fn execute_with<P: IntoParams>(&self, sql: &str, params: P) -> Result<QueryResult> {
        self.query_with(sql, params)
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
        let row: Vec<SchemaValue> = object
            .iter()
            .map(|(_, v)| json_value_to_schema_value(v))
            .collect();
        let outputs = reddb_server::RuntimeEntityPort::create_rows_batch_columnar_with_outputs(
            self.runtime.as_ref(),
            collection.to_string(),
            Arc::new(column_names),
            vec![row],
        )
        .map_err(|e| ClientError::new(ErrorCode::QueryError, e.to_string()))?;
        let rid = outputs.first().map(|output| output.id.raw().to_string());
        Ok(InsertResult {
            affected: outputs.len() as u64,
            rid: rid.clone(),
            id: rid,
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
    pub fn bulk_insert(
        &self,
        collection: &str,
        payloads: &[JsonValue],
    ) -> Result<BulkInsertResult> {
        if payloads.is_empty() {
            return Ok(BulkInsertResult {
                affected: 0,
                rids: Vec::new(),
                ids: Vec::new(),
            });
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
            let column_names: Vec<String> = objects[0].iter().map(|(k, _)| k.clone()).collect();
            let ncols = column_names.len();
            let mut rows: Vec<Vec<SchemaValue>> = Vec::with_capacity(objects.len());
            for obj in &objects {
                let mut values = Vec::with_capacity(ncols);
                for (_, v) in obj.iter() {
                    values.push(json_value_to_schema_value(v));
                }
                rows.push(values);
            }
            let outputs = reddb_server::RuntimeEntityPort::create_rows_batch_columnar_with_outputs(
                self.runtime.as_ref(),
                collection.to_string(),
                Arc::new(column_names),
                rows,
            )
            .map_err(|e| ClientError::new(ErrorCode::QueryError, e.to_string()))?;
            let rids: Vec<String> = outputs
                .iter()
                .map(|output| output.id.raw().to_string())
                .collect();
            return Ok(BulkInsertResult {
                affected: outputs.len() as u64,
                rids: rids.clone(),
                ids: rids,
            });
        }

        // Fallback: heterogeneous shapes. Per-row inserts retain existing
        // semantics for mixed-key payloads while still surfacing generated ids.
        let mut affected = 0u64;
        let mut ids = Vec::with_capacity(objects.len());
        for object in &objects {
            let payload = JsonValue::Object(object.to_vec());
            let result = self.insert(collection, &payload)?;
            affected += result.affected;
            if let Some(id) = result.id {
                ids.push(id);
            }
        }
        Ok(BulkInsertResult {
            affected,
            rids: ids.clone(),
            ids,
        })
    }

    pub fn delete(&self, collection: &str, rid: &str) -> Result<u64> {
        let rid = rid.replace('\'', "''");
        let sql = format!("DELETE FROM {collection} WHERE rid = '{rid}'");
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
            if n.is_finite() && n.fract() == 0.0 && *n >= i64::MIN as f64 && *n <= i64::MAX as f64 {
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

/// Convert an engine query result through the same value mapping used by the
/// embedded driver. Kept public for in-process tools that already own a
/// runtime, such as the CLI's ephemeral data-file query path.
pub fn query_result_from_runtime(qr: &reddb_server::runtime::RuntimeQueryResult) -> QueryResult {
    let columns = if qr.result.columns.is_empty() {
        qr.result
            .records
            .first()
            .map(|record| {
                let mut keys: Vec<String> = record
                    .column_names()
                    .iter()
                    .map(|key| key.to_string())
                    .collect();
                keys.sort();
                keys
            })
            .unwrap_or_default()
    } else {
        qr.result.columns.clone()
    };

    let rows: Vec<Vec<(String, ValueOut)>> =
        qr.result.records.iter().map(record_to_pairs).collect();

    QueryResult {
        statement: qr.statement_type.to_string(),
        affected: qr.affected_rows,
        columns,
        rows,
        notice: qr.notice.clone(),
    }
}

fn record_to_pairs(record: &UnifiedRecord) -> Vec<(String, ValueOut)> {
    let mut entries: Vec<(&str, &SchemaValue)> =
        record.iter_fields().map(|(k, v)| (k.as_ref(), v)).collect();
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
        SchemaValue::UnsignedInteger(n) => i64::try_from(*n)
            .map(ValueOut::Integer)
            .unwrap_or_else(|_| ValueOut::String(n.to_string())),
        SchemaValue::Float(n) => ValueOut::Float(*n),
        SchemaValue::DecimalText(s) => ValueOut::String(s.clone()),
        SchemaValue::BigInt(n) => ValueOut::Integer(*n),
        SchemaValue::TimestampMs(n)
        | SchemaValue::Timestamp(n)
        | SchemaValue::Duration(n)
        | SchemaValue::Decimal(n) => ValueOut::Integer(*n),
        SchemaValue::Password(_) | SchemaValue::Secret(_) => ValueOut::String("***".to_string()),
        SchemaValue::Json(bytes) => {
            reddb_server::json::from_slice::<reddb_server::json::Value>(bytes)
                .ok()
                .and_then(|json| reddb_server::json::to_string(&json).ok())
                .map(ValueOut::String)
                .unwrap_or(ValueOut::Null)
        }
        SchemaValue::Text(s) => ValueOut::String(s.to_string()),
        SchemaValue::Email(s)
        | SchemaValue::Url(s)
        | SchemaValue::NodeRef(s)
        | SchemaValue::EdgeRef(s) => ValueOut::String(s.clone()),
        other => ValueOut::String(format!("{other}")),
    }
}

#[cfg(test)]
mod value_out_tests {
    use super::*;

    #[test]
    fn conversion_preserves_the_arms_the_cli_relied_on() {
        // The divergent CLI duplicate was retired in favor of this
        // conversion (issue #2124); these arms are the ones that diverged.
        assert_eq!(
            schema_value_to_value_out(&SchemaValue::DecimalText("1.250".to_string())),
            ValueOut::String("1.250".to_string()),
        );
        assert_eq!(
            schema_value_to_value_out(&SchemaValue::Json(b"{\"b\": 2, \"a\": 1}".to_vec())),
            ValueOut::String("{\"a\":1,\"b\":2}".to_string()),
        );
        assert_eq!(
            schema_value_to_value_out(&SchemaValue::UnsignedInteger(7)),
            ValueOut::Integer(7),
        );
        assert_eq!(
            schema_value_to_value_out(&SchemaValue::UnsignedInteger(u64::MAX)),
            ValueOut::String(u64::MAX.to_string()),
        );
    }
}
