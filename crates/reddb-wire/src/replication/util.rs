use serde_json::Value as JsonValue;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplicationPayloadError {
    NotJson,
    NotObject,
    MissingField(&'static str),
    InvalidField(&'static str),
    InvalidHex(&'static str),
}

impl std::fmt::Display for ReplicationPayloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotJson => write!(f, "replication payload must be JSON"),
            Self::NotObject => write!(f, "replication payload must be a JSON object"),
            Self::MissingField(field) => write!(f, "missing replication field {field}"),
            Self::InvalidField(field) => write!(f, "invalid replication field {field}"),
            Self::InvalidHex(field) => write!(f, "invalid hex field {field}"),
        }
    }
}

impl std::error::Error for ReplicationPayloadError {}

pub(crate) type Result<T> = std::result::Result<T, ReplicationPayloadError>;

pub(crate) fn object_from_slice(bytes: &[u8]) -> Result<serde_json::Map<String, JsonValue>> {
    match serde_json::from_slice(bytes).map_err(|_| ReplicationPayloadError::NotJson)? {
        JsonValue::Object(obj) => Ok(obj),
        _ => Err(ReplicationPayloadError::NotObject),
    }
}

pub(crate) fn get_u64(
    obj: &serde_json::Map<String, JsonValue>,
    field: &'static str,
) -> Result<u64> {
    obj.get(field)
        .and_then(JsonValue::as_u64)
        .ok_or(ReplicationPayloadError::MissingField(field))
}

pub(crate) fn get_opt_u64(obj: &serde_json::Map<String, JsonValue>, field: &str) -> Option<u64> {
    obj.get(field).and_then(JsonValue::as_u64)
}

pub(crate) fn get_bool_default(
    obj: &serde_json::Map<String, JsonValue>,
    field: &str,
    default: bool,
) -> bool {
    obj.get(field)
        .and_then(JsonValue::as_bool)
        .unwrap_or(default)
}

pub(crate) fn get_string(
    obj: &serde_json::Map<String, JsonValue>,
    field: &'static str,
) -> Result<String> {
    obj.get(field)
        .and_then(JsonValue::as_str)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .ok_or(ReplicationPayloadError::MissingField(field))
}

pub(crate) fn get_opt_string(
    obj: &serde_json::Map<String, JsonValue>,
    field: &str,
) -> Option<String> {
    obj.get(field)
        .and_then(JsonValue::as_str)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const ALPHA: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(ALPHA[(byte >> 4) as usize] as char);
        out.push(ALPHA[(byte & 0x0f) as usize] as char);
    }
    out
}

pub(crate) fn hex_decode(field: &'static str, value: &str) -> Result<Vec<u8>> {
    let bytes = value.as_bytes();
    if bytes.len() % 2 != 0 {
        return Err(ReplicationPayloadError::InvalidHex(field));
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let hi = hex_value(bytes[i]).ok_or(ReplicationPayloadError::InvalidHex(field))?;
        let lo = hex_value(bytes[i + 1]).ok_or(ReplicationPayloadError::InvalidHex(field))?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Ok(out)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
