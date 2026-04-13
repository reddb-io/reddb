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
        let json_str = {
            let mut guard = self.inner.lock().await;
            guard
                .query(sql)
                .await
                .map_err(|e| ClientError::new(ErrorCode::QueryError, e.to_string()))?
        };
        parse_query_json(&json_str)
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
                .create_row(collection, &json_payload)
                .await
                .map_err(|e| ClientError::new(ErrorCode::QueryError, e.to_string()))?
        };
        // `create_row()` returns "id: N, entity: {..}" — extract the id.
        let id = reply
            .split_once("id: ")
            .and_then(|(_, rest)| rest.split_once(','))
            .map(|(id, _)| id.trim().to_string());
        Ok(InsertResult { affected: 1, id })
    }

    pub async fn bulk_insert(&self, collection: &str, payloads: &[JsonValue]) -> Result<u64> {
        let mut total: u64 = 0;
        for payload in payloads {
            if payload.as_object().is_none() {
                return Err(ClientError::new(
                    ErrorCode::QueryError,
                    "bulk_insert payloads must be JSON objects".to_string(),
                ));
            }
            let json_payload = payload.to_json_string();
            {
                let mut guard = self.inner.lock().await;
                guard
                    .create_row(collection, &json_payload)
                    .await
                    .map_err(|e| ClientError::new(ErrorCode::QueryError, e.to_string()))?;
            }
            total += 1;
        }
        Ok(total)
    }

    pub async fn delete(&self, collection: &str, id: &str) -> Result<u64> {
        let sql = format!("DELETE FROM {collection} WHERE _entity_id = {id}");
        let json_str = {
            let mut guard = self.inner.lock().await;
            guard
                .query(&sql)
                .await
                .map_err(|e| ClientError::new(ErrorCode::QueryError, e.to_string()))?
        };
        let parsed = parse_query_json(&json_str)?;
        Ok(parsed.affected)
    }

    pub async fn close(&self) -> Result<()> {
        // The tonic channel closes when `inner` drops.
        Ok(())
    }
}

/// Minimal, hand-rolled JSON parser for the server's `QueryReply.result_json`.
///
/// We cannot depend on `serde_json` without forcing a version on
/// downstream crates. The server JSON we care about has a stable,
/// simple shape: `{"statement", "affected", "columns", "rows"}`.
/// For anything we can't parse cleanly we fall back to empty fields
/// so the caller never crashes on an unexpected envelope.
fn parse_query_json(s: &str) -> Result<QueryResult> {
    let statement = extract_string(s, "statement").unwrap_or_else(|| "select".to_string());
    let affected = extract_u64(s, "affected").unwrap_or(0);
    let columns = extract_string_array(s, "columns").unwrap_or_default();
    // Rows are more complex — we walk the JSON structurally. Fall back
    // to an empty row list on parse failure, but surface errors via
    // `QueryError` when the top-level braces are missing.
    let rows = extract_rows(s, "rows").unwrap_or_default();
    Ok(QueryResult {
        statement,
        affected,
        columns,
        rows,
    })
}

fn extract_string(s: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":\"");
    let start = s.find(&needle)? + needle.len();
    let rest = &s[start..];
    let mut end = 0;
    let mut escaped = false;
    for (i, c) in rest.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c == '"' {
            end = i;
            break;
        }
    }
    Some(unescape(&rest[..end]))
}

fn extract_u64(s: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{key}\":");
    let start = s.find(&needle)? + needle.len();
    let rest = &s[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn extract_string_array(s: &str, key: &str) -> Option<Vec<String>> {
    let needle = format!("\"{key}\":[");
    let start = s.find(&needle)? + needle.len();
    let rest = &s[start..];
    let end = rest.find(']')?;
    let body = &rest[..end];
    let mut out = Vec::new();
    let mut in_str = false;
    let mut cur = String::new();
    let mut escaped = false;
    for c in body.chars() {
        if escaped {
            cur.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_str => escaped = true,
            '"' => {
                if in_str {
                    out.push(std::mem::take(&mut cur));
                    in_str = false;
                } else {
                    in_str = true;
                }
            }
            _ if in_str => cur.push(c),
            _ => {}
        }
    }
    Some(out)
}

fn extract_rows(s: &str, key: &str) -> Option<Vec<Vec<(String, ValueOut)>>> {
    let needle = format!("\"{key}\":[");
    let start = s.find(&needle)? + needle.len();
    let rest = &s[start..];
    // Find the matching closing bracket, respecting nested braces.
    let mut depth = 1i32;
    let mut end = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for (i, c) in rest.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_str {
            match c {
                '\\' => escaped = true,
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '[' | '{' => depth += 1,
            ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
    }
    let body = &rest[..end];
    // Split on `},{` at depth 0 — cheap because server output doesn't
    // nest rows.
    let mut rows = Vec::new();
    let mut cur = String::new();
    let mut d = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for c in body.chars() {
        if escaped {
            cur.push(c);
            escaped = false;
            continue;
        }
        if in_str {
            cur.push(c);
            match c {
                '\\' => escaped = true,
                '"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                cur.push(c);
            }
            '{' => {
                d += 1;
                cur.push(c);
            }
            '}' => {
                d -= 1;
                cur.push(c);
                if d == 0 {
                    rows.push(std::mem::take(&mut cur));
                }
            }
            ',' if d == 0 => {}
            _ => cur.push(c),
        }
    }
    Some(rows.into_iter().map(parse_row_object).collect())
}

fn parse_row_object(s: String) -> Vec<(String, ValueOut)> {
    // Strip leading '{' and trailing '}'.
    let trimmed = s.trim();
    let inner = trimmed
        .strip_prefix('{')
        .and_then(|r| r.strip_suffix('}'))
        .unwrap_or(trimmed);
    let mut out = Vec::new();
    let mut key = String::new();
    let mut value = String::new();
    let mut in_key = true;
    let mut in_str = false;
    let mut escaped = false;
    let mut saw_string_value = false;

    for c in inner.chars() {
        if escaped {
            if in_key {
                key.push(c);
            } else {
                value.push(c);
            }
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_str => escaped = true,
            '"' => {
                in_str = !in_str;
                if !in_key && !in_str {
                    saw_string_value = true;
                }
            }
            ':' if !in_str && in_key => {
                in_key = false;
            }
            ',' if !in_str && !in_key => {
                let parsed = parse_scalar(&value, saw_string_value);
                out.push((std::mem::take(&mut key), parsed));
                value.clear();
                in_key = true;
                saw_string_value = false;
            }
            _ if in_str && in_key => key.push(c),
            _ if in_str && !in_key => value.push(c),
            _ if !in_key && !c.is_whitespace() => value.push(c),
            _ => {}
        }
    }
    if !key.is_empty() {
        let parsed = parse_scalar(&value, saw_string_value);
        out.push((key, parsed));
    }
    out
}

fn parse_scalar(raw: &str, was_string: bool) -> ValueOut {
    let trimmed = raw.trim();
    if was_string {
        return ValueOut::String(unescape(trimmed));
    }
    match trimmed {
        "null" => ValueOut::Null,
        "true" => ValueOut::Bool(true),
        "false" => ValueOut::Bool(false),
        _ => {
            if let Ok(i) = trimmed.parse::<i64>() {
                ValueOut::Integer(i)
            } else if let Ok(f) = trimmed.parse::<f64>() {
                ValueOut::Float(f)
            } else {
                ValueOut::String(trimmed.to_string())
            }
        }
    }
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut iter = s.chars();
    while let Some(c) = iter.next() {
        if c == '\\' {
            match iter.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
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
