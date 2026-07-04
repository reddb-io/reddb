// Regression coverage for issue #555 — Documents: SQL aggregates +
// stable `COUNT(*) AS count` alias.
//
// Each test maps to one bullet in the issue's `## Acceptance` list:
//   - Each aggregate (COUNT/SUM/AVG/MIN/MAX) produces the expected
//     result over a document collection.
//   - `COUNT(*) AS count` returns column name exactly `count`.
//   - `GROUP BY body.field` works.
//   - Regression tests for aggregates + alias stability.
//
// User stories: PRD #449 #14, #15.

use reddb::storage::query::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("runtime")
}

fn numeric(value: &Value) -> f64 {
    match value {
        Value::Integer(n) => *n as f64,
        Value::UnsignedInteger(n) => *n as f64,
        Value::Float(n) => *n,
        other => panic!("expected numeric value, got {other:?}"),
    }
}

fn first_numeric_for(record: &UnifiedRecord, candidates: &[&str]) -> f64 {
    for c in candidates {
        if let Some(v) = record.get(c) {
            return numeric(v);
        }
    }
    panic!(
        "expected one of {candidates:?} to be present as numeric in {record:?}",
        candidates = candidates
    );
}

// Seed five documents with a known shape:
//   { "level": "info" | "warn" | "error", "score": <int> }
// COUNT/SUM/AVG/MIN/MAX of `score` are deterministic.
fn seed_documents(rt: &RedDBRuntime, collection: &str) {
    rt.execute_query(&format!("CREATE DOCUMENT {collection}"))
        .expect("CREATE DOCUMENT");
    let docs = [
        r#"{"level":"info","score":10}"#,
        r#"{"level":"info","score":20}"#,
        r#"{"level":"warn","score":30}"#,
        r#"{"level":"warn","score":40}"#,
        r#"{"level":"error","score":50}"#,
    ];
    for d in docs {
        rt.execute_query(&format!("INSERT INTO {collection} DOCUMENT VALUES ({d})"))
            .expect("INSERT DOCUMENT");
    }
}

// Bullet 1: each aggregate produces the expected result over a doc collection.
// COUNT(*) over the whole collection.
#[test]
fn sql_count_star_over_document_collection() {
    let rt = runtime();
    seed_documents(&rt, "issue555_count_star");

    let page = rt
        .execute_query("SELECT COUNT(*) FROM issue555_count_star")
        .expect("SELECT COUNT(*) FROM doc_collection should succeed");
    assert_eq!(page.result.records.len(), 1);
    let record = &page.result.records[0];
    let count = first_numeric_for(record, &["COUNT(*)", "count(*)", "COUNT", "count"]);
    assert_eq!(count, 5.0);
}

// Bullet 2: `COUNT(*) AS count` returns column name exactly `count`.
#[test]
fn sql_count_star_as_count_returns_stable_count_column_name() {
    let rt = runtime();
    seed_documents(&rt, "issue555_count_alias");

    let page = rt
        .execute_query("SELECT COUNT(*) AS count FROM issue555_count_alias")
        .expect("SELECT COUNT(*) AS count FROM doc_collection should succeed");
    assert_eq!(
        page.result.columns,
        vec!["count".to_string()],
        "COUNT(*) AS count must surface a column named exactly `count`; got {:?}",
        page.result.columns
    );
    assert_eq!(page.result.records.len(), 1);
    let record = &page.result.records[0];
    let value = record
        .get("count")
        .expect("record must have a field named `count`");
    assert_eq!(numeric(value), 5.0);
}

// Bullet 1 cont. — SUM(body.score).
#[test]
fn sql_sum_over_document_collection() {
    let rt = runtime();
    seed_documents(&rt, "issue555_sum");

    let page = rt
        .execute_query("SELECT SUM(body.score) AS total FROM issue555_sum")
        .expect("SELECT SUM(body.score) should succeed");
    assert_eq!(page.result.records.len(), 1);
    let record = &page.result.records[0];
    let total = numeric(record.get("total").expect("total column"));
    assert_eq!(total, 150.0);
}

// Bullet 1 cont. — AVG(body.score).
#[test]
fn sql_avg_over_document_collection() {
    let rt = runtime();
    seed_documents(&rt, "issue555_avg");

    let page = rt
        .execute_query("SELECT AVG(body.score) AS avg_score FROM issue555_avg")
        .expect("SELECT AVG(body.score) should succeed");
    assert_eq!(page.result.records.len(), 1);
    let record = &page.result.records[0];
    let avg = numeric(record.get("avg_score").expect("avg_score column"));
    assert_eq!(avg, 30.0);
}

// Bullet 1 cont. — MIN(body.score).
#[test]
fn sql_min_over_document_collection() {
    let rt = runtime();
    seed_documents(&rt, "issue555_min");

    let page = rt
        .execute_query("SELECT MIN(body.score) AS lowest FROM issue555_min")
        .expect("SELECT MIN(body.score) should succeed");
    assert_eq!(page.result.records.len(), 1);
    let record = &page.result.records[0];
    let min = numeric(record.get("lowest").expect("lowest column"));
    assert_eq!(min, 10.0);
}

// Bullet 1 cont. — MAX(body.score).
#[test]
fn sql_max_over_document_collection() {
    let rt = runtime();
    seed_documents(&rt, "issue555_max");

    let page = rt
        .execute_query("SELECT MAX(body.score) AS max_score FROM issue555_max")
        .expect("SELECT MAX(body.score) should succeed");
    assert_eq!(page.result.records.len(), 1);
    let record = &page.result.records[0];
    let max = numeric(record.get("max_score").expect("max_score column"));
    assert_eq!(max, 50.0);
}

// Bullet 3: `GROUP BY body.field` works. Combined with COUNT and
// stable alias surface.
#[test]
fn sql_group_by_body_field_with_count_alias() {
    let rt = runtime();
    seed_documents(&rt, "issue555_group_by");

    let page = rt
        .execute_query(
            "SELECT body.level, COUNT(*) AS count FROM issue555_group_by \
             GROUP BY body.level ORDER BY body.level",
        )
        .expect("GROUP BY body.field with COUNT(*) AS count should succeed");
    assert_eq!(page.result.records.len(), 3);

    // Collect (level, count) tuples — accept any reasonable column
    // name for the grouping key (body.level, body.LEVEL, level) so
    // the contract pinned here is the *value*, not the casing.
    let mut tuples: Vec<(String, u64)> = page
        .result
        .records
        .iter()
        .map(|rec| {
            let level = ["body.level", "body.LEVEL", "level"]
                .iter()
                .find_map(|k| match rec.get(k) {
                    Some(Value::Text(s)) => Some(s.to_string()),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("expected grouping key text value in {rec:?}"));
            let count = match rec.get("count") {
                Some(Value::Integer(n)) => *n as u64,
                Some(Value::UnsignedInteger(n)) => *n,
                Some(Value::Float(n)) => *n as u64,
                other => panic!("expected `count` column, got {other:?} in {rec:?}"),
            };
            (level, count)
        })
        .collect();
    tuples.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        tuples,
        vec![
            ("error".to_string(), 1),
            ("info".to_string(), 2),
            ("warn".to_string(), 2),
        ]
    );
}
