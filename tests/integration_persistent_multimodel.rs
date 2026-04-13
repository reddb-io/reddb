mod support;

use support::{
    assert_native_consistency, assert_shared_query_behavior, assert_sql_function_queries,
    build_api_fixture, build_sql_fixture, checkpoint_and_reopen, logical_snapshot,
    PersistentDbPath,
};

#[test]
#[ignore = "persistent multimodel fixture"]
fn persistent_sql_fixture_reopens_with_same_logical_snapshot() {
    let path = PersistentDbPath::new("sql_fixture");
    let rt = path.open_runtime();

    build_sql_fixture(&rt);
    let before = logical_snapshot(&rt);

    let rt = checkpoint_and_reopen(&path, rt);
    let after = logical_snapshot(&rt);

    assert_eq!(after, before);
    assert_shared_query_behavior(&rt);
    assert_sql_function_queries(&rt);
}

#[test]
#[ignore = "persistent multimodel fixture"]
fn persistent_api_fixture_reopens_with_same_logical_snapshot() {
    let path = PersistentDbPath::new("api_fixture");
    let rt = path.open_runtime();

    build_api_fixture(&rt);
    let before = logical_snapshot(&rt);

    let rt = checkpoint_and_reopen(&path, rt);
    let after = logical_snapshot(&rt);

    assert_eq!(after, before);
    assert_shared_query_behavior(&rt);
}

#[test]
#[ignore = "persistent multimodel fixture"]
fn persistent_sql_and_api_fixtures_match() {
    let sql_path = PersistentDbPath::new("sql_match");
    let api_path = PersistentDbPath::new("api_match");

    let sql_rt = sql_path.open_runtime();
    build_sql_fixture(&sql_rt);
    let sql_rt = checkpoint_and_reopen(&sql_path, sql_rt);

    let api_rt = api_path.open_runtime();
    build_api_fixture(&api_rt);
    let api_rt = checkpoint_and_reopen(&api_path, api_rt);

    assert_eq!(logical_snapshot(&sql_rt), logical_snapshot(&api_rt));
    assert_shared_query_behavior(&sql_rt);
    assert_shared_query_behavior(&api_rt);
}

#[test]
#[ignore = "persistent multimodel fixture"]
fn persistent_native_metadata_and_catalog_stay_consistent() {
    let path = PersistentDbPath::new("native_consistency");
    let rt = path.open_runtime();

    build_api_fixture(&rt);
    let rt = checkpoint_and_reopen(&path, rt);

    assert_native_consistency(&rt);
    assert_shared_query_behavior(&rt);
}
