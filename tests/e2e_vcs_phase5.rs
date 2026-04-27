//! Phase 5 e2e: cherry-pick + revert commit bookkeeping, non-FF
//! merge conflict materialisation in `red_conflicts`.
//!
//! These tests cover the commit-graph + merge-state layer. Data
//! body reconciliation lands in Phase 6; here we validate that the
//! runtime writes the right commits, refs, and merge-state rows and
//! surfaces a well-shaped MergeOutcome.

use std::sync::Arc;

use reddb::application::{
    Author, CheckoutInput, CheckoutTarget, CreateBranchInput, CreateCommitInput, MergeInput,
    MergeOpts, VcsUseCases,
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
fn cherry_pick_creates_new_commit_on_head() {
    let rt = rt();
    let c1 = commit(&rt, 1, "root");
    // Branch off and make a commit we want to cherry-pick.
    vcs(&rt)
        .branch_create(CreateBranchInput {
            name: "feature".to_string(),
            from: None,
            connection_id: 1,
        })
        .unwrap();
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: 1,
            target: CheckoutTarget::Branch("feature".to_string()),
            force: false,
        })
        .unwrap();
    let feature_commit = commit(&rt, 1, "feat payload");

    // Back to main.
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: 1,
            target: CheckoutTarget::Branch("main".to_string()),
            force: false,
        })
        .unwrap();

    let outcome = vcs(&rt)
        .cherry_pick(1, &feature_commit, author())
        .expect("cherry-pick");
    let pick = outcome.merge_commit.expect("picked commit present");
    assert_eq!(pick.parents, vec![c1.clone()]);
    assert!(pick.message.starts_with("cherry-pick: "));
    assert!(outcome.merge_state_id.as_ref().unwrap().starts_with("cp:"));
}

#[test]
fn cherry_pick_of_root_commit_errors() {
    let rt = rt();
    let root = commit(&rt, 1, "root");
    let err = vcs(&rt)
        .cherry_pick(1, &root, author())
        .expect_err("root has no parent");
    assert!(format!("{err}").contains("root commit"));
}

#[test]
fn cherry_pick_of_merge_commit_errors() {
    let rt = rt();
    commit(&rt, 1, "root");
    vcs(&rt)
        .branch_create(CreateBranchInput {
            name: "alpha".to_string(),
            from: None,
            connection_id: 1,
        })
        .unwrap();
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: 1,
            target: CheckoutTarget::Branch("alpha".to_string()),
            force: false,
        })
        .unwrap();
    commit(&rt, 1, "alpha-1");
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: 1,
            target: CheckoutTarget::Branch("main".to_string()),
            force: false,
        })
        .unwrap();
    commit(&rt, 1, "main-1");
    let outcome = vcs(&rt)
        .merge(MergeInput {
            connection_id: 1,
            from: "alpha".to_string(),
            opts: MergeOpts::default(),
            author: author(),
        })
        .unwrap();
    let merge_hash = outcome.merge_commit.unwrap().hash;

    let err = vcs(&rt)
        .cherry_pick(1, &merge_hash, author())
        .expect_err("merge commit refused");
    assert!(format!("{err}").contains("merge commit"));
}

#[test]
fn revert_creates_new_commit_with_prefixed_message() {
    let rt = rt();
    commit(&rt, 1, "root");
    let target = commit(&rt, 1, "feature: thing");
    // Advance so HEAD is beyond the reverted commit.
    commit(&rt, 1, "unrelated");

    let r = vcs(&rt).revert(1, &target, author()).expect("revert");
    assert!(r.message.starts_with("Revert \""));
    assert_eq!(r.parents.len(), 1);
}

#[test]
fn revert_of_root_errors() {
    let rt = rt();
    let root = commit(&rt, 1, "root");
    let err = vcs(&rt)
        .revert(1, &root, author())
        .expect_err("root has no parent");
    assert!(format!("{err}").contains("root commit"));
}

#[test]
fn non_ff_merge_records_conflicts_count() {
    let rt = rt();
    commit(&rt, 1, "root");
    vcs(&rt)
        .branch_create(CreateBranchInput {
            name: "alpha".to_string(),
            from: None,
            connection_id: 1,
        })
        .unwrap();
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: 1,
            target: CheckoutTarget::Branch("alpha".to_string()),
            force: false,
        })
        .unwrap();
    commit(&rt, 1, "alpha-1");
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: 1,
            target: CheckoutTarget::Branch("main".to_string()),
            force: false,
        })
        .unwrap();
    commit(&rt, 1, "main-1");

    let outcome = vcs(&rt)
        .merge(MergeInput {
            connection_id: 1,
            from: "alpha".to_string(),
            opts: MergeOpts::default(),
            author: author(),
        })
        .expect("non-ff merge");
    assert!(!outcome.fast_forward);
    // With only empty commits (no user data changes) the conflict
    // count must be zero — commit-graph divergence alone is not a
    // conflict.
    assert_eq!(outcome.conflicts.len(), 0);
    let msid = outcome.merge_state_id.unwrap();
    // conflicts_list reads back what materialize_merge_conflicts
    // wrote to red_conflicts; should match outcome.conflicts.
    let listed = vcs(&rt).conflicts_list(&msid).expect("conflicts_list");
    assert_eq!(listed.len(), 0);
}

#[test]
fn log_includes_merge_and_cherry_pick_commits() {
    let rt = rt();
    commit(&rt, 1, "c1");
    commit(&rt, 1, "c2");
    let c3 = commit(&rt, 1, "c3");
    // cherry-pick c3 back onto itself — HEAD advances with a new
    // commit whose message has the cherry-pick prefix.
    vcs(&rt).cherry_pick(1, &c3, author()).expect("cherry-pick");

    let log = vcs(&rt)
        .log(reddb::application::LogInput {
            connection_id: 1,
            range: reddb::application::LogRange::default(),
        })
        .expect("log");
    let messages: Vec<String> = log.iter().map(|c| c.message.clone()).collect();
    assert!(
        messages.iter().any(|m| m.starts_with("cherry-pick: ")),
        "log shows cherry-pick commit; got {:?}",
        messages
    );
}
