use super::*;

fn author() -> Author {
    Author {
        name: "test".into(),
        email: "test@reddb.io".into(),
    }
}

fn commit(runtime: &RedDBRuntime, connection: u64) -> Commit {
    runtime
        .vcs_commit(CreateCommitInput {
            connection_id: connection,
            message: "checkpoint".into(),
            author: author(),
            committer: None,
            amend: false,
            allow_empty: true,
        })
        .unwrap()
}

#[test]
fn metadata_wal_failures_leave_branches_checkout_tags_and_reset_unchanged() {
    let runtime = RedDBRuntime::in_memory().unwrap();
    let connection = 9701;
    crate::runtime::execution_context::set_current_connection_id(connection);
    runtime
        .execute_query("CREATE TABLE items (value INT)")
        .unwrap();
    runtime.vcs_set_versioned("items", true).unwrap();
    let base = commit(&runtime, connection);
    runtime
        .execute_query("INSERT INTO items (value) VALUES (1)")
        .unwrap();
    let head = commit(&runtime, connection);
    let store = runtime.db().store();
    let fail = || {
        store
            .test_fail_deferred_wal_append
            .store(true, std::sync::atomic::Ordering::Release)
    };
    let create_branch = || {
        runtime.vcs_branch_create(CreateBranchInput {
            connection_id: connection,
            name: "agent".into(),
            from: None,
        })
    };
    fail();
    assert!(create_branch().is_err());
    assert!(runtime
        .vcs_list_refs(Some("refs/heads/agent"))
        .unwrap()
        .is_empty());
    assert!(branches::load_view(&store, "refs/heads/agent")
        .unwrap()
        .is_none());
    create_branch().unwrap();
    fail();
    assert!(runtime
        .vcs_checkout(CheckoutInput {
            connection_id: connection,
            target: CheckoutTarget::Branch("agent".into()),
            force: false
        })
        .is_err());
    assert_eq!(
        runtime
            .vcs_status(StatusInput {
                connection_id: connection
            })
            .unwrap()
            .head_ref
            .as_deref(),
        Some("refs/heads/main")
    );
    for mode in [ResetMode::Soft, ResetMode::Mixed] {
        fail();
        assert!(runtime
            .vcs_reset(ResetInput {
                connection_id: connection,
                target: base.hash.clone(),
                mode
            })
            .is_err());
        assert_eq!(
            runtime
                .vcs_status(StatusInput {
                    connection_id: connection
                })
                .unwrap()
                .head_commit,
            Some(head.hash.clone())
        );
        assert_eq!(runtime.scan_collection("items", None, 10).unwrap().total, 1);
    }
    fail();
    assert!(runtime.vcs_branch_delete("agent").is_err());
    assert_eq!(
        runtime
            .vcs_list_refs(Some("refs/heads/agent"))
            .unwrap()
            .len(),
        1
    );
    assert!(branches::load_view(&store, "refs/heads/agent")
        .unwrap()
        .is_some());
    fail();
    assert!(runtime
        .vcs_tag_create(CreateTagInput {
            name: "release".into(),
            target: head.hash.clone(),
            annotation: None
        })
        .is_err());
    assert!(runtime
        .vcs_list_refs(Some("refs/tags/release"))
        .unwrap()
        .is_empty());
    crate::runtime::execution_context::clear_current_connection_id();
}

#[test]
fn malformed_reference_and_missing_ancestry_fail_closed() {
    let runtime = RedDBRuntime::in_memory().unwrap();
    let store = runtime.db().store();
    let mut publication = crate::storage::unified::store::RowPublication::default();
    branches::stage_metadata(
        &store,
        &mut publication,
        vc::REFS,
        "id",
        "refs/heads/broken",
        HashMap::from([
            ("id".into(), Value::text("refs/heads/broken")),
            ("type".into(), Value::text("branch")),
        ]),
    )
    .unwrap();
    store.publish_rows(publication).unwrap();
    assert!(runtime
        .vcs_resolve_commitish("broken")
        .unwrap_err()
        .to_string()
        .contains("invalid reference"));
    assert!(runtime
        .vcs_branch_create(CreateBranchInput {
            connection_id: 9702,
            name: "copy".into(),
            from: Some("broken".into())
        })
        .is_err());
    assert!(branches::load_view(&store, "refs/heads/copy")
        .unwrap()
        .is_none());
    assert!(ancestor_set(&store, "missing", 100)
        .unwrap_err()
        .to_string()
        .contains("missing ancestor"));
}
