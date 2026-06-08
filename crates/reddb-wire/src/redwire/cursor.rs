//! RedWire legacy cursor payload codec.

use crate::legacy::{encode_column_name, encode_value, WireValue};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclareCursorPayload {
    pub cursor_id: u32,
    pub sql: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FetchPayload {
    pub cursor_id: u32,
    pub max_rows: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloseCursorPayload {
    pub cursor_id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorPayloadError {
    TruncatedDeclareCursorId,
    TruncatedDeclareSqlLen,
    TruncatedDeclareSql,
    InvalidDeclareSql,
    TruncatedFetchCursorId,
    TruncatedFetchMaxRows,
    TruncatedCloseCursorId,
    SqlTooLarge,
    ColumnCountOverflow,
    RowCountOverflow,
}

impl fmt::Display for CursorPayloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TruncatedDeclareCursorId => write!(f, "truncated declare cursor_id"),
            Self::TruncatedDeclareSqlLen => write!(f, "truncated declare sql_len"),
            Self::TruncatedDeclareSql => write!(f, "truncated declare sql"),
            Self::InvalidDeclareSql => write!(f, "invalid UTF-8 in declare sql"),
            Self::TruncatedFetchCursorId => write!(f, "truncated fetch cursor_id"),
            Self::TruncatedFetchMaxRows => write!(f, "truncated fetch max_rows"),
            Self::TruncatedCloseCursorId => write!(f, "truncated close cursor_id"),
            Self::SqlTooLarge => write!(f, "declare sql is too large for RedWire cursor payload"),
            Self::ColumnCountOverflow => {
                write!(f, "column count is too large for RedWire cursor payload")
            }
            Self::RowCountOverflow => {
                write!(f, "row count is too large for RedWire cursor payload")
            }
        }
    }
}

impl std::error::Error for CursorPayloadError {}

pub fn encode_declare_cursor_payload(
    cursor_id: u32,
    sql: &str,
) -> Result<Vec<u8>, CursorPayloadError> {
    let sql_len = u32::try_from(sql.len()).map_err(|_| CursorPayloadError::SqlTooLarge)?;
    let mut out = Vec::with_capacity(8 + sql.len());
    out.extend_from_slice(&cursor_id.to_le_bytes());
    out.extend_from_slice(&sql_len.to_le_bytes());
    out.extend_from_slice(sql.as_bytes());
    Ok(out)
}

pub fn decode_declare_cursor_payload(
    payload: &[u8],
) -> Result<DeclareCursorPayload, CursorPayloadError> {
    let mut pos = 0usize;
    let cursor_id = u32::from_le_bytes(read_array(
        payload,
        &mut pos,
        CursorPayloadError::TruncatedDeclareCursorId,
    )?);
    let sql_len = u32::from_le_bytes(read_array(
        payload,
        &mut pos,
        CursorPayloadError::TruncatedDeclareSqlLen,
    )?) as usize;
    let sql_bytes = read_bytes(
        payload,
        &mut pos,
        sql_len,
        CursorPayloadError::TruncatedDeclareSql,
    )?;
    let sql = std::str::from_utf8(sql_bytes)
        .map(str::to_string)
        .map_err(|_| CursorPayloadError::InvalidDeclareSql)?;
    Ok(DeclareCursorPayload { cursor_id, sql })
}

pub fn encode_fetch_payload(cursor_id: u32, max_rows: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(8);
    out.extend_from_slice(&cursor_id.to_le_bytes());
    out.extend_from_slice(&max_rows.to_le_bytes());
    out
}

pub fn decode_fetch_payload(payload: &[u8]) -> Result<FetchPayload, CursorPayloadError> {
    let mut pos = 0usize;
    let cursor_id = u32::from_le_bytes(read_array(
        payload,
        &mut pos,
        CursorPayloadError::TruncatedFetchCursorId,
    )?);
    let max_rows = u32::from_le_bytes(read_array(
        payload,
        &mut pos,
        CursorPayloadError::TruncatedFetchMaxRows,
    )?);
    Ok(FetchPayload {
        cursor_id,
        max_rows,
    })
}

pub fn encode_close_cursor_payload(cursor_id: u32) -> Vec<u8> {
    cursor_id.to_le_bytes().to_vec()
}

pub fn decode_close_cursor_payload(
    payload: &[u8],
) -> Result<CloseCursorPayload, CursorPayloadError> {
    let mut pos = 0usize;
    let cursor_id = u32::from_le_bytes(read_array(
        payload,
        &mut pos,
        CursorPayloadError::TruncatedCloseCursorId,
    )?);
    Ok(CloseCursorPayload { cursor_id })
}

pub fn encode_cursor_ok_payload(
    cursor_id: u32,
    columns: &[impl AsRef<str>],
    total_rows: u64,
) -> Result<Vec<u8>, CursorPayloadError> {
    let ncols =
        u16::try_from(columns.len()).map_err(|_| CursorPayloadError::ColumnCountOverflow)?;
    let mut out = Vec::with_capacity(4 + 2 + 8 + columns.len() * 16);
    out.extend_from_slice(&cursor_id.to_le_bytes());
    out.extend_from_slice(&ncols.to_le_bytes());
    for col in columns {
        encode_column_name(&mut out, col.as_ref());
    }
    out.extend_from_slice(&total_rows.to_le_bytes());
    Ok(out)
}

pub fn encode_cursor_batch_payload(
    cursor_id: u32,
    rows: &[Vec<WireValue>],
    has_more: bool,
) -> Result<Vec<u8>, CursorPayloadError> {
    let nrows = u32::try_from(rows.len()).map_err(|_| CursorPayloadError::RowCountOverflow)?;
    let mut out = Vec::new();
    out.extend_from_slice(&cursor_id.to_le_bytes());
    out.extend_from_slice(&nrows.to_le_bytes());
    out.push(u8::from(has_more));
    for row in rows {
        for value in row {
            encode_value(&mut out, value);
        }
    }
    Ok(out)
}

fn read_bytes<'a>(
    payload: &'a [u8],
    pos: &mut usize,
    len: usize,
    err: CursorPayloadError,
) -> Result<&'a [u8], CursorPayloadError> {
    let end = pos.checked_add(len).ok_or(err.clone())?;
    if end > payload.len() {
        return Err(err);
    }
    let bytes = &payload[*pos..end];
    *pos = end;
    Ok(bytes)
}

fn read_array<const N: usize>(
    payload: &[u8],
    pos: &mut usize,
    err: CursorPayloadError,
) -> Result<[u8; N], CursorPayloadError> {
    let bytes = read_bytes(payload, pos, N, err)?;
    let mut out = [0u8; N];
    out.copy_from_slice(bytes);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declare_cursor_payload_round_trips() {
        let bytes = encode_declare_cursor_payload(7, "SELECT id FROM users").unwrap();
        assert_eq!(
            decode_declare_cursor_payload(&bytes).unwrap(),
            DeclareCursorPayload {
                cursor_id: 7,
                sql: "SELECT id FROM users".to_string(),
            }
        );
    }

    #[test]
    fn fetch_and_close_payloads_round_trip() {
        assert_eq!(
            decode_fetch_payload(&encode_fetch_payload(3, 50)).unwrap(),
            FetchPayload {
                cursor_id: 3,
                max_rows: 50,
            }
        );
        assert_eq!(
            decode_close_cursor_payload(&encode_close_cursor_payload(9)).unwrap(),
            CloseCursorPayload { cursor_id: 9 }
        );
    }

    #[test]
    fn cursor_ok_and_batch_payloads_encode_expected_headers() {
        let ok = encode_cursor_ok_payload(5, &["id", "name"], 20).unwrap();
        assert_eq!(u32::from_le_bytes([ok[0], ok[1], ok[2], ok[3]]), 5);
        assert_eq!(u16::from_le_bytes([ok[4], ok[5]]), 2);

        let batch = encode_cursor_batch_payload(
            5,
            &[vec![WireValue::I64(1), WireValue::Text("ada".to_string())]],
            true,
        )
        .unwrap();
        assert_eq!(
            u32::from_le_bytes([batch[0], batch[1], batch[2], batch[3]]),
            5
        );
        assert_eq!(
            u32::from_le_bytes([batch[4], batch[5], batch[6], batch[7]]),
            1
        );
        assert_eq!(batch[8], 1);
    }

    #[test]
    fn cursor_errors_preserve_legacy_messages() {
        assert_eq!(
            decode_declare_cursor_payload(&[0, 0, 0])
                .unwrap_err()
                .to_string(),
            "truncated declare cursor_id"
        );
        assert_eq!(
            decode_fetch_payload(&[0, 0, 0, 0]).unwrap_err().to_string(),
            "truncated fetch max_rows"
        );
        assert_eq!(
            decode_close_cursor_payload(&[0]).unwrap_err().to_string(),
            "truncated close cursor_id"
        );
    }
}
