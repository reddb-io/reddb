use super::*;

pub(super) fn native_manifest_kind_to_byte(kind: ManifestEventKind) -> u8 {
    match kind {
        ManifestEventKind::Insert => 1,
        ManifestEventKind::Update => 2,
        ManifestEventKind::Remove => 3,
        ManifestEventKind::Checkpoint => 4,
    }
}

pub(super) fn native_manifest_kind_from_byte(value: u8) -> &'static str {
    match value {
        1 => "insert",
        2 => "update",
        3 => "remove",
        4 => "checkpoint",
        _ => "unknown",
    }
}

pub(super) fn push_native_string(data: &mut Vec<u8>, value: &str) {
    let bytes = value.as_bytes();
    let len = bytes.len().min(u16::MAX as usize) as u16;
    data.extend_from_slice(&len.to_le_bytes());
    data.extend_from_slice(&bytes[..len as usize]);
}

pub(super) fn read_native_string(content: &[u8], pos: &mut usize) -> Result<String, StoreError> {
    if *pos + 2 > content.len() {
        return Err(StoreError::Serialization(
            "truncated native string length".to_string(),
        ));
    }
    let len = u16::from_le_bytes([content[*pos], content[*pos + 1]]) as usize;
    *pos += 2;
    if *pos + len > content.len() {
        return Err(StoreError::Serialization(
            "truncated native string payload".to_string(),
        ));
    }
    let value = String::from_utf8(content[*pos..*pos + len].to_vec())
        .map_err(|err| StoreError::Serialization(err.to_string()))?;
    *pos += len;
    Ok(value)
}

pub(super) fn push_native_string_list(data: &mut Vec<u8>, values: &[String]) {
    let count = values.len().min(u16::MAX as usize) as u16;
    data.extend_from_slice(&count.to_le_bytes());
    for value in values.iter().take(count as usize) {
        push_native_string(data, value);
    }
}

pub(super) fn read_native_string_list(content: &[u8], pos: &mut usize) -> Result<Vec<String>, StoreError> {
    if *pos + 2 > content.len() {
        return Err(StoreError::Serialization(
            "truncated native string list count".to_string(),
        ));
    }
    let count = u16::from_le_bytes([content[*pos], content[*pos + 1]]) as usize;
    *pos += 2;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(read_native_string(content, pos)?);
    }
    Ok(values)
}

