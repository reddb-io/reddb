use reddb_file::OperationalManifest;

#[test]
fn journal_appends_deltas_and_cold_boot_never_rewrites_metadata() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("data.rdb");
    let manifest = OperationalManifest::for_db_path(&path);
    let names = (0..2_000)
        .map(|id| format!("collection-{id:06}"))
        .collect::<Vec<_>>();
    manifest.recover_or_bootstrap(&names).expect("bootstrap");
    let head_path = manifest.current_manifest_path_for_test();
    let journal_path = head_path.with_file_name("manifest.records");
    let before = std::fs::read(&journal_path).expect("initial journal");
    assert!(before.len() > 128 * 1024);
    assert!(std::fs::metadata(&head_path).expect("head").len() < 512);

    // A distinct writer updates the same path. The warm reader must replay only
    // that committed suffix, not the previous 2,000 collection records.
    let writer = OperationalManifest::for_db_path(&path);
    writer.create_collection("new").expect("append collection");
    let after = std::fs::read(&journal_path).expect("updated journal");
    assert!(after.starts_with(&before));
    assert!(after.len() - before.len() < 512);
    let mut reader = manifest
        .begin_incremental_read()
        .expect("open")
        .expect("manifest");
    let mut reads = 0;
    loop {
        let step = reader.step(31).expect("step");
        assert!(step.bytes_read <= 31);
        reads += step.bytes_read;
        if step.complete {
            break;
        }
    }
    assert!(reads < 1024, "warm read consumed {reads} bytes");
    drop(reader);

    let head = std::fs::read(&head_path).expect("head bytes");
    for _ in 0..3 {
        let reopened = OperationalManifest::for_db_path(&path);
        reopened.recover_or_bootstrap(&[]).expect("cold recovery");
        assert_eq!(std::fs::read(&head_path).expect("head"), head);
        assert_eq!(std::fs::read(&journal_path).expect("journal"), after);
        assert!(reopened.collection_path_for_test("new").exists());
    }
}

#[test]
fn journal_ignores_unpublished_tail_but_rejects_committed_corruption() {
    use std::io::Write;
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("data.rdb");
    let manifest = OperationalManifest::for_db_path(&path);
    manifest.create_collection("live").expect("create");
    let journal_path = manifest
        .current_manifest_path_for_test()
        .with_file_name("manifest.records");
    let committed = std::fs::read(&journal_path).expect("journal");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&journal_path)
        .expect("append")
        .write_all(b"torn record")
        .expect("tail");
    let reopened = OperationalManifest::for_db_path(&path);
    reopened
        .recover_or_bootstrap(&[])
        .expect("ignore unpublished tail");
    reopened
        .create_collection("second")
        .expect("overwrite tail");
    assert!(std::fs::read(&journal_path)
        .expect("journal")
        .starts_with(&committed));
    let valid = std::fs::read(&journal_path).expect("valid journal");
    for truncated in [true, false] {
        let mut broken = valid.clone();
        if truncated {
            broken.pop();
        } else {
            broken[13] ^= 1;
        }
        std::fs::write(&journal_path, broken).expect("inject damage");
        let cold = OperationalManifest::for_db_path(&path);
        assert!(cold.recover_or_bootstrap(&[]).is_err());
        assert!(cold.collection_path_for_test("live").exists());
        assert!(cold.collection_path_for_test("second").exists());
    }
    std::fs::write(&journal_path, valid).expect("restore fixture");
    OperationalManifest::for_db_path(&path)
        .recover_or_bootstrap(&[])
        .expect("recover");
}

#[test]
fn legacy_manifest_migrates_once_and_preserves_renamed_files() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("data.rdb");
    let manifest = OperationalManifest::for_db_path(&path);
    manifest.create_collection("red_config").expect("create");
    let legacy = manifest
        .legacy_manifest_bytes_for_test()
        .expect("legacy fixture");
    std::fs::write(manifest.current_manifest_path_for_test(), &legacy).expect("legacy head");
    let reopened = OperationalManifest::for_db_path(&path);
    reopened.recover_or_bootstrap(&[]).expect("legacy recovery");
    assert_eq!(
        std::fs::read(reopened.current_manifest_path_for_test()).expect("head"),
        legacy
    );
    reopened
        .rename_collections(&[("red_config", "red.config")])
        .expect("migrate and rename");
    let cold = OperationalManifest::for_db_path(&path);
    cold.recover_or_bootstrap(&[]).expect("journal recovery");
    let document: serde_json::Value =
        serde_json::from_slice(&cold.legacy_manifest_bytes_for_test().expect("inspect"))
            .expect("JSON");
    assert_eq!(
        document["collections"]["red.config"]["path"],
        "red_config.rcol"
    );
    assert!(cold.collection_path_for_test("red_config").exists());
    assert!(document["collections"].get("red_config").is_none());
}

#[test]
fn retention_proof_does_not_rewrite_large_segment_metadata() {
    use reddb_file::{encode_append_only_segment, AppendOnlySegmentCodec, AppendOnlySegmentRow};
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("data.rdb");
    let manifest = OperationalManifest::for_db_path(&path);
    let bytes = encode_append_only_segment(AppendOnlySegmentCodec::None,
        &[AppendOnlySegmentRow { primary_key: vec![1], payload: vec![1] }]).expect("segment");
    manifest.publish_append_only_segment("events", 1, AppendOnlySegmentCodec::None, &bytes).expect("publish");
    let mut legacy: serde_json::Value = serde_json::from_slice(&manifest.legacy_manifest_bytes_for_test().expect("fixture")).expect("JSON");
    legacy["append_only_segments"][0]["primary_bloom"]["bits_hex"] = "ab".repeat(600_000).into();
    legacy.as_object_mut().expect("object").remove("checksum");
    let checksum = crc32fast::hash(&serde_json::to_vec(&legacy).expect("canonical"));
    legacy["checksum"] = format!("{checksum:08x}").into();
    let head_path = manifest.current_manifest_path_for_test();
    std::fs::write(&head_path, serde_json::to_vec(&legacy).expect("legacy")).expect("fixture");
    manifest.create_collection("other").expect("migrate");
    let before = std::fs::read(head_path.with_file_name("manifest.records")).expect("journal");
    assert!(before.len() > 1024 * 1024);
    let expected = manifest.append_only_segments().expect("segments").remove(0);
    let mut snapshot = manifest.begin_incremental_read().expect("open").expect("manifest");
    while !snapshot.step(8192).expect("read").complete {}
    assert!(snapshot.publish_revalidated_append_only_bounds(&expected, "ts", (1, 1)).expect("proof"));
    let after = std::fs::read(head_path.with_file_name("manifest.records")).expect("journal");
    assert!(after.starts_with(&before));
    assert!(after.len() - before.len() < 512);
    let cold = OperationalManifest::for_db_path(&path);
    cold.recover_or_bootstrap(&[]).expect("recovery");
    let entries = cold.append_only_segments().expect("segments");
    assert_eq!(entries[0].retention_timestamp_column.as_deref(), Some("ts"));
    assert_eq!(entries[0].primary_bloom, expected.primary_bloom);
}
