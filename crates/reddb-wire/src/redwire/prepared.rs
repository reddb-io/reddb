//! RedWire legacy prepared-statement payload codec.

use crate::legacy::{encode_value, try_decode_value, WireValue};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparePayload {
    pub stmt_id: u32,
    pub sql: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExecutePreparedPayload {
    pub stmt_id: u32,
    pub params: Vec<WireValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeallocatePayload {
    pub stmt_id: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreparedOkPayload {
    pub stmt_id: u32,
    pub param_count: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreparedPayloadError {
    TruncatedPrepareStmtId,
    TruncatedPrepareSqlLen,
    TruncatedPrepareSql,
    InvalidPrepareSql,
    TruncatedExecuteStmtId,
    TruncatedExecuteParamCount,
    ExecuteParamValue(&'static str),
    TruncatedDeallocateStmtId,
    SqlTooLarge,
    ParamCountOverflow,
}

impl fmt::Display for PreparedPayloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TruncatedPrepareStmtId => write!(f, "truncated prepare stmt_id"),
            Self::TruncatedPrepareSqlLen => write!(f, "truncated prepare sql_len"),
            Self::TruncatedPrepareSql => write!(f, "truncated prepare sql"),
            Self::InvalidPrepareSql => write!(f, "invalid UTF-8 in prepare sql"),
            Self::TruncatedExecuteStmtId => write!(f, "truncated execute stmt_id"),
            Self::TruncatedExecuteParamCount => write!(f, "truncated execute nparams"),
            Self::ExecuteParamValue(err) => write!(f, "{err}"),
            Self::TruncatedDeallocateStmtId => write!(f, "truncated deallocate stmt_id"),
            Self::SqlTooLarge => write!(f, "prepare sql is too large for RedWire prepared payload"),
            Self::ParamCountOverflow => {
                write!(
                    f,
                    "parameter count is too large for RedWire prepared payload"
                )
            }
        }
    }
}

impl std::error::Error for PreparedPayloadError {}

pub fn encode_prepare_payload(stmt_id: u32, sql: &str) -> Result<Vec<u8>, PreparedPayloadError> {
    let sql_len = u32::try_from(sql.len()).map_err(|_| PreparedPayloadError::SqlTooLarge)?;
    let mut out = Vec::with_capacity(8 + sql.len());
    out.extend_from_slice(&stmt_id.to_le_bytes());
    out.extend_from_slice(&sql_len.to_le_bytes());
    out.extend_from_slice(sql.as_bytes());
    Ok(out)
}

pub fn decode_prepare_payload(payload: &[u8]) -> Result<PreparePayload, PreparedPayloadError> {
    let mut pos = 0usize;
    let stmt_id = u32::from_le_bytes(read_array(
        payload,
        &mut pos,
        PreparedPayloadError::TruncatedPrepareStmtId,
    )?);
    let sql_len = u32::from_le_bytes(read_array(
        payload,
        &mut pos,
        PreparedPayloadError::TruncatedPrepareSqlLen,
    )?) as usize;
    let sql_bytes = read_bytes(
        payload,
        &mut pos,
        sql_len,
        PreparedPayloadError::TruncatedPrepareSql,
    )?;
    let sql = std::str::from_utf8(sql_bytes)
        .map(str::to_string)
        .map_err(|_| PreparedPayloadError::InvalidPrepareSql)?;
    Ok(PreparePayload { stmt_id, sql })
}

pub fn encode_execute_prepared_payload(
    stmt_id: u32,
    params: &[WireValue],
) -> Result<Vec<u8>, PreparedPayloadError> {
    let param_count =
        u16::try_from(params.len()).map_err(|_| PreparedPayloadError::ParamCountOverflow)?;
    let mut out = Vec::new();
    out.extend_from_slice(&stmt_id.to_le_bytes());
    out.extend_from_slice(&param_count.to_le_bytes());
    for param in params {
        encode_value(&mut out, param);
    }
    Ok(out)
}

pub fn decode_execute_prepared_payload(
    payload: &[u8],
) -> Result<ExecutePreparedPayload, PreparedPayloadError> {
    let mut pos = 0usize;
    let stmt_id = u32::from_le_bytes(read_array(
        payload,
        &mut pos,
        PreparedPayloadError::TruncatedExecuteStmtId,
    )?);
    let nparams = u16::from_le_bytes(read_array(
        payload,
        &mut pos,
        PreparedPayloadError::TruncatedExecuteParamCount,
    )?) as usize;
    let mut params = Vec::with_capacity(nparams);
    for _ in 0..nparams {
        params.push(
            try_decode_value(payload, &mut pos).map_err(PreparedPayloadError::ExecuteParamValue)?,
        );
    }
    Ok(ExecutePreparedPayload { stmt_id, params })
}

pub fn encode_deallocate_payload(stmt_id: u32) -> Vec<u8> {
    stmt_id.to_le_bytes().to_vec()
}

pub fn decode_deallocate_payload(
    payload: &[u8],
) -> Result<DeallocatePayload, PreparedPayloadError> {
    let mut pos = 0usize;
    let stmt_id = u32::from_le_bytes(read_array(
        payload,
        &mut pos,
        PreparedPayloadError::TruncatedDeallocateStmtId,
    )?);
    Ok(DeallocatePayload { stmt_id })
}

pub fn encode_prepared_ok_payload(
    stmt_id: u32,
    param_count: usize,
) -> Result<Vec<u8>, PreparedPayloadError> {
    let param_count =
        u16::try_from(param_count).map_err(|_| PreparedPayloadError::ParamCountOverflow)?;
    let mut out = Vec::with_capacity(6);
    out.extend_from_slice(&stmt_id.to_le_bytes());
    out.extend_from_slice(&param_count.to_le_bytes());
    Ok(out)
}

fn read_bytes<'a>(
    payload: &'a [u8],
    pos: &mut usize,
    len: usize,
    err: PreparedPayloadError,
) -> Result<&'a [u8], PreparedPayloadError> {
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
    err: PreparedPayloadError,
) -> Result<[u8; N], PreparedPayloadError> {
    let bytes = read_bytes(payload, pos, N, err)?;
    let mut out = [0u8; N];
    out.copy_from_slice(bytes);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_payload_round_trips() {
        let bytes = encode_prepare_payload(42, "SELECT * FROM users WHERE id = ?").unwrap();
        assert_eq!(
            decode_prepare_payload(&bytes).unwrap(),
            PreparePayload {
                stmt_id: 42,
                sql: "SELECT * FROM users WHERE id = ?".to_string(),
            }
        );
    }

    #[test]
    fn execute_prepared_payload_round_trips_wire_values() {
        let params = vec![WireValue::I64(7), WireValue::Text("ada".to_string())];
        let bytes = encode_execute_prepared_payload(9, &params).unwrap();
        assert_eq!(
            decode_execute_prepared_payload(&bytes).unwrap(),
            ExecutePreparedPayload { stmt_id: 9, params }
        );
    }

    #[test]
    fn deallocate_payload_round_trips() {
        let bytes = encode_deallocate_payload(11);
        assert_eq!(
            decode_deallocate_payload(&bytes).unwrap(),
            DeallocatePayload { stmt_id: 11 }
        );
    }

    #[test]
    fn prepared_errors_preserve_legacy_messages() {
        assert_eq!(
            decode_prepare_payload(&[0, 0, 0]).unwrap_err().to_string(),
            "truncated prepare stmt_id"
        );
        assert_eq!(
            decode_execute_prepared_payload(&[1, 0, 0, 0])
                .unwrap_err()
                .to_string(),
            "truncated execute nparams"
        );
        assert_eq!(
            decode_deallocate_payload(&[1]).unwrap_err().to_string(),
            "truncated deallocate stmt_id"
        );
    }
}
