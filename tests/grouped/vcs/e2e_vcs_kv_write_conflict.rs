//! First-committer-wins write-conflict detection for versioned KV.
//!
//! ADR 0014 mandates snapshot isolation with first-committer-wins write
//! conflict detection across all models. The versioned-KV write path
//! tombstones the prior version (set_xmax) and inserts a new version —
//! exactly like a table-row versioned UPDATE. Two concurrent
//! transactions that both tombstone the SAME prior version must NOT
//! both commit: the first committer wins, the second must fail with a
//! serialization conflict, leaving exactly ONE live version.
//!
//! These tests drive two explicit transactions deterministically by
//! switching the thread-local connection id between statements (the same
//! interleaving primitive the table-row first-committer-wins tests use),
//! so there is no thread-timing flakiness.

use reddb::application::VcsUseCases;
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn exec_err(rt: &RedDBRuntime, sql: &str) -> String {
    rt.execute_query(sql)
        .expect_err("query should fail")
        .to_string()
}

fn vcs(rt: &RedDBRuntime) -> VcsUseCases<'_, RedDBRuntime> {
    VcsUseCases::new(rt)
}

fn assert_conflict(message: &str) {
    assert!(
        message.contains("serialization conflict"),
        "expected serialization conflict, got {message}"
    );
}

/// Every physical version of `key` whose `value` is currently
/// snapshot-visible (live: xmax == 0 for the reader). Returns the raw
/// `value` strings. A correct first-committer-wins implementation must
/// leave exactly one live version per key.
fn live_values(rt: &RedDBRuntime, collection: &str, key: &str) -> Vec<String> {
    let result = rt
        .execute_query(&format!("SELECT value FROM {collection} WHERE key = '{key}'"))
        .expect("scan key");
    result
        .result
        .records
        .iter()
        .map(|record| match record.get("value") {
            Some(Value::Text(value)) => value.to_string(),
            other => panic!("expected text value, got {other:?}"),
        })
        .collect()
}

/// RED → GREEN: two concurrent transactions PUT the same versioned KV
/// key. Both observe v0 at their snapshot, both tombstone the v0 version
/// and insert their own new version. T1 commits first. Per
/// first-committer-wins, T2's COMMIT must FAIL with a serialization
/// conflict — and the post-state must hold exactly ONE live version.
///
/// On unmodified code the versioned-KV write path records no pending
/// conflict marker, so T2 commits silently (no conflict) and two live
/// versions transiently coexist — this test fails until the conflict
/// machinery is wired in.
#[test]
fn concurrent_versioned_kv_put_conflicts_on_second_commit() {
    let rt = rt();
    set_current_connection_id(51001);
    // Auto-create + opt into versioning, then seed v0 (committed).
    exec(&rt, "KV PUT wc.'k' = 'v0'");
    vcs(&rt).set_versioned("wc", true).unwrap();
    // Re-seed so v0 carries a versioned (monotonic) xmin under the gate.
    exec(&rt, "KV PUT wc.'k' = 'v0'");

    // Two transactions both snapshot the v0 state.
    set_current_connection_id(51002);
    exec(&rt, "BEGIN");
    set_current_connection_id(51003);
    exec(&rt, "BEGIN");

    // T1 PUT k=v1, T2 PUT k=v1b — both tombstone the same v0 version.
    set_current_connection_id(51002);
    exec(&rt, "KV PUT wc.'k' = 'v1'");
    set_current_connection_id(51003);
    exec(&rt, "KV PUT wc.'k' = 'v1b'");

    // T1 commits (winner).
    set_current_connection_id(51002);
    exec(&rt, "COMMIT");
    // T2 commits (loser) — must conflict.
    set_current_connection_id(51003);
    assert_conflict(&exec_err(&rt, "COMMIT"));

    // Post-state: exactly one live version, and it is the winner's.
    set_current_connection_id(51004);
    let live = live_values(&rt, "wc", "k");
    assert_eq!(
        live,
        vec!["v1".to_string()],
        "exactly one live version (the first committer's) must survive"
    );
    clear_current_connection_id();
}

/// Serialized case stays correct: T1 commits, THEN T2 starts fresh, sees
/// v1, PUTs v1b — no false conflict. This guards against the conflict
/// check firing on a legitimately-sequential workload.
#[test]
fn serialized_versioned_kv_put_does_not_conflict() {
    let rt = rt();
    set_current_connection_id(52001);
    exec(&rt, "KV PUT wcs.'k' = 'v0'");
    vcs(&rt).set_versioned("wcs", true).unwrap();
    exec(&rt, "KV PUT wcs.'k' = 'v0'");

    // T1 fully commits before T2 begins.
    set_current_connection_id(52002);
    exec(&rt, "BEGIN");
    exec(&rt, "KV PUT wcs.'k' = 'v1'");
    exec(&rt, "COMMIT");

    set_current_connection_id(52003);
    exec(&rt, "BEGIN");
    exec(&rt, "KV PUT wcs.'k' = 'v1b'");
    exec(&rt, "COMMIT");

    set_current_connection_id(52004);
    let live = live_values(&rt, "wcs", "k");
    assert_eq!(
        live,
        vec!["v1b".to_string()],
        "serialized writes both apply; latest wins, single live version"
    );
    clear_current_connection_id();
}

/// Concurrent DELETE vs PUT on the same versioned key conflicts too:
/// both tombstone the same prior version. T1 DELETEs (commits), T2 PUTs
/// (must conflict on COMMIT).
#[test]
fn concurrent_versioned_kv_delete_then_put_conflicts() {
    let rt = rt();
    set_current_connection_id(53001);
    exec(&rt, "KV PUT wcd.'k' = 'v0'");
    vcs(&rt).set_versioned("wcd", true).unwrap();
    exec(&rt, "KV PUT wcd.'k' = 'v0'");

    set_current_connection_id(53002);
    exec(&rt, "BEGIN");
    set_current_connection_id(53003);
    exec(&rt, "BEGIN");

    set_current_connection_id(53002);
    exec(&rt, "KV DELETE wcd.'k'");
    set_current_connection_id(53003);
    exec(&rt, "KV PUT wcd.'k' = 'v1'");

    set_current_connection_id(53002);
    exec(&rt, "COMMIT");
    set_current_connection_id(53003);
    assert_conflict(&exec_err(&rt, "COMMIT"));

    // The DELETE won: the key is absent live.
    set_current_connection_id(53004);
    let live = live_values(&rt, "wcd", "k");
    assert!(
        live.is_empty(),
        "the committed DELETE wins; no live version survives, got {live:?}"
    );
    clear_current_connection_id();
}
