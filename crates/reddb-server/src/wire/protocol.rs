//! Server-side adapter for the legacy RedDB binary protocol.
//!
//! The byte-level contract lives in `reddb-wire::legacy`. This module
//! only converts between protocol `WireValue` and engine `Value`.

use std::convert::TryFrom;

use crate::storage::schema::Value;
use reddb_wire::legacy::WireValue;

impl From<&Value> for WireValue {
    fn from(value: &Value) -> Self {
        match value {
            Value::Null => WireValue::Null,
            Value::Integer(n) => WireValue::I64(*n),
            Value::UnsignedInteger(n) => WireValue::U64(*n),
            Value::Float(f) => WireValue::F64(*f),
            Value::Text(s) => WireValue::Text(s.to_string()),
            Value::Blob(bytes) => WireValue::Bytes(bytes.clone()),
            Value::Boolean(b) => WireValue::Bool(*b),
            Value::Timestamp(t) => WireValue::Timestamp(*t as u64),
            _ => WireValue::Null,
        }
    }
}

impl From<Value> for WireValue {
    fn from(value: Value) -> Self {
        match value {
            Value::Null => WireValue::Null,
            Value::Integer(n) => WireValue::I64(n),
            Value::UnsignedInteger(n) => WireValue::U64(n),
            Value::Float(f) => WireValue::F64(f),
            Value::Text(s) => WireValue::Text(s.to_string()),
            Value::Blob(bytes) => WireValue::Bytes(bytes),
            Value::Boolean(b) => WireValue::Bool(b),
            Value::Timestamp(t) => WireValue::Timestamp(t as u64),
            _ => WireValue::Null,
        }
    }
}

impl TryFrom<WireValue> for Value {
    type Error = &'static str;

    fn try_from(value: WireValue) -> Result<Self, Self::Error> {
        match value {
            WireValue::Null => Ok(Value::Null),
            WireValue::I64(n) => Ok(Value::Integer(n)),
            WireValue::U64(n) => Ok(Value::UnsignedInteger(n)),
            WireValue::F64(f) => Ok(Value::Float(f)),
            WireValue::Text(s) => Ok(Value::text(s)),
            WireValue::Bool(b) => Ok(Value::Boolean(b)),
            WireValue::Bytes(bytes) => Ok(Value::Blob(bytes)),
            WireValue::Timestamp(t) => {
                let timestamp = i64::try_from(t).map_err(|_| "timestamp exceeds i64 range")?;
                Ok(Value::Timestamp(timestamp))
            }
        }
    }
}

#[inline]
pub fn encode_value(buf: &mut Vec<u8>, value: &Value) {
    reddb_wire::legacy::encode_value(buf, &WireValue::from(value));
}

#[inline]
pub fn decode_value(data: &[u8], pos: &mut usize) -> Value {
    try_decode_value(data, pos).unwrap_or(Value::Null)
}

#[inline]
pub fn try_decode_value(data: &[u8], pos: &mut usize) -> Result<Value, &'static str> {
    let value = reddb_wire::legacy::try_decode_value(data, pos)?;
    Value::try_from(value)
}
