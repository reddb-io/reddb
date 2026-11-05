//! Streaming recovery of the existing native store dump.
//!
//! Hosts rebuild their own collection managers, derived indexes and catalog.
//! Callbacks must populate a private recovery target: an error can follow an
//! earlier callback, so a partially restored target must never be published.
use super::{entity::UnifiedEntity, metadata::Metadata, metadata_codec::deserialize_entity_record};

pub trait SnapshotVisitor {
    fn collection(&mut self, name: &str) -> Result<(), String>;
    fn entity(
        &mut self,
        collection: &str,
        entity: UnifiedEntity,
        metadata: Option<Metadata>,
    ) -> Result<(), String>;
    fn cross_ref(&mut self, cross_ref: reddb_file::NativeDumpCrossRef) -> Result<(), String>;
    fn auxiliary(&mut self, bytes: &[u8]) -> Result<(), String>;
}

/// Validate integrity before visiting collections, then decode one record at a
/// time. Empty collections and the optional legacy tails retain native behavior.
pub fn visit_snapshot(bytes: &[u8], visitor: &mut impl SnapshotVisitor) -> Result<u32, String> {
    let version = reddb_file::decode_native_store_header(bytes).map_err(|err| err.to_string())?;
    let bytes = reddb_file::native_store_payload(bytes, version).map_err(|err| err.to_string())?;
    let mut position = 8;
    let count = reddb_file::decode_native_dump_count(bytes, &mut position)
        .map_err(|err| err.to_string())?;
    // Each collection header has at least a name length and an entity count.
    check_count(count, bytes, position, 2)?;
    for _ in 0..count {
        let collection = reddb_file::decode_native_dump_collection_header(bytes, &mut position)
            .map_err(|err| err.to_string())?;
        visitor.collection(&collection.name)?;
        // V7+ records have a u32 length; legacy entities use at least one byte.
        check_count(
            collection.entity_count,
            bytes,
            position,
            if version >= 7 { 4 } else { 1 },
        )?;
        for _ in 0..collection.entity_count {
            let (entity, metadata) = if version >= reddb_file::STORE_VERSION_V7 {
                let record = reddb_file::decode_native_dump_entity_record(bytes, &mut position)
                    .map_err(|err| err.to_string())?;
                deserialize_entity_record(record, version)
                    .map_err(|err| format!("Entity record deserialization error: {err}"))?
            } else {
                (
                    super::entity_codec::read_entity_binary(bytes, &mut position, version)
                        .map_err(|err| err.to_string())?,
                    None,
                )
            };
            visitor.entity(&collection.name, entity, metadata)?;
        }
    }
    if position < bytes.len() {
        let count = reddb_file::decode_native_dump_count(bytes, &mut position)
            .map_err(|err| err.to_string())?;
        // Source, target, type and target-collection length are each >= 1 byte.
        check_count(count, bytes, position, 4)?;
        for _ in 0..count {
            let cross_ref = reddb_file::decode_native_dump_cross_ref(bytes, &mut position)
                .map_err(|err| err.to_string())?;
            visitor.cross_ref(cross_ref)?;
        }
    }
    if version >= reddb_file::STORE_VERSION_V10 && position < bytes.len() {
        let auxiliary = reddb_file::decode_native_dump_entity_record(bytes, &mut position)
            .map_err(|err| err.to_string())?;
        if !auxiliary.is_empty() {
            visitor.auxiliary(auxiliary)?;
        }
    }
    Ok(version)
}

fn check_count(count: u32, bytes: &[u8], position: usize, size_min: usize) -> Result<(), String> {
    if u64::from(count) > (bytes.len().saturating_sub(position) / size_min) as u64 {
        return Err("snapshot count exceeds the remaining payload".into());
    }
    Ok(())
}
