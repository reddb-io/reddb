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

use reddb::application::{
    Author, CheckoutInput, CheckoutTarget, CreateBranchInput, CreateCommitInput, CreateTagInput,
    DiffChange, DiffInput, MergeInput, MergeOpts, StatusInput, VcsUseCases,
};
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

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

fn insert_edge(rt: &RedDBRuntime, collection: &str, from: u64, to: u64) -> u64 {
    let result = rt
        .execute_query(&format!(
            "INSERT INTO {collection} EDGE (label, from_rid, to_rid, weight) \
             VALUES ('knows', {from}, {to}, 1.0) RETURNING rid"
        ))
        .expect("insert edge");
    match result.result.records[0].get("rid") {
        Some(Value::UnsignedInteger(rid)) => *rid,
        Some(Value::Integer(rid)) => *rid as u64,
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
    let sql = format!(
        "SELECT score FROM {collection} AS OF COMMIT '{commit_hash}' WHERE label = '{label}'"
    );
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

#[test]
#[ignore = "VCS checkout does not yet isolate graph reads and writes by branch"]
fn graph_branch_checkout_isolates_writes_from_main() {
    let rt = rt();
    let conn = 70008;
    set_current_connection_id(conn);
    rt.execute_query("CREATE GRAPH graph_branch_isolation")
        .expect("create graph");
    vcs(&rt)
        .set_versioned("graph_branch_isolation", true)
        .unwrap();
    insert_node(&rt, "graph_branch_isolation", "alice", 1);
    commit(&rt, conn, "base");
    vcs(&rt)
        .branch_create(CreateBranchInput {
            name: "agent".to_string(),
            from: None,
            connection_id: conn,
        })
        .unwrap();
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: conn,
            target: CheckoutTarget::Branch("agent".to_string()),
            force: false,
        })
        .unwrap();
    insert_node(&rt, "graph_branch_isolation", "agent-node", 2);
    commit(&rt, conn, "agent write");
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: conn,
            target: CheckoutTarget::Branch("main".to_string()),
            force: false,
        })
        .unwrap();

    assert_eq!(live_score(&rt, "graph_branch_isolation", "alice"), Some(1));
    assert_eq!(
        live_score(&rt, "graph_branch_isolation", "agent-node"),
        None
    );
    clear_current_connection_id();
}

#[test]
fn graph_commit_excludes_transactions_still_open_at_checkpoint() {
    let rt = rt();
    let checkpoint_conn = 70011;
    let writer_conn = 70012;
    set_current_connection_id(checkpoint_conn);
    rt.execute_query("CREATE GRAPH graph_checkpoint_snapshot")
        .expect("create graph");
    vcs(&rt)
        .set_versioned("graph_checkpoint_snapshot", true)
        .unwrap();
    insert_node(&rt, "graph_checkpoint_snapshot", "existing", 1);
    commit(&rt, checkpoint_conn, "base");

    set_current_connection_id(writer_conn);
    rt.execute_query("BEGIN").expect("begin writer");
    insert_node(&rt, "graph_checkpoint_snapshot", "pending", 1);
    update_score(&rt, "graph_checkpoint_snapshot", "existing", 2);
    set_current_connection_id(checkpoint_conn);
    let checkpoint = commit(&rt, checkpoint_conn, "checkpoint");
    vcs(&rt)
        .tag(CreateTagInput {
            name: "checkpoint".to_string(),
            target: checkpoint.clone(),
            annotation: None,
        })
        .expect("tag checkpoint");
    set_current_connection_id(writer_conn);
    rt.execute_query("COMMIT").expect("commit writer");
    set_current_connection_id(checkpoint_conn);

    assert_eq!(
        as_of_score(&rt, "graph_checkpoint_snapshot", "pending", &checkpoint),
        None
    );
    assert_eq!(
        as_of_score(&rt, "graph_checkpoint_snapshot", "existing", &checkpoint),
        Some(1)
    );
    for spec in ["BRANCH 'main'", "TAG 'checkpoint'"] {
        assert_eq!(
            read_single_score(
                &rt,
                &format!(
                    "SELECT score FROM graph_checkpoint_snapshot AS OF {spec} WHERE label = 'existing'"
                )
            ),
            Some(1)
        );
    }
    assert_eq!(
        live_score(&rt, "graph_checkpoint_snapshot", "pending"),
        Some(1)
    );
    let after = commit(&rt, checkpoint_conn, "writer committed");
    let diff = vcs(&rt)
        .diff(DiffInput {
            from: checkpoint.clone(),
            to: after,
            collection: Some("graph_checkpoint_snapshot".to_string()),
            summary_only: true,
        })
        .expect("diff after writer commit");
    assert_eq!((diff.added, diff.removed, diff.modified), (1, 0, 1));
    set_current_connection_id(writer_conn);
    rt.execute_query("BEGIN").expect("begin second writer");
    update_score(&rt, "graph_checkpoint_snapshot", "existing", 3);
    assert_eq!(
        as_of_score(&rt, "graph_checkpoint_snapshot", "existing", &checkpoint),
        Some(1)
    );
    rt.execute_query("ROLLBACK")
        .expect("rollback second writer");
    clear_current_connection_id();
}

#[test]
fn graph_checkpoint_snapshot_survives_reopen() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("graph-vcs.rdb");
    let checkpoint;
    let writer_xid;
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime");
        set_current_connection_id(70014);
        rt.execute_query("CREATE GRAPH graph_reopen_snapshot")
            .expect("create graph");
        vcs(&rt)
            .set_versioned("graph_reopen_snapshot", true)
            .unwrap();
        insert_node(&rt, "graph_reopen_snapshot", "existing", 1);
        commit(&rt, 70014, "base");
        set_current_connection_id(70015);
        rt.execute_query("BEGIN").expect("begin writer");
        writer_xid = rt
            .snapshot_manager()
            .oldest_active_xid()
            .expect("writer xid");
        update_score(&rt, "graph_reopen_snapshot", "existing", 2);
        set_current_connection_id(70014);
        checkpoint = commit(&rt, 70014, "checkpoint");
        set_current_connection_id(70015);
        rt.execute_query("COMMIT").expect("commit writer");
        clear_current_connection_id();
    }
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen");
    set_current_connection_id(70014);
    assert!(rt.snapshot_manager().is_pinned(writer_xid));
    assert_eq!(
        as_of_score(&rt, "graph_reopen_snapshot", "existing", &checkpoint),
        Some(1)
    );
    assert_eq!(
        live_score(&rt, "graph_reopen_snapshot", "existing"),
        Some(2)
    );
    clear_current_connection_id();
}

#[test]
fn graph_as_of_branch_observes_ref_changes_with_result_cache_enabled() {
    let rt = rt();
    let conn = 70016;
    set_current_connection_id(conn);
    rt.execute_query("SET CONFIG runtime.result_cache.enabled = true")
        .expect("enable result cache");
    rt.execute_query("CREATE GRAPH graph_branch_cache")
        .expect("create graph");
    vcs(&rt).set_versioned("graph_branch_cache", true).unwrap();
    insert_node(&rt, "graph_branch_cache", "alice", 1);
    commit(&rt, conn, "base");
    update_score(&rt, "graph_branch_cache", "alice", 2);

    let query = "SELECT score FROM graph_branch_cache AS OF BRANCH 'main' WHERE label = 'alice'";
    assert_eq!(read_single_score(&rt, query), Some(1));
    assert_eq!(read_single_score(&rt, query), Some(1));
    commit(&rt, conn, "advance main");
    assert_eq!(read_single_score(&rt, query), Some(2));
    clear_current_connection_id();
}

#[test]
fn graph_diff_reports_node_update_as_one_modified_item() {
    let rt = rt();
    let conn = 70009;
    set_current_connection_id(conn);
    rt.execute_query("CREATE GRAPH graph_diff_update")
        .expect("create graph");
    vcs(&rt).set_versioned("graph_diff_update", true).unwrap();
    let rid = insert_node(&rt, "graph_diff_update", "alice", 1);
    let before = commit(&rt, conn, "before");
    update_score(&rt, "graph_diff_update", "alice", 2);
    let after = commit(&rt, conn, "after");

    let diff = vcs(&rt)
        .diff(DiffInput {
            from: before,
            to: after,
            collection: Some("graph_diff_update".to_string()),
            summary_only: false,
        })
        .expect("diff");
    assert_eq!((diff.added, diff.removed, diff.modified), (0, 0, 1));
    assert_eq!(diff.entries.len(), 1);
    assert_eq!(diff.entries[0].entity_id, rid.to_string());
    assert!(matches!(
        diff.entries[0].change,
        DiffChange::Modified { .. }
    ));
    clear_current_connection_id();
}

#[test]
fn graph_diff_reports_edge_update_as_one_modified_item() {
    let rt = rt();
    let conn = 70010;
    set_current_connection_id(conn);
    rt.execute_query("CREATE GRAPH graph_edge_diff")
        .expect("create graph");
    vcs(&rt).set_versioned("graph_edge_diff", true).unwrap();
    let alice = insert_node(&rt, "graph_edge_diff", "alice", 1);
    let bob = insert_node(&rt, "graph_edge_diff", "bob", 1);
    let rid = insert_edge(&rt, "graph_edge_diff", alice, bob);
    let before = commit(&rt, conn, "before");
    rt.execute_query("UPDATE graph_edge_diff EDGES SET weight = 2.0 WHERE label = 'knows'")
        .expect("update edge");
    let after = commit(&rt, conn, "after");

    let diff = vcs(&rt)
        .diff(DiffInput {
            from: before,
            to: after,
            collection: Some("graph_edge_diff".to_string()),
            summary_only: false,
        })
        .expect("diff");
    assert_eq!((diff.added, diff.removed, diff.modified), (0, 0, 1));
    assert_eq!(diff.entries.len(), 1);
    assert_eq!(diff.entries[0].entity_id, rid.to_string());
    assert!(matches!(
        diff.entries[0].change,
        DiffChange::Modified { .. }
    ));
    clear_current_connection_id();
}

#[test]
fn graph_merge_detects_conflicting_updates_to_nodes_and_edges() {
    let rt = rt();
    let conn = 70013;
    set_current_connection_id(conn);
    rt.execute_query("CREATE GRAPH graph_merge_conflict")
        .expect("create graph");
    vcs(&rt)
        .set_versioned("graph_merge_conflict", true)
        .unwrap();
    let rid = insert_node(&rt, "graph_merge_conflict", "alice", 1);
    let bob = insert_node(&rt, "graph_merge_conflict", "bob", 1);
    let edge_rid = insert_edge(&rt, "graph_merge_conflict", rid, bob);
    commit(&rt, conn, "base");
    vcs(&rt)
        .branch_create(CreateBranchInput {
            name: "agent".to_string(),
            from: None,
            connection_id: conn,
        })
        .unwrap();
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: conn,
            target: CheckoutTarget::Branch("agent".to_string()),
            force: false,
        })
        .unwrap();
    update_score(&rt, "graph_merge_conflict", "alice", 2);
    rt.execute_query(
        "UPDATE graph_merge_conflict NODES SET node_type = 'engineer' WHERE label = 'alice'",
    )
    .expect("agent updates node type");
    rt.execute_query("UPDATE graph_merge_conflict EDGES SET weight = 2.0 WHERE label = 'knows'")
        .expect("agent updates edge");
    commit(&rt, conn, "agent update");
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: conn,
            target: CheckoutTarget::Branch("main".to_string()),
            force: false,
        })
        .unwrap();
    update_score(&rt, "graph_merge_conflict", "alice", 3);
    rt.execute_query(
        "UPDATE graph_merge_conflict NODES SET node_type = 'manager' WHERE label = 'alice'",
    )
    .expect("main updates node type");
    rt.execute_query("UPDATE graph_merge_conflict EDGES SET weight = 3.0 WHERE label = 'knows'")
        .expect("main updates edge");
    let main_head = commit(&rt, conn, "main update");

    let outcome = vcs(&rt)
        .merge(MergeInput {
            connection_id: conn,
            from: "agent".to_string(),
            opts: MergeOpts::default(),
            author: author(),
        })
        .expect("merge");
    assert_eq!(outcome.conflicts.len(), 2);
    let node_conflict = outcome
        .conflicts
        .iter()
        .find(|c| c.entity_id == rid.to_string())
        .expect("node conflict");
    assert_eq!(node_conflict.conflicting_paths.len(), 2);
    assert!(node_conflict
        .conflicting_paths
        .iter()
        .any(|path| path == "properties.score"));
    assert!(node_conflict
        .conflicting_paths
        .iter()
        .any(|path| path == "node_type"));
    let edge_conflict = outcome
        .conflicts
        .iter()
        .find(|c| c.entity_id == edge_rid.to_string())
        .expect("edge conflict");
    assert_eq!(edge_conflict.conflicting_paths, ["weight"]);
    assert!(
        outcome.merge_commit.is_none(),
        "conflicts must pause the merge"
    );
    let status = vcs(&rt)
        .status(StatusInput {
            connection_id: conn,
        })
        .expect("status");
    assert_eq!(status.head_commit.as_deref(), Some(main_head.as_str()));
    assert_eq!(status.merge_state_id, outcome.merge_state_id);
    assert_eq!(status.unresolved_conflicts, 2);
    let error = vcs(&rt)
        .conflict_resolve(
            &node_conflict.id,
            reddb::json::Value::String("theirs".to_string()),
        )
        .expect_err("resolution must not discard a conflict without applying its data");
    assert!(error.to_string().contains("conflict retained"), "{error}");
    assert_eq!(
        vcs(&rt)
            .status(StatusInput {
                connection_id: conn,
            })
            .expect("status after rejected resolution")
            .unresolved_conflicts,
        2
    );
    let error = vcs(&rt)
        .commit(CreateCommitInput {
            connection_id: conn,
            message: "must not advance".to_string(),
            author: author(),
            committer: None,
            amend: false,
            allow_empty: true,
        })
        .expect_err("commit must not bypass the pending merge");
    assert!(error.to_string().contains("pending"), "{error}");
    let error = vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: conn,
            target: CheckoutTarget::Branch("agent".to_string()),
            force: false,
        })
        .expect_err("checkout must not discard a pending merge");
    assert!(error.to_string().contains("pending"), "{error}");
    clear_current_connection_id();
}

#[test]
fn pending_graph_merge_status_survives_reopen() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("graph-merge.rdb");
    let merge_state_id;
    let main_head;
    let conn = 70017;
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime");
        set_current_connection_id(conn);
        rt.execute_query("CREATE GRAPH graph_pending_merge")
            .expect("create graph");
        vcs(&rt).set_versioned("graph_pending_merge", true).unwrap();
        insert_node(&rt, "graph_pending_merge", "alice", 1);
        commit(&rt, conn, "base");
        vcs(&rt)
            .branch_create(CreateBranchInput {
                name: "agent".to_string(),
                from: None,
                connection_id: conn,
            })
            .unwrap();
        vcs(&rt)
            .checkout(CheckoutInput {
                connection_id: conn,
                target: CheckoutTarget::Branch("agent".to_string()),
                force: false,
            })
            .unwrap();
        update_score(&rt, "graph_pending_merge", "alice", 2);
        commit(&rt, conn, "agent update");
        vcs(&rt)
            .checkout(CheckoutInput {
                connection_id: conn,
                target: CheckoutTarget::Branch("main".to_string()),
                force: false,
            })
            .unwrap();
        update_score(&rt, "graph_pending_merge", "alice", 3);
        main_head = commit(&rt, conn, "main update");
        let outcome = vcs(&rt)
            .merge(MergeInput {
                connection_id: conn,
                from: "agent".to_string(),
                opts: MergeOpts::default(),
                author: author(),
            })
            .expect("merge");
        assert_eq!(outcome.conflicts.len(), 1);
        merge_state_id = outcome.merge_state_id.expect("merge state");
        clear_current_connection_id();
    }

    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen");
    set_current_connection_id(conn);
    let status = vcs(&rt)
        .status(StatusInput {
            connection_id: conn,
        })
        .unwrap();
    assert_eq!(status.head_commit.as_deref(), Some(main_head.as_str()));
    assert_eq!(
        status.merge_state_id.as_deref(),
        Some(merge_state_id.as_str())
    );
    assert_eq!(status.unresolved_conflicts, 1);
    clear_current_connection_id();
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
