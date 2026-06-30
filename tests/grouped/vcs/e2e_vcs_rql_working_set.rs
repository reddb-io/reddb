//! Issue #1571: VCS working-set verbs through RQL.
//!
//! CHECKPOINT / CHECKOUT / RESET / MERGE / CHERRY PICK / REVERT /
//! RESOLVE CONFLICT all route through `ReddbVcsPort` with the
//! connection id resolved implicitly from the executing connection
//! (`current_connection_id()`), and are verifiable through the slice-1
//! read surface (`red.commits` / `red.status`).
//!
//! Genuine data-body merge-conflict materialisation (the `red_conflicts`
//! rows a non-trivial merge would produce) is Phase 6 of the VCS engine
//! and is exercised at the port layer in `e2e_vcs_phase5`; here the
//! RESOLVE CONFLICT verb is verified by confirming it reaches the port
//! (which reports `not found` when no conflict exists for the key).

use reddb::application::{
    Author, CheckoutInput, CheckoutTarget, CreateBranchInput, CreateCommitInput, VcsUseCases,
};
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::storage::query::unified::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime")
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

/// Seed a commit on `conn` through the port (mirrors the read-surface
/// helper) so the RQL verbs have a working set / history to act on.
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

fn checkout_branch(rt: &RedDBRuntime, conn: u64, branch: &str) {
    vcs(rt)
        .checkout(CheckoutInput {
            connection_id: conn,
            target: CheckoutTarget::Branch(branch.to_string()),
            force: false,
        })
        .expect("checkout branch");
}

fn text<'a>(record: &'a UnifiedRecord, field: &str) -> &'a str {
    match record.get(field) {
        Some(Value::Text(value)) => value.as_ref(),
        other => panic!("expected text field {field}, got {other:?} in {record:?}"),
    }
}

fn head_field(rt: &RedDBRuntime, field: &str) -> String {
    let status = rt
        .execute_query(&format!("SELECT {field} FROM red.status"))
        .expect("red.status");
    assert_eq!(status.result.records.len(), 1, "expected one status row");
    text(&status.result.records[0], field).to_string()
}

fn head_commit_message(rt: &RedDBRuntime) -> String {
    let commits = rt
        .execute_query("SELECT message FROM red.commits ORDER BY height DESC LIMIT 1")
        .expect("red.commits");
    assert_eq!(commits.result.records.len(), 1);
    text(&commits.result.records[0], "message").to_string()
}

#[test]
fn rql_checkpoint_creates_commit_at_head() {
    let rt = rt();
    set_current_connection_id(1);
    commit(&rt, 1, "root");

    rt.execute_query("CHECKPOINT 'rql checkpoint' AUTHOR 'Ada <ada@reddb.io>'")
        .expect("checkpoint via rql");

    assert_eq!(head_commit_message(&rt), "rql checkpoint");
    clear_current_connection_id();
}

#[test]
fn rql_checkout_switches_head_ref() {
    let rt = rt();
    set_current_connection_id(2);
    let c1 = commit(&rt, 2, "root");
    vcs(&rt)
        .branch_create(CreateBranchInput {
            name: "feature".to_string(),
            from: Some(c1),
            connection_id: 2,
        })
        .expect("branch");

    rt.execute_query("CHECKOUT feature")
        .expect("checkout via rql");

    assert_eq!(head_field(&rt, "head_ref"), "refs/heads/feature");
    clear_current_connection_id();
}

#[test]
fn rql_reset_soft_moves_head_and_hard_is_unimplemented() {
    let rt = rt();
    set_current_connection_id(3);
    let c1 = commit(&rt, 3, "c1");
    commit(&rt, 3, "c2");

    rt.execute_query(&format!("RESET SOFT TO {c1}"))
        .expect("reset soft via rql");
    assert_eq!(head_field(&rt, "head_commit"), c1);

    // HARD mode parses + routes, but the port rejects it (Phase 4).
    let err = rt
        .execute_query(&format!("RESET HARD TO {c1}"))
        .expect_err("reset --hard not implemented");
    assert!(
        format!("{err}").contains("reset --hard"),
        "expected reset --hard error, got `{err}`"
    );
    clear_current_connection_id();
}

#[test]
fn rql_merge_fast_forward_then_checkpoint_round_trip() {
    let rt = rt();
    set_current_connection_id(4);
    commit(&rt, 4, "root");
    vcs(&rt)
        .branch_create(CreateBranchInput {
            name: "feature".to_string(),
            from: None,
            connection_id: 4,
        })
        .expect("branch");
    checkout_branch(&rt, 4, "feature");
    let feat = commit(&rt, 4, "feat-1");

    rt.execute_query("CHECKOUT main").expect("checkout main");
    rt.execute_query("MERGE 'feature'").expect("merge via rql");

    // Fast-forward advances main to the feature tip.
    assert_eq!(head_field(&rt, "head_ref"), "refs/heads/main");
    assert_eq!(head_field(&rt, "head_commit"), feat);

    rt.execute_query("CHECKPOINT 'post-merge'")
        .expect("checkpoint via rql");
    assert_eq!(head_commit_message(&rt), "post-merge");
    clear_current_connection_id();
}

#[test]
fn rql_cherry_pick_creates_commit_on_head() {
    let rt = rt();
    set_current_connection_id(5);
    commit(&rt, 5, "root");
    vcs(&rt)
        .branch_create(CreateBranchInput {
            name: "feature".to_string(),
            from: None,
            connection_id: 5,
        })
        .expect("branch");
    checkout_branch(&rt, 5, "feature");
    let feat = commit(&rt, 5, "feat payload");
    checkout_branch(&rt, 5, "main");

    rt.execute_query(&format!("CHERRY PICK {feat}"))
        .expect("cherry pick via rql");
    assert!(
        head_commit_message(&rt).starts_with("cherry-pick: "),
        "expected cherry-pick commit at head"
    );
    clear_current_connection_id();
}

#[test]
fn rql_revert_creates_commit_on_head() {
    let rt = rt();
    set_current_connection_id(6);
    commit(&rt, 6, "root");
    let target = commit(&rt, 6, "feature: thing");
    commit(&rt, 6, "unrelated");

    rt.execute_query(&format!("REVERT {target}"))
        .expect("revert via rql");
    assert!(
        head_commit_message(&rt).starts_with("Revert \""),
        "expected revert commit at head"
    );
    clear_current_connection_id();
}

#[test]
fn rql_resolve_conflict_reaches_port() {
    let rt = rt();
    set_current_connection_id(7);
    commit(&rt, 7, "root");

    // No conflict exists for the key, so the port reports it as missing —
    // proving the RQL verb parses, dispatches, and calls the VCS port for
    // both resolution strategies.
    let theirs = rt
        .execute_query("RESOLVE CONFLICT 'missing/1' USING THEIRS")
        .expect_err("no such conflict");
    assert!(
        format!("{theirs}").to_lowercase().contains("not found"),
        "expected not-found error, got `{theirs}`"
    );
    let ours = rt
        .execute_query("RESOLVE CONFLICT 'missing/2' USING OURS")
        .expect_err("no such conflict");
    assert!(
        format!("{ours}").to_lowercase().contains("not found"),
        "expected not-found error, got `{ours}`"
    );
    clear_current_connection_id();
}
