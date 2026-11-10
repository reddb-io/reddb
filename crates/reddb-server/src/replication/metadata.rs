//! Durable replication control records share the database WAL/checkpoint fence.
use crate::storage::unified::UnifiedStore;
use std::io;

pub(crate) fn read(store: &UnifiedStore, key: &str) -> io::Result<Option<Vec<u8>>> {
    match store
        .try_get_storage_metadata(key)
        .map_err(io::Error::other)?
    {
        None => Ok(None),
        Some(reddb_types::Value::Text(value)) => hex::decode(value.as_ref())
            .map(Some)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid replication control record: {key}"),
        )),
    }
}

pub(crate) fn write(store: &UnifiedStore, key: &str, bytes: &[u8]) -> io::Result<()> {
    if UnifiedStore::deferred_store_wal_capture_active() {
        return Err(io::Error::other(
            "replication control metadata cannot join an uncommitted transaction",
        ));
    }
    let _publication = store.vault_publication_lock.lock();
    timeline_crash_point(key, "timeline_before_wal");
    store
        .try_set_storage_metadata(key, &crate::serde_json::Value::String(hex::encode(bytes)))
        .map_err(io::Error::other)?;
    timeline_crash_point(key, "timeline_after_wal");
    store.sync_control_metadata().map_err(io::Error::other)?;
    timeline_crash_point(key, "timeline_after_sync");
    Ok(())
}

fn timeline_crash_point(key: &str, point: &str) {
    if key == TIMELINE
        && std::env::var("REDDB_PRIMARY_REPLICA_CRASH_AT")
            .ok()
            .as_deref()
            == Some(point)
    {
        std::process::exit(173);
    }
}

pub(crate) const SLOTS: &str = "replication.slots";
pub(crate) const TIMELINE: &str = "replication.timeline";

/// Call only after the replacement record's WAL durability barrier succeeds.
pub(crate) fn retire_legacy(path: &std::path::Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) fn timeline(
    store: &UnifiedStore,
    legacy_path: &std::path::Path,
) -> io::Result<reddb_file::TimelineHistory> {
    if let Some(bytes) = read(store, TIMELINE)? {
        return reddb_file::TimelineHistory::decode(&bytes).map_err(io::Error::other);
    }
    match reddb_file::TimelineHistory::read_from_path(legacy_path) {
        Ok(history) => Ok(history),
        Err(reddb_file::RdbFileError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(
            reddb_file::TimelineHistory::new(crate::utils::now_unix_millis()),
        ),
        Err(error) => Err(io::Error::other(error)),
    }
}
