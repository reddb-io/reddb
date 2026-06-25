//! Versioned GRAPH collections retain MVCC history (Phase 3 of the
//! multi-model versioning rollout; KV was Phase 1, documents Phase 2).
//!
//! A graph collection that has opted into versioning
//! (`vcs.set_versioned(collection, true)`) must keep prior node
//! versions physically alive so time-travel (`AS OF`) reads resolve the
//! snapshot-visible version per logical node — exactly like a versioned
//! table row / document. Graph nodes are stored as their own
//! `EntityKind::GraphNode`, but they carry the same `logical_id` /
//! `xmin` / `xmax` MVCC fields a table row does, so the table-row
//! versioning machinery applies once the node carries an explicit
//! logical id.
//!
//! A non-versioned graph collection keeps last-writer-wins semantics
//! (in-place mutation, no history accumulation).

use std::sync::Arc;

use reddb::application::{Author, CreateCommitInput, VcsUseCases};
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
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

/// Insert a node `(label, node_type, score)` and return its stable `rid`.
fn insert_node(rt: &RedDBRuntime, collection: &str, label: &str, score: i64) -> u64 {
    let sql = format!(
        "INSERT INTO {collection} NODE (label, node_type, score) \
         VALUES ('{label}', 'person', {score}) RETURNING rid"
    );
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

/// UPDATE the node's `score` to `value`.
fn update_score(rt: &RedDBRuntime, collection: &str, label: &str, value: i64) {
    let sql = format!("UPDATE {collection} NODES SET score = {value} WHERE label = '{label}'");
    rt.execute_query(&sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

/// Read `score` for the node with `label` at a historical commit via
/// SQL `AS OF`. Returns `None` when not visible.
fn as_of_score(rt: &RedDBRuntime, collection: &str, label: &str, commit_hash: &str) -> Option<i64> {
    let sql =
        format!("SELECT score FROM {collection} AS OF COMMIT '{commit_hash}' WHERE label = '{label}'");
    read_single_score(rt, &sql)
}

/// Read `score` for the node with `label` against the live snapshot.
fn live_score(rt: &RedDBRuntime, collection: &str, label: &str) -> Option<i64> {
    let sql = format!("SELECT score FROM {collection} WHERE label = '{label}'");
    read_single_score(rt, &sql)
}

fn read_single_score(rt: &RedDBRuntime, sql: &str) -> Option<i64> {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    assert!(
        result.result.records.len() <= 1,
        "read must resolve at most one visible version, got {} for `{sql}`",
        result.result.records.len()
    );
    result.result.records.first().map(|record| {
        let value = record.get("score").or_else(|| record.get("SCORE"));
        match value {
            Some(Value::Integer(v)) => *v,
            Some(Value::UnsignedInteger(v)) => *v as i64,
            Some(Value::Float(v)) => *v as i64,
            other => panic!("expected int score, got {other:?} in {record:?} for `{sql}`"),
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

/// Time-travel: versioned graph node, insert score=1 @P1, UPDATE to 2,
/// AS OF P1 must still see score=1.
#[test]
fn versioned_graph_node_update_retains_prior_version_for_as_of() {
    let rt = rt();
    set_current_connection_id(70001);
    rt.execute_query("CREATE GRAPH vgraph_hist")
        .expect("CREATE GRAPH");
    vcs(&rt).set_versioned("vgraph_hist", true).unwrap();

    let _rid = insert_node(&rt, "vgraph_hist", "alice", 1);
    let p1 = commit(&rt, 70001, "p1");

    update_score(&rt, "vgraph_hist", "alice", 2);
    let _p2 = commit(&rt, 70001, "p2");

    assert_eq!(
        live_score(&rt, "vgraph_hist", "alice"),
        Some(2),
        "live is newest"
    );
    assert_eq!(
        as_of_score(&rt, "vgraph_hist", "alice", &p1),
        Some(1),
        "AS OF P1 must resolve the prior node version"
    );
    clear_current_connection_id();
}

/// Delete-tombstone: versioned graph node, insert score=1 @P1, DELETE,
/// current read absent, AS OF P1 still resolves the node.
#[test]
fn versioned_graph_node_delete_keeps_history_for_as_of() {
    let rt = rt();
    set_current_connection_id(70002);
    rt.execute_query("CREATE GRAPH vgraph_del")
        .expect("CREATE GRAPH");
    vcs(&rt).set_versioned("vgraph_del", true).unwrap();

    let _rid = insert_node(&rt, "vgraph_del", "alice", 1);
    let p1 = commit(&rt, 70002, "p1");

    rt.execute_query("DELETE FROM vgraph_del WHERE label = 'alice'")
        .expect("DELETE node");
    let _p2 = commit(&rt, 70002, "p2");

    assert_eq!(
        live_score(&rt, "vgraph_del", "alice"),
        None,
        "deleted node must be absent from the live snapshot"
    );
    assert_eq!(
        as_of_score(&rt, "vgraph_del", "alice", &p1),
        Some(1),
        "AS OF P1 must still resolve the pre-delete node version"
    );
    clear_current_connection_id();
}

/// First-committer-wins: two txns both snapshot score=1, both UPDATE,
/// T1 commits, T2 commit must fail with a serialization conflict.
#[test]
fn concurrent_versioned_graph_node_update_conflicts_on_second_commit() {
    let rt = rt();
    set_current_connection_id(70003);
    rt.execute_query("CREATE GRAPH vgraph_wc")
        .expect("CREATE GRAPH");
    vcs(&rt).set_versioned("vgraph_wc", true).unwrap();
    let _rid = insert_node(&rt, "vgraph_wc", "alice", 1);
    commit(&rt, 70003, "seed");

    set_current_connection_id(70004);
    rt.execute_query("BEGIN").expect("T1 begin");
    set_current_connection_id(70005);
    rt.execute_query("BEGIN").expect("T2 begin");

    set_current_connection_id(70004);
    update_score(&rt, "vgraph_wc", "alice", 2);
    set_current_connection_id(70005);
    update_score(&rt, "vgraph_wc", "alice", 3);

    set_current_connection_id(70004);
    rt.execute_query("COMMIT").expect("T1 commit");
    set_current_connection_id(70005);
    let err = rt
        .execute_query("COMMIT")
        .expect_err("T2 commit must conflict");
    assert_conflict(&err);

    set_current_connection_id(70006);
    assert_eq!(
        live_score(&rt, "vgraph_wc", "alice"),
        Some(2),
        "winner's value survives"
    );
    clear_current_connection_id();
}

/// Regression: a NON-versioned graph collection keeps last-writer-wins —
/// sequential UPDATEs do not conflict, the live read sees the latest
/// value, and no prior versions accumulate.
#[test]
fn non_versioned_graph_node_is_last_writer_wins() {
    let rt = rt();
    set_current_connection_id(70007);
    rt.execute_query("CREATE GRAPH plain_graph")
        .expect("CREATE GRAPH");

    let _rid = insert_node(&rt, "plain_graph", "alice", 1);
    commit(&rt, 70007, "p1");

    update_score(&rt, "plain_graph", "alice", 2);
    update_score(&rt, "plain_graph", "alice", 3);
    commit(&rt, 70007, "p2");

    assert_eq!(
        live_score(&rt, "plain_graph", "alice"),
        Some(3),
        "last writer wins"
    );
    let count = rt
        .execute_query("SELECT score FROM plain_graph WHERE label = 'alice'")
        .expect("scan plain_graph")
        .result
        .records
        .len();
    assert_eq!(
        count, 1,
        "non-versioned graph node must keep no history; got {count} live rows"
    );
    clear_current_connection_id();
}
