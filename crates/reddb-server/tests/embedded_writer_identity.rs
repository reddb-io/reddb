use reddb_server::{RedDBOptions, RedDBRuntime};

#[test]
fn hardlink_cannot_open_a_second_resident_store_and_discard_commits() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("original.rdb");
    let alias = dir.path().join("alias.rdb");
    let first = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("writer");
    first
        .execute_query("CREATE TABLE audit_rows (id INT)")
        .expect("schema");
    first.flush().expect("checkpoint");
    std::fs::hard_link(&path, &alias).expect("hardlink");
    let error = RedDBRuntime::with_options(RedDBOptions::persistent(&alias))
        .err()
        .expect("second writer refused");
    assert!(
        error.to_string().contains("already open for writing"),
        "{error}"
    );
    first
        .execute_query("INSERT INTO audit_rows (id) VALUES (1)")
        .expect("commit");
    first.flush().expect("checkpoint acknowledged row");
    drop(first);
    let reopened =
        RedDBRuntime::with_options(RedDBOptions::persistent(&alias)).expect("reopen alias");
    assert_eq!(
        reopened
            .execute_query("SELECT * FROM audit_rows WHERE id = 1")
            .expect("query")
            .result
            .records
            .len(),
        1
    );
}
