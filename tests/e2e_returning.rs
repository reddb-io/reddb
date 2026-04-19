//! RETURNING clause end-to-end tests (T4 / PG gap item #1a).
//!
//! Covers INSERT Row path: RETURNING * and RETURNING <col list>.
//! UPDATE/DELETE currently error with NotImplemented — pinned here so
//! we notice when the follow-up (T4.3) lands.

use reddb::application::ExecuteQueryInput;
use reddb::storage::schema::Value;
use reddb::{QueryUseCases, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

#[test]
fn returning_star_on_insert_returns_inserted_row() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    let result = q
        .execute(ExecuteQueryInput {
            query: "INSERT INTO users (name, age) VALUES ('alice', 30) RETURNING *".into(),
        })
        .expect("insert returning * should succeed");

    assert_eq!(result.affected_rows, 1);
    assert_eq!(result.result.records.len(), 1);

    let rec = &result.result.records[0];
    let name = rec.values.get("name").expect("name column present");
    let age = rec.values.get("age").expect("age column present");
    assert!(
        matches!(name, Value::Text(s) if s == "alice"),
        "got {name:?}"
    );
    assert!(matches!(age, Value::Integer(30)), "got {age:?}");
    assert!(
        rec.values.get("red_entity_id").is_some(),
        "RETURNING * on INSERT should include red_entity_id"
    );
    assert!(
        result.result.columns.iter().any(|c| c == "red_entity_id"),
        "columns should list red_entity_id"
    );
}

#[test]
fn returning_column_list_projects_subset() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    let result = q
        .execute(ExecuteQueryInput {
            query: "INSERT INTO users (name, age) VALUES ('bob', 25) RETURNING name".into(),
        })
        .expect("returning name should succeed");

    assert_eq!(result.result.columns, vec!["name".to_string()]);
    let rec = &result.result.records[0];
    assert!(matches!(rec.values.get("name"), Some(Value::Text(s)) if s == "bob"));
    assert!(
        rec.values.get("age").is_none(),
        "age must not leak when not in RETURNING list"
    );
}

#[test]
fn returning_multi_row_insert_returns_all_rows() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    let result = q
        .execute(ExecuteQueryInput {
            query: "INSERT INTO users (name, age) VALUES ('a', 1), ('b', 2), ('c', 3) RETURNING *"
                .into(),
        })
        .expect("multi-row insert returning");

    assert_eq!(result.affected_rows, 3);
    assert_eq!(result.result.records.len(), 3);
}

#[test]
fn no_returning_leaves_result_empty() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    let result = q
        .execute(ExecuteQueryInput {
            query: "INSERT INTO users (name, age) VALUES ('x', 99)".into(),
        })
        .expect("plain insert");
    assert_eq!(result.affected_rows, 1);
    assert!(
        result.result.records.is_empty(),
        "no RETURNING => no rows in result"
    );
}

#[test]
fn returning_on_update_is_rejected_for_now() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "INSERT INTO users (name, age) VALUES ('u', 1)".into(),
    })
    .unwrap();
    let err = q
        .execute(ExecuteQueryInput {
            query: "UPDATE users SET age = 2 WHERE name = 'u' RETURNING *".into(),
        })
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("RETURNING on UPDATE"),
        "expected NotImplemented for UPDATE RETURNING, got: {msg}"
    );
}

#[test]
fn returning_on_delete_is_rejected_for_now() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "INSERT INTO users (name, age) VALUES ('d', 1)".into(),
    })
    .unwrap();
    let err = q
        .execute(ExecuteQueryInput {
            query: "DELETE FROM users WHERE name = 'd' RETURNING *".into(),
        })
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("RETURNING on DELETE"),
        "expected NotImplemented for DELETE RETURNING, got: {msg}"
    );
}
