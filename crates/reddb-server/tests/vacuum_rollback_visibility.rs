//! Vacuum must not forget transaction outcomes still referenced by row versions.
use reddb_server::{RedDBOptions, RedDBRuntime};

fn fixture() -> RedDBRuntime {
    fixture_with_options(RedDBOptions::in_memory())
}

fn fixture_with_options(options: RedDBOptions) -> RedDBRuntime {
    let runtime = RedDBRuntime::with_options(options).unwrap();
    for sql in [
        "CREATE TABLE target_rows (id TEXT, n INTEGER)",
        "CREATE TABLE other_rows (id TEXT, n INTEGER)",
        "INSERT INTO target_rows (id, n) VALUES ('committed', 1)",
        "BEGIN",
        "INSERT INTO target_rows (id, n) VALUES ('aborted', 2)",
        "INSERT INTO other_rows (id, n) VALUES ('aborted', 3)",
        "ROLLBACK",
    ] {
        runtime.execute_query(sql).unwrap();
    }
    assert_visible(&runtime);
    runtime
}

fn assert_visible(runtime: &RedDBRuntime) {
    let target = runtime
        .execute_query("SELECT id, n FROM target_rows")
        .unwrap();
    assert_eq!(
        target.result.records.len(),
        1,
        "aborted target row became visible"
    );
    assert!(
        runtime
            .execute_query("SELECT id, n FROM other_rows")
            .unwrap()
            .result
            .records
            .is_empty(),
        "vacuum exposed an aborted row in another collection"
    );
}

#[test]
fn targeted_vacuum_preserves_rollback_visibility() {
    let runtime = fixture();
    runtime.execute_query("VACUUM target_rows").unwrap();
    assert_visible(&runtime);
}

#[test]
fn vacuum_of_an_unrelated_table_preserves_rollback_visibility() {
    let runtime = fixture();
    runtime
        .execute_query("CREATE TABLE unrelated_rows (id TEXT)")
        .unwrap();
    runtime.execute_query("VACUUM unrelated_rows").unwrap();
    assert_visible(&runtime);
}

#[test]
fn repeated_full_vacuum_preserves_rollback_visibility() {
    let runtime = fixture();
    for _ in 0..3 {
        runtime.execute_query("VACUUM").unwrap();
        assert_visible(&runtime);
    }
}

#[test]
fn vacuum_checkpoint_and_reopen_do_not_publish_aborted_inserts() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fixture.rdb");
    // Exercise the legacy paged-file boundary as well as in-memory visibility.
    drop(reddb_server::storage::UnifiedStore::open(&path).unwrap());
    {
        let runtime = fixture_with_options(RedDBOptions::persistent(&path));
        runtime.execute_query("VACUUM target_rows").unwrap();
        assert_visible(&runtime);
        runtime.checkpoint().unwrap();
        assert_visible(&runtime);
    }
    let restored = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
    assert_visible(&restored);
}
