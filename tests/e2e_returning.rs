//! RETURNING clause end-to-end tests (T4 / PG gap item #1a).
//!
//! Covers INSERT Row, UPDATE, and DELETE. UPDATE returns the
//! post-image of the mutated row (matches PG).

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
        matches!(name, Value::Text(s) if s.as_ref() == "alice"),
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
    assert!(matches!(rec.values.get("name"), Some(Value::Text(s)) if s.as_ref() == "bob"));
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
fn returning_star_on_update_returns_post_image() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "INSERT INTO users (name, age) VALUES ('u', 1)".into(),
    })
    .unwrap();

    let result = q
        .execute(ExecuteQueryInput {
            query: "UPDATE users SET age = 2 WHERE name = 'u' RETURNING *".into(),
        })
        .expect("UPDATE RETURNING *");

    assert_eq!(result.affected_rows, 1);
    assert_eq!(result.result.records.len(), 1);
    let rec = &result.result.records[0];
    assert!(
        matches!(rec.values.get("name"), Some(Value::Text(s)) if s.as_ref() == "u"),
        "name preserved in post-image"
    );
    assert!(
        matches!(rec.values.get("age"), Some(Value::Integer(2))),
        "age must be the POST-update value (2), got {:?}",
        rec.values.get("age")
    );
}

#[test]
fn returning_column_list_on_update_projects_subset() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "INSERT INTO users (name, age) VALUES ('v', 5)".into(),
    })
    .unwrap();

    let result = q
        .execute(ExecuteQueryInput {
            query: "UPDATE users SET age = 6 WHERE name = 'v' RETURNING age".into(),
        })
        .expect("UPDATE RETURNING age");

    assert_eq!(result.result.columns, vec!["age".to_string()]);
    let rec = &result.result.records[0];
    assert!(matches!(rec.values.get("age"), Some(Value::Integer(6))));
    assert!(
        rec.values.get("name").is_none(),
        "name must not leak when not in RETURNING list"
    );
}

#[test]
fn returning_on_update_survives_where_column_mutation() {
    // If the UPDATE mutates a column referenced in WHERE, the row is
    // still returned — PG semantics. This verifies we capture ids
    // before the mutation, not via a post-UPDATE SELECT that would
    // miss rows whose WHERE predicate no longer matches.
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "INSERT INTO users (name, age) VALUES ('w', 10)".into(),
    })
    .unwrap();

    let result = q
        .execute(ExecuteQueryInput {
            query: "UPDATE users SET age = 99 WHERE age = 10 RETURNING *".into(),
        })
        .expect("UPDATE RETURNING after WHERE column mutation");

    assert_eq!(result.affected_rows, 1);
    assert_eq!(result.result.records.len(), 1);
    assert!(matches!(
        result.result.records[0].values.get("age"),
        Some(Value::Integer(99))
    ));
}

#[test]
fn update_without_returning_leaves_result_empty() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "INSERT INTO users (name, age) VALUES ('z', 0)".into(),
    })
    .unwrap();
    let result = q
        .execute(ExecuteQueryInput {
            query: "UPDATE users SET age = 1 WHERE name = 'z'".into(),
        })
        .expect("plain update");
    assert_eq!(result.affected_rows, 1);
    assert!(
        result.result.records.is_empty(),
        "no RETURNING => no rows in result"
    );
}

#[test]
fn returning_star_on_delete_returns_pre_image() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "INSERT INTO users (name, age) VALUES ('d1', 11), ('d2', 22)".into(),
    })
    .unwrap();

    let result = q
        .execute(ExecuteQueryInput {
            query: "DELETE FROM users WHERE age = 11 RETURNING *".into(),
        })
        .expect("DELETE RETURNING *");
    assert_eq!(result.affected_rows, 1);
    assert_eq!(result.result.records.len(), 1);
    let rec = &result.result.records[0];
    assert!(matches!(rec.values.get("name"), Some(Value::Text(s)) if s.as_ref() == "d1"));
    assert!(matches!(rec.values.get("age"), Some(Value::Integer(11))));

    // Row is actually gone — a follow-up SELECT must not return it.
    let after = q
        .execute(ExecuteQueryInput {
            query: "SELECT name FROM users".into(),
        })
        .unwrap();
    assert_eq!(after.result.records.len(), 1);
}

#[test]
fn returning_column_list_on_delete_projects_subset() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "INSERT INTO users (name, age) VALUES ('k', 9)".into(),
    })
    .unwrap();
    let result = q
        .execute(ExecuteQueryInput {
            query: "DELETE FROM users WHERE name = 'k' RETURNING name".into(),
        })
        .unwrap();
    assert_eq!(result.result.columns, vec!["name".to_string()]);
    let rec = &result.result.records[0];
    assert!(matches!(rec.values.get("name"), Some(Value::Text(s)) if s.as_ref() == "k"));
    assert!(
        rec.values.get("age").is_none(),
        "age must not leak when not in RETURNING list"
    );
}
