use super::*;
use serde_json::{Map, Value as JsonValue};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaRebootstrapReadyMarker {
    pub pending_path: PathBuf,
    pub checkpoint_lsn: u64,
    pub timeline: TimelineId,
}

pub fn encode_rebootstrap_ready_marker_json(
    ready: &ReplicaRebootstrapReadyMarker,
) -> RdbFileResult<Vec<u8>> {
    let mut object = Map::new();
    object.insert(
        "pending_path".into(),
        JsonValue::String(ready.pending_path.display().to_string()),
    );
    object.insert(
        "checkpoint_lsn".into(),
        JsonValue::Number(ready.checkpoint_lsn.into()),
    );
    object.insert(
        "timeline".into(),
        JsonValue::Number(ready.timeline.0.into()),
    );
    serde_json::to_vec(&JsonValue::Object(object))
        .map_err(|err| RdbFileError::InvalidOperation(format!("encode rebootstrap marker: {err}")))
}

pub fn decode_rebootstrap_ready_marker_json(
    bytes: &[u8],
) -> RdbFileResult<ReplicaRebootstrapReadyMarker> {
    let value: JsonValue = serde_json::from_slice(bytes).map_err(|err| {
        RdbFileError::InvalidOperation(format!("decode rebootstrap marker: {err}"))
    })?;
    Ok(ReplicaRebootstrapReadyMarker {
        pending_path: value
            .get("pending_path")
            .and_then(JsonValue::as_str)
            .map(PathBuf::from)
            .ok_or_else(|| RdbFileError::InvalidOperation("missing pending_path".into()))?,
        checkpoint_lsn: value
            .get("checkpoint_lsn")
            .and_then(JsonValue::as_u64)
            .ok_or_else(|| RdbFileError::InvalidOperation("missing checkpoint_lsn".into()))?,
        timeline: TimelineId(
            value
                .get("timeline")
                .and_then(JsonValue::as_u64)
                .ok_or_else(|| RdbFileError::InvalidOperation("missing timeline".into()))?,
        ),
    })
}

pub fn write_rebootstrap_ready_marker(
    data_path: impl AsRef<Path>,
    ready: &ReplicaRebootstrapReadyMarker,
) -> RdbFileResult<()> {
    let marker_path = crate::layout::rebootstrap_ready_marker_path(data_path.as_ref());
    write_bytes_atomically(&marker_path, &encode_rebootstrap_ready_marker_json(ready)?)
}

pub fn read_rebootstrap_ready_marker(
    data_path: impl AsRef<Path>,
) -> RdbFileResult<ReplicaRebootstrapReadyMarker> {
    let data_path = data_path.as_ref();
    let marker_path = crate::layout::rebootstrap_ready_marker_path(data_path);
    let ready = decode_rebootstrap_ready_marker_json(&fs::read(marker_path)?)?;
    let expected_pending = crate::layout::rebootstrap_pending_path(data_path);
    if ready.pending_path != expected_pending {
        return Err(RdbFileError::InvalidOperation(
            "invalid rebootstrap pending_path".into(),
        ));
    }
    Ok(ready)
}

pub fn cleanup_rebootstrap_artifacts(data_path: impl AsRef<Path>) {
    let data_path = data_path.as_ref();
    let _ = fs::remove_dir_all(crate::layout::rebootstrap_staging_root(data_path));
    let _ = fs::remove_file(crate::layout::rebootstrap_pending_path(data_path));
    let _ = fs::remove_file(crate::layout::rebootstrap_ready_marker_path(data_path));
    let _ = fs::remove_file(crate::layout::rebootstrap_intent_log_path(data_path));
    let _ = fs::remove_file(crate::layout::rebootstrap_previous_path(data_path));
}

pub fn discard_ready_rebootstrap_marker(data_path: impl AsRef<Path>) -> RdbFileResult<()> {
    let marker_path = crate::layout::rebootstrap_ready_marker_path(data_path.as_ref());
    fs::remove_file(&marker_path)?;
    fsync_parent_dir(&marker_path);
    Ok(())
}

pub fn promote_rebootstrap_pending_database(data_path: impl AsRef<Path>) -> RdbFileResult<()> {
    let data_path = data_path.as_ref();
    let pending_path = crate::layout::rebootstrap_pending_path(data_path);
    let previous_path = crate::layout::rebootstrap_previous_path(data_path);

    if let Some(parent) = data_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let _ = fs::remove_file(&previous_path);
    if data_path.exists() {
        fs::rename(data_path, &previous_path)?;
        fsync_parent_dir(data_path);
    }
    fs::rename(&pending_path, data_path)?;
    fsync_parent_dir(data_path);
    discard_ready_rebootstrap_marker(data_path)?;
    let _ = fs::remove_dir_all(crate::layout::rebootstrap_staging_root(data_path));
    Ok(())
}

fn fsync_parent_dir(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = fs::File::open(parent).and_then(|dir| dir.sync_all());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebootstrap_ready_marker_json_round_trips() {
        let ready = ReplicaRebootstrapReadyMarker {
            pending_path: PathBuf::from("/tmp/main.rebootstrap.pending.rdb"),
            checkpoint_lsn: 42,
            timeline: TimelineId(3),
        };

        let body = encode_rebootstrap_ready_marker_json(&ready).unwrap();
        let text = String::from_utf8(body.clone()).unwrap();
        assert!(text.contains("\"pending_path\""));
        assert!(text.contains("\"checkpoint_lsn\":42"));
        assert!(text.contains("\"timeline\":3"));
        assert_eq!(decode_rebootstrap_ready_marker_json(&body).unwrap(), ready);
    }

    #[test]
    fn rebootstrap_ready_marker_file_round_trips_and_validates_pending_path() {
        let root = std::env::temp_dir().join(format!(
            "reddb-file-rebootstrap-ready-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let data_path = root.join("main.rdb");
        let ready = ReplicaRebootstrapReadyMarker {
            pending_path: crate::layout::rebootstrap_pending_path(&data_path),
            checkpoint_lsn: 99,
            timeline: TimelineId(4),
        };

        write_rebootstrap_ready_marker(&data_path, &ready).unwrap();
        assert_eq!(read_rebootstrap_ready_marker(&data_path).unwrap(), ready);

        let bad = ReplicaRebootstrapReadyMarker {
            pending_path: root.join("other.rdb"),
            checkpoint_lsn: 99,
            timeline: TimelineId(4),
        };
        fs::write(
            crate::layout::rebootstrap_ready_marker_path(&data_path),
            encode_rebootstrap_ready_marker_json(&bad).unwrap(),
        )
        .unwrap();
        assert!(read_rebootstrap_ready_marker(&data_path).is_err());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cleanup_rebootstrap_artifacts_removes_all_sidecars() {
        let root = std::env::temp_dir().join(format!(
            "reddb-file-rebootstrap-cleanup-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let data_path = root.join("main.rdb");
        let staging_root = crate::layout::rebootstrap_staging_root(&data_path);
        fs::create_dir_all(&staging_root).unwrap();
        fs::write(
            crate::layout::rebootstrap_pending_path(&data_path),
            b"pending",
        )
        .unwrap();
        fs::write(
            crate::layout::rebootstrap_ready_marker_path(&data_path),
            b"ready",
        )
        .unwrap();
        fs::write(
            crate::layout::rebootstrap_intent_log_path(&data_path),
            b"intent",
        )
        .unwrap();
        fs::write(
            crate::layout::rebootstrap_previous_path(&data_path),
            b"previous",
        )
        .unwrap();

        cleanup_rebootstrap_artifacts(&data_path);

        assert!(!staging_root.exists());
        assert!(!crate::layout::rebootstrap_pending_path(&data_path).exists());
        assert!(!crate::layout::rebootstrap_ready_marker_path(&data_path).exists());
        assert!(!crate::layout::rebootstrap_intent_log_path(&data_path).exists());
        assert!(!crate::layout::rebootstrap_previous_path(&data_path).exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn promote_rebootstrap_pending_database_swaps_pending_and_cleans_ready_state() {
        let root = std::env::temp_dir().join(format!(
            "reddb-file-rebootstrap-promote-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let data_path = root.join("main.rdb");
        fs::write(&data_path, b"old").unwrap();
        fs::write(crate::layout::rebootstrap_pending_path(&data_path), b"new").unwrap();
        fs::write(
            crate::layout::rebootstrap_ready_marker_path(&data_path),
            b"ready",
        )
        .unwrap();
        let staging_root = crate::layout::rebootstrap_staging_root(&data_path);
        fs::create_dir_all(&staging_root).unwrap();

        promote_rebootstrap_pending_database(&data_path).unwrap();

        assert_eq!(fs::read(&data_path).unwrap(), b"new");
        assert_eq!(
            fs::read(crate::layout::rebootstrap_previous_path(&data_path)).unwrap(),
            b"old"
        );
        assert!(!crate::layout::rebootstrap_pending_path(&data_path).exists());
        assert!(!crate::layout::rebootstrap_ready_marker_path(&data_path).exists());
        assert!(!staging_root.exists());

        let _ = fs::remove_dir_all(root);
    }
}
