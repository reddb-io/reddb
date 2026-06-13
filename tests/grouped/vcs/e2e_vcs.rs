//! End-to-end tests for the VCS ("Git for Data") surface.
//!
//! Exercises commit / branch / tag / checkout / log / status / lca /
//! resolve_commitish / resolve_as_of / fast-forward merge / non-FF
//! merge (creates merge_state) / reset --soft / --mixed.
//!
//! Phase 3 scope — cherry-pick / revert / hard reset / data merge
//! remain stubbed and their tests live in Phase 4.

use std::sync::Arc;

use reddb::application::vcs::{
    AsOfSpec, Author, CheckoutInput, CheckoutTarget, CreateBranchInput, CreateCommitInput,
    CreateTagInput, DiffInput, LogInput, LogRange, MergeInput, MergeOpts, MergeStrategy,
    ResetInput, ResetMode, StatusInput,
};
use reddb::application::VcsUseCases;
use reddb::{RedDBOptions, RedDBRuntime};

fn rt() -> Arc<RedDBRuntime> {
    Arc::new(
        RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime boots"),
    )
}

fn author() -> Author {
    Author {
        name: "test".to_string(),
        email: "test@reddb.io".to_string(),
    }
}

fn commit_input(conn: u64, msg: &str) -> CreateCommitInput {
    CreateCommitInput {
        connection_id: conn,
        message: msg.to_string(),
        author: author(),
        committer: None,
        amend: false,
        allow_empty: true,
    }
}

fn vcs(rt: &RedDBRuntime) -> VcsUseCases<'_, RedDBRuntime> {
    VcsUseCases::new(rt)
}

#[test]
fn commit_and_log_linear_history() {
    let rt = rt();
    let c1 = vcs(&rt).commit(commit_input(1, "first")).expect("c1");
    let c2 = vcs(&rt).commit(commit_input(1, "second")).expect("c2");
    let c3 = vcs(&rt).commit(commit_input(1, "third")).expect("c3");

    assert_eq!(c1.height, 0);
    assert_eq!(c2.height, 1);
    assert_eq!(c3.height, 2);
    assert_eq!(c2.parents, vec![c1.hash.clone()]);
    assert_eq!(c3.parents, vec![c2.hash.clone()]);

    let log = vcs(&rt)
        .log(LogInput {
            connection_id: 1,
            range: LogRange::default(),
        })
        .expect("log");
    assert_eq!(log.len(), 3);
    // Most recent first (height desc).
    assert_eq!(log[0].hash, c3.hash);
    assert_eq!(log[1].hash, c2.hash);
    assert_eq!(log[2].hash, c1.hash);
}

#[test]
fn branches_and_checkout() {
    let rt = rt();
    let c1 = vcs(&rt).commit(commit_input(1, "root")).unwrap();

    let feature = vcs(&rt)
        .branch_create(CreateBranchInput {
            name: "feature-x".to_string(),
            from: None,
            connection_id: 1,
        })
        .expect("branch create");
    assert_eq!(feature.name, "refs/heads/feature-x");
    assert_eq!(feature.target, c1.hash);

    let branches = vcs(&rt).branch_list().expect("branch_list");
    let names: Vec<String> = branches.iter().map(|r| r.name.clone()).collect();
    assert!(names.contains(&"refs/heads/main".to_string()));
    assert!(names.contains(&"refs/heads/feature-x".to_string()));

    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: 1,
            target: CheckoutTarget::Branch("feature-x".to_string()),
            force: false,
        })
        .expect("checkout feature-x");

    let c2 = vcs(&rt).commit(commit_input(1, "on-feature")).unwrap();
    assert_eq!(c2.parents, vec![c1.hash.clone()]);

    // feature-x advanced, main stayed.
    let refs = vcs(&rt).branch_list().unwrap();
    let feature_target = refs
        .iter()
        .find(|r| r.name == "refs/heads/feature-x")
        .unwrap()
        .target
        .clone();
    let main_target = refs
        .iter()
        .find(|r| r.name == "refs/heads/main")
        .unwrap()
        .target
        .clone();
    assert_eq!(feature_target, c2.hash);
    assert_eq!(main_target, c1.hash);
}

#[test]
fn tags_pin_commits() {
    let rt = rt();
    let c1 = vcs(&rt).commit(commit_input(1, "v1")).unwrap();
    let tag = vcs(&rt)
        .tag(CreateTagInput {
            name: "v1.0".to_string(),
            target: c1.hash.clone(),
            annotation: None,
        })
        .expect("tag create");
    assert_eq!(tag.name, "refs/tags/v1.0");
    assert_eq!(tag.target, c1.hash);

    let tags = vcs(&rt).tag_list().unwrap();
    assert!(tags.iter().any(|r| r.name == "refs/tags/v1.0"));
}

#[test]
fn resolve_commitish_multiple_forms() {
    let rt = rt();
    let c1 = vcs(&rt).commit(commit_input(1, "only")).unwrap();

    // Full hash.
    assert_eq!(vcs(&rt).resolve_commitish(&c1.hash).unwrap(), c1.hash);
    // Short prefix (first 10 chars — unique with only one commit).
    let prefix = &c1.hash[..10];
    assert_eq!(vcs(&rt).resolve_commitish(prefix).unwrap(), c1.hash);
    // Branch name short form.
    assert_eq!(vcs(&rt).resolve_commitish("main").unwrap(), c1.hash);
    // Full ref.
    assert_eq!(
        vcs(&rt).resolve_commitish("refs/heads/main").unwrap(),
        c1.hash
    );
}

#[test]
fn resolve_as_of_by_snapshot_and_commit_and_branch() {
    let rt = rt();
    let c1 = vcs(&rt).commit(commit_input(1, "c1")).unwrap();
    let c2 = vcs(&rt).commit(commit_input(1, "c2")).unwrap();

    assert_eq!(
        vcs(&rt)
            .resolve_as_of(AsOfSpec::Commit(c1.hash.clone()))
            .unwrap(),
        c1.root_xid
    );
    assert_eq!(
        vcs(&rt)
            .resolve_as_of(AsOfSpec::Commit(c2.hash.clone()))
            .unwrap(),
        c2.root_xid
    );
    assert_eq!(
        vcs(&rt)
            .resolve_as_of(AsOfSpec::Branch("main".to_string()))
            .unwrap(),
        c2.root_xid
    );
    assert_eq!(
        vcs(&rt).resolve_as_of(AsOfSpec::Snapshot(12345)).unwrap(),
        12345
    );
}

#[test]
fn lca_finds_common_ancestor() {
    let rt = rt();
    let c1 = vcs(&rt).commit(commit_input(1, "root")).unwrap();
    // Branch off; checkout returns to main via branch_list / checkout.
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
    let c2 = vcs(&rt).commit(commit_input(1, "alpha-1")).unwrap();
    // Second connection on main.
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: 2,
            target: CheckoutTarget::Branch("main".to_string()),
            force: false,
        })
        .unwrap();
    let c3 = vcs(&rt).commit(commit_input(2, "main-1")).unwrap();

    let lca = vcs(&rt).lca(&c2.hash, &c3.hash).unwrap();
    assert_eq!(lca, Some(c1.hash));
}

#[test]
fn fast_forward_merge_advances_branch() {
    let rt = rt();
    let c1 = vcs(&rt).commit(commit_input(1, "root")).unwrap();
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
    let c2 = vcs(&rt).commit(commit_input(1, "feat-1")).unwrap();
    let c3 = vcs(&rt).commit(commit_input(1, "feat-2")).unwrap();

    // Switch back to main, then fast-forward merge feature.
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: 1,
            target: CheckoutTarget::Branch("main".to_string()),
            force: false,
        })
        .unwrap();
    let outcome = vcs(&rt)
        .merge(MergeInput {
            connection_id: 1,
            from: "feature".to_string(),
            opts: MergeOpts::default(),
            author: author(),
        })
        .expect("fast-forward merge");

    assert!(outcome.fast_forward);
    assert!(outcome.is_clean());
    let merged = outcome.merge_commit.unwrap();
    assert_eq!(merged.hash, c3.hash);

    let branches = vcs(&rt).branch_list().unwrap();
    let main_target = branches
        .iter()
        .find(|r| r.name == "refs/heads/main")
        .unwrap()
        .target
        .clone();
    assert_eq!(main_target, c3.hash);
    let _ = c1;
    let _ = c2;
}

#[test]
fn non_fast_forward_merge_creates_merge_state() {
    let rt = rt();
    let _root = vcs(&rt).commit(commit_input(1, "root")).unwrap();
    vcs(&rt)
        .branch_create(CreateBranchInput {
            name: "alpha".to_string(),
            from: None,
            connection_id: 1,
        })
        .unwrap();

    // Divergent commits on both branches.
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: 1,
            target: CheckoutTarget::Branch("alpha".to_string()),
            force: false,
        })
        .unwrap();
    let _a1 = vcs(&rt).commit(commit_input(1, "alpha-1")).unwrap();

    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: 1,
            target: CheckoutTarget::Branch("main".to_string()),
            force: false,
        })
        .unwrap();
    let _m1 = vcs(&rt).commit(commit_input(1, "main-1")).unwrap();

    let outcome = vcs(&rt)
        .merge(MergeInput {
            connection_id: 1,
            from: "alpha".to_string(),
            opts: MergeOpts::default(),
            author: author(),
        })
        .expect("non-ff merge");

    assert!(!outcome.fast_forward);
    let merge_commit = outcome.merge_commit.expect("merge commit created");
    assert_eq!(merge_commit.parents.len(), 2);
    assert!(outcome.merge_state_id.is_some());
}

#[test]
fn ff_only_strategy_refuses_non_ff() {
    let rt = rt();
    let _r = vcs(&rt).commit(commit_input(1, "root")).unwrap();
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
    let _a = vcs(&rt).commit(commit_input(1, "alpha-1")).unwrap();

    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: 1,
            target: CheckoutTarget::Branch("main".to_string()),
            force: false,
        })
        .unwrap();
    let _m = vcs(&rt).commit(commit_input(1, "main-1")).unwrap();

    let err = vcs(&rt)
        .merge(MergeInput {
            connection_id: 1,
            from: "alpha".to_string(),
            opts: MergeOpts {
                strategy: MergeStrategy::FastForwardOnly,
                message: None,
                abort_on_conflict: false,
            },
            author: author(),
        })
        .expect_err("ff-only must refuse");
    let msg = format!("{err}");
    assert!(msg.contains("not a fast-forward"), "got `{msg}`");
}

#[test]
fn reset_soft_moves_branch_back() {
    let rt = rt();
    let c1 = vcs(&rt).commit(commit_input(1, "c1")).unwrap();
    let _c2 = vcs(&rt).commit(commit_input(1, "c2")).unwrap();
    let _c3 = vcs(&rt).commit(commit_input(1, "c3")).unwrap();

    vcs(&rt)
        .reset(ResetInput {
            connection_id: 1,
            target: c1.hash.clone(),
            mode: ResetMode::Soft,
        })
        .expect("reset soft");

    let status = vcs(&rt)
        .status(StatusInput { connection_id: 1 })
        .expect("status");
    assert_eq!(status.head_commit.as_deref(), Some(c1.hash.as_str()));
    let branches = vcs(&rt).branch_list().unwrap();
    let main = branches
        .iter()
        .find(|r| r.name == "refs/heads/main")
        .unwrap();
    assert_eq!(main.target, c1.hash);
}

#[test]
fn reset_hard_is_phase_4() {
    let rt = rt();
    let c1 = vcs(&rt).commit(commit_input(1, "c1")).unwrap();
    let _c2 = vcs(&rt).commit(commit_input(1, "c2")).unwrap();
    let err = vcs(&rt)
        .reset(ResetInput {
            connection_id: 1,
            target: c1.hash.clone(),
            mode: ResetMode::Hard,
        })
        .expect_err("reset --hard not yet implemented");
    assert!(format!("{err}").contains("reset --hard"));
}

#[test]
fn diff_empty_when_no_user_data_changes() {
    let rt = rt();
    let c1 = vcs(&rt).commit(commit_input(1, "c1")).unwrap();
    let c2 = vcs(&rt).commit(commit_input(1, "c2")).unwrap();
    let diff = vcs(&rt)
        .diff(DiffInput {
            from: c1.hash.clone(),
            to: c2.hash.clone(),
            collection: None,
            summary_only: true,
        })
        .expect("diff");
    assert_eq!(diff.added, 0);
    assert_eq!(diff.removed, 0);
    assert_eq!(diff.modified, 0);
    assert_eq!(diff.from, c1.hash);
    assert_eq!(diff.to, c2.hash);
}

#[test]
fn status_reflects_checkout() {
    let rt = rt();
    let _c1 = vcs(&rt).commit(commit_input(7, "c1")).unwrap();
    vcs(&rt)
        .branch_create(CreateBranchInput {
            name: "side".to_string(),
            from: None,
            connection_id: 7,
        })
        .unwrap();
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: 7,
            target: CheckoutTarget::Branch("side".to_string()),
            force: false,
        })
        .unwrap();
    let status = vcs(&rt).status(StatusInput { connection_id: 7 }).unwrap();
    assert_eq!(status.head_ref.as_deref(), Some("refs/heads/side"));
}
