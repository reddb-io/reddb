use std::collections::BTreeMap;
use std::fs;

use reddb_file::{
    append_only_segment_primary_bloom_might_contain, decode_append_only_segment,
    encode_append_only_segment, AppendOnlySegmentCodec, AppendOnlySegmentRow,
    AppendOnlySegmentState, OperationalManifest, APPEND_ONLY_SEGMENT_CHUNK_BYTES,
};

#[test]
fn append_only_segment_frame_round_trips_metadata_and_checksums() {
    let rows = vec![
        AppendOnlySegmentRow {
            primary_key: b"0001".to_vec(),
            payload: b"{\"id\":1,\"v\":\"a\"}".to_vec(),
        },
        AppendOnlySegmentRow {
            primary_key: b"0002".to_vec(),
            payload: b"{\"id\":2,\"v\":\"b\"}".to_vec(),
        },
    ];

    let encoded = encode_append_only_segment(AppendOnlySegmentCodec::None, &rows).unwrap();
    let decoded = decode_append_only_segment(&encoded).unwrap();

    assert_eq!(decoded.version, 1);
    assert_eq!(decoded.codec, AppendOnlySegmentCodec::None);
    assert_eq!(decoded.chunk_size, APPEND_ONLY_SEGMENT_CHUNK_BYTES);
    assert_eq!(decoded.rows, rows);
    assert_eq!(decoded.primary_min.as_deref(), Some(&b"0001"[..]));
    assert_eq!(decoded.primary_max.as_deref(), Some(&b"0002"[..]));
    let bloom = decoded.primary_bloom.as_ref().expect("primary bloom");
    assert!(append_only_segment_primary_bloom_might_contain(
        bloom, b"0001"
    ));
    assert!(append_only_segment_primary_bloom_might_contain(
        bloom, b"0002"
    ));
    assert!(!append_only_segment_primary_bloom_might_contain(
        bloom, b"9999"
    ));
    assert!(!decoded.chunk_checksums.is_empty());
    assert!(decoded
        .chunk_checksums
        .iter()
        .all(|chunk| chunk.checksum.starts_with("crc32:")));
}

#[test]
fn operational_manifest_publishes_closed_append_only_segment_once() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("data.rdb");
    let manifest = OperationalManifest::for_db_path(&db_path);
    manifest.recover_or_bootstrap(&[]).unwrap();

    let rows = vec![AppendOnlySegmentRow {
        primary_key: b"42".to_vec(),
        payload: b"{\"id\":42}".to_vec(),
    }];
    let encoded = encode_append_only_segment(AppendOnlySegmentCodec::None, &rows).unwrap();
    let segment = manifest
        .publish_append_only_segment("events", 7, AppendOnlySegmentCodec::None, &encoded)
        .unwrap();

    assert_eq!(segment.collection, "events");
    assert_eq!(segment.segment_id, 7);
    assert_eq!(segment.codec, AppendOnlySegmentCodec::None);
    assert_eq!(segment.chunk_size, APPEND_ONLY_SEGMENT_CHUNK_BYTES);
    assert_eq!(segment.row_count, 1);
    assert!(segment.primary_min.is_some());
    assert!(segment.primary_max.is_some());
    assert!(segment.primary_bloom.is_some());
    assert!(!segment.chunk_checksums.is_empty());
    assert!(segment.path.ends_with(".raos"));
    assert!(manifest
        .append_only_segment_path_for_test(&segment.path)
        .exists());

    let republish = manifest.publish_append_only_segment(
        "events",
        7,
        AppendOnlySegmentCodec::None,
        b"different bytes",
    );
    assert!(
        republish.is_err(),
        "closed append-only segment path must not be modified in place"
    );

    let loaded = manifest.append_only_segments_for_test().unwrap();
    assert_eq!(loaded, vec![segment]);
}

#[test]
fn retention_candidates_obey_cutoff_budget_and_recovery_state() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("data.rdb");
    let manifest = OperationalManifest::for_db_path(&db_path);
    manifest.recover_or_bootstrap(&[]).unwrap();
    let encoded = encode_append_only_segment(
        AppendOnlySegmentCodec::None,
        &[AppendOnlySegmentRow {
            primary_key: b"1".to_vec(),
            payload: b"v".to_vec(),
        }],
    )
    .unwrap();
    for (id, max) in [(1, Some(5)), (2, Some(9)), (3, Some(10)), (4, None)] {
        manifest
            .publish_append_only_segment_with_retention_column(
                "events",
                id,
                AppendOnlySegmentCodec::None,
                &encoded,
                (max, max),
                Some("ts"),
            )
            .unwrap();
    }
    let cutoffs = BTreeMap::from([("events".to_string(), (10, "ts".to_string()))]);
    assert_eq!(
        manifest
            .append_only_retention_candidates(&cutoffs, 1)
            .unwrap(),
        vec![("events".into(), 1)]
    );
    assert!(manifest
        .append_only_retention_candidates(&cutoffs, 0)
        .unwrap()
        .is_empty());
    manifest
        .begin_retire_append_only_segment("events", 1)
        .unwrap();
    assert_eq!(
        manifest
            .append_only_retention_candidates(&cutoffs, 10)
            .unwrap(),
        vec![("events".into(), 2)]
    );
    drop(manifest);
    let reopened = OperationalManifest::for_db_path(&db_path);
    reopened.recover_or_bootstrap(&[]).unwrap();
    assert_eq!(
        reopened
            .append_only_retention_candidates(&cutoffs, 10)
            .unwrap(),
        vec![("events".into(), 2)]
    );
    assert_eq!(reopened.append_only_segments().unwrap().len(), 3);
}

#[test]
fn legacy_bounds_and_changed_timestamp_column_do_not_retire_segments() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("data.rdb");
    let manifest = OperationalManifest::for_db_path(&db_path);
    manifest.recover_or_bootstrap(&[]).unwrap();
    let encoded = encode_append_only_segment(
        AppendOnlySegmentCodec::None,
        &[AppendOnlySegmentRow {
            primary_key: b"1".to_vec(),
            payload: b"v".to_vec(),
        }],
    )
    .unwrap();
    let legacy = manifest
        .publish_append_only_segment_with_retention_bounds(
            "events",
            1,
            AppendOnlySegmentCodec::None,
            &encoded,
            Some(1),
            Some(1),
        )
        .unwrap();
    manifest
        .publish_append_only_segment_with_retention_column(
            "events",
            2,
            AppendOnlySegmentCodec::None,
            &encoded,
            (Some(1), Some(1)),
            Some("ts"),
        )
        .unwrap();
    let path = manifest.append_only_segment_path_for_test(&legacy.path);
    drop(manifest);

    let reopened = OperationalManifest::for_db_path(&db_path);
    reopened.recover_or_bootstrap(&[]).unwrap();
    assert!(path.exists());
    let other_column = BTreeMap::from([("events".to_string(), (10, "other_ts".to_string()))]);
    assert!(reopened
        .append_only_retention_candidates(&other_column, 10)
        .unwrap()
        .is_empty());
    let original_column = BTreeMap::from([("events".to_string(), (10, "ts".to_string()))]);
    assert_eq!(
        reopened
            .append_only_retention_candidates(&original_column, 10)
            .unwrap(),
        vec![("events".to_string(), 2)]
    );
    assert_eq!(reopened.append_only_segments().unwrap().len(), 2);
}

#[test]
fn operational_manifest_renames_append_only_identity_without_moving_segment() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("data.rdb");
    let manifest = OperationalManifest::for_db_path(&db_path);
    manifest.recover_or_bootstrap(&[]).unwrap();
    let rows = vec![AppendOnlySegmentRow {
        primary_key: b"1".to_vec(),
        payload: b"audit".to_vec(),
    }];
    let encoded = encode_append_only_segment(AppendOnlySegmentCodec::None, &rows).unwrap();
    let segment = manifest
        .publish_append_only_segment("red_ask_audit", 1, AppendOnlySegmentCodec::None, &encoded)
        .unwrap();
    let path = manifest.append_only_segment_path_for_test(&segment.path);

    manifest
        .rename_collections(&[("red_ask_audit", "red.ask.audit")])
        .unwrap();
    let renamed = manifest.append_only_segments().unwrap();
    assert_eq!(renamed.len(), 1);
    assert_eq!(renamed[0].collection, "red.ask.audit");
    assert_eq!(renamed[0].path, segment.path);
    assert!(path.exists());
    assert_eq!(
        manifest
            .append_only_published_rows("red.ask.audit")
            .unwrap(),
        1
    );
    assert_eq!(
        manifest
            .next_append_only_segment_id("red.ask.audit")
            .unwrap(),
        2
    );
}

#[test]
fn unpublished_append_only_segment_files_are_quarantined_on_recover() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("data.rdb");
    let manifest = OperationalManifest::for_db_path(&db_path);
    manifest.recover_or_bootstrap(&[]).unwrap();
    let orphan = manifest.append_only_segment_path_for_test("events-000000000009.raos");
    fs::write(&orphan, b"unpublished").unwrap();

    manifest.recover_or_bootstrap(&[]).unwrap();

    assert!(!orphan.exists());
    assert!(manifest
        .quarantine_path_for_test("events-000000000009.raos")
        .exists());
}

#[test]
fn interrupted_append_only_segment_retirement_recovers_without_orphaning_live_segments() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("data.rdb");
    let manifest = OperationalManifest::for_db_path(&db_path);
    manifest.recover_or_bootstrap(&[]).unwrap();

    let expired = encode_append_only_segment(
        AppendOnlySegmentCodec::None,
        &[AppendOnlySegmentRow {
            primary_key: b"0001".to_vec(),
            payload: b"{\"id\":1}".to_vec(),
        }],
    )
    .unwrap();
    let live = encode_append_only_segment(
        AppendOnlySegmentCodec::None,
        &[AppendOnlySegmentRow {
            primary_key: b"0002".to_vec(),
            payload: b"{\"id\":2}".to_vec(),
        }],
    )
    .unwrap();
    let expired_segment = manifest
        .publish_append_only_segment("events", 1, AppendOnlySegmentCodec::None, &expired)
        .unwrap();
    let live_segment = manifest
        .publish_append_only_segment("events", 2, AppendOnlySegmentCodec::None, &live)
        .unwrap();
    let expired_path = manifest.append_only_segment_path_for_test(&expired_segment.path);
    let live_path = manifest.append_only_segment_path_for_test(&live_segment.path);

    assert!(manifest
        .begin_retire_append_only_segment("events", 1)
        .unwrap());
    assert!(
        expired_path.exists(),
        "begin phase must not remove bytes yet"
    );
    let pending = manifest
        .append_only_segments_with_pending_for_test()
        .unwrap();
    assert_eq!(pending[0].state, AppendOnlySegmentState::PendingDrop);
    assert_eq!(pending[1].state, AppendOnlySegmentState::Active);

    manifest.recover_or_bootstrap(&[]).unwrap();

    assert!(
        !expired_path.exists(),
        "recovery completes pending retirement"
    );
    assert!(live_path.exists(), "live segment must not be touched");
    assert!(!manifest
        .quarantine_path_for_test(&expired_segment.path)
        .exists());
    assert_eq!(
        manifest.append_only_segments_for_test().unwrap(),
        vec![live_segment]
    );
    assert_eq!(
        manifest
            .append_only_published_rows_for_test("events")
            .unwrap(),
        2
    );
}
