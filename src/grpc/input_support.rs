use super::*;

/// Parse a single JSON-encoded bind value string (e.g. `"42"`, `"\"alice\""`, `"null"`)
/// into a `Value` for `bind_parameterized_query`.
pub(crate) fn parse_json_bind_value(s: &str) -> Result<Value, String> {
    let json_val: JsonValue =
        json_from_str(s).map_err(|e| format!("invalid JSON bind value `{s}`: {e}"))?;
    Ok(match json_val {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(b) => Value::Boolean(b),
        JsonValue::Number(n) => {
            if n.fract() == 0.0 && n.abs() < i64::MAX as f64 {
                Value::Integer(n as i64)
            } else {
                Value::Float(n)
            }
        }
        JsonValue::String(s) => Value::Text(s),
        JsonValue::Array(_) | JsonValue::Object(_) => {
            Value::Text(json_to_string(&json_val).unwrap_or_default())
        }
    })
}

pub(crate) fn resolve_projection_payload(
    runtime: &GrpcRuntime,
    payload: &JsonValue,
) -> Result<Option<RuntimeGraphProjection>, Status> {
    let named = json_string_field(payload, "projection_name");
    let inline = crate::application::graph_payload::parse_inline_projection(payload);
    runtime
        .graph_use_cases()
        .resolve_projection(named.as_deref(), inline)
        .map_err(to_status)
}
