//! String encoding helper functions for record serialization

use crate::storage::primitives::encoding::{read_varu32, write_varu32, DecodeError};

pub fn write_optional_string(buf: &mut Vec<u8>, value: &Option<String>) {
    match value {
        Some(text) => {
            buf.push(1);
            write_string(buf, text);
        }
        None => buf.push(0),
    }
}

pub fn read_optional_string(bytes: &[u8], pos: &mut usize) -> Result<Option<String>, DecodeError> {
    if *pos >= bytes.len() {
        return Err(DecodeError("unexpected eof (optional string flag)"));
    }
    let flag = bytes[*pos];
    *pos += 1;
    if flag == 0 {
        return Ok(None);
    }
    read_string(bytes, pos).map(Some)
}

pub fn write_string(buf: &mut Vec<u8>, value: &str) {
    let data = value.as_bytes();
    write_varu32(buf, data.len() as u32);
    buf.extend_from_slice(data);
}

pub fn read_string(bytes: &[u8], pos: &mut usize) -> Result<String, DecodeError> {
    let len = read_varu32(bytes, pos)? as usize;
    if bytes.len() < *pos + len {
        return Err(DecodeError("truncated string"));
    }
    let slice = &bytes[*pos..*pos + len];
    *pos += len;
    Ok(String::from_utf8_lossy(slice).to_string())
}

/// Helper to write length-prefixed strings (u16 length)
pub fn write_string_u16(buf: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    let len = bytes.len().min(65535) as u16;
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(&bytes[..len as usize]);
}

/// Helper to read length-prefixed strings (u16 length)
pub fn read_string_u16(buf: &[u8], offset: &mut usize) -> Option<String> {
    if buf.len() < *offset + 2 {
        return None;
    }

    let len = u16::from_le_bytes([buf[*offset], buf[*offset + 1]]) as usize;
    *offset += 2;

    if buf.len() < *offset + len {
        return None;
    }

    let s = String::from_utf8(buf[*offset..*offset + len].to_vec()).ok()?;
    *offset += len;

    Some(s)
}
