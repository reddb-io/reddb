//! JSON-RPC protocol handling for MCP server.
//!
//! Implements the Content-Length framed JSON-RPC transport used by the
//! Model Context Protocol over stdio.

use crate::json::{Map, Value as JsonValue};
use std::io::{BufRead, Write};

/// Read a JSON-RPC payload from a buffered reader.
///
/// Reads headers until it finds `Content-Length: N`, then reads exactly N
/// bytes of body. Returns `None` on EOF and `Err` on malformed input.
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

        let trimmed = header.trim_end_matches(|c| matches!(c, '\n' | '\r'));
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
    write!(
        writer,
        "Content-Length: {}\r\n\r\n{}",
        body.as_bytes().len(),
        body
    )
    .map_err(|e| format!("failed to write response: {}", e))?;
    writer
        .flush()
        .map_err(|e| format!("failed to flush: {}", e))
}

/// Build a JSON-RPC 2.0 result message.
pub fn build_result_message(id: Option<&JsonValue>, result: JsonValue) -> String {
    let mut object = Map::new();
    object.insert("jsonrpc".to_string(), JsonValue::String("2.0".to_string()));
    match id {
        Some(identifier) => {
            object.insert("id".to_string(), identifier.clone());
        }
        None => {
            object.insert("id".to_string(), JsonValue::Null);
        }
    }
    object.insert("result".to_string(), result);
    JsonValue::Object(object).to_string_compact()
}

/// Build a JSON-RPC 2.0 error message.
pub fn build_error_message(id: Option<&JsonValue>, code: i64, message: &str) -> String {
    let mut error = Map::new();
    error.insert("code".to_string(), JsonValue::Number(code as f64));
    error.insert(
        "message".to_string(),
        JsonValue::String(message.to_string()),
    );

    let mut object = Map::new();
    object.insert("jsonrpc".to_string(), JsonValue::String("2.0".to_string()));
    match id {
        Some(identifier) => {
            object.insert("id".to_string(), identifier.clone());
        }
        None => {
            object.insert("id".to_string(), JsonValue::Null);
        }
    }
    object.insert("error".to_string(), JsonValue::Object(error));
    JsonValue::Object(object).to_string_compact()
}

/// Build a JSON-RPC 2.0 notification (no id, no response expected).
pub fn build_notification(method: &str, params: JsonValue) -> String {
    let mut object = Map::new();
    object.insert("jsonrpc".to_string(), JsonValue::String("2.0".to_string()));
    object.insert("method".to_string(), JsonValue::String(method.to_string()));
    object.insert("params".to_string(), params);
    JsonValue::Object(object).to_string_compact()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::from_str;

    #[test]
    fn test_build_result_message() {
        let id = JsonValue::Number(1.0);
        let result = JsonValue::Bool(true);
        let msg = build_result_message(Some(&id), result);
        let parsed: JsonValue = from_str(&msg).unwrap();
        assert_eq!(parsed.get("jsonrpc").and_then(|v| v.as_str()), Some("2.0"));
        assert_eq!(parsed.get("id").and_then(|v| v.as_f64()), Some(1.0));
    }

    #[test]
    fn test_build_error_message() {
        let id = JsonValue::Number(2.0);
        let msg = build_error_message(Some(&id), -32601, "method not found");
        let parsed: JsonValue = from_str(&msg).unwrap();
        assert_eq!(parsed.get("jsonrpc").and_then(|v| v.as_str()), Some("2.0"));
        let error = parsed.get("error").unwrap();
        assert_eq!(error.get("code").and_then(|v| v.as_f64()), Some(-32601.0));
        assert_eq!(
            error.get("message").and_then(|v| v.as_str()),
            Some("method not found")
        );
    }

    #[test]
    fn test_build_notification() {
        let msg = build_notification("test/event", JsonValue::Null);
        let parsed: JsonValue = from_str(&msg).unwrap();
        assert_eq!(
            parsed.get("method").and_then(|v| v.as_str()),
            Some("test/event")
        );
        assert!(parsed.get("id").is_none());
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
}
