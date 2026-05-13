//! RedWire `QueryWithParams` payload codec.
//!
//! Payload layout v1:
//! `u32 sql_len` + UTF-8 SQL + `u32 param_count` + encoded values.

use std::fmt;

pub const FEATURE_PARAMS: u32 = 0x0000_0001;
pub const MAX_PARAM_COUNT: usize = 65_536;

const TAG_NULL: u8 = 0x00;
const TAG_BOOL: u8 = 0x01;
const TAG_INT: u8 = 0x02;
const TAG_FLOAT: u8 = 0x03;
const TAG_TEXT: u8 = 0x04;
const TAG_BYTES: u8 = 0x05;
const TAG_VECTOR: u8 = 0x06;
const TAG_JSON: u8 = 0x07;
const TAG_TIMESTAMP: u8 = 0x08;
const TAG_UUID: u8 = 0x09;

#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    Vector(Vec<f32>),
    Json(Vec<u8>),
    Timestamp(i64),
    Uuid([u8; 16]),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamCodecError {
    LengthOverflow(&'static str),
    ParamCountOverLimit(u32),
    Truncated(&'static str),
    InvalidUtf8(&'static str),
    InvalidBool(u8),
    UnknownTag(u8),
    TrailingBytes(usize),
}

impl fmt::Display for ParamCodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LengthOverflow(field) => write!(f, "{field} is too large for RedWire v1"),
            Self::ParamCountOverLimit(count) => {
                write!(f, "param_count {count} exceeds RedWire v1 limit")
            }
            Self::Truncated(field) => write!(f, "truncated {field}"),
            Self::InvalidUtf8(field) => write!(f, "{field} must be valid UTF-8"),
            Self::InvalidBool(byte) => write!(f, "invalid bool payload byte {byte}"),
            Self::UnknownTag(tag) => write!(f, "unknown parameter value tag 0x{tag:02x}"),
            Self::TrailingBytes(count) => write!(f, "{count} trailing bytes after payload"),
        }
    }
}

impl std::error::Error for ParamCodecError {}

pub fn encode_query_with_params(
    sql: &str,
    params: &[ParamValue],
) -> Result<Vec<u8>, ParamCodecError> {
    if sql.len() > u32::MAX as usize {
        return Err(ParamCodecError::LengthOverflow("sql"));
    }
    if params.len() > u32::MAX as usize {
        return Err(ParamCodecError::LengthOverflow("params"));
    }
    if params.len() > MAX_PARAM_COUNT {
        return Err(ParamCodecError::ParamCountOverLimit(params.len() as u32));
    }

    let mut out = Vec::new();
    out.extend_from_slice(&(sql.len() as u32).to_le_bytes());
    out.extend_from_slice(sql.as_bytes());
    out.extend_from_slice(&(params.len() as u32).to_le_bytes());
    for value in params {
        encode_value(value, &mut out)?;
    }
    Ok(out)
}

pub fn decode_query_with_params(
    payload: &[u8],
) -> Result<(String, Vec<ParamValue>), ParamCodecError> {
    let mut pos = 0;
    let sql_len = read_u32(payload, &mut pos, "sql_len")? as usize;
    let sql_bytes = read_bytes(payload, &mut pos, sql_len, "sql")?;
    let sql = std::str::from_utf8(sql_bytes)
        .map_err(|_| ParamCodecError::InvalidUtf8("sql"))?
        .to_string();
    let param_count = read_u32(payload, &mut pos, "param_count")?;
    if param_count as usize > MAX_PARAM_COUNT {
        return Err(ParamCodecError::ParamCountOverLimit(param_count));
    }
    let mut params = Vec::with_capacity(param_count as usize);
    for _ in 0..param_count {
        params.push(decode_value(payload, &mut pos)?);
    }
    if pos != payload.len() {
        return Err(ParamCodecError::TrailingBytes(payload.len() - pos));
    }
    Ok((sql, params))
}

pub fn encode_value(value: &ParamValue, out: &mut Vec<u8>) -> Result<(), ParamCodecError> {
    match value {
        ParamValue::Null => out.push(TAG_NULL),
        ParamValue::Bool(value) => {
            out.push(TAG_BOOL);
            out.push(u8::from(*value));
        }
        ParamValue::Int(value) => {
            out.push(TAG_INT);
            out.extend_from_slice(&value.to_le_bytes());
        }
        ParamValue::Float(value) => {
            out.push(TAG_FLOAT);
            out.extend_from_slice(&value.to_le_bytes());
        }
        ParamValue::Text(value) => {
            out.push(TAG_TEXT);
            write_len_prefixed(value.as_bytes(), out, "text")?;
        }
        ParamValue::Bytes(value) => {
            out.push(TAG_BYTES);
            write_len_prefixed(value, out, "bytes")?;
        }
        ParamValue::Vector(values) => {
            out.push(TAG_VECTOR);
            if values.len() > u32::MAX as usize {
                return Err(ParamCodecError::LengthOverflow("vector"));
            }
            out.extend_from_slice(&(values.len() as u32).to_le_bytes());
            for value in values {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
        ParamValue::Json(value) => {
            out.push(TAG_JSON);
            write_len_prefixed(value, out, "json")?;
        }
        ParamValue::Timestamp(value) => {
            out.push(TAG_TIMESTAMP);
            out.extend_from_slice(&value.to_le_bytes());
        }
        ParamValue::Uuid(value) => {
            out.push(TAG_UUID);
            out.extend_from_slice(value);
        }
    }
    Ok(())
}

pub fn decode_value(payload: &[u8], pos: &mut usize) -> Result<ParamValue, ParamCodecError> {
    let tag = *read_bytes(payload, pos, 1, "value tag")?
        .first()
        .expect("read one byte");
    match tag {
        TAG_NULL => Ok(ParamValue::Null),
        TAG_BOOL => {
            let value = read_bytes(payload, pos, 1, "bool")?[0];
            match value {
                0 => Ok(ParamValue::Bool(false)),
                1 => Ok(ParamValue::Bool(true)),
                other => Err(ParamCodecError::InvalidBool(other)),
            }
        }
        TAG_INT => Ok(ParamValue::Int(read_i64(payload, pos, "int")?)),
        TAG_FLOAT => Ok(ParamValue::Float(f64::from_le_bytes(read_array(
            payload, pos, "float",
        )?))),
        TAG_TEXT => {
            let len = read_u32(payload, pos, "text_len")? as usize;
            let bytes = read_bytes(payload, pos, len, "text")?;
            let text = std::str::from_utf8(bytes)
                .map_err(|_| ParamCodecError::InvalidUtf8("text"))?
                .to_string();
            Ok(ParamValue::Text(text))
        }
        TAG_BYTES => {
            let len = read_u32(payload, pos, "bytes_len")? as usize;
            Ok(ParamValue::Bytes(
                read_bytes(payload, pos, len, "bytes")?.to_vec(),
            ))
        }
        TAG_VECTOR => {
            let len = read_u32(payload, pos, "vector_len")? as usize;
            let byte_len = len
                .checked_mul(4)
                .ok_or(ParamCodecError::LengthOverflow("vector"))?;
            ensure_remaining(payload, *pos, byte_len, "vector")?;
            let mut values = Vec::with_capacity(len);
            for _ in 0..len {
                values.push(f32::from_le_bytes(read_array(payload, pos, "vector")?));
            }
            Ok(ParamValue::Vector(values))
        }
        TAG_JSON => {
            let len = read_u32(payload, pos, "json_len")? as usize;
            Ok(ParamValue::Json(
                read_bytes(payload, pos, len, "json")?.to_vec(),
            ))
        }
        TAG_TIMESTAMP => Ok(ParamValue::Timestamp(read_i64(payload, pos, "timestamp")?)),
        TAG_UUID => Ok(ParamValue::Uuid(read_array(payload, pos, "uuid")?)),
        other => Err(ParamCodecError::UnknownTag(other)),
    }
}

fn write_len_prefixed(
    value: &[u8],
    out: &mut Vec<u8>,
    field: &'static str,
) -> Result<(), ParamCodecError> {
    if value.len() > u32::MAX as usize {
        return Err(ParamCodecError::LengthOverflow(field));
    }
    out.extend_from_slice(&(value.len() as u32).to_le_bytes());
    out.extend_from_slice(value);
    Ok(())
}

fn read_u32(payload: &[u8], pos: &mut usize, field: &'static str) -> Result<u32, ParamCodecError> {
    Ok(u32::from_le_bytes(read_array(payload, pos, field)?))
}

fn read_i64(payload: &[u8], pos: &mut usize, field: &'static str) -> Result<i64, ParamCodecError> {
    Ok(i64::from_le_bytes(read_array(payload, pos, field)?))
}

fn read_array<const N: usize>(
    payload: &[u8],
    pos: &mut usize,
    field: &'static str,
) -> Result<[u8; N], ParamCodecError> {
    let bytes = read_bytes(payload, pos, N, field)?;
    let mut out = [0u8; N];
    out.copy_from_slice(bytes);
    Ok(out)
}

fn read_bytes<'a>(
    payload: &'a [u8],
    pos: &mut usize,
    len: usize,
    field: &'static str,
) -> Result<&'a [u8], ParamCodecError> {
    let end = pos
        .checked_add(len)
        .ok_or(ParamCodecError::Truncated(field))?;
    if end > payload.len() {
        return Err(ParamCodecError::Truncated(field));
    }
    let bytes = &payload[*pos..end];
    *pos = end;
    Ok(bytes)
}

fn ensure_remaining(
    payload: &[u8],
    pos: usize,
    len: usize,
    field: &'static str,
) -> Result<(), ParamCodecError> {
    let end = pos
        .checked_add(len)
        .ok_or(ParamCodecError::Truncated(field))?;
    if end > payload.len() {
        return Err(ParamCodecError::Truncated(field));
    }
    Ok(())
}
