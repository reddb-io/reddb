use std::collections::BTreeMap;

use crate::json::{to_string as json_to_string, Value as JsonValue};

pub(crate) fn json_string_field(payload: &JsonValue, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(JsonValue::as_str)
        .map(str::to_string)
}

pub(crate) fn json_object_string_map_field(
    payload: &JsonValue,
    field: &str,
) -> Option<BTreeMap<String, String>> {
    let object = payload.get(field)?.as_object()?;
    Some(
        object
            .iter()
            .map(|(key, value)| (key.to_string(), json_metadata_value_to_string(value)))
            .collect(),
    )
}

pub(crate) fn json_metadata_value_to_string(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "null".to_string(),
        JsonValue::Bool(value) => value.to_string(),
        JsonValue::Number(value) => value.to_string(),
        JsonValue::String(value) => value.clone(),
        JsonValue::Array(_) | JsonValue::Object(_) => {
            json_to_string(value).unwrap_or_else(|_| "".to_string())
        }
    }
}

pub(crate) fn json_string_list_field(payload: &JsonValue, field: &str) -> Option<Vec<String>> {
    let values = payload.get(field)?.as_array()?;
    let out: Vec<String> = values
        .iter()
        .filter_map(JsonValue::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    (!out.is_empty()).then_some(out)
}

pub(crate) fn json_bool_field(payload: &JsonValue, field: &str) -> Option<bool> {
    payload.get(field).and_then(JsonValue::as_bool)
}

pub(crate) fn json_usize_field(payload: &JsonValue, field: &str) -> Option<usize> {
    payload
        .get(field)
        .and_then(JsonValue::as_i64)
        .and_then(|value| usize::try_from(value).ok())
}

pub(crate) fn json_f32_field(payload: &JsonValue, field: &str) -> Option<f32> {
    payload
        .get(field)
        .and_then(JsonValue::as_f64)
        .map(|value| value as f32)
}
