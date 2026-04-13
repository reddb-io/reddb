//! JSON-RPC 2.0 line-delimited stdio mode for the `red` binary.
//!
//! See `PLAN_DRIVERS.md` for the protocol spec. This module is the
//! sole server-side implementation of the protocol — drivers in
//! every language target this contract.
//!
//! Loop:
//!   1. Read a line from stdin (UTF-8, terminated by `\n`).
//!   2. Parse it as a JSON-RPC 2.0 request envelope.
//!   3. Dispatch on `method` to the runtime.
//!   4. Serialize the response as a single line on stdout, flush.
//!   5. Repeat until EOF or `close` method received.
//!
//! Errors do not crash the loop. Panics inside a method handler are
//! caught and reported as `INTERNAL_ERROR` so a buggy query cannot
//! kill the daemon.

use std::io::{BufRead, BufReader, Stdin, Write};
use std::panic::AssertUnwindSafe;

use crate::json::{self as json, Value};
use crate::runtime::{RedDBRuntime, RuntimeQueryResult};
use crate::storage::query::unified::UnifiedRecord;
use crate::storage::schema::Value as SchemaValue;

/// Protocol version reported by the `version` method.
pub const PROTOCOL_VERSION: &str = "1.0";

/// Stable error codes. Drivers map these to idiomatic exceptions.
pub mod error_code {
    pub const PARSE_ERROR: &str = "PARSE_ERROR";
    pub const INVALID_REQUEST: &str = "INVALID_REQUEST";
    pub const INVALID_PARAMS: &str = "INVALID_PARAMS";
    pub const QUERY_ERROR: &str = "QUERY_ERROR";
    pub const NOT_FOUND: &str = "NOT_FOUND";
    pub const INTERNAL_ERROR: &str = "INTERNAL_ERROR";
}

/// Run the stdio JSON-RPC loop against the provided runtime.
///
/// Returns the process exit code. Returns `0` on normal shutdown
/// (EOF or explicit `close`). Returns non-zero only on a fatal I/O
/// error reading stdin or writing stdout.
pub fn run(runtime: &RedDBRuntime) -> i32 {
    run_with_io(runtime, std::io::stdin(), &mut std::io::stdout())
}

/// Same as [`run`] but takes explicit I/O handles. Used by tests.
pub fn run_with_io<W: Write>(runtime: &RedDBRuntime, stdin: Stdin, stdout: &mut W) -> i32 {
    let reader = BufReader::new(stdin.lock());
    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                let _ = writeln!(
                    stdout,
                    "{}",
                    error_response(&Value::Null, error_code::INTERNAL_ERROR, &e.to_string())
                );
                let _ = stdout.flush();
                return 1;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let response = handle_line(runtime, &line);
        if writeln!(stdout, "{}", response).is_err() || stdout.flush().is_err() {
            return 1;
        }
        // `close` is special: respond, then exit cleanly.
        if response.contains("\"__close__\":true") {
            return 0;
        }
    }
    0
}

/// Parse one input line and dispatch. Always returns a single-line
/// JSON string suitable for direct write to stdout. Never panics
/// (panics inside handlers are caught and reported).
fn handle_line(runtime: &RedDBRuntime, line: &str) -> String {
    let parsed: Value = match json::from_str(line) {
        Ok(v) => v,
        Err(err) => {
            return error_response(
                &Value::Null,
                error_code::PARSE_ERROR,
                &format!("invalid JSON: {err}"),
            );
        }
    };

    let id = parsed.get("id").cloned().unwrap_or(Value::Null);

    let method = match parsed.get("method").and_then(Value::as_str) {
        Some(m) => m.to_string(),
        None => {
            return error_response(&id, error_code::INVALID_REQUEST, "missing 'method' field");
        }
    };

    let params = parsed.get("params").cloned().unwrap_or(Value::Null);

    let dispatch = std::panic::catch_unwind(AssertUnwindSafe(|| {
        dispatch_method(runtime, &method, &params)
    }));

    match dispatch {
        Ok(Ok(result)) => success_response(&id, &result, method == "close"),
        Ok(Err((code, msg))) => error_response(&id, code, &msg),
        Err(_) => error_response(&id, error_code::INTERNAL_ERROR, "handler panicked (caught)"),
    }
}

/// Dispatch a parsed method call. Returns the `result` value on
/// success or `(error_code, message)` on failure.
fn dispatch_method(
    runtime: &RedDBRuntime,
    method: &str,
    params: &Value,
) -> Result<Value, (&'static str, String)> {
    match method {
        "version" => Ok(Value::Object(
            [
                (
                    "version".to_string(),
                    Value::String(env!("CARGO_PKG_VERSION").to_string()),
                ),
                (
                    "protocol".to_string(),
                    Value::String(PROTOCOL_VERSION.to_string()),
                ),
            ]
            .into_iter()
            .collect(),
        )),

        "health" => Ok(Value::Object(
            [
                ("ok".to_string(), Value::Bool(true)),
                (
                    "version".to_string(),
                    Value::String(env!("CARGO_PKG_VERSION").to_string()),
                ),
            ]
            .into_iter()
            .collect(),
        )),

        "query" => {
            let sql = params.get("sql").and_then(Value::as_str).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'sql' string".to_string(),
            ))?;
            let qr = runtime
                .execute_query(sql)
                .map_err(|e| (error_code::QUERY_ERROR, e.to_string()))?;
            Ok(query_result_to_json(&qr))
        }

        "insert" => {
            let collection = params.get("collection").and_then(Value::as_str).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'collection' string".to_string(),
            ))?;
            let payload = params.get("payload").ok_or((
                error_code::INVALID_PARAMS,
                "missing 'payload' object".to_string(),
            ))?;
            let payload_obj = payload.as_object().ok_or((
                error_code::INVALID_PARAMS,
                "'payload' must be a JSON object".to_string(),
            ))?;
            let sql = build_insert_sql(collection, payload_obj.iter());
            let qr = runtime
                .execute_query(&sql)
                .map_err(|e| (error_code::QUERY_ERROR, e.to_string()))?;
            Ok(insert_result_to_json(&qr))
        }

        "bulk_insert" => {
            let collection = params.get("collection").and_then(Value::as_str).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'collection' string".to_string(),
            ))?;
            let payloads = params.get("payloads").and_then(Value::as_array).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'payloads' array".to_string(),
            ))?;
            let mut total_affected: u64 = 0;
            for entry in payloads {
                let obj = entry.as_object().ok_or((
                    error_code::INVALID_PARAMS,
                    "each payload must be a JSON object".to_string(),
                ))?;
                let sql = build_insert_sql(collection, obj.iter());
                let qr = runtime
                    .execute_query(&sql)
                    .map_err(|e| (error_code::QUERY_ERROR, e.to_string()))?;
                total_affected += qr.affected_rows;
            }
            Ok(Value::Object(
                [("affected".to_string(), Value::Number(total_affected as f64))]
                    .into_iter()
                    .collect(),
            ))
        }

        "get" => {
            let collection = params.get("collection").and_then(Value::as_str).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'collection' string".to_string(),
            ))?;
            let id = params.get("id").and_then(Value::as_str).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'id' string".to_string(),
            ))?;
            let sql = format!("SELECT * FROM {collection} WHERE _entity_id = {id} LIMIT 1");
            let qr = runtime
                .execute_query(&sql)
                .map_err(|e| (error_code::QUERY_ERROR, e.to_string()))?;
            let entity = qr
                .result
                .records
                .first()
                .map(record_to_json_object)
                .unwrap_or(Value::Null);
            Ok(Value::Object(
                [("entity".to_string(), entity)].into_iter().collect(),
            ))
        }

        "delete" => {
            let collection = params.get("collection").and_then(Value::as_str).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'collection' string".to_string(),
            ))?;
            let id = params.get("id").and_then(Value::as_str).ok_or((
                error_code::INVALID_PARAMS,
                "missing 'id' string".to_string(),
            ))?;
            let sql = format!("DELETE FROM {collection} WHERE _entity_id = {id}");
            let qr = runtime
                .execute_query(&sql)
                .map_err(|e| (error_code::QUERY_ERROR, e.to_string()))?;
            Ok(Value::Object(
                [(
                    "affected".to_string(),
                    Value::Number(qr.affected_rows as f64),
                )]
                .into_iter()
                .collect(),
            ))
        }

        "close" => {
            let _ = runtime.checkpoint();
            Ok(Value::Null)
        }

        other => Err((
            error_code::INVALID_REQUEST,
            format!("unknown method: {other}"),
        )),
    }
}

// ---------------------------------------------------------------------------
// Response builders
// ---------------------------------------------------------------------------

fn success_response(id: &Value, result: &Value, is_close: bool) -> String {
    // For `close` we tag the response so the loop knows to exit after
    // flushing. The tag is stripped from the wire by replacing it
    // before serialization — actually we just include it as a sentinel
    // field that drivers ignore (forward compat).
    let mut envelope = json::Map::new();
    envelope.insert("jsonrpc".to_string(), Value::String("2.0".to_string()));
    envelope.insert("id".to_string(), id.clone());
    envelope.insert("result".to_string(), result.clone());
    if is_close {
        envelope.insert("__close__".to_string(), Value::Bool(true));
    }
    Value::Object(envelope).to_string_compact()
}

fn error_response(id: &Value, code: &str, message: &str) -> String {
    let mut err = json::Map::new();
    err.insert("code".to_string(), Value::String(code.to_string()));
    err.insert("message".to_string(), Value::String(message.to_string()));
    err.insert("data".to_string(), Value::Null);

    let mut envelope = json::Map::new();
    envelope.insert("jsonrpc".to_string(), Value::String("2.0".to_string()));
    envelope.insert("id".to_string(), id.clone());
    envelope.insert("error".to_string(), Value::Object(err));
    Value::Object(envelope).to_string_compact()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_insert_sql<'a, I>(collection: &str, fields: I) -> String
where
    I: Iterator<Item = (&'a String, &'a Value)>,
{
    let mut cols = Vec::new();
    let mut vals = Vec::new();
    for (k, v) in fields {
        cols.push(k.clone());
        vals.push(value_to_sql_literal(v));
    }
    format!(
        "INSERT INTO {collection} ({}) VALUES ({})",
        cols.join(", "),
        vals.join(", "),
    )
}

fn value_to_sql_literal(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => {
            if n.fract() == 0.0 {
                format!("{}", *n as i64)
            } else {
                n.to_string()
            }
        }
        Value::String(s) => format!("'{}'", s.replace('\'', "''")),
        other => format!("'{}'", other.to_string_compact().replace('\'', "''")),
    }
}

fn query_result_to_json(qr: &RuntimeQueryResult) -> Value {
    let mut envelope = json::Map::new();
    envelope.insert(
        "statement".to_string(),
        Value::String(qr.statement_type.to_string()),
    );
    envelope.insert(
        "affected".to_string(),
        Value::Number(qr.affected_rows as f64),
    );

    let mut columns = Vec::new();
    if let Some(first) = qr.result.records.first() {
        let mut keys: Vec<&String> = first.values.keys().collect();
        keys.sort();
        columns = keys.into_iter().map(|k| Value::String(k.clone())).collect();
    }
    envelope.insert("columns".to_string(), Value::Array(columns));

    let rows: Vec<Value> = qr
        .result
        .records
        .iter()
        .map(record_to_json_object)
        .collect();
    envelope.insert("rows".to_string(), Value::Array(rows));

    Value::Object(envelope)
}

fn insert_result_to_json(qr: &RuntimeQueryResult) -> Value {
    let mut envelope = json::Map::new();
    envelope.insert(
        "affected".to_string(),
        Value::Number(qr.affected_rows as f64),
    );
    // First row of the result, if any, contains the inserted entity id.
    if let Some(first) = qr.result.records.first() {
        if let Some(id_val) = first
            .values
            .iter()
            .find(|(k, _)| k.as_str() == "_entity_id")
            .map(|(_, v)| schema_value_to_json(v))
        {
            envelope.insert("id".to_string(), id_val);
        }
    }
    Value::Object(envelope)
}

fn record_to_json_object(record: &UnifiedRecord) -> Value {
    let mut map = json::Map::new();
    let mut entries: Vec<(&String, &SchemaValue)> = record.values.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    for (k, v) in entries {
        map.insert(k.clone(), schema_value_to_json(v));
    }
    Value::Object(map)
}

fn schema_value_to_json(v: &SchemaValue) -> Value {
    match v {
        SchemaValue::Null => Value::Null,
        SchemaValue::Boolean(b) => Value::Bool(*b),
        SchemaValue::Integer(n) => Value::Number(*n as f64),
        SchemaValue::UnsignedInteger(n) => Value::Number(*n as f64),
        SchemaValue::Float(n) => Value::Number(*n),
        SchemaValue::BigInt(n) => Value::Number(*n as f64),
        SchemaValue::TimestampMs(n)
        | SchemaValue::Timestamp(n)
        | SchemaValue::Duration(n)
        | SchemaValue::Decimal(n) => Value::Number(*n as f64),
        SchemaValue::Password(_) | SchemaValue::Secret(_) => Value::String("***".to_string()),
        SchemaValue::Text(s)
        | SchemaValue::Email(s)
        | SchemaValue::Url(s)
        | SchemaValue::NodeRef(s)
        | SchemaValue::EdgeRef(s) => Value::String(s.clone()),
        other => Value::String(format!("{other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_runtime() -> RedDBRuntime {
        RedDBRuntime::in_memory().expect("in-memory runtime")
    }

    #[test]
    fn version_method_returns_version_and_protocol() {
        let rt = make_runtime();
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"version","params":{}}"#;
        let resp = handle_line(&rt, line);
        assert!(resp.contains("\"id\":1"));
        assert!(resp.contains("\"protocol\":\"1.0\""));
        assert!(resp.contains("\"version\""));
    }

    #[test]
    fn health_method_returns_ok_true() {
        let rt = make_runtime();
        let resp = handle_line(
            &rt,
            r#"{"jsonrpc":"2.0","id":"abc","method":"health","params":{}}"#,
        );
        assert!(resp.contains("\"ok\":true"));
        assert!(resp.contains("\"id\":\"abc\""));
    }

    #[test]
    fn parse_error_for_invalid_json() {
        let rt = make_runtime();
        let resp = handle_line(&rt, "not json {");
        assert!(resp.contains("\"code\":\"PARSE_ERROR\""));
        assert!(resp.contains("\"id\":null"));
    }

    #[test]
    fn invalid_request_when_method_missing() {
        let rt = make_runtime();
        let resp = handle_line(&rt, r#"{"jsonrpc":"2.0","id":1,"params":{}}"#);
        assert!(resp.contains("\"code\":\"INVALID_REQUEST\""));
    }

    #[test]
    fn unknown_method_is_invalid_request() {
        let rt = make_runtime();
        let resp = handle_line(
            &rt,
            r#"{"jsonrpc":"2.0","id":1,"method":"frobnicate","params":{}}"#,
        );
        assert!(resp.contains("\"code\":\"INVALID_REQUEST\""));
        assert!(resp.contains("frobnicate"));
    }

    #[test]
    fn invalid_params_when_query_sql_missing() {
        let rt = make_runtime();
        let resp = handle_line(
            &rt,
            r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{}}"#,
        );
        assert!(resp.contains("\"code\":\"INVALID_PARAMS\""));
    }

    #[test]
    fn close_method_marks_response_for_shutdown() {
        let rt = make_runtime();
        let resp = handle_line(
            &rt,
            r#"{"jsonrpc":"2.0","id":1,"method":"close","params":{}}"#,
        );
        assert!(resp.contains("\"__close__\":true"));
    }

    #[test]
    fn query_select_one_returns_rows() {
        let rt = make_runtime();
        let resp = handle_line(
            &rt,
            r#"{"jsonrpc":"2.0","id":1,"method":"query","params":{"sql":"SELECT 1 AS one"}}"#,
        );
        assert!(resp.contains("\"result\""));
        assert!(!resp.contains("\"error\""));
    }
}
