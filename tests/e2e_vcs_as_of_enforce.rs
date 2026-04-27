//! Phase 4c: AS OF enforcement in the query executor.
//!
//! Once the parser stores an `AS OF` clause on `TableQuery`, the
//! runtime resolves it to an MVCC xid and installs that snapshot
//! for the query's lifetime — so scans filter on visibility at the
//! chosen point in time instead of the connection's current view.

use std::sync::Arc;

use reddb::application::{
    AsOfSpec, Author, CheckoutInput, CheckoutTarget, CreateBranchInput, CreateCommitInput,
    VcsUseCases,
};
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

#[test]
fn as_of_commit_resolves_to_commit_root_xid() {
    let rt = rt();
    let c1 = commit(&rt, 1, "c1");
    let c2 = commit(&rt, 1, "c2");

    let x1 = vcs(&rt)
        .resolve_as_of(AsOfSpec::Commit(c1.clone()))
        .expect("resolve c1");
    let x2 = vcs(&rt)
        .resolve_as_of(AsOfSpec::Commit(c2.clone()))
        .expect("resolve c2");
    assert!(x1 <= x2, "commit ordering preserved in xid ordering");
}

#[test]
fn as_of_branch_sql_parses_and_executes() {
    let rt = rt();
    let _c1 = commit(&rt, 1, "c1");
    // Query the bootstrap `red_commits` collection — always exists
    // and should return our one commit record. The AS OF BRANCH
    // clause must parse, resolve, and install the snapshot; scan
    // must succeed.
    let result = rt
        .execute_query("SELECT * FROM red_commits AS OF BRANCH 'main'")
        .expect("query executes");
    assert!(!result.result.records.is_empty(), "red_commits has rows");
}

#[test]
fn as_of_commit_sql_parses_and_executes() {
    let rt = rt();
    let c1 = commit(&rt, 1, "c1");
    let sql = format!("SELECT * FROM red_commits AS OF COMMIT '{}'", c1);
    let result = rt.execute_query(&sql).expect("query executes");
    assert!(!result.result.records.is_empty());
}

#[test]
fn as_of_unknown_commit_errors() {
    let rt = rt();
    let _ = commit(&rt, 1, "c1");
    let err = rt
        .execute_query(
            "SELECT * FROM red_commits AS OF COMMIT \
             '0000000000000000000000000000000000000000000000000000000000000000'",
        )
        .expect_err("unknown commit hash");
    let msg = format!("{err}");
    assert!(msg.contains("not found"), "got `{msg}`");
}

#[test]
fn as_of_snapshot_does_not_error_on_valid_xid() {
    // Raw xid 1 is below every real allocation and must succeed
    // unconditionally — resolver returns it as-is and the scan
    // runs against that snapshot. Rows stamped xmin=0 (pre-MVCC /
    // autocommit bootstrap) stay visible; tx-stamped rows with
    // higher xmin are filtered out. This test only asserts the
    // query executes — deeper visibility semantics are covered by
    // the MVCC snapshot unit tests.
    let rt = rt();
    let _c = commit(&rt, 1, "c1");
    let _ = rt
        .execute_query("SELECT * FROM red_commits AS OF SNAPSHOT 1")
        .expect("snapshot xid executes");
}

#[test]
fn as_of_branch_after_checkout_divergent_history() {
    let rt = rt();
    commit(&rt, 1, "root");
    vcs(&rt)
        .branch_create(CreateBranchInput {
            name: "side".to_string(),
            from: None,
            connection_id: 1,
        })
        .unwrap();
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: 1,
            target: CheckoutTarget::Branch("side".to_string()),
            force: false,
        })
        .unwrap();
    let side_head = commit(&rt, 1, "side-1");

    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: 1,
            target: CheckoutTarget::Branch("main".to_string()),
            force: false,
        })
        .unwrap();
    let main_head = commit(&rt, 1, "main-1");

    // Each branch's head commit has its own root_xid — so AS OF
    // BRANCH resolves to distinct xids for main vs side.
    let side_xid = vcs(&rt)
        .resolve_as_of(AsOfSpec::Branch("side".to_string()))
        .unwrap();
    let main_xid = vcs(&rt)
        .resolve_as_of(AsOfSpec::Branch("main".to_string()))
        .unwrap();
    assert_ne!(side_xid, main_xid);

    let by_hash_side = vcs(&rt).resolve_as_of(AsOfSpec::Commit(side_head)).unwrap();
    let by_hash_main = vcs(&rt).resolve_as_of(AsOfSpec::Commit(main_head)).unwrap();
    assert_eq!(by_hash_side, side_xid);
    assert_eq!(by_hash_main, main_xid);
}
