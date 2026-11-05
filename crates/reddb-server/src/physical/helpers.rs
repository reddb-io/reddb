use super::*;

pub(super) fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub(super) fn unix_ms_now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

pub(super) fn trim_snapshot_history(snapshots: &mut Vec<SnapshotDescriptor>, retention: usize) {
    let retention = retention.max(1);
    if snapshots.len() > retention {
        let drop_count = snapshots.len() - retention;
        snapshots.drain(0..drop_count);
    }
}

pub(super) fn trim_manifest_history(events: &mut Vec<ManifestEvent>) {
    if events.len() > DEFAULT_MANIFEST_EVENT_HISTORY {
        let drop_count = events.len() - DEFAULT_MANIFEST_EVENT_HISTORY;
        events.drain(0..drop_count);
    }
}

pub(super) fn build_manifest_events(
    previous_roots: Option<&BTreeMap<String, u64>>,
    current_roots: &BTreeMap<String, u64>,
    sequence: u64,
) -> Vec<ManifestEvent> {
    let mut events = Vec::new();

    if let Some(previous_roots) = previous_roots {
        for (collection, previous_root) in previous_roots {
            if !current_roots.contains_key(collection) {
                events.push(ManifestEvent {
                    collection: collection.clone(),
                    object_key: collection.clone(),
                    kind: ManifestEventKind::Remove,
                    block: reddb_file::physical_manifest_block_reference(*previous_root, sequence),
                    snapshot_min: sequence,
                    snapshot_max: Some(sequence),
                });
            }
        }
    }

    for (collection, root) in current_roots {
        let kind = match previous_roots.and_then(|roots| roots.get(collection)) {
            None => ManifestEventKind::Insert,
            Some(previous_root) if previous_root != root => ManifestEventKind::Update,
            Some(_) => continue,
        };

        events.push(ManifestEvent {
            collection: collection.clone(),
            object_key: collection.clone(),
            kind,
            block: reddb_file::physical_manifest_block_reference(*root, sequence),
            snapshot_min: sequence,
            snapshot_max: None,
        });
    }

    events.push(reddb_file::physical_superblock_checkpoint_event(sequence));

    events
}
