//! Versioned DOCUMENT collections retain MVCC history (Phase 2 of the
//! multi-model versioning rollout; KV was Phase 1).
//!
//! A document collection that has opted into versioning
//! (`vcs.set_versioned(collection, true)`) must keep prior versions
//! physically alive so time-travel (`AS OF`) reads resolve the
//! snapshot-visible version per logical document — exactly like a
//! versioned table row / KV key. Version-selection keys on the
//! document's `logical_id` (documents already carry one) rather than
//! the KV `key` text shim.
//!
//! A non-versioned document collection keeps last-writer-wins
//! semantics (in-place mutation, no history accumulation).

use std::sync::Arc;

use reddb::application::{
    Author, CreateCommitInput, PatchEntityInput, PatchEntityOperation, PatchEntityOperationType,
    RuntimeEntityPort, VcsUseCases,
};
use reddb::json::Value as RedJsonValue;
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::storage::schema::Value;
use reddb::storage::EntityId;
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

/// Insert a single document `{a: <a>}` into `collection` and return
/// its stable `rid`.
fn insert_doc(rt: &RedDBRuntime, collection: &str, a: i64) -> u64 {
    let sql =
        format!("INSERT INTO {collection} DOCUMENT (body) VALUES ('{{\"a\":{a}}}') RETURNING *");
    let result = rt
        .execute_query(&sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    assert_eq!(result.result.records.len(), 1, "{sql}");
    match result.result.records[0].get("rid") {
        Some(Value::UnsignedInteger(rid)) => *rid,
        Some(Value::Integer(rid)) => *rid as u64,
        Some(Value::Text(rid)) => rid.parse().expect("rid text -> u64"),
        other => panic!("expected rid, got {other:?}"),
    }
}

/// PATCH the document body field `a` to `value` via the public patch
/// core (the body-merge path — produces a merged full document).
fn patch_doc_a(rt: &RedDBRuntime, collection: &str, rid: u64, value: i64) {
    rt.patch_entity(PatchEntityInput {
        collection: collection.to_string(),
        id: EntityId::new(rid),
        payload: RedJsonValue::Null,
        operations: vec![PatchEntityOperation {
            op: PatchEntityOperationType::Set,
            path: vec!["body".to_string(), "a".to_string()],
            value: Some(RedJsonValue::Number(value as f64)),
        }],
    })
    .unwrap_or_else(|err| panic!("patch a={value}: {err:?}"));
}

/// Read body field `a` for the document with `rid` at a historical
/// commit via SQL `AS OF`. Returns `None` when not visible.
fn as_of_a(rt: &RedDBRuntime, collection: &str, rid: u64, commit_hash: &str) -> Option<i64> {
    let sql =
        format!("SELECT body.a FROM {collection} AS OF COMMIT '{commit_hash}' WHERE rid = {rid}");
    read_single_a(rt, &sql)
}

/// Read body field `a` for the document with `rid` against the current
/// (live) snapshot.
fn live_a(rt: &RedDBRuntime, collection: &str, rid: u64) -> Option<i64> {
    let sql = format!("SELECT body.a FROM {collection} WHERE rid = {rid}");
    read_single_a(rt, &sql)
}

fn read_single_a(rt: &RedDBRuntime, sql: &str) -> Option<i64> {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    assert!(
        result.result.records.len() <= 1,
        "read must resolve at most one visible version, got {} for `{sql}`",
        result.result.records.len()
    );
    result.result.records.first().map(|record| {
        let value = record
            .get("body.A")
            .or_else(|| record.get("body.a"))
            .or_else(|| record.get("a"))
            .or_else(|| record.get("A"));
        match value {
            Some(Value::Integer(v)) => *v,
            Some(Value::UnsignedInteger(v)) => *v as i64,
            Some(Value::Float(v)) => *v as i64,
            Some(Value::Json(bytes)) => {
                let parsed: serde_json::Value = serde_json::from_slice(bytes)
                    .unwrap_or_else(|err| panic!("decode json a: {err} for `{sql}`"));
                parsed
                    .as_i64()
                    .unwrap_or_else(|| panic!("json a not int: {parsed:?} for `{sql}`"))
            }
            other => panic!("expected int a, got {other:?} in {record:?} for `{sql}`"),
        }
    })
}

fn assert_conflict(err: &reddb::RedDBError) {
    let msg = format!("{err:?}");
    assert!(
        msg.contains("serialization conflict"),
        "expected serialization conflict, got: {msg}"
    );
}

/// Time-travel: versioned document, insert {a:1}@P1, PATCH to {a:2},
/// AS OF P1 must still see {a:1}.
#[test]
fn versioned_document_patch_retains_prior_version_for_as_of() {
    let rt = rt();
    set_current_connection_id(60001);
    rt.execute_query("CREATE DOCUMENT vdoc_hist")
        .expect("CREATE DOCUMENT");
    vcs(&rt).set_versioned("vdoc_hist", true).unwrap();

    let rid = insert_doc(&rt, "vdoc_hist", 1);
    let p1 = commit(&rt, 60001, "p1");

    patch_doc_a(&rt, "vdoc_hist", rid, 2);
    let _p2 = commit(&rt, 60001, "p2");

    assert_eq!(live_a(&rt, "vdoc_hist", rid), Some(2), "live is newest");
    assert_eq!(
        as_of_a(&rt, "vdoc_hist", rid, &p1),
        Some(1),
        "AS OF P1 must resolve the prior document version"
    );
    clear_current_connection_id();
}

/// Delete-tombstone: versioned document, insert {a:1}@P1, DELETE,
/// current read absent, AS OF P1 still resolves the document.
#[test]
fn versioned_document_delete_keeps_history_for_as_of() {
    let rt = rt();
    set_current_connection_id(60002);
    rt.execute_query("CREATE DOCUMENT vdoc_del")
        .expect("CREATE DOCUMENT");
    vcs(&rt).set_versioned("vdoc_del", true).unwrap();

    let rid = insert_doc(&rt, "vdoc_del", 1);
    let p1 = commit(&rt, 60002, "p1");

    rt.execute_query(&format!("DELETE FROM vdoc_del WHERE rid = {rid}"))
        .expect("DELETE document");
    let _p2 = commit(&rt, 60002, "p2");

    assert_eq!(
        live_a(&rt, "vdoc_del", rid),
        None,
        "deleted document must be absent from the live snapshot"
    );
    assert_eq!(
        as_of_a(&rt, "vdoc_del", rid, &p1),
        Some(1),
        "AS OF P1 must still resolve the pre-delete document version"
    );
    clear_current_connection_id();
}

/// First-committer-wins: two txns both snapshot {a:1}, both PATCH,
/// T1 commits, T2 commit must fail with a serialization conflict.
#[test]
fn concurrent_versioned_document_patch_conflicts_on_second_commit() {
    let rt = rt();
    set_current_connection_id(60003);
    rt.execute_query("CREATE DOCUMENT vdoc_wc")
        .expect("CREATE DOCUMENT");
    vcs(&rt).set_versioned("vdoc_wc", true).unwrap();
    let rid = insert_doc(&rt, "vdoc_wc", 1);
    commit(&rt, 60003, "seed");

    // Two transactions both snapshot the {a:1} state.
    set_current_connection_id(60004);
    rt.execute_query("BEGIN").expect("T1 begin");
    set_current_connection_id(60005);
    rt.execute_query("BEGIN").expect("T2 begin");

    // T1 PATCH a=2, T2 PATCH a=3 — both tombstone the same version.
    set_current_connection_id(60004);
    patch_doc_a(&rt, "vdoc_wc", rid, 2);
    set_current_connection_id(60005);
    patch_doc_a(&rt, "vdoc_wc", rid, 3);

    // T1 commits (winner).
    set_current_connection_id(60004);
    rt.execute_query("COMMIT").expect("T1 commit");
    // T2 commits (loser) — must conflict.
    set_current_connection_id(60005);
    let err = rt
        .execute_query("COMMIT")
        .expect_err("T2 commit must conflict");
    assert_conflict(&err);

    // Post-state: exactly one live version (the winner's).
    set_current_connection_id(60006);
    assert_eq!(
        live_a(&rt, "vdoc_wc", rid),
        Some(2),
        "winner's value survives"
    );
    clear_current_connection_id();
}

/// Regression: a NON-versioned document collection keeps
/// last-writer-wins — sequential PATCHes do not conflict, the live
/// read sees the latest value, and no prior versions accumulate (the
/// physical version count for the logical document stays at 1).
#[test]
fn non_versioned_document_is_last_writer_wins() {
    let rt = rt();
    set_current_connection_id(60007);
    rt.execute_query("CREATE DOCUMENT plain_doc")
        .expect("CREATE DOCUMENT");
    // No set_versioned — stays non-versioned.

    let rid = insert_doc(&rt, "plain_doc", 1);
    commit(&rt, 60007, "p1");

    patch_doc_a(&rt, "plain_doc", rid, 2);
    patch_doc_a(&rt, "plain_doc", rid, 3);
    commit(&rt, 60007, "p2");

    assert_eq!(live_a(&rt, "plain_doc", rid), Some(3), "last writer wins");
    // Non-versioned: the live scan resolves exactly one row for the
    // logical document — no prior versions linger to be selected.
    let count = rt
        .execute_query(&format!("SELECT body.a FROM plain_doc WHERE rid = {rid}"))
        .expect("scan plain_doc")
        .result
        .records
        .len();
    assert_eq!(
        count, 1,
        "non-versioned document must keep no history; got {count} live rows"
    );
    clear_current_connection_id();
}
