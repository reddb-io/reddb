use crate::{RdbFileError, RdbFileResult};
use std::collections::BTreeMap;

pub(super) fn parse_json_value(
    json: &str,
    label: &'static str,
) -> RdbFileResult<serde_json::Value> {
    serde_json::from_str(json)
        .map_err(|err| RdbFileError::InvalidOperation(format!("invalid {label}: {err}")))
}

pub(super) fn parse_json_fragment(
    label: &'static str,
    json: &str,
) -> RdbFileResult<serde_json::Value> {
    parse_json_value(json, label)
}

pub(super) fn parse_json_fragment_array(
    label: &'static str,
    fragments: &[String],
) -> RdbFileResult<serde_json::Value> {
    fragments
        .iter()
        .map(|fragment| parse_json_fragment(label, fragment))
        .collect::<RdbFileResult<Vec<_>>>()
        .map(serde_json::Value::Array)
}

pub(super) fn expect_object<'a>(
    value: &'a serde_json::Value,
    context: &'static str,
) -> RdbFileResult<&'a serde_json::Map<String, serde_json::Value>> {
    value
        .as_object()
        .ok_or_else(|| invalid(format!("{context} must be an object")))
}

pub(super) fn required<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<&'a serde_json::Value> {
    object
        .get(key)
        .ok_or_else(|| invalid(format!("missing field '{key}'")))
}

pub(super) fn required_json_fragment(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<String> {
    serde_json::to_string(required(object, key)?)
        .map_err(|err| invalid(format!("encode field '{key}' JSON fragment: {err}")))
}

pub(super) fn required_json_fragment_array(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<Vec<String>> {
    let value = required(object, key)?;
    let Some(values) = value.as_array() else {
        return Err(invalid(format!("field '{key}' must be an array")));
    };
    values
        .iter()
        .map(|value| {
            serde_json::to_string(value)
                .map_err(|err| invalid(format!("encode field '{key}' JSON fragment: {err}")))
        })
        .collect()
}

pub(super) fn optional_json_fragment_array(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<Vec<String>> {
    if object.contains_key(key) {
        required_json_fragment_array(object, key)
    } else {
        Ok(Vec::new())
    }
}

pub(super) fn optional_u64_map(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<BTreeMap<String, u64>> {
    let Some(value) = object.get(key) else {
        return Ok(BTreeMap::new());
    };
    let Some(map) = value.as_object() else {
        return Err(invalid(format!("field '{key}' must be an object")));
    };
    map.iter()
        .map(|(item_key, item_value)| Ok((item_key.clone(), json_u64_value(item_value)?)))
        .collect()
}

pub(super) fn json_string_required(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<String> {
    required(object, key)?
        .as_str()
        .map(ToString::to_string)
        .ok_or_else(|| invalid(format!("field '{key}' must be a string")))
}

pub(super) fn json_bool_required(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<bool> {
    required(object, key)?
        .as_bool()
        .ok_or_else(|| invalid(format!("field '{key}' must be a bool")))
}

pub(super) fn json_u8_required(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<u8> {
    let value = required(object, key)?;
    if let Some(text) = value.as_str() {
        return text
            .parse::<u8>()
            .map_err(|_| invalid("invalid u8 string value"));
    }
    value
        .as_u64()
        .and_then(|value| u8::try_from(value).ok())
        .ok_or_else(|| invalid("invalid u8 value"))
}

pub(super) fn json_u32_required(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<u32> {
    let value = required(object, key)?;
    if let Some(text) = value.as_str() {
        return text
            .parse::<u32>()
            .map_err(|_| invalid("invalid u32 string value"));
    }
    value
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| invalid("invalid u32 value"))
}

pub(super) fn json_u64_required(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<u64> {
    json_u64_value(required(object, key)?)
}

pub(super) fn json_u128_required(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<u128> {
    json_u128_value(required(object, key)?)
}

pub(super) fn json_usize_required(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<usize> {
    let value = required(object, key)?;
    if let Some(text) = value.as_str() {
        return text
            .parse::<usize>()
            .map_err(|_| invalid("invalid usize string value"));
    }
    value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| invalid("invalid usize value"))
}

pub(super) fn json_u64_value(value: &serde_json::Value) -> RdbFileResult<u64> {
    if let Some(text) = value.as_str() {
        return text
            .parse::<u64>()
            .map_err(|_| invalid("invalid u64 string value"));
    }
    value.as_u64().ok_or_else(|| invalid("invalid u64 value"))
}

pub(super) fn json_u128_value(value: &serde_json::Value) -> RdbFileResult<u128> {
    if let Some(text) = value.as_str() {
        return text
            .parse::<u128>()
            .map_err(|_| invalid("invalid u128 string value"));
    }
    value
        .as_u64()
        .map(u128::from)
        .ok_or_else(|| invalid("invalid u128 value"))
}

pub(super) fn json_usize_value(value: &serde_json::Value) -> RdbFileResult<usize> {
    if let Some(text) = value.as_str() {
        return text
            .parse::<usize>()
            .map_err(|_| invalid("invalid usize string value"));
    }
    value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| invalid("invalid usize value"))
}

pub(super) fn optional_string_field(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<Option<String>> {
    match object.get(key) {
        Some(serde_json::Value::String(value)) => Ok(Some(value.clone())),
        Some(serde_json::Value::Null) | None => Ok(None),
        Some(_) => Err(invalid(format!("field '{key}' must be a string or null"))),
    }
}

pub(super) fn optional_f64_field(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<Option<f64>> {
    match object.get(key) {
        Some(serde_json::Value::Number(value)) => value
            .as_f64()
            .ok_or_else(|| invalid(format!("field '{key}' must be a finite number")))
            .map(Some),
        Some(serde_json::Value::Null) | None => Ok(None),
        Some(_) => Err(invalid(format!("field '{key}' must be a number or null"))),
    }
}

pub(super) fn optional_i64_field(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<Option<i64>> {
    match object.get(key) {
        Some(serde_json::Value::Number(value)) => value
            .as_i64()
            .ok_or_else(|| invalid(format!("field '{key}' must be an integer")))
            .map(Some),
        Some(serde_json::Value::Null) | None => Ok(None),
        Some(_) => Err(invalid(format!("field '{key}' must be an integer or null"))),
    }
}

pub(super) fn optional_u8_field(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<Option<u8>> {
    match object.get(key) {
        Some(serde_json::Value::Number(value)) => value
            .as_u64()
            .and_then(|value| u8::try_from(value).ok())
            .ok_or_else(|| invalid(format!("field '{key}' must be a u8")))
            .map(Some),
        Some(serde_json::Value::Null) | None => Ok(None),
        Some(_) => Err(invalid(format!("field '{key}' must be a u8 or null"))),
    }
}

pub(super) fn optional_u64_field(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<Option<u64>> {
    match object.get(key) {
        Some(serde_json::Value::Null) | None => Ok(None),
        Some(value) => json_u64_value(value).map(Some),
    }
}

pub(super) fn optional_usize_field(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<Option<usize>> {
    match object.get(key) {
        Some(serde_json::Value::Null) | None => Ok(None),
        Some(value) => json_usize_value(value).map(Some),
    }
}

pub(super) fn json_u64(value: u64) -> serde_json::Value {
    serde_json::Value::String(value.to_string())
}

pub(super) fn json_u128(value: u128) -> serde_json::Value {
    serde_json::Value::String(value.to_string())
}

pub(super) fn json_usize(value: usize) -> serde_json::Value {
    serde_json::Value::Number((value as u64).into())
}

pub(super) fn optional_f64_json(value: Option<f64>) -> serde_json::Value {
    value
        .and_then(serde_json::Number::from_f64)
        .map(serde_json::Value::Number)
        .unwrap_or(serde_json::Value::Null)
}

pub(super) fn optional_u8_json(value: Option<u8>) -> serde_json::Value {
    value
        .map(serde_json::Value::from)
        .unwrap_or(serde_json::Value::Null)
}

pub(super) fn optional_u64_json(value: Option<u64>) -> serde_json::Value {
    value.map(json_u64).unwrap_or(serde_json::Value::Null)
}

pub(super) fn optional_usize_json(value: Option<usize>) -> serde_json::Value {
    value.map(json_usize).unwrap_or(serde_json::Value::Null)
}

pub(super) fn optional_string_json(value: Option<&String>) -> serde_json::Value {
    value
        .cloned()
        .map(serde_json::Value::String)
        .unwrap_or(serde_json::Value::Null)
}

pub(super) fn string_array_json(values: &[String]) -> serde_json::Value {
    serde_json::Value::Array(
        values
            .iter()
            .cloned()
            .map(serde_json::Value::String)
            .collect(),
    )
}

pub(super) fn string_array_from_json(value: Option<&serde_json::Value>) -> Option<Vec<String>> {
    value.and_then(serde_json::Value::as_array).map(|values| {
        values
            .iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect()
    })
}

pub(super) fn physical_array_from_json<T>(
    value: Option<&serde_json::Value>,
    item_from_json: fn(&serde_json::Value) -> RdbFileResult<T>,
) -> RdbFileResult<Vec<T>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Some(values) = value.as_array() else {
        return Err(invalid("physical array field must be an array"));
    };
    values.iter().map(item_from_json).collect()
}

pub(super) fn invalid(message: impl Into<String>) -> RdbFileError {
    RdbFileError::InvalidOperation(message.into())
}
