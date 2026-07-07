//! Embedded integration tests for the ephemeral-store tracer
//! (PRD #1785, issue #1786): fixture file → materialize → query.
//!
//! Exercises `RedDBRuntime::materialize_data_file` directly against an
//! in-memory runtime, the same seam the `red` binary drives.

use std::fs;
use std::path::Path;

use reddb_server::runtime::impl_ephemeral::sanitize_stem;
use reddb_server::{RedDBRuntime, POSITIONAL_ALIAS};

/// Auto-cleaning temp dir for fixture files.
fn temp_dir(label: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("reddb-test-ephemeral-{label}-"))
        .tempdir()
        .expect("temp dir")
}

fn write_fixture(dir: &Path, name: &str, contents: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, contents).expect("write fixture");
    path
}

#[test]
fn csv_materializes_as_row_table_addressable_by_stem_and_alias() {
    let dir = temp_dir("csv-basic");
    // Ages 30 and 9 discriminate numeric vs. lexical comparison: `age > 26`
    // returns only Alice under numeric typing, but both rows if the column
    // stayed textual ('9' > '26' lexically).
    let path = write_fixture(
        dir.path(),
        "people.csv",
        "id,name,age\n1,Alice,30\n2,Bob,9\n",
    );

    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let table = rt.materialize_data_file(&path).expect("materialize csv");

    assert_eq!(table.collection, "people");
    assert_eq!(table.alias, POSITIONAL_ALIAS);
    assert_eq!(table.rows_imported, 2);

    // Addressable by the sanitized file-stem name.
    let by_stem = rt
        .execute_query("SELECT * FROM people")
        .expect("select by stem");
    assert_eq!(by_stem.result.records.len(), 2);

    // Addressable by the positional alias `t`.
    let by_alias = rt
        .execute_query("SELECT * FROM t")
        .expect("select by alias");
    assert_eq!(by_alias.result.records.len(), 2);

    // Header-derived column with inferred integer type: numeric `>` keeps
    // only the age-30 row through either name.
    let filtered_stem = rt
        .execute_query("SELECT name FROM people WHERE age > 26")
        .expect("numeric filter by stem");
    assert_eq!(filtered_stem.result.records.len(), 1);

    let filtered_alias = rt
        .execute_query("SELECT name FROM t WHERE age > 26")
        .expect("numeric filter by alias");
    assert_eq!(filtered_alias.result.records.len(), 1);

    // Aggregates resolve identically through the alias (regression guard:
    // a rewrite-view alias silently dropped the aggregate).
    let count_alias = rt
        .execute_query("SELECT count(*) AS n FROM t")
        .expect("aggregate by alias");
    assert_eq!(count_alias.result.records.len(), 1);
}

#[test]
fn tsv_materializes_with_tab_delimiter() {
    let dir = temp_dir("tsv-basic");
    let path = write_fixture(dir.path(), "places.tsv", "id\tcity\n1\tLisbon\n2\tPorto\n");

    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let table = rt.materialize_data_file(&path).expect("materialize tsv");
    assert_eq!(table.collection, "places");
    assert_eq!(table.rows_imported, 2);

    let rows = rt
        .execute_query("SELECT city FROM t WHERE city = 'Porto'")
        .expect("query tsv");
    assert_eq!(rows.result.records.len(), 1);
}

#[test]
fn ndjson_materializes_as_document_collection() {
    let dir = temp_dir("ndjson-docs");
    let path = write_fixture(
        dir.path(),
        "events.ndjson",
        "{\"UserId\":7,\"name\":\"Ada\",\"event_type\":\"login\"}\n\
         {\"UserId\":8,\"name\":\"Linus\",\"event_type\":\"logout\"}\n",
    );

    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let table = rt.materialize_data_file(&path).expect("materialize ndjson");

    assert_eq!(table.collection, "events");
    assert_eq!(table.alias, POSITIONAL_ALIAS);
    assert_eq!(table.rows_imported, 2);

    let by_stem = rt
        .execute_query("SELECT name FROM events WHERE UserId = 8")
        .expect("query ndjson by exact body key");
    assert_eq!(by_stem.result.records.len(), 1);

    let by_alias = rt
        .execute_query("SELECT name FROM t WHERE event_type = 'login'")
        .expect("query ndjson by alias");
    assert_eq!(by_alias.result.records.len(), 1);

    let wrong_case = rt
        .execute_query("SELECT name FROM events WHERE userid = 8")
        .expect("query ndjson wrong case");
    assert_eq!(
        wrong_case.result.records.len(),
        0,
        "document body keys must keep exact casing"
    );
}

#[test]
fn json_array_materializes_as_document_collection() {
    let dir = temp_dir("json-docs");
    let path = write_fixture(
        dir.path(),
        "products.json",
        r#"[{"sku":"A","qty":3},{"sku":"B","qty":10}]"#,
    );

    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let table = rt
        .materialize_data_file(&path)
        .expect("materialize json array");

    assert_eq!(table.collection, "products");
    assert_eq!(table.rows_imported, 2);

    let rows = rt
        .execute_query("SELECT sku FROM products WHERE qty > 5")
        .expect("query json array documents");
    assert_eq!(rows.result.records.len(), 1);

    let alias_rows = rt
        .execute_query("SELECT sku FROM t WHERE qty = 3")
        .expect("query json array alias");
    assert_eq!(alias_rows.result.records.len(), 1);
}

#[test]
fn file_stem_is_sanitized_into_a_collection_name() {
    let dir = temp_dir("sanitize");
    let path = write_fixture(dir.path(), "vendas-2026 (v2).csv", "a,b\n1,2\n");

    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let table = rt.materialize_data_file(&path).expect("materialize");
    assert_eq!(table.collection, "vendas_2026_v2");
    // The sanitized name resolves; the raw stem never would.
    let rows = rt
        .execute_query("SELECT * FROM vendas_2026_v2")
        .expect("query sanitized name");
    assert_eq!(rows.result.records.len(), 1);
}

#[test]
fn missing_file_is_a_didactic_error_not_a_panic() {
    let dir = temp_dir("missing");
    let path = dir.path().join("nope.csv");

    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let err = rt
        .materialize_data_file(&path)
        .expect_err("missing file must error");
    let msg = err.to_string();
    assert!(msg.contains("no such file"), "unexpected message: {msg}");
}

#[test]
fn unsupported_extension_is_a_didactic_error() {
    let dir = temp_dir("unsupported");
    let path = write_fixture(dir.path(), "data.txt", "{}\n");

    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let err = rt
        .materialize_data_file(&path)
        .expect_err("txt is out of scope for this slice");
    let msg = err.to_string();
    assert!(
        msg.contains("not a supported ephemeral data file"),
        "unexpected message: {msg}"
    );
}

#[test]
fn malformed_ndjson_names_the_offending_line() {
    let dir = temp_dir("bad-ndjson");
    let path = write_fixture(
        dir.path(),
        "broken.ndjson",
        "{\"name\":\"ok\"}\n{\"name\":\"broken\"\n",
    );

    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let err = rt
        .materialize_data_file(&path)
        .expect_err("malformed ndjson must error");
    let msg = err.to_string();
    assert!(msg.contains("line 2"), "unexpected message: {msg}");
}

#[test]
fn json_array_non_object_element_names_the_element() {
    let dir = temp_dir("bad-json-element");
    let path = write_fixture(dir.path(), "broken.json", r#"[{"name":"ok"}, 42]"#);

    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let err = rt
        .materialize_data_file(&path)
        .expect_err("non-object json array element must error");
    let msg = err.to_string();
    assert!(msg.contains("element 2"), "unexpected message: {msg}");
}

#[test]
fn non_array_json_top_level_is_rejected() {
    let dir = temp_dir("bad-json-top");
    let path = write_fixture(dir.path(), "object.json", r#"{"name":"not an array"}"#);

    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let err = rt
        .materialize_data_file(&path)
        .expect_err("non-array json must error");
    let msg = err.to_string();
    assert!(
        msg.contains("top-level JSON value must be an array"),
        "unexpected message: {msg}"
    );
}

#[test]
fn malformed_csv_is_a_didactic_error_not_a_panic() {
    let dir = temp_dir("malformed");
    // Unterminated quoted field: the RFC-4180 parser rejects it.
    let path = write_fixture(dir.path(), "broken.csv", "a,b\n1,\"unterminated\n");

    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let err = rt
        .materialize_data_file(&path)
        .expect_err("malformed csv must error");
    let msg = err.to_string();
    assert!(msg.contains("failed to load"), "unexpected message: {msg}");
}

#[test]
fn sanitize_stem_rules() {
    assert_eq!(sanitize_stem("data").as_deref(), Some("data"));
    assert_eq!(
        sanitize_stem("vendas-2026 (v2)").as_deref(),
        Some("vendas_2026_v2")
    );
    assert_eq!(sanitize_stem("2026").as_deref(), Some("_2026"));
    assert_eq!(sanitize_stem("***"), None);
}
