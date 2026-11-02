//! Legacy RedDB binary protocol vocabulary.
//!
//! This is the pre-RedWire frame shape still used by the direct TCP
//! handlers and fast paths:
//!
//! ```text
//! [total_len: u32 LE][msg_type: u8][payload...]
//! ```
//!
//! The crate owns the byte-level contract. The conversions between the
//! engine `Value` (now in the keystone crate `reddb-io-types`, below wire)
//! and `WireValue` live here: the orphan rule pins `From`/`TryFrom` impls
//! that mention `WireValue` to this, its home crate (ADR 0052, #1061).

use reddb_types::Value;

// Message type constants.
pub const MSG_QUERY: u8 = 0x01;
pub const MSG_RESULT: u8 = 0x02;
pub const MSG_ERROR: u8 = 0x03;
pub const MSG_BULK_INSERT: u8 = 0x04;
pub const MSG_BULK_OK: u8 = 0x05;
pub const MSG_BULK_INSERT_BINARY: u8 = 0x06;
pub const MSG_QUERY_BINARY: u8 = 0x07;
pub const MSG_BULK_INSERT_PREVALIDATED: u8 = 0x08;
pub const MSG_BULK_STREAM_START: u8 = 0x09;
pub const MSG_BULK_STREAM_ROWS: u8 = 0x0A;
pub const MSG_BULK_STREAM_COMMIT: u8 = 0x0B;
pub const MSG_BULK_STREAM_ACK: u8 = 0x0C;
pub const MSG_PREPARE: u8 = 0x0D;
pub const MSG_PREPARED_OK: u8 = 0x0E;
pub const MSG_EXECUTE_PREPARED: u8 = 0x0F;
pub const MSG_DEALLOCATE: u8 = 0x10;
pub const MSG_DECLARE_CURSOR: u8 = 0x11;
pub const MSG_CURSOR_OK: u8 = 0x12;
pub const MSG_FETCH: u8 = 0x13;
pub const MSG_CURSOR_BATCH: u8 = 0x14;
pub const MSG_CLOSE_CURSOR: u8 = 0x15;

// Value type tags.
pub const VAL_NULL: u8 = 0;
pub const VAL_I64: u8 = 1;
pub const VAL_F64: u8 = 2;
pub const VAL_TEXT: u8 = 3;
pub const VAL_BOOL: u8 = 4;
pub const VAL_U64: u8 = 5;

#[derive(Debug, Clone, PartialEq)]
pub enum WireValue {
    Null,
    I64(i64),
    U64(u64),
    F64(f64),
    Text(String),
    Bool(bool),
    Bytes(Vec<u8>),
    Timestamp(u64),
}

// Conversions between the engine `Value` (keystone crate `reddb-io-types`)
// and the legacy `WireValue`. Re-homed from `reddb-server`'s
// `wire::protocol` (ADR 0052, #1061): once `Value` moved below wire, the
// orphan rule required these impls to live in `WireValue`'s home crate.
// Bodies are byte-faithful relocations; the byte contract is unchanged.

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

/// Write a legacy frame header: [total_len: u32 LE][msg_type: u8].
#[inline]
pub fn write_frame_header(buf: &mut Vec<u8>, msg_type: u8, payload_len: u32) {
    let total = payload_len + 1; // +1 for msg_type
    buf.extend_from_slice(&total.to_le_bytes());
    buf.push(msg_type);
}

pub fn build_legacy_frame(msg_type: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    write_frame_header(&mut out, msg_type, payload.len() as u32);
    out.extend_from_slice(payload);
    out
}

pub fn build_legacy_result_frame(payload: &[u8]) -> Vec<u8> {
    build_legacy_frame(MSG_RESULT, payload)
}

/// Encode a result payload directly into its final frame allocation. The
/// callback must append after the reserved header without modifying it.
/// Capacity is a hint, not an admission limit.
pub fn build_legacy_result_frame_with(
    payload_capacity: usize,
    encode: impl FnOnce(&mut Vec<u8>),
) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload_capacity.saturating_add(5));
    write_frame_header(&mut out, MSG_RESULT, 0);
    encode(&mut out);
    assert!(
        out.len() >= 5,
        "encoder must preserve the legacy frame header"
    );
    let total = u32::try_from(out.len() - 4).expect("legacy frame length fits u32");
    out[..4].copy_from_slice(&total.to_le_bytes());
    out
}

/// Finish a result in caller-owned, already admitted capacity. The first five
/// bytes are reserved for the legacy envelope; no payload copy or growth occurs.
pub fn build_legacy_result_frame_in_place(out: &mut [u8]) -> Result<(), &'static str> {
    if out.len() < 5 {
        return Err("truncated legacy result buffer");
    }
    let total = u32::try_from(out.len() - 4).map_err(|_| "legacy result exceeds frame length")?;
    out[..4].copy_from_slice(&total.to_le_bytes());
    out[4] = MSG_RESULT;
    Ok(())
}

pub fn build_legacy_error_frame(message: &[u8]) -> Vec<u8> {
    build_legacy_frame(MSG_ERROR, message)
}

pub fn build_legacy_bulk_ok_frame(payload: &[u8]) -> Vec<u8> {
    build_legacy_frame(MSG_BULK_OK, payload)
}

pub fn build_legacy_bulk_stream_ack_frame() -> Vec<u8> {
    build_legacy_frame(MSG_BULK_STREAM_ACK, &[])
}

pub fn build_legacy_prepared_ok_frame(payload: &[u8]) -> Vec<u8> {
    build_legacy_frame(MSG_PREPARED_OK, payload)
}

pub fn build_legacy_cursor_ok_frame(payload: &[u8]) -> Vec<u8> {
    build_legacy_frame(MSG_CURSOR_OK, payload)
}

pub fn build_legacy_cursor_batch_frame(payload: &[u8]) -> Vec<u8> {
    build_legacy_frame(MSG_CURSOR_BATCH, payload)
}

#[inline]
pub fn encode_value(buf: &mut Vec<u8>, value: &WireValue) {
    match value {
        WireValue::Null | WireValue::Bytes(_) => buf.push(VAL_NULL),
        WireValue::I64(n) => {
            buf.push(VAL_I64);
            buf.extend_from_slice(&n.to_le_bytes());
        }
        WireValue::U64(n) | WireValue::Timestamp(n) => {
            buf.push(VAL_U64);
            buf.extend_from_slice(&n.to_le_bytes());
        }
        WireValue::F64(f) => {
            buf.push(VAL_F64);
            buf.extend_from_slice(&f.to_le_bytes());
        }
        WireValue::Text(s) => {
            buf.push(VAL_TEXT);
            let bytes = s.as_bytes();
            buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(bytes);
        }
        WireValue::Bool(b) => {
            buf.push(VAL_BOOL);
            buf.push(*b as u8);
        }
    }
}

/// Encode borrowed engine values without constructing owned WireValue text/blob
/// copies. Unsupported variants (including Blob) retain the legacy Null tag.
#[inline]
pub fn encode_native_value(buf: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Text(text) => {
            buf.push(VAL_TEXT);
            buf.extend_from_slice(&(text.len() as u32).to_le_bytes());
            buf.extend_from_slice(text.as_bytes());
        }
        Value::Integer(value) => encode_value(buf, &WireValue::I64(*value)),
        Value::UnsignedInteger(value) => encode_value(buf, &WireValue::U64(*value)),
        Value::Float(value) => encode_value(buf, &WireValue::F64(*value)),
        Value::Boolean(value) => encode_value(buf, &WireValue::Bool(*value)),
        Value::Timestamp(value) => encode_value(buf, &WireValue::Timestamp(*value as u64)),
        _ => buf.push(VAL_NULL),
    }
}

/// Exact encoded length without allocating or traversing text contents.
#[inline]
pub fn native_value_encoded_len(value: &Value) -> usize {
    match value {
        Value::Text(text) => 5 + text.len(),
        Value::Integer(_) | Value::UnsignedInteger(_) | Value::Float(_) | Value::Timestamp(_) => 9,
        Value::Boolean(_) => 2,
        _ => 1,
    }
}

#[inline]
pub fn decode_value(data: &[u8], pos: &mut usize) -> WireValue {
    try_decode_value(data, pos).unwrap_or(WireValue::Null)
}

#[inline]
pub fn try_decode_value(data: &[u8], pos: &mut usize) -> Result<WireValue, &'static str> {
    if *pos >= data.len() {
        return Err("missing value tag");
    }

    let tag = data[*pos];
    *pos += 1;

    match tag {
        VAL_NULL => Ok(WireValue::Null),
        VAL_I64 => Ok(WireValue::I64(i64::from_le_bytes(read_array::<8>(
            data,
            pos,
            "truncated i64 value",
        )?))),
        VAL_U64 => Ok(WireValue::U64(u64::from_le_bytes(read_array::<8>(
            data,
            pos,
            "truncated u64 value",
        )?))),
        VAL_F64 => Ok(WireValue::F64(f64::from_le_bytes(read_array::<8>(
            data,
            pos,
            "truncated f64 value",
        )?))),
        VAL_TEXT => {
            let len =
                u32::from_le_bytes(read_array::<4>(data, pos, "truncated text length")?) as usize;
            let bytes = read_bytes(data, pos, len, "truncated text value")?;
            let cow = std::string::String::from_utf8_lossy(bytes);
            Ok(WireValue::Text(cow.into_owned()))
        }
        VAL_BOOL => {
            let bytes = read_bytes(data, pos, 1, "truncated bool value")?;
            Ok(WireValue::Bool(bytes[0] != 0))
        }
        _ => Err("unknown value tag"),
    }
}

#[inline]
fn read_bytes<'a>(
    data: &'a [u8],
    pos: &mut usize,
    len: usize,
    err: &'static str,
) -> Result<&'a [u8], &'static str> {
    let end = pos.saturating_add(len);
    if end > data.len() {
        return Err(err);
    }
    let bytes = &data[*pos..end];
    *pos = end;
    Ok(bytes)
}

#[inline]
fn read_array<const N: usize>(
    data: &[u8],
    pos: &mut usize,
    err: &'static str,
) -> Result<[u8; N], &'static str> {
    let bytes = read_bytes(data, pos, N, err)?;
    let mut array = [0u8; N];
    array.copy_from_slice(bytes);
    Ok(array)
}

#[inline]
pub fn encode_column_name(buf: &mut Vec<u8>, name: &str) {
    let bytes = name.as_bytes();
    buf.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    buf.extend_from_slice(bytes);
}

pub fn encode_result_payload_header<'a, I>(buf: &mut Vec<u8>, columns: I, row_count: u32) -> usize
where
    I: IntoIterator<Item = &'a str>,
    I::IntoIter: ExactSizeIterator,
{
    let columns = columns.into_iter();
    buf.extend_from_slice(&(columns.len() as u16).to_le_bytes());
    for column in columns {
        encode_column_name(buf, column);
    }
    let row_count_offset = buf.len();
    buf.extend_from_slice(&row_count.to_le_bytes());
    row_count_offset
}

pub fn set_result_payload_row_count(
    buf: &mut [u8],
    row_count_offset: usize,
    row_count: u32,
) -> Result<(), &'static str> {
    let end = row_count_offset.saturating_add(4);
    if end > buf.len() {
        return Err("result payload row-count offset out of bounds");
    }
    buf[row_count_offset..end].copy_from_slice(&row_count.to_le_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admitted_in_place_result_matches_canonical_envelope() {
        for payload in [b"".as_slice(), b"hello", &[0, 255, 1, 2]] {
            let expected = build_legacy_result_frame(payload);
            let mut actual = vec![0; 5 + payload.len()];
            actual[5..].copy_from_slice(payload);
            build_legacy_result_frame_in_place(&mut actual).unwrap();
            assert_eq!(actual, expected);
        }
        for length in 0..5 {
            let mut truncated = vec![0; length];
            assert!(build_legacy_result_frame_in_place(&mut truncated).is_err());
        }
    }

    #[test]
    fn borrowed_native_encoding_matches_owned_conversion_and_exact_length() {
        let values = [
            Value::Null,
            Value::Integer(i64::MIN),
            Value::UnsignedInteger(u64::MAX),
            Value::Float(-1.25),
            Value::Boolean(true),
            Value::Boolean(false),
            Value::Timestamp(-1),
            Value::text(""),
            Value::text("quotes\"雪\n"),
            Value::text("x".repeat(192 * 1024)),
            Value::Blob(vec![1; 192 * 1024]),
        ];
        for value in values {
            let mut expected = Vec::new();
            encode_value(&mut expected, &WireValue::from(&value));
            let mut actual = Vec::with_capacity(native_value_encoded_len(&value));
            encode_native_value(&mut actual, &value);
            assert_eq!(actual, expected);
            assert_eq!(actual.len(), native_value_encoded_len(&value));
            assert_eq!(actual.len(), actual.capacity());
        }
    }

    // Pins the WireValue<->Value field mapping re-homed here from
    // reddb-server's wire::protocol (ADR 0052, #1061). The conversion is a
    // byte-faithful relocation, so this round-trips every losslessly-mapped
    // variant and locks the supported tag correspondence.
    #[test]
    fn value_wirevalue_field_mapping_round_trips() {
        let cases = [
            (Value::Null, WireValue::Null),
            (Value::Integer(-7), WireValue::I64(-7)),
            (Value::UnsignedInteger(9), WireValue::U64(9)),
            (Value::Float(1.5), WireValue::F64(1.5)),
            (Value::text("hi"), WireValue::Text("hi".to_string())),
            (Value::Blob(vec![1, 2, 3]), WireValue::Bytes(vec![1, 2, 3])),
            (Value::Boolean(true), WireValue::Bool(true)),
            (Value::Timestamp(42), WireValue::Timestamp(42)),
        ];
        for (value, wire) in cases {
            // Value -> WireValue (owned and borrowed paths agree).
            assert_eq!(WireValue::from(value.clone()), wire);
            assert_eq!(WireValue::from(&value), wire);
            // WireValue -> Value round-trips back to the original.
            assert_eq!(Value::try_from(wire.clone()), Ok(value));
        }
    }

    #[test]
    fn wirevalue_timestamp_rejects_i64_overflow() {
        let overflow = WireValue::Timestamp(u64::MAX);
        assert_eq!(
            Value::try_from(overflow),
            Err("timestamp exceeds i64 range")
        );
    }

    #[test]
    fn frame_header_keeps_legacy_length_shape() {
        let mut out = Vec::new();
        write_frame_header(&mut out, MSG_RESULT, 10);
        assert_eq!(out, [11, 0, 0, 0, MSG_RESULT]);
    }

    #[test]
    fn legacy_frame_builders_wrap_payloads() {
        assert_eq!(
            build_legacy_result_frame(b"ok"),
            [3, 0, 0, 0, MSG_RESULT, b'o', b'k']
        );
        assert_eq!(
            build_legacy_error_frame(b"no"),
            [3, 0, 0, 0, MSG_ERROR, b'n', b'o']
        );
        assert_eq!(
            build_legacy_bulk_ok_frame(b"\x02\0\0\0\0\0\0\0"),
            [9, 0, 0, 0, MSG_BULK_OK, 2, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            build_legacy_bulk_stream_ack_frame(),
            [1, 0, 0, 0, MSG_BULK_STREAM_ACK]
        );
        assert_eq!(
            build_legacy_prepared_ok_frame(b"p"),
            [2, 0, 0, 0, MSG_PREPARED_OK, b'p']
        );
        assert_eq!(
            build_legacy_cursor_ok_frame(b"c"),
            [2, 0, 0, 0, MSG_CURSOR_OK, b'c']
        );
        assert_eq!(
            build_legacy_cursor_batch_frame(b"b"),
            [2, 0, 0, 0, MSG_CURSOR_BATCH, b'b']
        );
    }

    #[test]
    fn direct_result_frame_encoder_matches_legacy_bytes_across_growth() {
        for size in [0, 1, 255, 1024, 192 * 1024] {
            let payload = vec![0x71; size];
            let frame = build_legacy_result_frame_with(8, |out| {
                out.extend_from_slice(&payload[..size / 2]);
                out.extend_from_slice(&payload[size / 2..]);
            });
            assert_eq!(frame, build_legacy_result_frame(&payload));
        }
    }

    #[test]
    fn wire_values_round_trip_legacy_tags() {
        let values = [
            WireValue::Null,
            WireValue::I64(-7),
            WireValue::U64(42),
            WireValue::F64(3.5),
            WireValue::Text("hello".to_string()),
            WireValue::Bool(true),
            WireValue::Timestamp(1234),
        ];

        let mut bytes = Vec::new();
        for value in &values {
            encode_value(&mut bytes, value);
        }

        let mut pos = 0;
        assert_eq!(try_decode_value(&bytes, &mut pos), Ok(WireValue::Null));
        assert_eq!(try_decode_value(&bytes, &mut pos), Ok(WireValue::I64(-7)));
        assert_eq!(try_decode_value(&bytes, &mut pos), Ok(WireValue::U64(42)));
        assert_eq!(try_decode_value(&bytes, &mut pos), Ok(WireValue::F64(3.5)));
        assert_eq!(
            try_decode_value(&bytes, &mut pos),
            Ok(WireValue::Text("hello".to_string()))
        );
        assert_eq!(
            try_decode_value(&bytes, &mut pos),
            Ok(WireValue::Bool(true))
        );
        assert_eq!(try_decode_value(&bytes, &mut pos), Ok(WireValue::U64(1234)));
        assert_eq!(pos, bytes.len());
    }

    #[test]
    fn result_payload_header_encodes_columns_and_row_count() {
        let mut bytes = Vec::new();
        let row_count_offset = encode_result_payload_header(&mut bytes, ["id", "name"], 3);

        assert_eq!(
            bytes,
            [
                2, 0, // ncols
                2, 0, b'i', b'd', // "id"
                4, 0, b'n', b'a', b'm', b'e', // "name"
                3, 0, 0, 0, // nrows
            ]
        );
        assert_eq!(row_count_offset, bytes.len() - 4);
        set_result_payload_row_count(&mut bytes, row_count_offset, 5).unwrap();
        assert_eq!(
            &bytes[row_count_offset..row_count_offset + 4],
            &[5, 0, 0, 0]
        );
    }
}
