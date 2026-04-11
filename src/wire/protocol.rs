/// RedDB Wire Protocol — binary TCP, zero JSON overhead.
///
/// Frame: [total_len: u32 LE][msg_type: u8][payload...]
///
/// Message types (client → server):
///   0x01 Query       [sql_bytes...]
///   0x04 BulkInsert  [coll_len:u16][coll_bytes][n:u32][json_len:u32 + json_bytes]...
///
/// Message types (server → client):
///   0x02 Result      [ncols:u16][col_name_len:u16 + col_name]...[nrows:u32][row...]
///                     row = [val_type:u8 + val_data]... per column
///   0x03 Error       [error_bytes...]
///   0x05 BulkOk      [count:u64]

// --- Message type constants ---
pub const MSG_QUERY: u8 = 0x01;
pub const MSG_RESULT: u8 = 0x02;
pub const MSG_ERROR: u8 = 0x03;
pub const MSG_BULK_INSERT: u8 = 0x04;
pub const MSG_BULK_OK: u8 = 0x05;

// --- Value type tags ---
pub const VAL_NULL: u8 = 0;
pub const VAL_I64: u8 = 1;
pub const VAL_F64: u8 = 2;
pub const VAL_TEXT: u8 = 3;
pub const VAL_BOOL: u8 = 4;
pub const VAL_U64: u8 = 5;

use crate::storage::schema::Value;

/// Write a frame header: [total_len: u32 LE][msg_type: u8]
#[inline]
pub fn write_frame_header(buf: &mut Vec<u8>, msg_type: u8, payload_len: u32) {
    let total = payload_len + 1; // +1 for msg_type
    buf.extend_from_slice(&total.to_le_bytes());
    buf.push(msg_type);
}

/// Encode a Value to wire format bytes, appending to buf.
#[inline]
pub fn encode_value(buf: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Null => buf.push(VAL_NULL),
        Value::Integer(n) => {
            buf.push(VAL_I64);
            buf.extend_from_slice(&n.to_le_bytes());
        }
        Value::UnsignedInteger(n) => {
            buf.push(VAL_U64);
            buf.extend_from_slice(&n.to_le_bytes());
        }
        Value::Float(f) => {
            buf.push(VAL_F64);
            buf.extend_from_slice(&f.to_le_bytes());
        }
        Value::Text(s) => {
            buf.push(VAL_TEXT);
            let bytes = s.as_bytes();
            buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(bytes);
        }
        Value::Boolean(b) => {
            buf.push(VAL_BOOL);
            buf.push(*b as u8);
        }
        Value::Timestamp(t) => {
            buf.push(VAL_U64);
            buf.extend_from_slice(&t.to_le_bytes());
        }
        _ => buf.push(VAL_NULL),
    }
}

/// Decode a Value from wire bytes at the given position.
#[inline]
pub fn decode_value(data: &[u8], pos: &mut usize) -> Value {
    if *pos >= data.len() {
        return Value::Null;
    }
    let tag = data[*pos];
    *pos += 1;
    match tag {
        VAL_NULL => Value::Null,
        VAL_I64 => {
            let v = i64::from_le_bytes(data[*pos..*pos + 8].try_into().unwrap_or([0; 8]));
            *pos += 8;
            Value::Integer(v)
        }
        VAL_U64 => {
            let v = u64::from_le_bytes(data[*pos..*pos + 8].try_into().unwrap_or([0; 8]));
            *pos += 8;
            Value::UnsignedInteger(v)
        }
        VAL_F64 => {
            let v = f64::from_le_bytes(data[*pos..*pos + 8].try_into().unwrap_or([0; 8]));
            *pos += 8;
            Value::Float(v)
        }
        VAL_TEXT => {
            let len =
                u32::from_le_bytes(data[*pos..*pos + 4].try_into().unwrap_or([0; 4])) as usize;
            *pos += 4;
            let s = String::from_utf8_lossy(&data[*pos..*pos + len]).to_string();
            *pos += len;
            Value::Text(s)
        }
        VAL_BOOL => {
            let v = data[*pos] != 0;
            *pos += 1;
            Value::Boolean(v)
        }
        _ => Value::Null,
    }
}

/// Encode a column name to wire format.
#[inline]
pub fn encode_column_name(buf: &mut Vec<u8>, name: &str) {
    let bytes = name.as_bytes();
    buf.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    buf.extend_from_slice(bytes);
}

/// Decode a column name from wire bytes.
#[inline]
pub fn decode_column_name(data: &[u8], pos: &mut usize) -> String {
    let len = u16::from_le_bytes(data[*pos..*pos + 2].try_into().unwrap_or([0; 2])) as usize;
    *pos += 2;
    let s = String::from_utf8_lossy(&data[*pos..*pos + len]).to_string();
    *pos += len;
    s
}
