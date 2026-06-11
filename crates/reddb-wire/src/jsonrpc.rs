//! Content-Length framed JSON-RPC 2.0 codec.
//!
//! This is the message framing used by JSON-RPC over stdio transports such as
//! the Model Context Protocol (MCP): each message is prefixed with a
//! `Content-Length: N\r\n\r\n` header followed by exactly `N` bytes of UTF-8
//! JSON body.
//!
//! The framing codec ([`read_payload`], [`write_message`]) is serializer
//! agnostic — it operates on raw strings. The JSON-RPC 2.0 envelope builders
//! ([`build_result_message`], [`build_error_message`], [`build_notification`])
//! are parameterized over a [`JsonRpcSerializer`] so this crate does not depend
//! on any concrete JSON value type; the caller binds its own serializer.

use std::io::{BufRead, Write};

/// Read a JSON-RPC payload from a buffered reader.
///
/// Reads headers until it finds `Content-Length: N`, then reads exactly N
/// bytes of body. Returns `None` on EOF and `Err` on malformed input.
///
/// Behavioral contract (must be preserved by any caller relying on it):
/// the `Content-Length` header match is case-insensitive, the body is read
/// with `read_exact` for exactly N bytes, and a single optional trailing
/// `\r\n` (or bare `\n`) between messages is consumed.
pub fn read_payload<R: BufRead>(reader: &mut R) -> Result<Option<String>, String> {
    let mut content_length: Option<usize> = None;
    let mut header = String::new();

    loop {
        header.clear();
        let bytes = reader
            .read_line(&mut header)
            .map_err(|e| format!("failed to read header: {}", e))?;
        if bytes == 0 {
            return Ok(None);
        }

        let trimmed = header.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            break;
        }

        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("content-length:") {
            let value = trimmed["Content-Length:".len()..].trim();
            let length = value
                .parse::<usize>()
                .map_err(|_| "invalid Content-Length header".to_string())?;
            content_length = Some(length);
        }
    }

    let length = content_length.ok_or_else(|| "missing Content-Length header".to_string())?;
    let mut buffer = vec![0u8; length];
    reader
        .read_exact(&mut buffer)
        .map_err(|e| format!("failed to read payload: {}", e))?;

    // Consume optional trailing newline between messages.
    if let Ok(buf) = reader.fill_buf() {
        let to_consume = if buf.starts_with(b"\r\n") {
            Some(2)
        } else if buf.starts_with(b"\n") {
            Some(1)
        } else {
            None
        };
        if let Some(count) = to_consume {
            reader.consume(count);
        }
    }

    String::from_utf8(buffer)
        .map(Some)
        .map_err(|_| "payload is not UTF-8".to_string())
}

/// Write a Content-Length framed JSON-RPC message to a writer.
pub fn write_message<W: Write>(writer: &mut W, body: &str) -> Result<(), String> {
    write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)
        .map_err(|e| format!("failed to write response: {}", e))?;
    writer
        .flush()
        .map_err(|e| format!("failed to flush: {}", e))
}

/// Abstraction over a JSON serializer so the JSON-RPC envelope builders do not
/// depend on a concrete JSON value type.
///
/// The caller binds this to its own JSON value type. The envelope builders only
/// require constructing nulls, strings, integer numbers, and objects, plus
/// compact serialization. Object key ordering in the emitted bytes is entirely
/// determined by the implementor's [`object`](JsonRpcSerializer::object) and
/// [`to_compact_string`](JsonRpcSerializer::to_compact_string).
pub trait JsonRpcSerializer {
    /// The JSON value type produced by this serializer.
    type Value: Clone;

    /// A JSON `null`.
    fn null() -> Self::Value;

    /// A JSON string.
    fn string(value: &str) -> Self::Value;

    /// A JSON number from a signed integer.
    fn number(value: i64) -> Self::Value;

    /// A JSON object built from the given key/value entries, in order.
    fn object(entries: Vec<(&'static str, Self::Value)>) -> Self::Value;

    /// Serialize a value to a compact (whitespace-free) JSON string.
    fn to_compact_string(value: &Self::Value) -> String;
}

/// Build a JSON-RPC 2.0 result message.
///
/// A `None` id is encoded as JSON `null`, matching the original transport.
pub fn build_result_message<S: JsonRpcSerializer>(
    id: Option<&S::Value>,
    result: S::Value,
) -> String {
    let id_value = match id {
        Some(identifier) => identifier.clone(),
        None => S::null(),
    };
    let object = S::object(vec![
        ("jsonrpc", S::string("2.0")),
        ("id", id_value),
        ("result", result),
    ]);
    S::to_compact_string(&object)
}

/// Build a JSON-RPC 2.0 error message.
///
/// A `None` id is encoded as JSON `null`, matching the original transport.
pub fn build_error_message<S: JsonRpcSerializer>(
    id: Option<&S::Value>,
    code: i64,
    message: &str,
) -> String {
    let error = S::object(vec![
        ("code", S::number(code)),
        ("message", S::string(message)),
    ]);
    let id_value = match id {
        Some(identifier) => identifier.clone(),
        None => S::null(),
    };
    let object = S::object(vec![
        ("jsonrpc", S::string("2.0")),
        ("id", id_value),
        ("error", error),
    ]);
    S::to_compact_string(&object)
}

/// Build a JSON-RPC 2.0 notification (no id, no response expected).
pub fn build_notification<S: JsonRpcSerializer>(method: &str, params: S::Value) -> String {
    let object = S::object(vec![
        ("jsonrpc", S::string("2.0")),
        ("method", S::string(method)),
        ("params", params),
    ]);
    S::to_compact_string(&object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Minimal self-contained JSON value + serializer for the framing tests.
    ///
    /// `Object` is backed by a `BTreeMap`, so it sorts keys on serialization —
    /// mirroring `reddb-server`'s production serializer, which is what pins the
    /// emitted field order.
    #[derive(Clone)]
    enum TestJson {
        Null,
        Number(i64),
        Str(String),
        Object(BTreeMap<String, TestJson>),
    }

    struct TestSerializer;

    impl JsonRpcSerializer for TestSerializer {
        type Value = TestJson;

        fn null() -> TestJson {
            TestJson::Null
        }
        fn string(value: &str) -> TestJson {
            TestJson::Str(value.to_string())
        }
        fn number(value: i64) -> TestJson {
            TestJson::Number(value)
        }
        fn object(entries: Vec<(&'static str, TestJson)>) -> TestJson {
            let mut map = BTreeMap::new();
            for (key, value) in entries {
                map.insert(key.to_string(), value);
            }
            TestJson::Object(map)
        }
        fn to_compact_string(value: &TestJson) -> String {
            let mut out = String::new();
            write(value, &mut out);
            out
        }
    }

    fn write(value: &TestJson, out: &mut String) {
        match value {
            TestJson::Null => out.push_str("null"),
            TestJson::Number(n) => out.push_str(&n.to_string()),
            TestJson::Str(s) => {
                out.push('"');
                out.push_str(s);
                out.push('"');
            }
            TestJson::Object(map) => {
                out.push('{');
                for (idx, (key, value)) in map.iter().enumerate() {
                    if idx > 0 {
                        out.push(',');
                    }
                    out.push('"');
                    out.push_str(key);
                    out.push('"');
                    out.push(':');
                    write(value, out);
                }
                out.push('}');
            }
        }
    }

    #[test]
    fn test_read_payload_basic() {
        let body = r#"{"id":1}"#;
        let msg = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut reader = std::io::BufReader::new(msg.as_bytes());
        let payload = read_payload(&mut reader).unwrap();
        assert_eq!(payload, Some(body.to_string()));
    }

    #[test]
    fn test_read_payload_eof() {
        let input = b"";
        let mut reader = std::io::BufReader::new(&input[..]);
        let payload = read_payload(&mut reader).unwrap();
        assert!(payload.is_none());
    }

    #[test]
    fn test_read_payload_case_insensitive_header() {
        let body = r#"{"ok":true}"#;
        let msg = format!("content-LENGTH: {}\r\n\r\n{}", body.len(), body);
        let mut reader = std::io::BufReader::new(msg.as_bytes());
        let payload = read_payload(&mut reader).unwrap();
        assert_eq!(payload, Some(body.to_string()));
    }

    #[test]
    fn test_read_payload_consumes_trailing_newline_and_reads_next() {
        // Two back-to-back messages separated by a bare "\n": the trailing
        // newline after the first body must be consumed so the second frame
        // parses cleanly.
        let first = r#"{"n":1}"#;
        let second = r#"{"n":2}"#;
        let msg = format!(
            "Content-Length: {}\r\n\r\n{}\nContent-Length: {}\r\n\r\n{}",
            first.len(),
            first,
            second.len(),
            second
        );
        let mut reader = std::io::BufReader::new(msg.as_bytes());
        assert_eq!(read_payload(&mut reader).unwrap(), Some(first.to_string()));
        assert_eq!(read_payload(&mut reader).unwrap(), Some(second.to_string()));
    }

    #[test]
    fn test_write_message_framing_roundtrip() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":true}"#;
        let mut buffer = Vec::new();
        write_message(&mut buffer, body).unwrap();
        let written = String::from_utf8(buffer).unwrap();
        assert_eq!(
            written,
            format!("Content-Length: {}\r\n\r\n{}", body.len(), body)
        );

        // Frame survives a read_payload round-trip.
        let mut reader = std::io::BufReader::new(written.as_bytes());
        assert_eq!(read_payload(&mut reader).unwrap(), Some(body.to_string()));
    }

    #[test]
    fn test_build_result_message_field_order() {
        let id = TestJson::Number(1);
        let msg =
            build_result_message::<TestSerializer>(Some(&id), TestJson::Str("ok".to_string()));
        // BTreeMap sorts keys: id, jsonrpc, result.
        assert_eq!(msg, r#"{"id":1,"jsonrpc":"2.0","result":"ok"}"#);
    }

    #[test]
    fn test_build_result_message_null_id() {
        let msg = build_result_message::<TestSerializer>(None, TestJson::Null);
        assert_eq!(msg, r#"{"id":null,"jsonrpc":"2.0","result":null}"#);
    }

    #[test]
    fn test_build_error_message_field_order() {
        let id = TestJson::Number(2);
        let msg = build_error_message::<TestSerializer>(Some(&id), -32601, "method not found");
        assert_eq!(
            msg,
            r#"{"error":{"code":-32601,"message":"method not found"},"id":2,"jsonrpc":"2.0"}"#
        );
    }

    #[test]
    fn test_build_notification_field_order() {
        let msg = build_notification::<TestSerializer>("test/event", TestJson::Null);
        assert_eq!(
            msg,
            r#"{"jsonrpc":"2.0","method":"test/event","params":null}"#
        );
    }
}
