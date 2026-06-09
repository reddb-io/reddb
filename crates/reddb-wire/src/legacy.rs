//! Legacy RedDB binary protocol vocabulary.
//!
//! This is the pre-RedWire frame shape still used by the direct TCP
//! handlers and fast paths:
//!
//! ```text
//! [total_len: u32 LE][msg_type: u8][payload...]
//! ```
//!
//! The crate owns the byte-level contract. Engine-specific conversion
//! to storage values belongs in `reddb-server`.

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
