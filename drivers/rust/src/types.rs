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

impl QueryResult {
    /// Build a `QueryResult` from the JSON envelope the v2 server
    /// emits in a `Result` frame. Today the envelope only carries
    /// `{ statement, affected }` — column / row streaming is the
    /// follow-up that introduces `RowDescription` + `DataRow`
    /// frames.
    pub fn from_envelope(value: serde_json::Value) -> Self {
        let obj = value.as_object().cloned().unwrap_or_default();
        let statement = obj
            .get("statement")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let affected = obj
            .get("affected")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        Self {
            statement,
            affected,
            columns: Vec::new(),
            rows: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct InsertResult {
    pub affected: u64,
    /// Present when the engine surfaces an inserted entity id.
    pub id: Option<String>,
}
