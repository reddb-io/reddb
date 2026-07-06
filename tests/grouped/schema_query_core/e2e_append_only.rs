//! End-to-end tests for `CREATE TABLE ... APPEND ONLY`.
//!
//! The feature is a first-class catalog flag: the runtime rejects
//! UPDATE / DELETE before RLS, before RETURNING, before any scan is
//! even planned. Error messages name the table and the DDL so the
//! operator can self-service the fix.
//!
//! The physical append-only segment contract is file-owned: runtime tests
//! assert the end-to-end flush/reopen behavior, while the segment format and
//! manifest metadata are parsed through `reddb-file`.

#[path = "../../support/mod.rs"]
mod support;

use reddb::application::ExecuteQueryInput;
use reddb::{QueryUseCases, RedDBOptions, RedDBRuntime};
use reddb_file::append_only_segment::{
    read_append_only_segment, AppendOnlySegmentCodec, APPEND_ONLY_SEGMENT_CHUNK_SIZE,
    APPEND_ONLY_SEGMENT_FORMAT_VERSION,
};

fn persistent_rt(db: &support::TempDbFile) -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::persistent(db.path())).expect("persistent runtime")
}

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

fn exec(q: &QueryUseCases<'_, RedDBRuntime>, sql: &str) {
    q.execute(ExecuteQueryInput { query: sql.into() })
        .unwrap_or_else(|err| panic!("{sql}: {err}"));
}

fn exec_err(q: &QueryUseCases<'_, RedDBRuntime>, sql: &str) -> String {
    match q.execute(ExecuteQueryInput { query: sql.into() }) {
        Ok(_) => panic!("expected error for: {sql}"),
        Err(err) => err.to_string(),
    }
}

#[test]
fn append_only_table_accepts_inserts() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    exec(&q, "CREATE TABLE audit_log (id INT, msg TEXT) APPEND ONLY");
    exec(&q, "INSERT INTO audit_log (id, msg) VALUES (1, 'hello')");
    exec(&q, "INSERT INTO audit_log (id, msg) VALUES (2, 'world')");
    let result = q
        .execute(ExecuteQueryInput {
            query: "SELECT id FROM audit_log".into(),
        })
        .expect("select should succeed");
    assert_eq!(result.result.records.len(), 2);
}

#[test]
fn append_only_table_rejects_update_with_clear_message() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    exec(&q, "CREATE TABLE events (id INT, v TEXT) APPEND ONLY");
    exec(&q, "INSERT INTO events (id, v) VALUES (1, 'x')");
    let err = exec_err(&q, "UPDATE events SET v = 'y' WHERE id = 1");
    assert!(err.contains("events"), "error names the table: {err}");
    assert!(err.contains("APPEND ONLY"), "error cites DDL: {err}");
    assert!(err.contains("UPDATE"), "error names the operation: {err}");
    // Data must be unchanged.
    let sel = q
        .execute(ExecuteQueryInput {
            query: "SELECT v FROM events WHERE id = 1".into(),
        })
        .unwrap();
    let v = sel.result.records[0]
        .get("v")
        .expect("v present")
        .to_string();
    assert!(v.contains('x'), "v must stay 'x': {v}");
}

#[test]
fn append_only_table_rejects_delete_with_clear_message() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    exec(&q, "CREATE TABLE ledger (id INT, amt INT) APPEND ONLY");
    exec(&q, "INSERT INTO ledger (id, amt) VALUES (1, 100)");
    let err = exec_err(&q, "DELETE FROM ledger WHERE id = 1");
    assert!(err.contains("ledger"));
    assert!(err.contains("APPEND ONLY"));
    assert!(err.contains("DELETE"));
    // Row still there.
    let sel = q
        .execute(ExecuteQueryInput {
            query: "SELECT id FROM ledger".into(),
        })
        .unwrap();
    assert_eq!(sel.result.records.len(), 1);
}

#[test]
fn with_append_only_true_is_equivalent_to_trailing_keyword() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    exec(
        &q,
        "CREATE TABLE metrics (id INT, val INT) WITH (append_only = true)",
    );
    exec(&q, "INSERT INTO metrics (id, val) VALUES (1, 10)");
    let err = exec_err(&q, "UPDATE metrics SET val = 20 WHERE id = 1");
    assert!(err.contains("APPEND ONLY"));
}

#[test]
fn non_append_only_table_keeps_mutable_semantics() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    exec(&q, "CREATE TABLE users (id INT, name TEXT)");
    exec(&q, "INSERT INTO users (id, name) VALUES (1, 'alice')");
    // UPDATE must succeed — default is mutable.
    exec(&q, "UPDATE users SET name = 'bob' WHERE id = 1");
    let sel = q
        .execute(ExecuteQueryInput {
            query: "SELECT name FROM users WHERE id = 1".into(),
        })
        .unwrap();
    let name = sel.result.records[0].get("name").unwrap().to_string();
    assert!(name.contains("bob"));
}

#[test]
fn alter_table_set_append_only_flips_on() {
    // Start mutable. UPDATE works. Flip to append-only. UPDATE fails.
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    exec(&q, "CREATE TABLE flips (id INT, v TEXT)");
    exec(&q, "INSERT INTO flips (id, v) VALUES (1, 'a')");
    exec(&q, "UPDATE flips SET v = 'b' WHERE id = 1");
    // Now enable append-only via ALTER TABLE.
    exec(&q, "ALTER TABLE flips SET APPEND_ONLY = true");
    let err = exec_err(&q, "UPDATE flips SET v = 'c' WHERE id = 1");
    assert!(err.contains("APPEND ONLY"), "error should cite flag: {err}");

    // Row unchanged: last successful UPDATE set v='b'.
    let sel = q
        .execute(ExecuteQueryInput {
            query: "SELECT v FROM flips WHERE id = 1".into(),
        })
        .unwrap();
    let v = sel.result.records[0].get("v").unwrap().to_string();
    assert!(v.contains('b'), "v expected 'b', got {v}");
}

#[test]
fn alter_table_unset_append_only_re_enables_mutations() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    exec(&q, "CREATE TABLE switch (id INT, v TEXT) APPEND ONLY");
    exec(&q, "INSERT INTO switch (id, v) VALUES (1, 'x')");
    // Append-only rejects UPDATE.
    exec_err(&q, "UPDATE switch SET v = 'y' WHERE id = 1");
    // Flip the flag off.
    exec(&q, "ALTER TABLE switch SET APPEND_ONLY = false");
    // Now UPDATE must succeed.
    exec(&q, "UPDATE switch SET v = 'y' WHERE id = 1");
    let sel = q
        .execute(ExecuteQueryInput {
            query: "SELECT v FROM switch WHERE id = 1".into(),
        })
        .unwrap();
    let v = sel.result.records[0].get("v").unwrap().to_string();
    assert!(v.contains('y'), "v expected 'y', got {v}");
}

#[test]
fn append_only_still_allows_select_and_insert_returning() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    exec(&q, "CREATE TABLE trace (id INT, span TEXT) APPEND ONLY");
    let result = q
        .execute(ExecuteQueryInput {
            query: "INSERT INTO trace (id, span) VALUES (1, 'root') RETURNING span".into(),
        })
        .expect("INSERT RETURNING on APPEND ONLY must succeed");
    assert_eq!(result.result.records.len(), 1);
}

#[test]
fn append_only_checkpoint_publishes_immutable_segment_and_reopens_rows() {
    let db = support::temp_db_file("append-only-segment-v1");
    let rt = persistent_rt(&db);
    {
        let q = QueryUseCases::new(&rt);
        exec(&q, "CREATE TABLE audit_log (id INT, msg TEXT) APPEND ONLY");
        exec(&q, "INSERT INTO audit_log (id, msg) VALUES (1, 'hello')");
        exec(&q, "INSERT INTO audit_log (id, msg) VALUES (2, 'world')");
    }

    rt.checkpoint()
        .expect("checkpoint should flush append-only segment");

    let manifest = reddb_file::OperationalManifest::for_db_path(db.path());
    let segments = manifest
        .append_only_segments_for_test("audit_log")
        .expect("append-only segment entries");
    assert_eq!(segments.len(), 1, "one closed segment should be published");
    let entry = &segments[0];
    assert_eq!(entry.format_version, APPEND_ONLY_SEGMENT_FORMAT_VERSION);
    assert_eq!(entry.chunk_size, APPEND_ONLY_SEGMENT_CHUNK_SIZE);
    assert_eq!(entry.codec, AppendOnlySegmentCodec::Zstd);
    assert_eq!(entry.row_count, 2);
    assert!(
        !entry.chunks.is_empty(),
        "manifest entry must record chunk checksums"
    );
    assert!(
        entry.chunks.iter().all(|chunk| chunk.checksum != 0),
        "chunk checksums must be non-zero: {:?}",
        entry.chunks
    );

    let segment_path = manifest.append_only_segment_path_for_test(&entry.path);
    let decoded = read_append_only_segment(&segment_path)
        .expect("segment bytes must decode through reddb-file");
    assert_eq!(decoded.codec, AppendOnlySegmentCodec::Zstd);
    assert_eq!(decoded.rows, 2);
    let bytes_before_reopen = std::fs::read(&segment_path).expect("segment exists before reopen");

    drop(rt);
    let reopened = persistent_rt(&db);
    let q = QueryUseCases::new(&reopened);
    let result = q
        .execute(ExecuteQueryInput {
            query: "SELECT msg FROM audit_log ORDER BY id ASC".into(),
        })
        .expect("append-only rows should survive reopen");
    assert_eq!(result.result.records.len(), 2);
    assert!(result.result.records[0]
        .get("msg")
        .expect("msg column")
        .to_string()
        .contains("hello"));
    assert!(result.result.records[1]
        .get("msg")
        .expect("msg column")
        .to_string()
        .contains("world"));

    let bytes_after_reopen = std::fs::read(&segment_path).expect("segment exists after reopen");
    assert_eq!(
        bytes_after_reopen, bytes_before_reopen,
        "closed append-only segments must not be modified in place"
    );

    let orphan = manifest.append_only_segment_path_for_test("unpublished-audit.segment");
    std::fs::write(&orphan, b"prepared but unpublished").expect("write orphan segment");
    drop(reopened);
    let _ = persistent_rt(&db);
    assert!(
        !orphan.exists(),
        "unpublished segment file should be quarantined"
    );
    assert!(
        manifest
            .quarantine_path_for_test("unpublished-audit.segment")
            .exists(),
        "orphan segment should land in operational quarantine"
    );
}
