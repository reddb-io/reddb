//! Server-side adapter for the legacy RedDB binary protocol.
//!
//! The byte-level contract lives in `reddb-wire::legacy`. This module
//! only converts between protocol `WireValue` and engine `Value`.

use reddb_types::Value;

// The `WireValue`<->`Value` conversion impls moved to `reddb-wire`'s
// `legacy` module (ADR 0052, #1061): once `Value` was re-homed to the
// keystone crate below wire, the orphan rule required those impls to live
// in `WireValue`'s home crate. The helper functions below still drive the
// legacy codec and rely on those impls being in scope via `reddb_wire`.

#[inline]
pub fn encode_value(buf: &mut Vec<u8>, value: &Value) {
    reddb_wire::legacy::encode_native_value(buf, value);
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

#[cfg(test)]
mod allocation_tests {
    use super::*;

    #[test]
    fn borrowed_text_and_blob_encoding_never_allocate_temporary_value_copies() {
        for value in [
            Value::text("x".repeat(192 * 1024)),
            Value::Blob(vec![1; 192 * 1024]),
        ] {
            let mut buffer =
                Vec::with_capacity(reddb_wire::legacy::native_value_encoded_len(&value));
            let (_, allocations) = crate::storage::no_malloc_hot_paths::measure_allocations(|| {
                encode_value(&mut buffer, &value);
            });
            assert_eq!(
                allocations.allocated_bytes, 0,
                "borrowed value encoder: {allocations:?}"
            );
            assert_eq!(buffer.len(), buffer.capacity());
        }
    }
}
