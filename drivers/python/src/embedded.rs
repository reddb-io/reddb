//! Embedded backend — wraps `reddb::runtime::RedDBRuntime` in-process.
//!
//! Compiled only when the `embedded` Cargo feature is on (default).

use std::path::PathBuf;
use std::sync::Arc;

use reddb::api::RedDBOptions;
use reddb::runtime::RedDBRuntime;
use reddb::storage::query::modes::parse_multi;
use reddb::storage::query::unified::UnifiedRecord;
use reddb::storage::query::user_params;
use reddb::storage::schema::Value as SchemaValue;
use reddb::RuntimeEntityPort;

pub use reddb::storage::schema::Value as ParamValue;

#[derive(Clone)]
pub struct EmbeddedRuntime {
    runtime: Arc<RedDBRuntime>,
}

pub struct QueryRows {
    pub statement: String,
    pub affected: u64,
    pub columns: Vec<String>,
    /// Each row: list of (column, value-as-Python-friendly-string-or-number).
    /// Real Python objects are built later by `high_level.rs` from these.
    pub rows: Vec<Vec<(String, ScalarOut)>>,
}

pub struct InsertObjectResult {
    pub affected: u64,
    pub id: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ScalarOut {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Json(String),
}

impl EmbeddedRuntime {
    pub fn open(path: PathBuf) -> Result<Self, String> {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(path))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            runtime: Arc::new(rt),
        })
    }

    pub fn in_memory() -> Result<Self, String> {
        let rt = RedDBRuntime::in_memory().map_err(|e| e.to_string())?;
        Ok(Self {
            runtime: Arc::new(rt),
        })
    }

    pub fn query(&self, sql: &str) -> Result<QueryRows, String> {
        let qr = self.runtime.execute_query(sql).map_err(|e| e.to_string())?;
        Ok(map_query_result(&qr))
    }

    /// Parameterized query: parse `sql`, bind `$N` slots with `params`,
    /// then run the expression directly (skips the SQL plan cache).
    pub fn query_with_params(&self, sql: &str, params: &[ParamValue]) -> Result<QueryRows, String> {
        let parsed = parse_multi(sql).map_err(|e| e.to_string())?;
        let bound = user_params::bind(&parsed, params).map_err(|e| e.to_string())?;
        let qr = self
            .runtime
            .execute_query_expr(bound)
            .map_err(|e| e.to_string())?;
        Ok(map_query_result(&qr))
    }

    pub fn insert_object(
        &self,
        collection: &str,
        fields: &[(String, ScalarOut)],
    ) -> Result<InsertObjectResult, String> {
        let column_names: Vec<String> = fields.iter().map(|(name, _)| name.clone()).collect();
        let row: Vec<SchemaValue> = fields
            .iter()
            .map(|(_, value)| scalar_to_schema_value(value))
            .collect();
        let outputs = RuntimeEntityPort::create_rows_batch_columnar_with_outputs(
            self.runtime.as_ref(),
            collection.to_string(),
            Arc::new(column_names),
            vec![row],
        )
        .map_err(|e| e.to_string())?;
        Ok(InsertObjectResult {
            affected: outputs.len() as u64,
            id: outputs.first().map(|output| output.id.raw().to_string()),
        })
    }

    pub fn delete(&self, collection: &str, id: &str) -> Result<u64, String> {
        let sql = format!("DELETE FROM {collection} WHERE rid = {id}");
        let qr = self
            .runtime
            .execute_query(&sql)
            .map_err(|e| e.to_string())?;
        Ok(qr.affected_rows)
    }

    pub fn checkpoint(&self) -> Result<(), String> {
        self.runtime.checkpoint().map_err(|e| e.to_string())
    }

    pub fn clone_runtime(&self) -> Arc<RedDBRuntime> {
        self.runtime.clone()
    }
}

fn scalar_to_schema_value(v: &ScalarOut) -> SchemaValue {
    match v {
        ScalarOut::Null => SchemaValue::Null,
        ScalarOut::Bool(b) => SchemaValue::Boolean(*b),
        ScalarOut::Int(n) => SchemaValue::Integer(*n),
        ScalarOut::Float(n) => SchemaValue::Float(*n),
        ScalarOut::Text(s) => SchemaValue::Text(Arc::from(s.as_str())),
        ScalarOut::Json(s) => SchemaValue::Json(s.as_bytes().to_vec()),
    }
}

fn map_query_result(qr: &reddb::runtime::RuntimeQueryResult) -> QueryRows {
    let columns: Vec<String> = qr
        .result
        .records
        .first()
        .map(|r| {
            let mut keys: Vec<String> = r.iter_fields().map(|(k, _)| k.to_string()).collect();
            keys.sort();
            keys
        })
        .unwrap_or_default();

    let rows: Vec<Vec<(String, ScalarOut)>> =
        qr.result.records.iter().map(record_to_pairs).collect();

    QueryRows {
        statement: qr.statement_type.to_string(),
        affected: qr.affected_rows,
        columns,
        rows,
    }
}

fn record_to_pairs(record: &UnifiedRecord) -> Vec<(String, ScalarOut)> {
    let mut entries: Vec<(&std::sync::Arc<str>, &SchemaValue)> = record.iter_fields().collect();
    entries.sort_by(|a, b| a.0.as_ref().cmp(b.0.as_ref()));
    entries
        .into_iter()
        .map(|(k, v)| (k.to_string(), schema_value_to_scalar(v)))
        .collect()
}

fn schema_value_to_scalar(v: &SchemaValue) -> ScalarOut {
    match v {
        SchemaValue::Null => ScalarOut::Null,
        SchemaValue::Boolean(b) => ScalarOut::Bool(*b),
        SchemaValue::Integer(n) => ScalarOut::Int(*n),
        SchemaValue::UnsignedInteger(n) => ScalarOut::Int(*n as i64),
        SchemaValue::Float(n) => ScalarOut::Float(*n),
        SchemaValue::BigInt(n) => ScalarOut::Int(*n),
        SchemaValue::TimestampMs(n)
        | SchemaValue::Timestamp(n)
        | SchemaValue::Duration(n)
        | SchemaValue::Decimal(n) => ScalarOut::Int(*n),
        SchemaValue::Password(_) | SchemaValue::Secret(_) => ScalarOut::Text("***".to_string()),
        SchemaValue::Text(s) => ScalarOut::Text(s.to_string()),
        SchemaValue::Json(bytes) => {
            ScalarOut::Json(String::from_utf8_lossy(bytes.as_slice()).to_string())
        }
        SchemaValue::Email(s)
        | SchemaValue::Url(s)
        | SchemaValue::NodeRef(s)
        | SchemaValue::EdgeRef(s) => ScalarOut::Text(s.clone()),
        other => ScalarOut::Text(format!("{other}")),
    }
}
