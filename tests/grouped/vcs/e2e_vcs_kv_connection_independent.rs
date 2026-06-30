//! Issue #1382 — a commit captures the branch HEAD state independent of
//! the *calling connection's* workset.
//!
//! An embedding app (e.g. red-request) writes through one persistent
//! connection (the `red connect` RQL conduit) and commits through a
//! *different* connection. The committing connection never issued the
//! writes, so the original workset-scoped commit model captured an empty
//! workset — the commit pinned nothing and `AS OF COMMIT '<hash>'` fell
//! through to the live value.
//!
//! `vcs_commit` now pins a fresh global MVCC snapshot (`root_xid`) at
//! commit time and uses the connection only to decide which branch ref
//! to advance — never to scope which writes are captured. So a commit
//! issued on a connection that wrote nothing still snapshots the durable
//! HEAD state produced by other connections.
//!
//! Since VCS slice 4 retired the HTTP `/repo/*` transport, the commit
//! verb is exercised through the RQL surface (`CHECKPOINT`), which
//! resolves the connection id implicitly from the executing connection
//! (`current_connection_id()`). Switching the current connection between
//! the writer and the committer reproduces the original cross-connection
//! split that `POST /repo/commits` used to express explicitly. These
//! tests lock the HEAD-capture guarantee for the versioned KV model that
//! red-request stores into.

use std::sync::Arc;

use reddb::application::VcsUseCases;
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn rt() -> Arc<RedDBRuntime> {
    Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime"))
}

fn vcs(rt: &RedDBRuntime) -> VcsUseCases<'_, RedDBRuntime> {
    VcsUseCases::new(rt)
}

/// Checkpoint (commit) on the connection identified by `conn` through the
/// RQL surface. Mirrors the cross-connection split that the retired
/// `POST /repo/commits` path expressed: the committing connection is
/// distinct from the conduit that issued the writes, and `CHECKPOINT`
/// resolves it implicitly via `current_connection_id()`. Returns the
/// resulting HEAD commit hash, read back through the slice-1 `red.commits`
/// surface.
fn checkpoint_on(rt: &RedDBRuntime, conn: u64, msg: &str) -> String {
    set_current_connection_id(conn);
    rt.execute_query(&format!("CHECKPOINT '{msg}'"))
        .unwrap_or_else(|err| panic!("CHECKPOINT '{msg}': {err:?}"));
    let result = rt
        .execute_query("SELECT hash FROM red.commits ORDER BY height DESC LIMIT 1")
        .expect("red.commits");
    match result.result.records.first().and_then(|r| r.get("hash")) {
        Some(Value::Text(hash)) => hash.to_string(),
        other => panic!("expected text hash from red.commits, got {other:?}"),
    }
}

/// Read `value` for a KV `key` at a historical commit via SQL `AS OF`.
fn as_of_value(
    rt: &RedDBRuntime,
    collection: &str,
    key: &str,
    commit_hash: &str,
) -> Option<String> {
    let sql =
        format!("SELECT value FROM {collection} AS OF COMMIT '{commit_hash}' WHERE key = '{key}'");
    let result = rt
        .execute_query(&sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    assert!(
        result.result.records.len() <= 1,
        "AS OF must resolve at most one visible version per key, got {} for `{sql}`",
        result.result.records.len()
    );
    result
        .result
        .records
        .first()
        .map(|record| match record.get("value") {
            Some(Value::Text(value)) => value.to_string(),
            other => panic!("expected text value, got {other:?}"),
        })
}

/// The issue's exact repro, faithful to the connection split: one
/// connection writes, a *different* connection checkpoints, then the
/// writer updates the value. `AS OF` the cross-connection commit must
/// resolve the historical version — not the live one.
///
/// Writer connection = `7001` (the RQL/`red connect` conduit).
/// Committer connection = `9002` (the connection that issues `CHECKPOINT`
/// without ever writing a single value).
#[test]
fn commit_on_other_connection_captures_writer_head() {
    let rt = rt();
    const WRITER: u64 = 7001;
    const COMMITTER: u64 = 9002;

    // Writer conduit creates + seeds the collection, then opts it into
    // versioning (red-request stores everything as KV `rr_*`).
    set_current_connection_id(WRITER);
    rt.execute_query("KV PUT spike.'k' = 'v1'").unwrap();
    vcs(&rt).set_versioned("spike", true).unwrap();
    rt.execute_query("KV PUT spike.'k' = 'v1'").unwrap();

    // A different connection checkpoints. It carries no workset of its
    // own, yet must snapshot the writer's durable HEAD (k = v1).
    let c1 = checkpoint_on(&rt, COMMITTER, "c1");

    // Writer advances the value.
    set_current_connection_id(WRITER);
    rt.execute_query("KV PUT spike.'k' = 'v2'").unwrap();

    // Live read (writer connection) sees the latest.
    assert_eq!(
        as_of_live(&rt, "spike", "k"),
        Some("v2".to_string()),
        "live read must see the latest version"
    );

    // Time-travel read at the cross-connection commit must return v1 —
    // the HEAD state at commit time, independent of which connection
    // committed. Pre-fix this returned v2 (empty-workset commit pinned
    // nothing).
    assert_eq!(
        as_of_value(&rt, "spike", "k", &c1),
        Some("v1".to_string()),
        "AS OF a commit made on another connection must resolve the writer's HEAD-at-commit value"
    );

    clear_current_connection_id();
}

/// Live read helper bound to the current connection.
fn as_of_live(rt: &RedDBRuntime, collection: &str, key: &str) -> Option<String> {
    let sql = format!("SELECT value FROM {collection} WHERE key = '{key}'");
    let result = rt
        .execute_query(&sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    result
        .result
        .records
        .first()
        .map(|record| match record.get("value") {
            Some(Value::Text(value)) => value.to_string(),
            other => panic!("expected text value, got {other:?}"),
        })
}

/// Multi-version chain across three distinct connections: each PUT comes
/// from its own conduit connection and each checkpoint from a fourth
/// commit-only connection. Every commit point must resolve to its own
/// version regardless of the connection split.
#[test]
fn cross_connection_multi_version_chain() {
    let rt = rt();
    const COMMITTER: u64 = 5000;

    set_current_connection_id(4001);
    rt.execute_query("KV PUT xconn.'k' = 'seed'").unwrap();
    vcs(&rt).set_versioned("xconn", true).unwrap();
    rt.execute_query("KV PUT xconn.'k' = 'v1'").unwrap();
    let p1 = checkpoint_on(&rt, COMMITTER, "v1");

    set_current_connection_id(4002);
    rt.execute_query("KV PUT xconn.'k' = 'v2'").unwrap();
    let p2 = checkpoint_on(&rt, COMMITTER, "v2");

    set_current_connection_id(4003);
    rt.execute_query("KV PUT xconn.'k' = 'v3'").unwrap();
    let p3 = checkpoint_on(&rt, COMMITTER, "v3");

    assert_eq!(as_of_value(&rt, "xconn", "k", &p1), Some("v1".to_string()));
    assert_eq!(as_of_value(&rt, "xconn", "k", &p2), Some("v2".to_string()));
    assert_eq!(as_of_value(&rt, "xconn", "k", &p3), Some("v3".to_string()));

    clear_current_connection_id();
}
