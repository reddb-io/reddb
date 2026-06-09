//! Issue #827 — causal bookmarks and contiguous replica apply waits.

#[allow(dead_code)]
mod support;

use std::time::Duration;

use reddb::replication::cdc::ChangeRecord;
use reddb::replication::logical::{ApplyMode, LogicalChangeApplier};
use reddb::replication::{CausalBookmark, DEFAULT_REPLICATION_TERM};
use reddb::storage::RedDB;
use reddb::{RedDBOptions, RedDBRuntime, ReplicationConfig};

fn temp_path(prefix: &str) -> support::TempDbFile {
    support::temp_db_file(&format!("issue-827-{prefix}"))
}

fn record(lsn: u64, payload: &[u8]) -> ChangeRecord {
    support::logical_insert_record("issue_827_users", lsn, 1000 + lsn, payload)
}

#[test]
fn writes_return_opaque_bookmark_with_term_and_commit_lsn() {
    let rt = RedDBRuntime::with_options(
        RedDBOptions::in_memory().with_replication(ReplicationConfig::primary().with_term(7)),
    )
    .expect("runtime");

    rt.execute_query("CREATE TABLE issue_827_bookmark (id INT, name TEXT)")
        .expect("create table");
    let inserted = rt
        .execute_query("INSERT INTO issue_827_bookmark (id, name) VALUES (1, 'ana')")
        .expect("insert");

    let token = inserted.bookmark.expect("write returns bookmark");
    assert!(
        token.starts_with("rbm1."),
        "bookmark token should be opaque and versioned: {token}"
    );

    let bookmark = CausalBookmark::decode(&token).expect("bookmark decodes internally");
    assert_eq!(bookmark.term(), 7);
    assert_eq!(bookmark.commit_lsn(), rt.cdc_current_lsn());
}

#[test]
fn causal_session_carries_extracts_and_injects_bookmark() {
    let rt = RedDBRuntime::with_options(
        RedDBOptions::in_memory().with_replication(ReplicationConfig::primary()),
    )
    .expect("runtime");
    rt.execute_query("CREATE TABLE issue_827_session (id INT, name TEXT)")
        .expect("create table");

    let mut writer = rt.causal_session();
    writer
        .execute_query("INSERT INTO issue_827_session (id, name) VALUES (1, 'rio')")
        .expect("insert through session");
    let token = writer.bookmark_token().expect("session exposes bookmark");

    let mut reader = rt.causal_session();
    reader.inject_bookmark(&token).expect("inject bookmark");
    let result = reader
        .execute_query("SELECT name FROM issue_827_session WHERE id = 1")
        .expect("causal read waits and succeeds");

    assert_eq!(result.result.records.len(), 1);
}

#[test]
fn bookmark_wait_uses_contiguous_applied_lsn_not_gappy_received_frontier() {
    let path = temp_path("gap-wait");
    let db = RedDB::open(&path).expect("db");
    let applier = LogicalChangeApplier::new(0);

    applier
        .apply(&db, &record(1, b"a"), ApplyMode::Replica)
        .expect("apply lsn 1");
    let bookmark = CausalBookmark::new(DEFAULT_REPLICATION_TERM, 5);

    let gap = applier
        .apply(&db, &record(5, b"e"), ApplyMode::Replica)
        .expect_err("later LSN with missing predecessors is gappy");
    assert!(gap.to_string().contains("LSN gap"));
    assert_eq!(applier.received_frontier_lsn(), 5);
    assert_eq!(applier.last_applied_lsn(), 1);

    let err = applier
        .wait_for_bookmark(&bookmark, Duration::from_millis(25))
        .expect_err("wait must not release on the gappy received frontier");
    assert!(err.is_timeout(), "expected timeout, got {err:?}");

    for lsn in 2..=5 {
        applier
            .apply(&db, &record(lsn, &[lsn as u8]), ApplyMode::Replica)
            .unwrap_or_else(|err| panic!("apply lsn {lsn}: {err}"));
    }
    applier
        .wait_for_bookmark(&bookmark, Duration::from_secs(1))
        .expect("contiguous apply reaches bookmark");
}
