use std::collections::BTreeMap;

use reddb_file::{
    encode_append_only_segment, AppendOnlySegmentCodec, AppendOnlySegmentRow, OperationalManifest,
};

#[test]
fn incremental_manifest_limits_reads_and_rejects_a_replaced_generation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("data.rdb");
    let manifest = OperationalManifest::for_db_path(&path);
    manifest.recover_or_bootstrap(&[]).unwrap();
    let bytes = encode_append_only_segment(
        AppendOnlySegmentCodec::None,
        &[AppendOnlySegmentRow {
            primary_key: b"1".to_vec(),
            payload: b"row".to_vec(),
        }],
    )
    .unwrap();
    let entry = manifest
        .publish_append_only_segment_with_retention_bounds(
            "events",
            1,
            AppendOnlySegmentCodec::None,
            &bytes,
            Some(1),
            Some(1),
        )
        .unwrap();
    let current = manifest.current_manifest_path_for_test();
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&current).unwrap()).unwrap();
    value["append_only_segments"][0]["primary_bloom"]["bits_hex"] =
        serde_json::Value::String("ab".repeat(600_000));
    value.as_object_mut().unwrap().remove("checksum");
    let checksum = crc32fast::hash(&serde_json::to_vec(&value).unwrap());
    value["checksum"] = serde_json::Value::String(format!("{checksum:08x}"));
    std::fs::write(&current, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    assert!(std::fs::metadata(&current).unwrap().len() > 1024 * 1024);

    let mut reader = manifest.begin_incremental_read().unwrap().unwrap();
    assert!(!reader.step(0).unwrap().complete);
    let mut steps = 0;
    loop {
        let progress = reader.step(8192).unwrap();
        assert!(progress.bytes_read <= 8192);
        steps += 1;
        if progress.complete {
            break;
        }
        assert!(progress.bytes_read > 0);
    }
    assert!(steps > 128);
    assert!(reader.is_current().unwrap());
    let cutoffs = BTreeMap::from([("events".to_string(), (10, "ts".to_string()))]);
    let candidate = reader
        .next_unverified_append_only_segment(&cutoffs, None)
        .unwrap()
        .unwrap();
    assert_eq!(candidate.segment_id, entry.segment_id);
    assert!(reader.contains_append_only_segment(&candidate).unwrap());

    // Atomic publication replaces the file while the reader still pins the
    // previous generation. The stale snapshot may never authorize deletion.
    manifest.create_collection("other").unwrap();
    assert!(!reader.is_current().unwrap());
    assert!(reader
        .retire_append_only_retention_batch(&cutoffs, 1)
        .is_err());
    assert!(manifest
        .append_only_segment_path_for_test(&entry.path)
        .exists());
    assert_eq!(manifest.append_only_segments().unwrap().len(), 1);
}

#[test]
fn incremental_manifest_commits_only_a_verified_current_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("data.rdb");
    let manifest = OperationalManifest::for_db_path(&path);
    manifest.recover_or_bootstrap(&[]).unwrap();
    let bytes = encode_append_only_segment(
        AppendOnlySegmentCodec::None,
        &[AppendOnlySegmentRow {
            primary_key: b"1".to_vec(),
            payload: b"row".to_vec(),
        }],
    )
    .unwrap();
    let entry = manifest
        .publish_append_only_segment_with_retention_bounds(
            "events",
            1,
            AppendOnlySegmentCodec::None,
            &bytes,
            Some(1),
            Some(1),
        )
        .unwrap();
    let mut reader = manifest.begin_incremental_read().unwrap().unwrap();
    while !reader.step(17).unwrap().complete {}
    assert!(reader.is_current().unwrap());
    assert!(reader
        .publish_revalidated_append_only_bounds(&entry, "ts", (1, 1))
        .unwrap());
    assert!(!reader.is_current().unwrap());
    drop(reader);

    let mut reader = manifest.begin_incremental_read().unwrap().unwrap();
    while !reader.step(17).unwrap().complete {}
    let cutoffs = BTreeMap::from([("events".to_string(), (10, "ts".to_string()))]);
    assert_eq!(
        reader
            .retire_append_only_retention_batch(&cutoffs, 1)
            .unwrap(),
        vec![("events".to_string(), 1)]
    );
    assert!(!reader.is_current().unwrap());
    drop(reader);
    manifest.recover_or_bootstrap(&[]).unwrap();
    assert!(manifest.append_only_segments().unwrap().is_empty());
    assert_eq!(manifest.append_only_published_rows("events").unwrap(), 1);
}
