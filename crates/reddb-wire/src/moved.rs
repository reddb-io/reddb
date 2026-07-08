//! MOVED-style stale-ownership redirect payload.
//!
//! The shape is shared by transports: a non-owner returns it when routing
//! metadata says another member currently owns the target range. Clients update
//! their routing cache from this payload and retry once against `owner_addr`.

use serde_json::{Map, Value};

use crate::redwire::{Frame, MessageKind};

pub const MOVED_CODE: &str = "MOVED";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovedRedirect {
    pub slot: Option<u64>,
    pub collection: String,
    pub range_id: u64,
    pub owner_addr: String,
    pub ownership_epoch: u64,
    pub catalog_version: u64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MovedRedirectError {
    InvalidJson,
    ExpectedObject,
    MissingField(&'static str),
    InvalidField(&'static str),
}

impl std::fmt::Display for MovedRedirectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidJson => write!(f, "MOVED payload is not valid JSON"),
            Self::ExpectedObject => write!(f, "MOVED payload must be a JSON object"),
            Self::MissingField(field) => write!(f, "MOVED payload missing field {field}"),
            Self::InvalidField(field) => write!(f, "MOVED payload field {field} is invalid"),
        }
    }
}

impl std::error::Error for MovedRedirectError {}

pub fn encode_moved_redirect(payload: &MovedRedirect) -> Vec<u8> {
    let mut obj = Map::new();
    obj.insert("code".to_string(), Value::String(MOVED_CODE.to_string()));
    if let Some(slot) = payload.slot {
        obj.insert("slot".to_string(), Value::Number(slot.into()));
    }
    obj.insert(
        "collection".to_string(),
        Value::String(payload.collection.clone()),
    );
    obj.insert(
        "range_id".to_string(),
        Value::Number(payload.range_id.into()),
    );
    obj.insert(
        "owner_addr".to_string(),
        Value::String(payload.owner_addr.clone()),
    );
    obj.insert(
        "ownership_epoch".to_string(),
        Value::Number(payload.ownership_epoch.into()),
    );
    obj.insert(
        "catalog_version".to_string(),
        Value::Number(payload.catalog_version.into()),
    );
    obj.insert("reason".to_string(), Value::String(payload.reason.clone()));
    serde_json::to_vec(&Value::Object(obj)).expect("MOVED payload JSON encoding cannot fail")
}

pub fn decode_moved_redirect(bytes: &[u8]) -> Result<MovedRedirect, MovedRedirectError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| MovedRedirectError::InvalidJson)?;
    let obj = value
        .as_object()
        .ok_or(MovedRedirectError::ExpectedObject)?;
    let code = required_str(obj, "code")?;
    if code != MOVED_CODE {
        return Err(MovedRedirectError::InvalidField("code"));
    }
    Ok(MovedRedirect {
        slot: optional_u64(obj, "slot")?,
        collection: required_str(obj, "collection")?.to_string(),
        range_id: required_u64(obj, "range_id")?,
        owner_addr: required_str(obj, "owner_addr")?.to_string(),
        ownership_epoch: required_u64(obj, "ownership_epoch")?,
        catalog_version: required_u64(obj, "catalog_version")?,
        reason: required_str(obj, "reason")?.to_string(),
    })
}

pub fn build_moved_redirect_frame(correlation_id: u64, payload: &MovedRedirect) -> Frame {
    Frame::new(
        MessageKind::MovedRedirect,
        correlation_id,
        encode_moved_redirect(payload),
    )
}

fn required_str<'a>(
    obj: &'a Map<String, Value>,
    field: &'static str,
) -> Result<&'a str, MovedRedirectError> {
    obj.get(field)
        .ok_or(MovedRedirectError::MissingField(field))?
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or(MovedRedirectError::InvalidField(field))
}

fn required_u64(obj: &Map<String, Value>, field: &'static str) -> Result<u64, MovedRedirectError> {
    obj.get(field)
        .ok_or(MovedRedirectError::MissingField(field))?
        .as_u64()
        .ok_or(MovedRedirectError::InvalidField(field))
}

fn optional_u64(
    obj: &Map<String, Value>,
    field: &'static str,
) -> Result<Option<u64>, MovedRedirectError> {
    match obj.get(field) {
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or(MovedRedirectError::InvalidField(field)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> MovedRedirect {
        MovedRedirect {
            slot: Some(42),
            collection: "orders".to_string(),
            range_id: 7,
            owner_addr: "node-b:5050".to_string(),
            ownership_epoch: 9,
            catalog_version: 11,
            reason: "transaction".to_string(),
        }
    }

    #[test]
    fn moved_payload_round_trips_owner_epoch_version_and_slot() {
        let encoded = encode_moved_redirect(&payload());
        let decoded = decode_moved_redirect(&encoded).expect("decode MOVED");

        assert_eq!(decoded, payload());
    }

    #[test]
    fn moved_frame_uses_server_to_client_kind() {
        let frame = build_moved_redirect_frame(99, &payload());

        assert_eq!(frame.kind, MessageKind::MovedRedirect);
        assert_eq!(frame.correlation_id, 99);
        assert_eq!(
            decode_moved_redirect(&frame.payload).expect("decode frame payload"),
            payload()
        );
    }
}
