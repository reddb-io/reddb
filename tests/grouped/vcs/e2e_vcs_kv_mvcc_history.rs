//! Versioned KV collections retain MVCC history (un-defers the
//! "full multi-model adoption" deferred by ADR 0014, for KV first).
//!
//! A KV collection that has opted into versioning
//! (`vcs.set_versioned(collection, true)`) must keep prior versions
//! physically alive so time-travel (`AS OF`) reads resolve the
//! snapshot-visible version per logical key — exactly like a table
//! row update. A non-versioned KV collection keeps last-writer-wins
//! semantics (physical pre-delete on PUT, physical delete on DELETE).

use std::sync::Arc;

use reddb::application::{Author, CreateCommitInput, VcsUseCases};
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn rt() -> Arc<RedDBRuntime> {
    Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime"))
}

fn author() -> Author {
    Author {
        name: "test".to_string(),
        email: "test@reddb.io".to_string(),
    }
}

fn vcs(rt: &RedDBRuntime) -> VcsUseCases<'_, RedDBRuntime> {
    VcsUseCases::new(rt)
}

fn commit(rt: &RedDBRuntime, conn: u64, msg: &str) -> String {
    vcs(rt)
        .commit(CreateCommitInput {
            connection_id: conn,
            message: msg.to_string(),
            author: author(),
            committer: None,
            amend: false,
            allow_empty: true,
        })
        .expect("commit")
        .hash
}

/// Read `value` for a KV `key` at a historical commit via SQL `AS OF`.
/// Returns `None` when the key has no snapshot-visible version.
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

/// Read `value` for a KV `key` against the current (live) snapshot via
/// a SQL scan. Returns `None` when no live version exists.
fn live_value(rt: &RedDBRuntime, collection: &str, key: &str) -> Option<String> {
    let sql = format!("SELECT value FROM {collection} WHERE key = '{key}'");
    let result = rt
        .execute_query(&sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    assert!(
        result.result.records.len() <= 1,
        "live read must resolve at most one visible version per key, got {}",
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

/// Test A — time-travel on a versioned KV collection.
///
/// PUT k=v1, commit (capture point P1), PUT k=v2, commit.
/// `AS OF P1` of k must return v1 (history retained). Without the
/// fix, the versioned PUT physically pre-deletes the v1 row, so the
/// AS OF read returns v2 (or nothing) — this must FAIL on unmodified
/// code and PASS after.
#[test]
fn versioned_kv_as_of_returns_prior_version() {
    let rt = rt();
    // First PUT auto-creates the KV collection; then opt it into
    // versioning before the writes whose history we assert on.
    rt.execute_query("KV PUT vkv.'config:host' = 'v0'").unwrap();
    vcs(&rt).set_versioned("vkv", true).unwrap();

    rt.execute_query("KV PUT vkv.'config:host' = 'v1'").unwrap();
    let p1 = commit(&rt, 1, "put v1");

    rt.execute_query("KV PUT vkv.'config:host' = 'v2'").unwrap();
    let _p2 = commit(&rt, 1, "put v2");

    // Live read sees the latest.
    assert_eq!(
        live_value(&rt, "vkv", "config:host"),
        Some("v2".to_string()),
        "live read must see latest version"
    );

    // Time-travel read at P1 must see v1 — history retained.
    assert_eq!(
        as_of_value(&rt, "vkv", "config:host", &p1),
        Some("v1".to_string()),
        "AS OF P1 must return the prior version v1"
    );
}

/// Test B — delete tombstone + history on a versioned KV collection.
///
/// PUT k=v1 (commit P1), DELETE k (commit). Current read of k is
/// ABSENT, but `AS OF P1` returns v1. Without the fix, the versioned
/// DELETE physically removes the row, so `AS OF P1` cannot find v1.
#[test]
fn versioned_kv_delete_tombstones_and_keeps_history() {
    let rt = rt();
    // First PUT auto-creates the KV collection; then opt into versioning.
    rt.execute_query("KV PUT vkv2.'seed' = 'seed'").unwrap();
    vcs(&rt).set_versioned("vkv2", true).unwrap();

    rt.execute_query("KV PUT vkv2.'flag:beta' = 'v1'").unwrap();
    let p1 = commit(&rt, 1, "put v1");

    let deleted = rt.execute_query("KV DELETE vkv2.'flag:beta'").unwrap();
    assert_eq!(deleted.affected_rows, 1, "delete reports the key existed");
    let _p2 = commit(&rt, 1, "delete");

    // Current read: key is absent (tombstoned).
    assert_eq!(
        live_value(&rt, "vkv2", "flag:beta"),
        None,
        "current read of a deleted key must be ABSENT"
    );
    // KV GET always returns one envelope record; an absent key carries
    // a Null `value`.
    let dsl_get = rt.execute_query("KV GET vkv2.'flag:beta'").unwrap();
    assert_eq!(dsl_get.result.records.len(), 1, "KV GET envelope record");
    assert_eq!(
        dsl_get.result.records[0].get("value"),
        Some(&Value::Null),
        "KV GET of a deleted key must carry a Null value"
    );

    // Time-travel read at P1 still returns v1 — history retained.
    assert_eq!(
        as_of_value(&rt, "vkv2", "flag:beta", &p1),
        Some("v1".to_string()),
        "AS OF P1 must still return v1 after delete"
    );
}

/// Test C — non-versioned KV is UNCHANGED (last-writer-wins).
///
/// PUT k=v1, PUT k=v2 — only one physical row exists (no history
/// accumulation). DELETE k physically removes it. This guards the
/// `is_versioned` gate so config/secret KV keep their fast path.
#[test]
fn non_versioned_kv_keeps_last_writer_wins() {
    let rt = rt();
    // First PUT auto-creates the KV collection; deliberately NOT versioned.
    rt.execute_query("KV PUT nvkv.'k' = 'v1'").unwrap();
    assert!(!vcs(&rt).is_versioned("nvkv").unwrap());

    rt.execute_query("KV PUT nvkv.'k' = 'v2'").unwrap();

    // Exactly one physical row for the key — no history accumulation.
    let all = rt
        .execute_query("SELECT key, value FROM nvkv WHERE key = 'k'")
        .unwrap();
    assert_eq!(
        all.result.records.len(),
        1,
        "non-versioned KV must keep a single physical row per key (last-writer-wins)"
    );
    match all.result.records[0].get("value") {
        Some(Value::Text(value)) => assert_eq!(&**value, "v2"),
        other => panic!("expected v2, got {other:?}"),
    }

    // DELETE physically removes the row.
    let deleted = rt.execute_query("KV DELETE nvkv.'k'").unwrap();
    assert_eq!(deleted.affected_rows, 1);
    let after = rt
        .execute_query("SELECT key, value FROM nvkv WHERE key = 'k'")
        .unwrap();
    assert_eq!(
        after.result.records.len(),
        0,
        "non-versioned DELETE physically removes the row"
    );
}

/// Test D — multi-version chain: each commit point resolves to its own
/// version, and the live read tracks the latest. Exercises version
/// selection across more than two physical versions per key.
#[test]
fn versioned_kv_multi_version_chain_time_travel() {
    let rt = rt();
    rt.execute_query("KV PUT chain.'k' = 'seed'").unwrap();
    vcs(&rt).set_versioned("chain", true).unwrap();

    rt.execute_query("KV PUT chain.'k' = 'v1'").unwrap();
    let p1 = commit(&rt, 1, "v1");
    rt.execute_query("KV PUT chain.'k' = 'v2'").unwrap();
    let p2 = commit(&rt, 1, "v2");
    rt.execute_query("KV PUT chain.'k' = 'v3'").unwrap();
    let p3 = commit(&rt, 1, "v3");

    assert_eq!(live_value(&rt, "chain", "k"), Some("v3".to_string()));
    assert_eq!(as_of_value(&rt, "chain", "k", &p1), Some("v1".to_string()));
    assert_eq!(as_of_value(&rt, "chain", "k", &p2), Some("v2".to_string()));
    assert_eq!(as_of_value(&rt, "chain", "k", &p3), Some("v3".to_string()));
}

/// Test E — resurrection: after a tombstoning DELETE, a fresh PUT of the
/// same key is visible live again, while the pre-delete history remains
/// reachable by AS OF.
#[test]
fn versioned_kv_put_after_delete_resurrects_key() {
    let rt = rt();
    rt.execute_query("KV PUT res.'k' = 'seed'").unwrap();
    vcs(&rt).set_versioned("res", true).unwrap();

    rt.execute_query("KV PUT res.'k' = 'v1'").unwrap();
    let p1 = commit(&rt, 1, "v1");
    rt.execute_query("KV DELETE res.'k'").unwrap();
    let _pdel = commit(&rt, 1, "delete");
    assert_eq!(live_value(&rt, "res", "k"), None, "deleted: absent live");

    rt.execute_query("KV PUT res.'k' = 'v2'").unwrap();
    let p2 = commit(&rt, 1, "v2");

    assert_eq!(
        live_value(&rt, "res", "k"),
        Some("v2".to_string()),
        "re-PUT resurrects the key live"
    );
    assert_eq!(
        as_of_value(&rt, "res", "k", &p1),
        Some("v1".to_string()),
        "pre-delete history still reachable"
    );
    assert_eq!(as_of_value(&rt, "res", "k", &p2), Some("v2".to_string()));
}
