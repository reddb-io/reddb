//! RedWire binary bulk payload codec.
//!
//! Layout:
//! `[collection_len:u16][collection][column_count:u16]`
//! `([column_len:u16][column_name])*`
//! `[row_count:u32]([legacy WireValue])*(column_count * row_count)`.

use crate::legacy::{encode_value, try_decode_value, WireValue};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkBinaryFlavor {
    Binary,
    Prevalidated,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BulkBinaryPayload {
    pub collection: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<WireValue>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BulkBinaryError {
    PayloadTooShort(BulkBinaryFlavor),
    MissingCollectionLength(BulkBinaryFlavor),
    TruncatedCollectionName(BulkBinaryFlavor),
    InvalidCollectionName(BulkBinaryFlavor),
    MissingColumnCount(BulkBinaryFlavor),
    MissingColumnNameLength(BulkBinaryFlavor),
    TruncatedColumnName(BulkBinaryFlavor),
    InvalidColumnName(BulkBinaryFlavor),
    MissingRowCount(BulkBinaryFlavor),
    Value(BulkBinaryFlavor, &'static str),
    LengthOverflow(&'static str),
    RowWidthMismatch { got: usize, expected: usize },
    RowCountOverflow,
}

impl fmt::Display for BulkBinaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PayloadTooShort(flavor) => write!(f, "{}: payload too short", prefix(*flavor)),
            Self::MissingCollectionLength(flavor) => {
                write!(f, "{}: missing collection length", label(*flavor))
            }
            Self::TruncatedCollectionName(flavor) => {
                write!(f, "{}: truncated collection name", label(*flavor))
            }
            Self::InvalidCollectionName(flavor) => {
                write!(f, "{}: invalid collection name", label(*flavor))
            }
            Self::MissingColumnCount(flavor) => {
                write!(f, "{}: missing column count", label(*flavor))
            }
            Self::MissingColumnNameLength(flavor) => {
                write!(f, "{}: missing column name length", label(*flavor))
            }
            Self::TruncatedColumnName(flavor) => {
                write!(f, "{}: truncated column name", label(*flavor))
            }
            Self::InvalidColumnName(flavor) => {
                write!(f, "{}: invalid column name", label(*flavor))
            }
            Self::MissingRowCount(flavor) => write!(f, "{}: missing row count", label(*flavor)),
            Self::Value(flavor, err) => write!(f, "{}: {err}", label(*flavor)),
            Self::LengthOverflow(field) => {
                write!(f, "{field} is too large for RedWire binary bulk")
            }
            Self::RowWidthMismatch { got, expected } => {
                write!(f, "row had {got} values for {expected} columns")
            }
            Self::RowCountOverflow => write!(f, "row count is too large for RedWire binary bulk"),
        }
    }
}

impl std::error::Error for BulkBinaryError {}

pub fn encode_bulk_binary_payload(
    collection: &str,
    columns: &[&str],
    rows: &[Vec<WireValue>],
) -> Result<Vec<u8>, BulkBinaryError> {
    write_len_u16(collection.len(), "collection")?;
    write_len_u16(columns.len(), "columns")?;
    if rows.len() > u32::MAX as usize {
        return Err(BulkBinaryError::RowCountOverflow);
    }
    let mut out = Vec::with_capacity(64 + rows.len() * columns.len() * 16);
    write_string_u16(&mut out, collection, "collection")?;
    out.extend_from_slice(&(columns.len() as u16).to_le_bytes());
    for column in columns {
        write_string_u16(&mut out, column, "column")?;
    }
    out.extend_from_slice(&(rows.len() as u32).to_le_bytes());
    for row in rows {
        if row.len() != columns.len() {
            return Err(BulkBinaryError::RowWidthMismatch {
                got: row.len(),
                expected: columns.len(),
            });
        }
        for value in row {
            encode_value(&mut out, value);
        }
    }
    Ok(out)
}

pub fn decode_bulk_binary_payload(
    payload: &[u8],
    flavor: BulkBinaryFlavor,
) -> Result<BulkBinaryPayload, BulkBinaryError> {
    let mut pos = 0;
    if payload.len() < 6 {
        return Err(BulkBinaryError::PayloadTooShort(flavor));
    }
    let coll_len = read_u16(
        payload,
        &mut pos,
        BulkBinaryError::MissingCollectionLength(flavor),
    )? as usize;
    let collection = read_string(
        payload,
        &mut pos,
        coll_len,
        BulkBinaryError::TruncatedCollectionName(flavor),
        BulkBinaryError::InvalidCollectionName(flavor),
    )?;
    let ncols = read_u16(
        payload,
        &mut pos,
        BulkBinaryError::MissingColumnCount(flavor),
    )? as usize;
    let mut columns = Vec::with_capacity(ncols);
    for _ in 0..ncols {
        let name_len = read_u16(
            payload,
            &mut pos,
            BulkBinaryError::MissingColumnNameLength(flavor),
        )? as usize;
        columns.push(read_string(
            payload,
            &mut pos,
            name_len,
            BulkBinaryError::TruncatedColumnName(flavor),
            BulkBinaryError::InvalidColumnName(flavor),
        )?);
    }
    let nrows = read_u32(payload, &mut pos, BulkBinaryError::MissingRowCount(flavor))? as usize;
    let mut rows = Vec::with_capacity(nrows);
    for _ in 0..nrows {
        let mut values = Vec::with_capacity(ncols);
        for _ in 0..ncols {
            values.push(
                try_decode_value(payload, &mut pos)
                    .map_err(|err| BulkBinaryError::Value(flavor, err))?,
            );
        }
        rows.push(values);
    }
    Ok(BulkBinaryPayload {
        collection,
        columns,
        rows,
    })
}

fn prefix(flavor: BulkBinaryFlavor) -> &'static str {
    match flavor {
        BulkBinaryFlavor::Binary => "binary bulk",
        BulkBinaryFlavor::Prevalidated => "binary bulk prevalidated",
    }
}

fn label(flavor: BulkBinaryFlavor) -> &'static str {
    match flavor {
        BulkBinaryFlavor::Binary => "binary bulk",
        BulkBinaryFlavor::Prevalidated => "prevalidated",
    }
}

fn write_string_u16(
    out: &mut Vec<u8>,
    value: &str,
    field: &'static str,
) -> Result<(), BulkBinaryError> {
    write_len_u16(value.len(), field)?;
    out.extend_from_slice(&(value.len() as u16).to_le_bytes());
    out.extend_from_slice(value.as_bytes());
    Ok(())
}

fn write_len_u16(len: usize, field: &'static str) -> Result<(), BulkBinaryError> {
    if len > u16::MAX as usize {
        return Err(BulkBinaryError::LengthOverflow(field));
    }
    Ok(())
}

fn read_u16(payload: &[u8], pos: &mut usize, err: BulkBinaryError) -> Result<u16, BulkBinaryError> {
    let bytes = read_bytes(payload, pos, 2, err)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(payload: &[u8], pos: &mut usize, err: BulkBinaryError) -> Result<u32, BulkBinaryError> {
    let bytes = read_bytes(payload, pos, 4, err)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_string(
    payload: &[u8],
    pos: &mut usize,
    len: usize,
    truncated_err: BulkBinaryError,
    utf8_err: BulkBinaryError,
) -> Result<String, BulkBinaryError> {
    let bytes = read_bytes(payload, pos, len, truncated_err)?;
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| utf8_err)
}

fn read_bytes<'a>(
    payload: &'a [u8],
    pos: &mut usize,
    len: usize,
    err: BulkBinaryError,
) -> Result<&'a [u8], BulkBinaryError> {
    let end = pos.saturating_add(len);
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
    fn binary_bulk_payload_round_trips_values() {
        let rows = vec![vec![
            WireValue::I64(7),
            WireValue::Text("Ada".into()),
            WireValue::Bool(true),
        ]];
        let bytes = encode_bulk_binary_payload("users", &["id", "name", "active"], &rows).unwrap();
        let decoded = decode_bulk_binary_payload(&bytes, BulkBinaryFlavor::Binary).unwrap();
        assert_eq!(decoded.collection, "users");
        assert_eq!(decoded.columns, vec!["id", "name", "active"]);
        assert_eq!(decoded.rows, rows);
    }

    #[test]
    fn binary_bulk_decode_preserves_error_prefixes() {
        assert_eq!(
            decode_bulk_binary_payload(&[0; 5], BulkBinaryFlavor::Binary)
                .unwrap_err()
                .to_string(),
            "binary bulk: payload too short"
        );
        let payload = vec![1, 0, b't', 1, 0, 1, 0, b'x', 1, 0, 0, 0, 1];
        assert_eq!(
            decode_bulk_binary_payload(&payload, BulkBinaryFlavor::Prevalidated)
                .unwrap_err()
                .to_string(),
            "prevalidated: truncated i64 value"
        );
    }
}
