//! Canonical native metadata bytes and entity-record adapter, shared with WASM.
use super::entity::{EntityId, UnifiedEntity};
use super::metadata::{Metadata, MetadataValue};

fn read_entity(data: &[u8], version: u32) -> Result<UnifiedEntity, String> {
    super::entity_codec::read_entity_binary(data, &mut 0, version).map_err(invalid)
}

fn invalid(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn read_count(data: &[u8], position: &mut usize, bytes_min: usize) -> Result<usize, String> {
    let count = usize::try_from(read_u32(data, position)?).map_err(invalid)?;
    if count > data.len().saturating_sub(*position) / bytes_min {
        return Err(invalid("metadata count exceeds the remaining payload"));
    }
    Ok(count)
}

/// Preserve native entity/value and optional typed metadata framing.
pub fn serialize_entity_record(
    entity: &UnifiedEntity,
    metadata: Option<&Metadata>,
    version: u32,
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(256);
    super::entity_codec::write_entity_binary(&mut bytes, entity, version);
    let entity = bytes;
    if metadata.is_some_and(|metadata| !metadata.fields.is_empty()) {
        reddb_file::encode_native_entity_record_frame(&entity, Some(&serialize_metadata(metadata)))
    } else {
        reddb_file::encode_native_entity_record_frame(&entity, None)
    }
}

/// Decode the existing framed record, retaining the pre-V7 entity-only fallback.
pub fn deserialize_entity_record(
    data: &[u8],
    version: u32,
) -> Result<(UnifiedEntity, Option<Metadata>), String> {
    let Some(frame) = reddb_file::decode_native_entity_record_frame(data).map_err(invalid)? else {
        return read_entity(data, version).map(|entity| (entity, None));
    };
    let entity = read_entity(frame.entity, version)?;
    let metadata = if frame.metadata.is_empty() {
        None
    } else {
        let metadata = deserialize_metadata(frame.metadata)?;
        (!metadata.is_empty()).then_some(metadata)
    };
    Ok((entity, metadata))
}

pub fn serialize_metadata(metadata: Option<&Metadata>) -> Vec<u8> {
    let Some(metadata) = metadata else {
        return Vec::new();
    };
    if metadata.is_empty() {
        return Vec::new();
    }

    let mut entries: Vec<_> = metadata.iter().collect();
    entries.sort_by_key(|(a, _)| *a);

    let mut buf = Vec::new();
    buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (key, value) in entries {
        write_string(&mut buf, key);
        write_metadata_value(&mut buf, value);
    }
    buf
}

pub fn deserialize_metadata(data: &[u8]) -> Result<Metadata, String> {
    let mut pos = 0usize;
    let count = read_count(data, &mut pos, 5)?;
    let mut metadata = Metadata::new();
    for _ in 0..count {
        let key = read_string(data, &mut pos)?;
        let value = read_metadata_value(data, &mut pos, 0)?;
        metadata.set(key, value);
    }
    Ok(metadata)
}

fn write_string(buf: &mut Vec<u8>, value: &str) {
    reddb_file::encode_native_len_prefixed_str(buf, value);
}

fn write_bytes(buf: &mut Vec<u8>, value: &[u8]) {
    reddb_file::encode_native_len_prefixed_bytes(buf, value);
}

fn write_ref_target(buf: &mut Vec<u8>, target: &super::metadata::RefTarget) {
    use super::metadata::RefTarget;

    match target {
        RefTarget::TableRow { table, row_id } => {
            buf.push(0);
            write_string(buf, table);
            buf.extend_from_slice(&row_id.to_le_bytes());
        }
        RefTarget::Node {
            collection,
            node_id,
        } => {
            buf.push(1);
            write_string(buf, collection);
            buf.extend_from_slice(&node_id.raw().to_le_bytes());
        }
        RefTarget::Edge {
            collection,
            edge_id,
        } => {
            buf.push(2);
            write_string(buf, collection);
            buf.extend_from_slice(&edge_id.raw().to_le_bytes());
        }
        RefTarget::Vector {
            collection,
            vector_id,
        } => {
            buf.push(3);
            write_string(buf, collection);
            buf.extend_from_slice(&vector_id.raw().to_le_bytes());
        }
        RefTarget::Entity {
            collection,
            entity_id,
        } => {
            buf.push(4);
            write_string(buf, collection);
            buf.extend_from_slice(&entity_id.raw().to_le_bytes());
        }
    }
}

fn write_metadata_value(buf: &mut Vec<u8>, value: &MetadataValue) {
    match value {
        MetadataValue::Null => buf.push(0),
        MetadataValue::Bool(v) => {
            buf.push(1);
            buf.push(u8::from(*v));
        }
        MetadataValue::Int(v) => {
            buf.push(2);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        MetadataValue::Float(v) => {
            buf.push(3);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        MetadataValue::String(v) => {
            buf.push(4);
            write_string(buf, v);
        }
        MetadataValue::Bytes(v) => {
            buf.push(5);
            write_bytes(buf, v);
        }
        MetadataValue::Array(values) => {
            buf.push(6);
            buf.extend_from_slice(&(values.len() as u32).to_le_bytes());
            for value in values {
                write_metadata_value(buf, value);
            }
        }
        MetadataValue::Object(values) => {
            buf.push(7);
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_by_key(|(a, _)| *a);
            buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
            for (key, value) in entries {
                write_string(buf, key);
                write_metadata_value(buf, value);
            }
        }
        MetadataValue::Timestamp(v) => {
            buf.push(8);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        MetadataValue::Geo { lat, lon } => {
            buf.push(9);
            buf.extend_from_slice(&lat.to_le_bytes());
            buf.extend_from_slice(&lon.to_le_bytes());
        }
        MetadataValue::Reference(target) => {
            buf.push(10);
            write_ref_target(buf, target);
        }
        MetadataValue::References(targets) => {
            buf.push(11);
            buf.extend_from_slice(&(targets.len() as u32).to_le_bytes());
            for target in targets {
                write_ref_target(buf, target);
            }
        }
    }
}

fn read_exact_slice<'a>(data: &'a [u8], pos: &mut usize, len: usize) -> Result<&'a [u8], String> {
    if *pos > data.len() || len > data.len() - *pos {
        return Err(invalid("truncated metadata payload".to_string()));
    }
    let slice = &data[*pos..*pos + len];
    *pos += len;
    Ok(slice)
}

fn read_u32(data: &[u8], pos: &mut usize) -> Result<u32, String> {
    let bytes = read_exact_slice(data, pos, 4)?;
    let mut raw = [0u8; 4];
    raw.copy_from_slice(bytes);
    Ok(u32::from_le_bytes(raw))
}

fn read_u64(data: &[u8], pos: &mut usize) -> Result<u64, String> {
    let bytes = read_exact_slice(data, pos, 8)?;
    let mut raw = [0u8; 8];
    raw.copy_from_slice(bytes);
    Ok(u64::from_le_bytes(raw))
}

fn read_i64(data: &[u8], pos: &mut usize) -> Result<i64, String> {
    let bytes = read_exact_slice(data, pos, 8)?;
    let mut raw = [0u8; 8];
    raw.copy_from_slice(bytes);
    Ok(i64::from_le_bytes(raw))
}

fn read_f64(data: &[u8], pos: &mut usize) -> Result<f64, String> {
    let bytes = read_exact_slice(data, pos, 8)?;
    let mut raw = [0u8; 8];
    raw.copy_from_slice(bytes);
    Ok(f64::from_le_bytes(raw))
}

fn read_u8(data: &[u8], pos: &mut usize) -> Result<u8, String> {
    let bytes = read_exact_slice(data, pos, 1)?;
    Ok(bytes[0])
}

fn read_string(data: &[u8], pos: &mut usize) -> Result<String, String> {
    reddb_file::decode_native_len_prefixed_string(data, pos).map_err(|err| invalid(err.to_string()))
}

fn read_bytes(data: &[u8], pos: &mut usize) -> Result<Vec<u8>, String> {
    reddb_file::decode_native_len_prefixed_bytes(data, pos)
        .map(|bytes| bytes.to_vec())
        .map_err(|err| invalid(err.to_string()))
}

fn read_ref_target(data: &[u8], pos: &mut usize) -> Result<super::metadata::RefTarget, String> {
    use super::metadata::RefTarget;

    match read_u8(data, pos)? {
        0 => Ok(RefTarget::TableRow {
            table: read_string(data, pos)?,
            row_id: read_u64(data, pos)?,
        }),
        1 => Ok(RefTarget::Node {
            collection: read_string(data, pos)?,
            node_id: EntityId::new(read_u64(data, pos)?),
        }),
        2 => Ok(RefTarget::Edge {
            collection: read_string(data, pos)?,
            edge_id: EntityId::new(read_u64(data, pos)?),
        }),
        3 => Ok(RefTarget::Vector {
            collection: read_string(data, pos)?,
            vector_id: EntityId::new(read_u64(data, pos)?),
        }),
        4 => Ok(RefTarget::Entity {
            collection: read_string(data, pos)?,
            entity_id: EntityId::new(read_u64(data, pos)?),
        }),
        tag => Err(invalid(format!("unknown metadata ref target tag {tag}"))),
    }
}

fn read_metadata_value(data: &[u8], pos: &mut usize, depth: u32) -> Result<MetadataValue, String> {
    if depth > reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
        return Err(invalid("metadata nesting exceeds the maximum depth"));
    }
    match read_u8(data, pos)? {
        0 => Ok(MetadataValue::Null),
        1 => Ok(MetadataValue::Bool(read_u8(data, pos)? != 0)),
        2 => Ok(MetadataValue::Int(read_i64(data, pos)?)),
        3 => Ok(MetadataValue::Float(read_f64(data, pos)?)),
        4 => Ok(MetadataValue::String(read_string(data, pos)?)),
        5 => Ok(MetadataValue::Bytes(read_bytes(data, pos)?)),
        6 => {
            let count = read_count(data, pos, 1)?;
            let mut values = Vec::with_capacity(count);
            for _ in 0..count {
                values.push(read_metadata_value(data, pos, depth + 1)?);
            }
            Ok(MetadataValue::Array(values))
        }
        7 => {
            let count = read_count(data, pos, 5)?;
            let mut values = std::collections::HashMap::with_capacity(count);
            for _ in 0..count {
                let key = read_string(data, pos)?;
                let value = read_metadata_value(data, pos, depth + 1)?;
                values.insert(key, value);
            }
            Ok(MetadataValue::Object(values))
        }
        8 => Ok(MetadataValue::Timestamp(read_u64(data, pos)?)),
        9 => Ok(MetadataValue::Geo {
            lat: read_f64(data, pos)?,
            lon: read_f64(data, pos)?,
        }),
        10 => Ok(MetadataValue::Reference(read_ref_target(data, pos)?)),
        11 => {
            let count = read_count(data, pos, 13)?;
            let mut targets = Vec::with_capacity(count);
            for _ in 0..count {
                targets.push(read_ref_target(data, pos)?);
            }
            Ok(MetadataValue::References(targets))
        }
        tag => Err(invalid(format!("unknown metadata value tag {tag}"))),
    }
}
