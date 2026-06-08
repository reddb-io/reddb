use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use reddb_file::{ReplicationSlot, ReplicationSlotInvalidationCause};
use tracing::warn;

pub(super) fn load_replication_slots(
    path: Option<&Path>,
    now_ms: u128,
) -> BTreeMap<String, ReplicationSlot> {
    let Some(path) = path else {
        return BTreeMap::new();
    };
    match reddb_file::ReplicationSlotCatalog::read_legacy_json_from_path(path, now_ms) {
        Ok(catalog) => catalog
            .slots
            .into_iter()
            .map(|slot| (slot.replica_id.clone(), slot))
            .collect(),
        Err(reddb_file::RdbFileError::Io(err)) if err.kind() == io::ErrorKind::NotFound => {
            BTreeMap::new()
        }
        Err(err) => {
            warn!(
                target: "reddb::replication::slots",
                path = %path.display(),
                error = %err,
                "failed to decode replication slot store"
            );
            BTreeMap::new()
        }
    }
}

pub(super) fn load_replication_slot_catalog(
    path: Option<&Path>,
    now_ms: u128,
) -> BTreeMap<String, ReplicationSlot> {
    let Some(path) = path else {
        return BTreeMap::new();
    };
    let catalog = match reddb_file::ReplicationSlotCatalog::read_from_path(path) {
        Ok(catalog) => catalog,
        Err(reddb_file::RdbFileError::Io(err)) if err.kind() == io::ErrorKind::NotFound => {
            return BTreeMap::new();
        }
        Err(err) => {
            warn!(
                target: "reddb::replication::slots",
                path = %path.display(),
                error = %err,
                "failed to read binary replication slot catalog"
            );
            return BTreeMap::new();
        }
    };
    catalog
        .slots
        .into_iter()
        .map(|slot| {
            let mut slot = slot;
            if slot.last_seen_at_unix_ms == 0 {
                slot.last_seen_at_unix_ms = now_ms;
            }
            if !slot.active && slot.invalidation_reason.is_none() {
                slot.invalidation_reason = Some(ReplicationSlotInvalidationCause::Horizon);
                slot.invalidated_at_unix_ms = Some(now_ms);
            }
            (slot.replica_id.clone(), slot)
        })
        .collect()
}

pub(super) fn persist_replication_slots(
    path: Option<&Path>,
    slots: &BTreeMap<String, ReplicationSlot>,
) -> io::Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    slot_catalog_from_map(slots)
        .and_then(|catalog| catalog.write_legacy_json_to_path(path))
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))
}

pub(super) fn persist_replication_slot_catalog(
    path: Option<&Path>,
    slots: &BTreeMap<String, ReplicationSlot>,
) -> io::Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    slot_catalog_from_map(slots)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?
        .write_to_path(path)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))
}

fn slot_catalog_from_map(
    slots: &BTreeMap<String, ReplicationSlot>,
) -> reddb_file::RdbFileResult<reddb_file::ReplicationSlotCatalog> {
    let mut catalog = reddb_file::ReplicationSlotCatalog::new(reddb_file::TimelineId::initial());
    for slot in slots.values() {
        catalog.upsert(slot.clone())?;
    }
    Ok(catalog)
}
