mod support;

use reddb::RedDBRuntime;
use support::{
    apply_end_to_end_mutations, assert_end_to_end_query_behavior, assert_shared_query_behavior,
    build_api_fixture, build_sql_fixture, checkpoint_and_reopen, logical_snapshot,
    PersistentDbPath,
};

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("failed to create in-memory runtime")
}

#[test]
fn e2e_multimodel_sql_fixture_embedded() {
    let rt = rt();

    build_sql_fixture(&rt);
    assert_shared_query_behavior(&rt);

    apply_end_to_end_mutations(&rt);
    assert_end_to_end_query_behavior(&rt);
}

#[test]
fn e2e_multimodel_api_fixture_embedded() {
    let rt = rt();

    build_api_fixture(&rt);
    assert_shared_query_behavior(&rt);

    apply_end_to_end_mutations(&rt);
    assert_end_to_end_query_behavior(&rt);
}

#[test]
fn e2e_multimodel_sql_fixture_persistent_reopen() {
    let path = PersistentDbPath::new("e2e_sql_flow");
    let rt = path.open_runtime();

    build_sql_fixture(&rt);
    apply_end_to_end_mutations(&rt);
    assert_end_to_end_query_behavior(&rt);

    let before = logical_snapshot(&rt);
    let rt = checkpoint_and_reopen(&path, rt);
    let after = logical_snapshot(&rt);

    assert_eq!(after, before, "logical snapshot should survive reopen");
    assert_end_to_end_query_behavior(&rt);
}

#[test]
fn e2e_multimodel_api_fixture_persistent_reopen() {
    let path = PersistentDbPath::new("e2e_api_flow");
    let rt = path.open_runtime();

    build_api_fixture(&rt);
    apply_end_to_end_mutations(&rt);
    assert_end_to_end_query_behavior(&rt);

    let before = logical_snapshot(&rt);
    let rt = checkpoint_and_reopen(&path, rt);
    let after = logical_snapshot(&rt);

    assert_eq!(after, before, "logical snapshot should survive reopen");
    assert_end_to_end_query_behavior(&rt);
}
