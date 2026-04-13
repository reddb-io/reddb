//! Embedded backend — wraps `reddb::runtime::RedDBRuntime` in-process.
//!
//! Compiled only when the `embedded` Cargo feature is on (default).

use std::path::PathBuf;
use std::sync::Arc;

use reddb::api::RedDBOptions;
use reddb::runtime::RedDBRuntime;
use reddb::storage::query::unified::UnifiedRecord;
use reddb::storage::schema::Value as SchemaValue;

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

#[derive(Debug, Clone)]
pub enum ScalarOut {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
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

    pub fn insert_object(
        &self,
        collection: &str,
        fields: &[(String, ScalarOut)],
    ) -> Result<u64, String> {
        let sql = build_insert_sql(collection, fields);
        let qr = self.runtime.execute_query(&sql).map_err(|e| e.to_string())?;
        Ok(qr.affected_rows)
    }

    pub fn delete(&self, collection: &str, id: &str) -> Result<u64, String> {
        let sql = format!("DELETE FROM {collection} WHERE _entity_id = {id}");
        let qr = self.runtime.execute_query(&sql).map_err(|e| e.to_string())?;
        Ok(qr.affected_rows)
    }

    pub fn checkpoint(&self) -> Result<(), String> {
        self.runtime.checkpoint().map_err(|e| e.to_string())
    }
}

fn build_insert_sql(collection: &str, fields: &[(String, ScalarOut)]) -> String {
    let mut cols = Vec::new();
    let mut vals = Vec::new();
    for (k, v) in fields {
        cols.push(k.clone());
        vals.push(scalar_to_sql_literal(v));
    }
    format!(
        "INSERT INTO {collection} ({}) VALUES ({})",
        cols.join(", "),
        vals.join(", "),
    )
}

fn scalar_to_sql_literal(v: &ScalarOut) -> String {
    match v {
        ScalarOut::Null => "NULL".to_string(),
        ScalarOut::Bool(b) => b.to_string(),
        ScalarOut::Int(n) => n.to_string(),
        ScalarOut::Float(n) => {
            if n.fract() == 0.0 && n.is_finite() {
                format!("{}", *n as i64)
            } else {
                n.to_string()
            }
        }
        ScalarOut::Text(s) => format!("'{}'", s.replace('\'', "''")),
    }
}

fn map_query_result(qr: &reddb::runtime::RuntimeQueryResult) -> QueryRows {
    let columns: Vec<String> = qr
        .result
        .records
        .first()
        .map(|r| {
            let mut keys: Vec<String> = r.values.keys().cloned().collect();
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
    let mut entries: Vec<(&String, &SchemaValue)> = record.values.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    entries
        .into_iter()
        .map(|(k, v)| (k.clone(), schema_value_to_scalar(v)))
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
        SchemaValue::Text(s)
        | SchemaValue::Email(s)
        | SchemaValue::Url(s)
        | SchemaValue::NodeRef(s)
        | SchemaValue::EdgeRef(s) => ScalarOut::Text(s.clone()),
        other => ScalarOut::Text(format!("{other}")),
    }
}
