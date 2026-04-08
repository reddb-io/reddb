use super::*;

pub(super) fn json_u64(value: u64) -> JsonValue {
    JsonValue::String(value.to_string())
}

pub(super) fn json_u128(value: u128) -> JsonValue {
    JsonValue::String(value.to_string())
}

pub(super) fn json_required<'a>(
    object: &'a Map<String, JsonValue>,
    key: &str,
) -> io::Result<&'a JsonValue> {
    object
        .get(key)
        .ok_or_else(|| invalid_data(format!("missing field '{key}'")))
}

pub(super) fn json_string_required(object: &Map<String, JsonValue>, key: &str) -> io::Result<String> {
    json_required(object, key)?
        .as_str()
        .map(|value| value.to_string())
        .ok_or_else(|| invalid_data(format!("field '{key}' must be a string")))
}

pub(super) fn json_bool_required(object: &Map<String, JsonValue>, key: &str) -> io::Result<bool> {
    json_required(object, key)?
        .as_bool()
        .ok_or_else(|| invalid_data(format!("field '{key}' must be a bool")))
}

pub(super) fn json_u8_required(object: &Map<String, JsonValue>, key: &str) -> io::Result<u8> {
    json_u8_value(json_required(object, key)?)
}

pub(super) fn json_u32_required(object: &Map<String, JsonValue>, key: &str) -> io::Result<u32> {
    json_u32_value(json_required(object, key)?)
}

pub(super) fn json_u64_required(object: &Map<String, JsonValue>, key: &str) -> io::Result<u64> {
    json_u64_value(json_required(object, key)?)
}

pub(super) fn json_u128_required(object: &Map<String, JsonValue>, key: &str) -> io::Result<u128> {
    json_u128_value(json_required(object, key)?)
}

pub(super) fn json_usize_required(object: &Map<String, JsonValue>, key: &str) -> io::Result<usize> {
    json_usize_value(json_required(object, key)?)
}

pub(super) fn json_u8_value(value: &JsonValue) -> io::Result<u8> {
    if let Some(text) = value.as_str() {
        return text
            .parse::<u8>()
            .map_err(|_| invalid_data("invalid u8 string value"));
    }
    value
        .as_i64()
        .and_then(|value| u8::try_from(value).ok())
        .ok_or_else(|| invalid_data("invalid u8 value"))
}

pub(super) fn json_u32_value(value: &JsonValue) -> io::Result<u32> {
    if let Some(text) = value.as_str() {
        return text
            .parse::<u32>()
            .map_err(|_| invalid_data("invalid u32 string value"));
    }
    value
        .as_i64()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| invalid_data("invalid u32 value"))
}

pub(super) fn json_u64_value(value: &JsonValue) -> io::Result<u64> {
    if let Some(text) = value.as_str() {
        return text
            .parse::<u64>()
            .map_err(|_| invalid_data("invalid u64 string value"));
    }
    value
        .as_i64()
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| invalid_data("invalid u64 value"))
}

pub(super) fn json_u128_value(value: &JsonValue) -> io::Result<u128> {
    if let Some(text) = value.as_str() {
        return text
            .parse::<u128>()
            .map_err(|_| invalid_data("invalid u128 string value"));
    }
    value
        .as_i64()
        .and_then(|value| u128::try_from(value).ok())
        .ok_or_else(|| invalid_data("invalid u128 value"))
}

pub(super) fn json_usize_value(value: &JsonValue) -> io::Result<usize> {
    if let Some(text) = value.as_str() {
        return text
            .parse::<usize>()
            .map_err(|_| invalid_data("invalid usize string value"));
    }
    value
        .as_i64()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| invalid_data("invalid usize value"))
}

pub(super) fn expect_object<'a>(
    value: &'a JsonValue,
    context: &str,
) -> io::Result<&'a Map<String, JsonValue>> {
    value
        .as_object()
        .ok_or_else(|| invalid_data(format!("{context} must be an object")))
}

pub(super) fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub(super) fn unix_ms_now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

pub(super) fn sanitize_export_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "export".to_string()
    } else {
        sanitized
    }
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
                    block: manifest_block_reference(*previous_root, sequence),
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
            block: manifest_block_reference(*root, sequence),
            snapshot_min: sequence,
            snapshot_max: None,
        });
    }

    events.push(ManifestEvent {
        collection: "__system__".to_string(),
        object_key: format!("superblock:{sequence}"),
        kind: ManifestEventKind::Checkpoint,
        block: manifest_block_reference(sequence, sequence),
        snapshot_min: sequence,
        snapshot_max: None,
    });

    events
}

pub(super) fn manifest_block_reference(root: u64, sequence: u64) -> BlockReference {
    BlockReference {
        index: root,
        checksum: ((root as u128) << 64) | sequence as u128,
    }
}
