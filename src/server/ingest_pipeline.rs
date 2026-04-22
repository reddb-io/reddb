//! JSON-first ingest pipeline — autodetects JSON-array bodies,
//! NDJSON streams, and envelope-style payloads, then hands the
//! rows over to the storage layer as a `Vec<HashMap<String, Value>>`.
//!
//! The goal is ergonomic JSON ingest without forcing callers into
//! the SQL path. Log shippers (Vector / Fluent Bit / custom
//! collectors), browser agents, and mobile clients all want to
//! `POST /ingest/{collection}` with a JSON body and get a compact
//! ack back — not synthesise INSERT statements.
//!
//! # Supported shapes
//!
//! * **JSON array**: `[{...}, {...}]` — bulk call, one HTTP request.
//!   Already served by the existing `/collections/{name}/bulk/rows`;
//!   re-implemented here for parity + uniform error model.
//! * **NDJSON** (newline-delimited): `{...}\n{...}\n` — pipeable,
//!   streaming-friendly, no outer brackets. Content-Type
//!   `application/x-ndjson` triggers this path.
//! * **Envelope**: `{"rows":[{...},{...}], "ts_field":"ts"}` —
//!   self-describing, lets the caller override per-request settings
//!   without changing the collection schema.
//! * **Single object**: `{...}` — degrades to a 1-row bulk. Dumb
//!   convenience so curl smoke-tests don't need an array wrapper.
//!
//! # Streaming
//!
//! [`IngestSession`] is the streaming entry point. Feed it byte
//! chunks as they arrive (HTTP chunked reads, WebSocket frames,
//! WAL tail), and it emits rows as soon as each newline-delimited
//! record completes. The session tolerates split-at-any-byte
//! chunks without losing state.

use std::collections::HashMap;

use crate::json::{parse_json, Map, Value as JsonValue};
use crate::storage::schema::Value;

/// One parsed row ready for the storage batch path.
pub type IngestRow = HashMap<String, Value>;

/// Content-Type hints the caller may pass. `Auto` inspects the
/// first non-whitespace byte to pick between JSON-array / object /
/// NDJSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestContentType {
    Auto,
    JsonArray,
    JsonObject,
    NdJson,
}

impl IngestContentType {
    /// Map a Content-Type header value onto the enum. Unknown /
    /// missing types fall back to `Auto`.
    pub fn from_header(header: Option<&str>) -> Self {
        let Some(h) = header else {
            return IngestContentType::Auto;
        };
        let h = h
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match h.as_str() {
            "application/x-ndjson" | "application/ndjson" | "text/ndjson" => {
                IngestContentType::NdJson
            }
            "application/json" => IngestContentType::Auto,
            _ => IngestContentType::Auto,
        }
    }
}

/// Outcome of a parse pass.
#[derive(Debug, Clone, Default)]
pub struct IngestReport {
    pub rows: Vec<IngestRow>,
    /// 1-based line numbers (for NDJSON) or array indices (for
    /// JSON arrays) that failed to parse, paired with the reason.
    pub failed: Vec<(usize, String)>,
}

impl IngestReport {
    pub fn total(&self) -> usize {
        self.rows.len() + self.failed.len()
    }

    pub fn accepted(&self) -> usize {
        self.rows.len()
    }

    pub fn rejected(&self) -> usize {
        self.failed.len()
    }

    pub fn ok(&self) -> bool {
        self.failed.is_empty()
    }
}

/// One-shot parser — call this from a regular request handler.
/// Reads the entire body up-front; fine for payloads up to a few
/// MiB. For larger / truly streaming sources use [`IngestSession`].
pub fn parse_body(body: &[u8], hint: IngestContentType) -> IngestReport {
    if matches!(hint, IngestContentType::NdJson) {
        return parse_ndjson(body);
    }
    // Autodetect: inspect first non-whitespace byte.
    let first = body.iter().copied().find(|b| !b.is_ascii_whitespace());
    match first {
        Some(b'[') => parse_json_array(body),
        Some(b'{') => parse_json_object_or_envelope(body),
        Some(b'\n') | Some(b'\r') | None => IngestReport::default(),
        _ => {
            // Could be NDJSON starting with a literal like `true`
            // or a number — try NDJSON as a last resort.
            parse_ndjson(body)
        }
    }
}

fn parse_json_array(body: &[u8]) -> IngestReport {
    let Ok(parsed) = parse_json(std::str::from_utf8(body).unwrap_or("")) else {
        let mut report = IngestReport::default();
        report
            .failed
            .push((0, "body is not valid JSON".to_string()));
        return report;
    };
    let value = JsonValue::from(parsed);
    let Some(arr) = value.as_array() else {
        let mut report = IngestReport::default();
        report
            .failed
            .push((0, "JSON root is not an array".to_string()));
        return report;
    };
    let mut report = IngestReport::default();
    for (idx, item) in arr.iter().enumerate() {
        match decode_row(item) {
            Ok(row) => report.rows.push(row),
            Err(msg) => report.failed.push((idx, msg)),
        }
    }
    report
}

fn parse_json_object_or_envelope(body: &[u8]) -> IngestReport {
    let Ok(parsed) = parse_json(std::str::from_utf8(body).unwrap_or("")) else {
        let mut report = IngestReport::default();
        report
            .failed
            .push((0, "body is not valid JSON".to_string()));
        return report;
    };
    let value = JsonValue::from(parsed);
    let Some(obj) = value.as_object() else {
        let mut report = IngestReport::default();
        report
            .failed
            .push((0, "JSON root is not an object".to_string()));
        return report;
    };
    // Envelope: { "rows": [...] } — optionally with metadata.
    if let Some(rows) = obj.get("rows") {
        if let Some(arr) = rows.as_array() {
            let mut report = IngestReport::default();
            for (idx, item) in arr.iter().enumerate() {
                match decode_row(item) {
                    Ok(row) => report.rows.push(row),
                    Err(msg) => report.failed.push((idx, msg)),
                }
            }
            return report;
        }
    }
    // Fallback: treat the object as a single row.
    match decode_row(&value) {
        Ok(row) => {
            let mut report = IngestReport::default();
            report.rows.push(row);
            report
        }
        Err(msg) => {
            let mut report = IngestReport::default();
            report.failed.push((0, msg));
            report
        }
    }
}

fn parse_ndjson(body: &[u8]) -> IngestReport {
    let text = std::str::from_utf8(body).unwrap_or("");
    let mut report = IngestReport::default();
    for (line_idx, raw) in text.lines().enumerate() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue; // allow blank lines + comment-style pipes
        }
        match parse_json(trimmed) {
            Ok(parsed) => {
                let value = JsonValue::from(parsed);
                match decode_row(&value) {
                    Ok(row) => report.rows.push(row),
                    Err(msg) => report.failed.push((line_idx + 1, msg)),
                }
            }
            Err(err) => {
                report.failed.push((line_idx + 1, err));
            }
        }
    }
    report
}

fn decode_row(value: &JsonValue) -> Result<IngestRow, String> {
    let Some(obj) = value.as_object() else {
        return Err(format!(
            "expected JSON object per row, got {}",
            json_type_name(value)
        ));
    };
    let mut row = HashMap::with_capacity(obj.len());
    for (k, v) in obj {
        row.insert(k.clone(), json_to_value(v));
    }
    Ok(row)
}

fn json_type_name(v: &JsonValue) -> &'static str {
    match v {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "bool",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    }
}

fn json_to_value(v: &JsonValue) -> Value {
    match v {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(b) => Value::Boolean(*b),
        JsonValue::Number(n) => {
            // Preserve integer-ness when the number has no fraction —
            // downstream codecs pick T64 / Delta on i64 columns and
            // would lose compression if we widened everything to f64.
            if n.fract() == 0.0 && n.abs() <= i64::MAX as f64 {
                Value::Integer(*n as i64)
            } else {
                Value::Float(*n)
            }
        }
        JsonValue::String(s) => Value::text(s.clone()),
        JsonValue::Array(arr) => Value::Array(arr.iter().map(json_to_value).collect()),
        JsonValue::Object(_) => Value::text(v.to_string_compact()),
    }
}

// ============================================================================
// Streaming session — chunked / WebSocket / NDJSON-over-time source.
// ============================================================================

/// Incremental NDJSON parser. Feed arbitrary byte chunks via
/// [`Self::feed`]; it buffers across chunk boundaries and returns
/// every complete row as soon as the trailing newline arrives.
///
/// Typical flow:
///
/// ```
/// # use reddb::server::ingest_pipeline::IngestSession;
/// let mut session = IngestSession::new();
/// let rows1 = session.feed(b"{\"ts\":1,\"msg\":\"a\"}\n{\"ts\":");
/// assert_eq!(rows1.rows.len(), 1);
///
/// let rows2 = session.feed(b"2,\"msg\":\"b\"}\n");
/// assert_eq!(rows2.rows.len(), 1);
/// // Flush at the end to surface any trailing (newline-less) line.
/// let _tail = session.finish();
/// ```
#[derive(Debug, Default)]
pub struct IngestSession {
    buffer: Vec<u8>,
    line_counter: usize,
    total_accepted: usize,
    total_rejected: usize,
}

impl IngestSession {
    pub fn new() -> Self {
        Self::default()
    }

    /// Accept raw bytes; emit complete rows. The returned report
    /// only contains rows / failures for newline-terminated lines
    /// observed in this call. The buffered tail is kept for the
    /// next call.
    pub fn feed(&mut self, chunk: &[u8]) -> IngestReport {
        self.buffer.extend_from_slice(chunk);
        let mut report = IngestReport::default();
        // Drain complete lines (terminated by `\n`).
        loop {
            let Some(newline) = self.buffer.iter().position(|b| *b == b'\n') else {
                break;
            };
            let line_bytes: Vec<u8> = self.buffer.drain(..=newline).collect();
            // `line_bytes` includes the trailing `\n` (and maybe `\r`).
            let line_str = std::str::from_utf8(&line_bytes)
                .unwrap_or("")
                .trim_end_matches(&['\r', '\n'][..])
                .trim();
            self.line_counter += 1;
            if line_str.is_empty() || line_str.starts_with('#') {
                continue;
            }
            match parse_json(line_str) {
                Ok(parsed) => {
                    let value = JsonValue::from(parsed);
                    match decode_row(&value) {
                        Ok(row) => {
                            report.rows.push(row);
                            self.total_accepted += 1;
                        }
                        Err(msg) => {
                            report.failed.push((self.line_counter, msg));
                            self.total_rejected += 1;
                        }
                    }
                }
                Err(err) => {
                    report.failed.push((self.line_counter, err));
                    self.total_rejected += 1;
                }
            }
        }
        report
    }

    /// Flush any buffered line (last record without trailing `\n`).
    /// Call once when the source closes.
    pub fn finish(&mut self) -> IngestReport {
        if self.buffer.is_empty() {
            return IngestReport::default();
        }
        let tail = std::mem::take(&mut self.buffer);
        let tail_with_newline: Vec<u8> = tail.into_iter().chain(std::iter::once(b'\n')).collect();
        self.feed(&tail_with_newline)
    }

    pub fn total_accepted(&self) -> usize {
        self.total_accepted
    }

    pub fn total_rejected(&self) -> usize {
        self.total_rejected
    }

    pub fn buffered_bytes(&self) -> usize {
        self.buffer.len()
    }
}

// ============================================================================
// Ack payload helpers — callers render these to JSON responses.
// ============================================================================

/// Canonical response body shape. Kept in one place so every
/// transport (HTTP bulk, HTTP NDJSON, WebSocket frame, gRPC) speaks
/// the same ack contract.
pub fn ack_payload(accepted: usize, rejected: usize, failures: &[(usize, String)]) -> JsonValue {
    let mut obj = Map::new();
    obj.insert("ok".to_string(), JsonValue::Bool(rejected == 0));
    obj.insert("accepted".to_string(), JsonValue::Number(accepted as f64));
    obj.insert("rejected".to_string(), JsonValue::Number(rejected as f64));
    if !failures.is_empty() {
        let details: Vec<JsonValue> = failures
            .iter()
            .map(|(line, reason)| {
                let mut o = Map::new();
                o.insert("line".to_string(), JsonValue::Number(*line as f64));
                o.insert("error".to_string(), JsonValue::String(reason.clone()));
                JsonValue::Object(o)
            })
            .collect();
        obj.insert("failures".to_string(), JsonValue::Array(details));
    }
    JsonValue::Object(obj)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn val_text(v: &Value) -> Option<&str> {
        match v {
            Value::Text(s) => Some(s.as_ref()),
            _ => None,
        }
    }

    fn val_int(v: &Value) -> Option<i64> {
        match v {
            Value::Integer(n) => Some(*n),
            _ => None,
        }
    }

    #[test]
    fn content_type_detection_recognises_ndjson_variants() {
        assert_eq!(
            IngestContentType::from_header(Some("application/x-ndjson")),
            IngestContentType::NdJson
        );
        assert_eq!(
            IngestContentType::from_header(Some("application/ndjson; charset=utf-8")),
            IngestContentType::NdJson
        );
        assert_eq!(
            IngestContentType::from_header(Some("application/json")),
            IngestContentType::Auto
        );
        assert_eq!(
            IngestContentType::from_header(None),
            IngestContentType::Auto
        );
    }

    #[test]
    fn parse_json_array_body() {
        let body = br#"[
            {"ts": 1, "msg": "hi"},
            {"ts": 2, "msg": "world"}
        ]"#;
        let report = parse_body(body, IngestContentType::Auto);
        assert_eq!(report.accepted(), 2);
        assert_eq!(report.rejected(), 0);
        assert_eq!(val_int(&report.rows[0]["ts"]), Some(1));
        assert_eq!(val_text(&report.rows[0]["msg"]), Some("hi"));
    }

    #[test]
    fn parse_single_object_body() {
        let body = br#"{"ts": 42, "msg": "alone"}"#;
        let report = parse_body(body, IngestContentType::Auto);
        assert_eq!(report.accepted(), 1);
        assert_eq!(val_int(&report.rows[0]["ts"]), Some(42));
    }

    #[test]
    fn parse_envelope_with_rows_key() {
        let body = br#"{
            "collection": "events",
            "rows": [
                {"ts": 1},
                {"ts": 2},
                {"ts": 3}
            ]
        }"#;
        let report = parse_body(body, IngestContentType::Auto);
        assert_eq!(report.accepted(), 3);
    }

    #[test]
    fn parse_ndjson_body() {
        let body = b"{\"ts\":1}\n{\"ts\":2}\n{\"ts\":3}\n";
        let report = parse_body(body, IngestContentType::NdJson);
        assert_eq!(report.accepted(), 3);
        assert_eq!(report.rejected(), 0);
    }

    #[test]
    fn parse_ndjson_tolerates_blank_and_comment_lines() {
        let body = b"\n# comment\n{\"ts\":1}\n\n# another\n{\"ts\":2}\n";
        let report = parse_body(body, IngestContentType::NdJson);
        assert_eq!(report.accepted(), 2);
        assert_eq!(report.rejected(), 0);
    }

    #[test]
    fn parse_ndjson_reports_per_line_failures() {
        let body = b"{\"ts\":1}\nnot-json\n{\"ts\":3}\n";
        let report = parse_body(body, IngestContentType::NdJson);
        assert_eq!(report.accepted(), 2);
        assert_eq!(report.rejected(), 1);
        assert_eq!(report.failed[0].0, 2);
    }

    #[test]
    fn parse_array_item_that_is_not_an_object_is_rejected_with_index() {
        let body = br#"[{"ts":1}, 42, {"ts":3}]"#;
        let report = parse_body(body, IngestContentType::Auto);
        assert_eq!(report.accepted(), 2);
        assert_eq!(report.rejected(), 1);
        assert_eq!(report.failed[0].0, 1); // zero-based array index
    }

    #[test]
    fn numbers_preserve_integer_precision() {
        let body = br#"[{"a": 42, "b": 3.14, "c": 9999999999}]"#;
        let report = parse_body(body, IngestContentType::Auto);
        let row = &report.rows[0];
        assert_eq!(val_int(&row["a"]), Some(42));
        assert!(matches!(row["b"], Value::Float(_)));
        assert_eq!(val_int(&row["c"]), Some(9999999999));
    }

    #[test]
    fn nested_object_flattens_to_text_by_default() {
        let body = br#"[{"payload": {"nested": "value"}}]"#;
        let report = parse_body(body, IngestContentType::Auto);
        let row = &report.rows[0];
        // Nested objects are serialised compactly so they round-trip
        // through schemas that don't know about nested docs yet.
        let text = val_text(&row["payload"]).unwrap_or("");
        assert!(text.contains("nested"));
    }

    #[test]
    fn session_emits_rows_as_newlines_arrive() {
        let mut s = IngestSession::new();
        let a = s.feed(b"{\"ts\":1}\n{\"ts\":");
        assert_eq!(a.accepted(), 1);
        assert_eq!(a.rejected(), 0);
        let b = s.feed(b"2}\n");
        assert_eq!(b.accepted(), 1);
        let end = s.finish();
        assert_eq!(end.accepted(), 0);
        assert_eq!(s.total_accepted(), 2);
    }

    #[test]
    fn session_flushes_trailing_line_without_newline_on_finish() {
        let mut s = IngestSession::new();
        s.feed(b"{\"ts\":1}\n");
        let end = s.finish();
        assert_eq!(end.accepted(), 0); // no buffered tail
        s.feed(b"{\"ts\":2}");
        // With no newline, the 2nd row isn't emitted yet.
        assert_eq!(s.total_accepted(), 1);
        let end = s.finish();
        assert_eq!(end.accepted(), 1);
        assert_eq!(s.total_accepted(), 2);
    }

    #[test]
    fn session_carries_failures_with_cumulative_line_numbers() {
        let mut s = IngestSession::new();
        s.feed(b"{\"ts\":1}\nbroken\n");
        s.feed(b"{\"ts\":3}\nalso broken\n");
        assert_eq!(s.total_accepted(), 2);
        assert_eq!(s.total_rejected(), 2);
    }

    #[test]
    fn session_tolerates_crlf_endings() {
        let mut s = IngestSession::new();
        s.feed(b"{\"ts\":1}\r\n{\"ts\":2}\r\n");
        assert_eq!(s.total_accepted(), 2);
    }

    #[test]
    fn ack_payload_shape_is_stable() {
        let ack = ack_payload(5, 0, &[]);
        let obj = ack.as_object().unwrap();
        assert!(obj.contains_key("ok"));
        assert!(obj.contains_key("accepted"));
        assert!(obj.contains_key("rejected"));
        assert!(!obj.contains_key("failures"));
    }

    #[test]
    fn ack_payload_includes_failure_details_when_present() {
        let ack = ack_payload(2, 1, &[(3, "broken JSON".to_string())]);
        let raw = ack.to_string_compact();
        assert!(raw.contains("\"ok\":false"));
        assert!(raw.contains("\"rejected\":1"));
        assert!(raw.contains("broken JSON"));
        assert!(raw.contains("\"line\":3"));
    }
}
