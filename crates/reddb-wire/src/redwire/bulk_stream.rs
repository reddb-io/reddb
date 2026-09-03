//! RedWire legacy bulk-stream payload codec.
//!
//! Start layout:
//! `[collection_len:u16][collection][column_count:u16]([column_len:u16][column_name])*`.
//! Rows layout:
//! `[row_count:u32]([legacy WireValue])*(column_count * row_count)`.

use crate::legacy::{encode_value, try_decode_value, WireValue};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkStreamStartPayload {
    pub collection: String,
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BulkStreamRowsPayload {
    pub rows: Vec<Vec<WireValue>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BulkStreamError {
    MissingCollectionLength,
    TruncatedCollectionName,
    InvalidCollectionName,
    MissingColumnCount,
    MissingColumnNameLength,
    TruncatedColumnName,
    InvalidColumnName,
    MissingRowCount,
    Value(&'static str),
    LengthOverflow(&'static str),
    RowWidthMismatch { got: usize, expected: usize },
    RowCountOverflow,
}

impl fmt::Display for BulkStreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingCollectionLength => write!(f, "stream start: missing collection length"),
            Self::TruncatedCollectionName => write!(f, "stream start: truncated collection name"),
            Self::InvalidCollectionName => write!(f, "stream start: invalid collection name"),
            Self::MissingColumnCount => write!(f, "stream start: missing column count"),
            Self::MissingColumnNameLength => {
                write!(f, "stream start: missing column name length")
            }
            Self::TruncatedColumnName => write!(f, "stream start: truncated column name"),
            Self::InvalidColumnName => write!(f, "stream start: invalid column name"),
            Self::MissingRowCount => write!(f, "stream rows: missing row count"),
            Self::Value(err) => write!(f, "stream rows: {err}"),
            Self::LengthOverflow(field) => {
                write!(f, "{field} is too large for RedWire bulk stream")
            }
            Self::RowWidthMismatch { got, expected } => {
                write!(f, "row had {got} values for {expected} columns")
            }
            Self::RowCountOverflow => write!(f, "row count is too large for RedWire bulk stream"),
        }
    }
}

impl std::error::Error for BulkStreamError {}

pub fn encode_bulk_stream_start_payload(
    collection: &str,
    columns: &[&str],
) -> Result<Vec<u8>, BulkStreamError> {
    write_len_u16(collection.len(), "collection")?;
    write_len_u16(columns.len(), "columns")?;
    let mut out = Vec::with_capacity(4 + collection.len() + columns.len() * 16);
    write_string_u16(&mut out, collection, "collection")?;
    out.extend_from_slice(&(columns.len() as u16).to_le_bytes());
    for column in columns {
        write_string_u16(&mut out, column, "column")?;
    }
    Ok(out)
}

pub fn decode_bulk_stream_start_payload(
    payload: &[u8],
) -> Result<BulkStreamStartPayload, BulkStreamError> {
    let mut pos = 0;
    let coll_len = read_u16(payload, &mut pos, BulkStreamError::MissingCollectionLength)? as usize;
    let collection = read_string(
        payload,
        &mut pos,
        coll_len,
        BulkStreamError::TruncatedCollectionName,
        BulkStreamError::InvalidCollectionName,
    )?;
    let ncols = read_u16(payload, &mut pos, BulkStreamError::MissingColumnCount)? as usize;
    let mut columns = Vec::with_capacity(ncols);
    for _ in 0..ncols {
        let len = read_u16(payload, &mut pos, BulkStreamError::MissingColumnNameLength)? as usize;
        columns.push(read_string(
            payload,
            &mut pos,
            len,
            BulkStreamError::TruncatedColumnName,
            BulkStreamError::InvalidColumnName,
        )?);
    }
    Ok(BulkStreamStartPayload {
        collection,
        columns,
    })
}

pub fn encode_bulk_stream_rows_payload(
    rows: &[Vec<WireValue>],
    column_count: usize,
) -> Result<Vec<u8>, BulkStreamError> {
    if rows.len() > u32::MAX as usize {
        return Err(BulkStreamError::RowCountOverflow);
    }
    let mut out = Vec::with_capacity(4 + rows.len() * column_count * 16);
    out.extend_from_slice(&(rows.len() as u32).to_le_bytes());
    for row in rows {
        if row.len() != column_count {
            return Err(BulkStreamError::RowWidthMismatch {
                got: row.len(),
                expected: column_count,
            });
        }
        for value in row {
            encode_value(&mut out, value);
        }
    }
    Ok(out)
}

pub fn decode_bulk_stream_rows_payload(
    payload: &[u8],
    column_count: usize,
) -> Result<BulkStreamRowsPayload, BulkStreamError> {
    let mut pos = 0;
    let nrows = read_u32(payload, &mut pos, BulkStreamError::MissingRowCount)? as usize;
    // A rows frame with no columns carries no data, and with `column_count`
    // zero the value loop below consumes nothing — so nothing would ever hit
    // the truncation error that normally terminates decoding, and the row
    // loop would run up to `u32::MAX` times pushing empty vectors. Refuse
    // the frame instead.
    if column_count == 0 {
        return Err(BulkStreamError::RowWidthMismatch {
            got: 0,
            expected: 1,
        });
    }
    // Reject, rather than merely under-allocate for, a row count the payload
    // cannot possibly hold: every value costs at least one byte, so a count
    // above `remaining` is a malformed frame. Clamping only the capacity hint
    // still let the loop itself run for hours on a few bytes of input.
    let remaining = payload.len().saturating_sub(pos);
    if nrows > remaining / column_count {
        return Err(BulkStreamError::RowCountOverflow);
    }
    let mut rows = Vec::with_capacity(nrows);
    for _ in 0..nrows {
        let mut values = Vec::with_capacity(column_count);
        for _ in 0..column_count {
            values.push(try_decode_value(payload, &mut pos).map_err(BulkStreamError::Value)?);
        }
        rows.push(values);
    }
    Ok(BulkStreamRowsPayload { rows })
}

fn write_string_u16(
    out: &mut Vec<u8>,
    value: &str,
    field: &'static str,
) -> Result<(), BulkStreamError> {
    write_len_u16(value.len(), field)?;
    out.extend_from_slice(&(value.len() as u16).to_le_bytes());
    out.extend_from_slice(value.as_bytes());
    Ok(())
}

fn write_len_u16(len: usize, field: &'static str) -> Result<(), BulkStreamError> {
    if len > u16::MAX as usize {
        return Err(BulkStreamError::LengthOverflow(field));
    }
    Ok(())
}

fn read_u16(payload: &[u8], pos: &mut usize, err: BulkStreamError) -> Result<u16, BulkStreamError> {
    let bytes = read_bytes(payload, pos, 2, err)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(payload: &[u8], pos: &mut usize, err: BulkStreamError) -> Result<u32, BulkStreamError> {
    let bytes = read_bytes(payload, pos, 4, err)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_string(
    payload: &[u8],
    pos: &mut usize,
    len: usize,
    truncated_err: BulkStreamError,
    utf8_err: BulkStreamError,
) -> Result<String, BulkStreamError> {
    let bytes = read_bytes(payload, pos, len, truncated_err)?;
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| utf8_err)
}

fn read_bytes<'a>(
    payload: &'a [u8],
    pos: &mut usize,
    len: usize,
    err: BulkStreamError,
) -> Result<&'a [u8], BulkStreamError> {
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
    fn stream_start_payload_round_trips() {
        let bytes = encode_bulk_stream_start_payload("events", &["id", "name"]).unwrap();
        let decoded = decode_bulk_stream_start_payload(&bytes).unwrap();
        assert_eq!(decoded.collection, "events");
        assert_eq!(decoded.columns, vec!["id", "name"]);
    }

    #[test]
    fn stream_rows_payload_round_trips_values() {
        let rows = vec![vec![WireValue::I64(7), WireValue::Text("Ada".into())]];
        let bytes = encode_bulk_stream_rows_payload(&rows, 2).unwrap();
        assert_eq!(
            decode_bulk_stream_rows_payload(&bytes, 2).unwrap().rows,
            rows
        );
    }

    #[test]
    fn stream_rows_payload_preserves_error_prefix() {
        let payload = vec![1, 0, 0, 0, 1];
        assert_eq!(
            decode_bulk_stream_rows_payload(&payload, 1)
                .unwrap_err()
                .to_string(),
            "stream rows: truncated i64 value"
        );
    }

    #[test]
    fn hostile_row_count_is_rejected_not_merely_under_allocated() {
        // A 4-byte payload claiming u32::MAX rows used to request a ~96 GiB
        // `Vec`, and an allocation failure aborts the process rather than
        // erroring the connection. Clamping only the capacity hint was not
        // enough: the row loop still ran, so the count itself must be
        // rejected against what the payload can hold.
        let mut payload = Vec::new();
        payload.extend_from_slice(&u32::MAX.to_le_bytes());
        let err = decode_bulk_stream_rows_payload(&payload, 4)
            .expect_err("a row count with no rows behind it must be refused");
        assert!(
            matches!(err, BulkStreamError::RowCountOverflow),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn zero_column_rows_frame_is_refused() {
        // Found by the `redwire_bulk` fuzz target: with no columns the value
        // loop consumes nothing, so the truncation error that normally ends
        // decoding never fires and the row loop runs up to u32::MAX times
        // pushing empty vectors — gigabytes of allocation from four bytes.
        let mut payload = Vec::new();
        payload.extend_from_slice(&0x0a7f_007fu32.to_le_bytes());
        let err = decode_bulk_stream_rows_payload(&payload, 0)
            .expect_err("a rows frame with no columns carries no data");
        assert!(
            matches!(err, BulkStreamError::RowWidthMismatch { .. }),
            "unexpected error: {err:?}"
        );
    }
}
