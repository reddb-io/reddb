#![cfg(feature = "embedded")]

use reddb_client::embedded::EmbeddedClient;
use reddb_client::types::ValueOut;

#[test]
fn selected_columns_exclude_implicit_envelope_fields() {
    let client = EmbeddedClient::in_memory().expect("open embedded fixture");
    client
        .query("CREATE TABLE projection (id INT, payload TEXT)")
        .expect("create");
    client
        .query("INSERT INTO projection (id, payload) VALUES (1, 'kept')")
        .expect("insert");
    let result = client
        .query("SELECT payload, id FROM projection")
        .expect("select");
    assert_eq!(result.columns, ["payload", "id"]);
    assert_eq!(
        result.rows,
        [vec![
            ("payload".to_owned(), ValueOut::String("kept".to_owned())),
            ("id".to_owned(), ValueOut::Integer(1)),
        ]]
    );
    let metadata = client
        .query("SELECT rid FROM projection")
        .expect("explicit metadata");
    assert_eq!(metadata.rows[0].len(), 1);
    assert_ne!(metadata.rows[0][0].1, ValueOut::Null);
    let aggregate = client
        .query("SELECT COUNT(*) AS total FROM projection")
        .expect("aggregate");
    assert_eq!(
        aggregate.rows,
        [vec![("total".to_owned(), ValueOut::Integer(1))]]
    );
}

#[test]
fn selected_column_added_after_insert_is_explicit_null() {
    let client = EmbeddedClient::in_memory().expect("open embedded fixture");
    client
        .query("CREATE TABLE old_sessions (id INT)")
        .expect("create");
    client
        .query("INSERT INTO old_sessions (id) VALUES (1)")
        .expect("insert old row");
    client
        .query("ALTER TABLE old_sessions ADD COLUMN revision INT")
        .expect("add nullable column");
    let result = client
        .query("SELECT id, revision FROM old_sessions")
        .expect("select");
    assert_eq!(result.columns, ["id", "revision"]);
    assert_eq!(result.rows.len(), 1);
    assert_eq!(
        result.rows[0].iter().find(|(name, _)| name == "revision"),
        Some(&("revision".to_owned(), ValueOut::Null)),
    );
}

#[test]
fn conversion_supplies_null_for_absent_selected_legacy_field() {
    let runtime = reddb_server::RedDBRuntime::in_memory().expect("runtime");
    runtime
        .execute_query("CREATE TABLE legacy (id INT)")
        .expect("create");
    runtime
        .execute_query("INSERT INTO legacy (id) VALUES (1)")
        .expect("insert");
    let mut query = runtime
        .execute_query("SELECT id FROM legacy")
        .expect("read legacy shape");
    // Historical engines can declare a selected nullable column in the result
    // schema without materializing that field in an older row's record.
    query.result.columns.push("revision".to_owned());
    let result = reddb_client::embedded::query_result_from_runtime(&query);
    assert_eq!(
        result.rows[0].iter().find(|(name, _)| name == "revision"),
        Some(&("revision".to_owned(), ValueOut::Null))
    );
}
