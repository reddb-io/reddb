//! Public value types — kept dependency-free on purpose so that
//! `reddb-client` does not force a serde version on consumers.
//!
//! Users that want to plug serde in can implement their own
//! conversions on top of these types. They mirror the JSON-RPC
//! shapes documented in `PLAN_DRIVERS.md`.

use std::fmt;

/// A small, hand-rolled JSON value used for `insert` / `bulk_insert`
/// payloads. We intentionally do not depend on `serde_json` so that
/// downstream crates can pick their own serde major version.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsonValue>),
    Object(Vec<(String, JsonValue)>),
}

impl JsonValue {
    pub fn null() -> Self {
        JsonValue::Null
    }

    pub fn bool(b: bool) -> Self {
        JsonValue::Bool(b)
    }

    pub fn number(n: impl Into<f64>) -> Self {
        JsonValue::Number(n.into())
    }

    pub fn string(s: impl Into<String>) -> Self {
        JsonValue::String(s.into())
    }

    pub fn object<I, K>(entries: I) -> Self
    where
        I: IntoIterator<Item = (K, JsonValue)>,
        K: Into<String>,
    {
        JsonValue::Object(entries.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    pub fn array<I>(items: I) -> Self
    where
        I: IntoIterator<Item = JsonValue>,
    {
        JsonValue::Array(items.into_iter().collect())
    }

    pub fn as_object(&self) -> Option<&[(String, JsonValue)]> {
        match self {
            JsonValue::Object(entries) => Some(entries.as_slice()),
            _ => None,
        }
    }

    pub fn to_json_string(&self) -> String {
        let mut out = String::new();
        write_json(self, &mut out);
        out
    }
}

fn write_json(value: &JsonValue, out: &mut String) {
    match value {
        JsonValue::Null => out.push_str("null"),
        JsonValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        JsonValue::Number(n) => {
            if n.fract() == 0.0 && n.is_finite() {
                out.push_str(&format!("{}", *n as i64));
            } else {
                out.push_str(&format!("{n}"));
            }
        }
        JsonValue::String(s) => {
            out.push('"');
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if (c as u32) < 0x20 => {
                        out.push_str(&format!("\\u{:04x}", c as u32));
                    }
                    c => out.push(c),
                }
            }
            out.push('"');
        }
        JsonValue::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(item, out);
            }
            out.push(']');
        }
        JsonValue::Object(entries) => {
            out.push('{');
            for (i, (k, v)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(&JsonValue::String(k.clone()), out);
                out.push(':');
                write_json(v, out);
            }
            out.push('}');
        }
    }
}

/// A scalar value as it comes out of a query. Mirrors the JSON-RPC
/// row shape but with native Rust types.
#[derive(Debug, Clone, PartialEq)]
pub enum ValueOut {
    Null,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
}

impl fmt::Display for ValueOut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValueOut::Null => f.write_str("null"),
            ValueOut::Bool(b) => write!(f, "{b}"),
            ValueOut::Integer(n) => write!(f, "{n}"),
            ValueOut::Float(n) => write!(f, "{n}"),
            ValueOut::String(s) => write!(f, "{s}"),
        }
    }
}

/// Shape returned by [`crate::Reddb::query`]. Field order matches
/// the JSON-RPC protocol so cross-language tests are trivial.
#[derive(Debug, Clone)]
pub struct QueryResult {
    pub statement: String,
    pub affected: u64,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<(String, ValueOut)>>,
}

pub type Row = Vec<(String, ValueOut)>;

#[derive(Debug, Clone, Default)]
pub struct ListOptions<'a> {
    pub filter: Option<&'a str>,
    pub order_by: Option<&'a str>,
    pub limit: Option<u64>,
}

impl<'a> ListOptions<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn filter(mut self, filter: &'a str) -> Self {
        self.filter = Some(filter);
        self
    }

    pub fn order_by(mut self, order_by: &'a str) -> Self {
        self.order_by = Some(order_by);
        self
    }

    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }
}

#[derive(Debug, Clone)]
pub struct ListResult {
    pub items: Vec<Row>,
    pub affected: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteResult {
    pub affected: u64,
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistsResult {
    pub exists: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DocumentItem {
    pub rid: String,
    pub fields: Row,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KvItem {
    pub collection: String,
    pub key: String,
    pub value: ValueOut,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KvWatchEvent {
    pub key: String,
    pub op: String,
    pub before: serde_json::Value,
    pub after: serde_json::Value,
    pub lsn: u64,
    pub committed_at: u64,
    pub dropped_event_count: u64,
}

#[cfg(any(feature = "redwire", feature = "http"))]
impl QueryResult {
    /// Build a `QueryResult` from the JSON envelope the server
    /// emits in a `Result` frame.
    pub fn from_envelope(value: serde_json::Value) -> Self {
        let Some(obj) = value.as_object() else {
            return Self {
                statement: String::new(),
                affected: 0,
                columns: Vec::new(),
                rows: Vec::new(),
            };
        };
        let result_obj = obj.get("result").and_then(|v| v.as_object()).unwrap_or(obj);
        let statement = obj
            .get("statement")
            .or_else(|| obj.get("statement_type"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let affected = obj
            .get("affected")
            .or_else(|| obj.get("affected_rows"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let columns: Vec<String> = result_obj
            .get("columns")
            .and_then(|v| v.as_array())
            .map(|cols| {
                cols.iter()
                    .filter_map(|col| col.as_str().map(ToOwned::to_owned))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let row_values = result_obj
            .get("records")
            .or_else(|| result_obj.get("rows"))
            .and_then(|v| v.as_array());
        let rows = row_values
            .map(|records| {
                records
                    .iter()
                    .map(|record| parse_record(record, &columns))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            statement,
            affected,
            columns,
            rows,
        }
    }
}

#[cfg(any(feature = "redwire", feature = "http"))]
fn parse_record(record: &serde_json::Value, columns: &[String]) -> Vec<(String, ValueOut)> {
    let Some(record_obj) = record.as_object() else {
        return Vec::new();
    };
    let values = record_obj
        .get("values")
        .and_then(|v| v.as_object())
        .unwrap_or(record_obj);
    if columns.is_empty() {
        return values
            .iter()
            .map(|(key, value)| (key.clone(), json_to_value_out(value)))
            .collect();
    }
    columns
        .iter()
        .map(|column| {
            (
                column.clone(),
                values
                    .get(column)
                    .map(json_to_value_out)
                    .unwrap_or(ValueOut::Null),
            )
        })
        .collect()
}

#[cfg(any(feature = "redwire", feature = "http"))]
fn json_to_value_out(value: &serde_json::Value) -> ValueOut {
    match value {
        serde_json::Value::Null => ValueOut::Null,
        serde_json::Value::Bool(value) => ValueOut::Bool(*value),
        serde_json::Value::Number(value) => {
            if let Some(n) = value.as_i64() {
                ValueOut::Integer(n)
            } else if let Some(n) = value.as_f64() {
                ValueOut::Float(n)
            } else {
                ValueOut::String(value.to_string())
            }
        }
        serde_json::Value::String(value) => ValueOut::String(value.clone()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            ValueOut::String(value.to_string())
        }
    }
}

#[derive(Debug, Clone)]
pub struct InsertResult {
    pub affected: u64,
    /// Present when the engine surfaces an inserted RedDB ID.
    pub rid: Option<String>,
    /// Legacy alias for `rid`.
    pub id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkInsertResult {
    pub affected: u64,
    /// Present when the engine surfaces inserted RedDB IDs for the batch.
    pub rids: Vec<String>,
    /// Legacy alias for `rids`.
    pub ids: Vec<String>,
}
