use reddb_file::OperationalManifest;
use reddb_server::{RedDBOptions, RedDBRuntime};

#[test]
fn append_only_flush_publishes_closed_segment_and_reopen_reads_rows() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("append_only.rdb");

    let first_segment_path = {
        let rt =
            RedDBRuntime::with_options(RedDBOptions::persistent(&db_path)).expect("runtime boots");
        rt.execute_query("CREATE TABLE events (id INT, msg TEXT) APPEND ONLY")
            .expect("create append-only table");
        rt.execute_query("INSERT INTO events (id, msg) VALUES (1, 'a'), (2, 'b')")
            .expect("insert append-only rows");
        rt.flush().expect("flush append-only segment");

        let manifest = OperationalManifest::for_db_path(&db_path);
        let segments = manifest.append_only_segments_for_test().unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].collection, "events");
        assert_eq!(segments[0].codec, reddb_file::AppendOnlySegmentCodec::Zstd);
        assert_eq!(segments[0].row_count, 2);
        assert!(segments[0].primary_min.is_some());
        assert!(segments[0].primary_max.is_some());
        assert!(segments[0].primary_bloom.is_some());
        assert!(!segments[0].chunk_checksums.is_empty());
        manifest.append_only_segment_path_for_test(&segments[0].path)
    };

    let first_metadata = std::fs::metadata(&first_segment_path).unwrap();

    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&db_path))
            .expect("runtime reopens");
        let rows = rt
            .execute_query("SELECT id FROM events")
            .expect("reopened append-only select succeeds");
        assert_eq!(rows.result.records.len(), 2);

        rt.execute_query("INSERT INTO events (id, msg) VALUES (3, 'c')")
            .expect("append after reopen");
        rt.flush().expect("second append-only flush");
    }

    let second_metadata = std::fs::metadata(&first_segment_path).unwrap();
    assert_eq!(second_metadata.len(), first_metadata.len());
    assert_eq!(
        second_metadata.modified().unwrap(),
        first_metadata.modified().unwrap()
    );

    let manifest = OperationalManifest::for_db_path(&db_path);
    let segments = manifest.append_only_segments_for_test().unwrap();
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[0].collection, "events");
    assert_eq!(segments[1].collection, "events");
}
