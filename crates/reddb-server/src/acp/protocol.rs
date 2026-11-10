//! ACP v1 newline-delimited JSON-RPC helpers.

use std::io::{self, Write};

use crate::json::{Map, Value as JsonValue};

pub fn build_result_message(id: Option<&JsonValue>, result: JsonValue) -> String {
    let mut message = Map::new();
    message.insert("id".into(), id.cloned().unwrap_or(JsonValue::Null));
    message.insert("jsonrpc".into(), JsonValue::String("2.0".into()));
    message.insert("result".into(), result);
    JsonValue::Object(message).to_string_compact()
}

pub fn build_error_message(id: Option<&JsonValue>, code: i64, message: &str) -> String {
    let mut error = Map::new();
    error.insert("code".into(), JsonValue::Integer(code));
    error.insert("message".into(), JsonValue::String(message.to_string()));

    let mut response = Map::new();
    response.insert("error".into(), JsonValue::Object(error));
    response.insert("id".into(), id.cloned().unwrap_or(JsonValue::Null));
    response.insert("jsonrpc".into(), JsonValue::String("2.0".into()));
    JsonValue::Object(response).to_string_compact()
}

pub fn build_notification(method: &str, params: JsonValue) -> String {
    let mut notification = Map::new();
    notification.insert("jsonrpc".into(), JsonValue::String("2.0".into()));
    notification.insert("method".into(), JsonValue::String(method.to_string()));
    notification.insert("params".into(), params);
    JsonValue::Object(notification).to_string_compact()
}

pub fn write_message(writer: &mut impl Write, message: &str) -> io::Result<()> {
    writer.write_all(message.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_one_json_rpc_message_per_line() {
        let mut output = Vec::new();
        write_message(&mut output, &build_result_message(None, JsonValue::Null)).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\"id\":null,\"jsonrpc\":\"2.0\",\"result\":null}\n"
        );
    }
}
