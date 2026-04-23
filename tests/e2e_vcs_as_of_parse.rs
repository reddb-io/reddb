//! Parser coverage for the `AS OF` time-travel clause.
//!
//! Phase 4b: the parser records the clause in `TableQuery.as_of`;
//! executor-side snapshot binding lands in Phase 4c.

use reddb::storage::query::ast::{AsOfClause, QueryExpr};
use reddb::storage::query::parser::parse;

fn as_of_of(sql: &str) -> AsOfClause {
    match parse(sql).unwrap_or_else(|e| panic!("parse `{sql}`: {e:?}")) {
        QueryExpr::Table(q) => q
            .as_of
            .unwrap_or_else(|| panic!("expected as_of to be Some for `{sql}`")),
        other => panic!("expected Table, got {:?}", other),
    }
}

#[test]
fn as_of_commit_literal() {
    let spec = as_of_of("SELECT * FROM users AS OF COMMIT 'abc123'");
    match spec {
        AsOfClause::Commit(h) => assert_eq!(h, "abc123"),
        other => panic!("{:?}", other),
    }
}

#[test]
fn as_of_branch_literal() {
    let spec = as_of_of("SELECT * FROM users AS OF BRANCH 'main'");
    match spec {
        AsOfClause::Branch(b) => assert_eq!(b, "main"),
        other => panic!("{:?}", other),
    }
}

#[test]
fn as_of_tag_literal() {
    let spec = as_of_of("SELECT id FROM users AS OF TAG 'v1.0'");
    match spec {
        AsOfClause::Tag(t) => assert_eq!(t, "v1.0"),
        other => panic!("{:?}", other),
    }
}

#[test]
fn as_of_timestamp_integer() {
    let spec = as_of_of("SELECT * FROM users AS OF TIMESTAMP 1710000000000");
    match spec {
        AsOfClause::TimestampMs(ts) => assert_eq!(ts, 1_710_000_000_000),
        other => panic!("{:?}", other),
    }
}

#[test]
fn as_of_snapshot_integer() {
    let spec = as_of_of("SELECT * FROM users AS OF SNAPSHOT 42");
    match spec {
        AsOfClause::Snapshot(x) => assert_eq!(x, 42),
        other => panic!("{:?}", other),
    }
}

#[test]
fn as_of_coexists_with_where_and_limit() {
    let sql = "SELECT * FROM users AS OF BRANCH 'main' WHERE age > 21 LIMIT 5";
    let expr = parse(sql).expect("parse mixed clauses");
    let QueryExpr::Table(q) = expr else {
        panic!("expected Table");
    };
    assert!(matches!(q.as_of, Some(AsOfClause::Branch(_))));
    assert_eq!(q.limit, Some(5));
    assert!(q.filter.is_some() || q.where_expr.is_some());
}

#[test]
fn no_as_of_leaves_field_none() {
    let expr = parse("SELECT * FROM users WHERE age > 18").expect("plain select");
    let QueryExpr::Table(q) = expr else {
        panic!("expected Table");
    };
    assert!(q.as_of.is_none());
}

#[test]
fn select_alias_with_as_still_works() {
    // Prior bug: `is_clause_keyword` now includes Token::As, which
    // could short-circuit `SELECT col AS alias` parsing. Guard
    // against regression.
    let expr = parse("SELECT name AS full_name FROM users").expect("alias");
    let QueryExpr::Table(q) = expr else {
        panic!("expected Table");
    };
    assert!(q.as_of.is_none());
    assert_eq!(q.select_items.len(), 1);
}
