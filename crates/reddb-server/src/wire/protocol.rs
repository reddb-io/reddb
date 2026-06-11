//! Server-side adapter for the legacy RedDB binary protocol.
//!
//! The byte-level contract lives in `reddb-wire::legacy`. This module
//! only converts between protocol `WireValue` and engine `Value`.

use crate::storage::schema::Value;
use reddb_wire::legacy::WireValue;

// The `WireValue`<->`Value` conversion impls moved to `reddb-wire`'s
// `legacy` module (ADR 0052, #1061): once `Value` was re-homed to the
// keystone crate below wire, the orphan rule required those impls to live
// in `WireValue`'s home crate. The helper functions below still drive the
// legacy codec and rely on those impls being in scope via `reddb_wire`.

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
