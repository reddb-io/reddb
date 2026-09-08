use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

#[test]
fn sql_quoted_identifiers_persist_and_roundtrip_schema() {
    let directory = tempfile::tempdir().expect("directory");
    let options = RedDBOptions::persistent(directory.path().join("quotes.rdb"));
    let ddl;
    {
        let rt = RedDBRuntime::with_options(options.clone()).expect("runtime");
        rt.execute_query(r#"CREATE TABLE "select" ("key name" INT PRIMARY KEY, "a""b" TEXT, "CASE" INT, "body" JSON)"#).expect("quoted schema");
        rt.execute_query_with_params(
            r#"INSERT INTO "select" ("key name", "a""b", "CASE", "body") VALUES ($1,$2,$3,$4)"#,
            &[
                Value::Integer(1),
                Value::text("a'\"b"),
                Value::Integer(7),
                Value::Json(br#"{"text":"unchanged","array":["one","two"]}"#.to_vec()),
            ],
        )
        .expect("parameters");
        let result = rt
            .execute_query(
                r#"SELECT "a""b" AS "output name", "CASE" FROM "select" WHERE "key name"=1"#,
            )
            .expect("columns");
        assert_eq!(
            result.result.records[0].get("output name"),
            Some(&Value::text("a'\"b"))
        );
        assert_eq!(
            result.result.records[0].get("CASE"),
            Some(&Value::Integer(7))
        );
        rt.execute_query(r#"UPDATE "select" SET "a""b"='replacement' WHERE "key name"=1"#)
            .expect("update");
        let result = rt
            .execute_query(r#"SHOW CREATE TABLE "select""#)
            .expect("DDL");
        let Some(Value::Text(value)) = result.result.records[0].get("ddl") else {
            panic!("DDL")
        };
        ddl = value.to_string();
        assert!(ddl.contains(r#""a""b""#), "{ddl}");
    }
    let rt = RedDBRuntime::with_options(options).expect("reopen");
    let result = rt
        .execute_query(r#"SELECT "a""b" FROM "select" WHERE "key name"=1"#)
        .expect("reopened column");
    assert_eq!(
        result.result.records[0].get("a\"b"),
        Some(&Value::text("replacement"))
    );
    let fresh = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("fresh");
    fresh.execute_query(&ddl).expect("reconstruct schema");
}

#[test]
fn sql_quoted_columns_do_not_share_cached_literal_plans() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    rt.execute_query("CREATE TABLE labels (first_label TEXT, second_label TEXT)")
        .expect("table");
    rt.execute_query("INSERT INTO labels (first_label,second_label) VALUES ('left','right')")
        .expect("values");
    for (sql, expected) in [
        (r#"SELECT "first_label" AS value FROM labels"#, "left"),
        (r#"SELECT "second_label" AS value FROM labels"#, "right"),
        (
            r#"SELECT 'first_label' AS value FROM labels"#,
            "first_label",
        ),
        (r#"SELECT "first_label" AS value FROM labels"#, "left"),
    ] {
        let result = rt.execute_query(sql).expect("distinct cached query");
        assert_eq!(
            result.result.records[0].get("value"),
            Some(&Value::text(expected))
        );
    }
}
