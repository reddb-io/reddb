//! RedWire legacy JSON bulk-insert payload codec.
//!
//! Wire shape:
//! `[collection_len u16][collection bytes][row_count u32]`
//! followed by `row_count` JSON strings as `[json_len u32][json bytes]`.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkJsonPayload {
    pub collection: String,
    pub json_payloads: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BulkJsonError {
    PayloadTooShort,
    MissingCollectionLength,
    TruncatedCollectionName,
    InvalidCollectionName,
    MissingRowCount,
    MissingJsonLength,
    TruncatedJsonPayload,
    InvalidJsonPayload,
    FieldTooLarge(&'static str),
    RowCountOverflow,
}

impl fmt::Display for BulkJsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PayloadTooShort => write!(f, "bulk insert: payload too short"),
            Self::MissingCollectionLength => {
                write!(f, "bulk insert: missing collection length")
            }
            Self::TruncatedCollectionName => {
                write!(f, "bulk insert: truncated collection name")
            }
            Self::InvalidCollectionName => write!(f, "bulk insert: invalid collection name"),
            Self::MissingRowCount => write!(f, "bulk insert: missing row count"),
            Self::MissingJsonLength => write!(f, "bulk insert: missing JSON length"),
            Self::TruncatedJsonPayload => write!(f, "bulk insert: truncated JSON payload"),
            Self::InvalidJsonPayload => write!(f, "bulk insert: invalid JSON payload"),
            Self::FieldTooLarge(field) => {
                write!(f, "{field} is too large for RedWire JSON bulk insert")
            }
            Self::RowCountOverflow => {
                write!(f, "row count is too large for RedWire JSON bulk insert")
            }
        }
    }
}

impl std::error::Error for BulkJsonError {}

pub fn encode_bulk_json_payload(
    collection: &str,
    json_payloads: &[String],
) -> Result<Vec<u8>, BulkJsonError> {
    let collection_len =
        u16::try_from(collection.len()).map_err(|_| BulkJsonError::FieldTooLarge("collection"))?;
    let row_count =
        u32::try_from(json_payloads.len()).map_err(|_| BulkJsonError::RowCountOverflow)?;

    let mut out = Vec::new();
    out.extend_from_slice(&collection_len.to_le_bytes());
    out.extend_from_slice(collection.as_bytes());
    out.extend_from_slice(&row_count.to_le_bytes());
    for payload in json_payloads {
        let json_len =
            u32::try_from(payload.len()).map_err(|_| BulkJsonError::FieldTooLarge("json"))?;
        out.extend_from_slice(&json_len.to_le_bytes());
        out.extend_from_slice(payload.as_bytes());
    }
    Ok(out)
}

pub fn decode_bulk_json_payload(payload: &[u8]) -> Result<BulkJsonPayload, BulkJsonError> {
    if payload.len() < 2 {
        return Err(BulkJsonError::PayloadTooShort);
    }

    let mut pos = 0;
    let coll_len = read_u16(payload, &mut pos, BulkJsonError::MissingCollectionLength)? as usize;
    let collection = read_string(
        payload,
        &mut pos,
        coll_len,
        BulkJsonError::TruncatedCollectionName,
        BulkJsonError::InvalidCollectionName,
    )?;

    let nrows = read_u32(payload, &mut pos, BulkJsonError::MissingRowCount)? as usize;
    let mut json_payloads = Vec::with_capacity(nrows);
    for _ in 0..nrows {
        let json_len = read_u32(payload, &mut pos, BulkJsonError::MissingJsonLength)? as usize;
        json_payloads.push(read_string(
            payload,
            &mut pos,
            json_len,
            BulkJsonError::TruncatedJsonPayload,
            BulkJsonError::InvalidJsonPayload,
        )?);
    }

    Ok(BulkJsonPayload {
        collection,
        json_payloads,
    })
}

fn read_u16(payload: &[u8], pos: &mut usize, err: BulkJsonError) -> Result<u16, BulkJsonError> {
    let bytes = read_bytes(payload, pos, 2, err)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(payload: &[u8], pos: &mut usize, err: BulkJsonError) -> Result<u32, BulkJsonError> {
    let bytes = read_bytes(payload, pos, 4, err)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_string(
    payload: &[u8],
    pos: &mut usize,
    len: usize,
    truncated_err: BulkJsonError,
    utf8_err: BulkJsonError,
) -> Result<String, BulkJsonError> {
    let bytes = read_bytes(payload, pos, len, truncated_err)?;
    std::str::from_utf8(bytes)
        .map(str::to_string)
        .map_err(|_| utf8_err)
}

fn read_bytes<'a>(
    payload: &'a [u8],
    pos: &mut usize,
    len: usize,
    err: BulkJsonError,
) -> Result<&'a [u8], BulkJsonError> {
    let Some(end) = pos.checked_add(len) else {
        return Err(err);
    };
    if end > payload.len() {
        return Err(err);
    }
    let bytes = &payload[*pos..end];
    *pos = end;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulk_json_payload_round_trips() {
        let rows = vec![r#"{"id":1}"#.to_string(), r#"{"id":2}"#.to_string()];
        let bytes = encode_bulk_json_payload("events", &rows).unwrap();
        let decoded = decode_bulk_json_payload(&bytes).unwrap();
        assert_eq!(decoded.collection, "events");
        assert_eq!(decoded.json_payloads, rows);
    }

    #[test]
    fn bulk_json_decode_preserves_error_prefixes() {
        assert_eq!(
            decode_bulk_json_payload(&[0]).unwrap_err().to_string(),
            "bulk insert: payload too short"
        );

        let payload = vec![1, 0, b't', 1, 0, 0, 0, 10, 0, 0, 0, b'{'];
        assert_eq!(
            decode_bulk_json_payload(&payload).unwrap_err().to_string(),
            "bulk insert: truncated JSON payload"
        );
    }
}
