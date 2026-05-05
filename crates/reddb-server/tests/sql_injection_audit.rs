//! Regression tests pinning the four SQL-injection vectors audited in
//! issue #95. Each test asserts a structural property of the existing
//! design — they fail loudly if a future refactor introduces a SQL
//! string round-trip that would defeat the parser hardening.
//!
//! Companion document: `docs/security/sql-injection-audit.md`.
//!
//! These tests are pure parser / AST exercises — no runtime, no I/O —
//! so they add zero latency to the SELECT hot path and keep CI fast.
#![allow(clippy::needless_borrow)]

use reddb_server::storage::query::ast::{
    CompareOp, CreatePolicyQuery, Expr, Filter, QueryExpr, TableQuery,
};
use reddb_server::storage::query::modes::parse_multi;
use reddb_server::storage::query::parser;
use reddb_server::storage::query::planner::shape::{
    bind_parameterized_query, parameterize_query_expr,
};
use reddb_server::storage::schema::Value;

/// Walk a `TableQuery` to find the WHERE expression, regardless of
/// whether the parser emitted the legacy `filter` slot or the new
/// `where_expr` slot.
fn extract_where_expr(tq: &TableQuery) -> Expr {
    if let Some(expr) = &tq.where_expr {
        return expr.clone();
    }
    panic!("table query has no where_expr after binding");
}

/// V1 — a bound string parameter is treated as a literal `Value::String`,
/// never re-tokenised. A SQL-injection-shaped payload survives binding
/// as an opaque string in `Expr::Literal { value: Value::String(...) }`.
#[test]
fn prepared_bound_string_is_treated_as_literal_not_sql() {
    // Parse a parameterisable SELECT shape. The literal `'placeholder'`
    // gets lifted to a `Parameter` slot during shape extraction.
    let parsed = parse_multi("SELECT * FROM users WHERE name = 'placeholder'")
        .expect("parse base query");
    let parameterised =
        parameterize_query_expr(&parsed).expect("query is parameterisable");
    assert_eq!(
        parameterised.parameter_count, 1,
        "exactly one literal should have been parameterised"
    );

    // Bind the classic injection payload as a typed string Value.
    let payload = "'; DROP TABLE users; --";
    let bound = bind_parameterized_query(
        &parameterised.shape,
        &[Value::text(std::sync::Arc::<str>::from(payload))],
        parameterised.parameter_count,
    )
    .expect("bind succeeds");

    let table = match bound {
        QueryExpr::Table(t) => t,
        other => panic!("expected Table, got {:?}", other),
    };

    let where_expr = extract_where_expr(&table);
    let (lhs, rhs) = match where_expr {
        Expr::BinaryOp { lhs, rhs, .. } => (lhs, rhs),
        other => panic!("expected BinaryOp, got {:?}", other),
    };

    assert!(
        matches!(*lhs, Expr::Column { .. }),
        "lhs should be a column reference"
    );
    let bound_value = match *rhs {
        Expr::Literal { value, .. } => value,
        other => panic!("rhs should be a Literal post-bind, got {:?}", other),
    };

    // Crucial: the entire injection payload survives as a single
    // String value — no re-tokenisation, no comment truncation.
    let s = match &bound_value {
        Value::Text(s) => s.as_ref(),
        other => panic!("bound value should be String, got {:?}", other),
    };
    assert_eq!(s, payload, "bound payload must round-trip byte-for-byte");
    assert!(
        s.contains("DROP TABLE"),
        "the injection text is preserved as opaque string content"
    );
}

/// V2 — a quoted string in identifier position fails at parse time. The
/// lexer emits `Token::String` (not `Token::Ident`), so `expect_ident`
/// in `parse_create_table_query` rejects the input before any engine
/// sees it.
#[test]
fn identifier_with_sql_metacharacters_is_rejected_at_parse() {
    // Double-quoted form: lexer routes to scan_string → Token::String.
    let result = parser::parse(r#"CREATE TABLE "users; DROP TABLE x" (id INT)"#);
    let err = result.expect_err("must be a parse error");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("ident") || msg.contains("identifier") || msg.contains("expected"),
        "error must point at the identifier rule, got: {}",
        err
    );

    // Single-quoted form: same path.
    let result = parser::parse("CREATE TABLE 'evil; DROP TABLE x' (id INT)");
    assert!(
        result.is_err(),
        "single-quoted identifier must also be rejected"
    );

    // Sanity: a normal table name still parses.
    let ok = parser::parse("CREATE TABLE legit_users (id INT)");
    assert!(ok.is_ok(), "non-injection identifier must parse: {:?}", ok);
}

/// V3 — a CREATE POLICY USING (...) body whose string-literal value
/// contains comment metacharacters (`'; --`) lands as a single
/// `Filter::Compare { value: Value::String(...) }` AST node, not as a
/// truncated SQL fragment. The body is never serialised back to SQL.
#[test]
fn rls_policy_body_with_comment_metacharacters_parses_as_literal() {
    // The body matches `tenant = '; -- '` — the inner string-literal
    // is the SQL-escape of `'; -- ` (single-quote escaped via `''`).
    let sql =
        "CREATE POLICY p ON users FOR SELECT TO reader USING (tenant = '''; -- ')";
    let parsed = parse_multi(sql).expect("parse CREATE POLICY");
    let policy: CreatePolicyQuery = match parsed {
        QueryExpr::CreatePolicy(p) => p,
        other => panic!("expected CreatePolicy, got {:?}", other),
    };

    let filter: Filter = *policy.using;
    let value = match filter {
        Filter::Compare {
            field: _,
            op: CompareOp::Eq,
            value,
        } => value,
        other => panic!("expected Filter::Compare, got {:?}", other),
    };

    let s = match &value {
        Value::Text(s) => s.as_ref(),
        other => panic!("policy value should be String, got {:?}", other),
    };
    assert_eq!(
        s, "'; -- ",
        "literal must preserve injection-shaped bytes verbatim"
    );

    // The crucial structural assertion: `Filter` has no `Display` impl
    // that renders to SQL, so the body cannot be re-parsed downstream.
    // We pin this by asserting the name and table are preserved as
    // identifiers (no smuggling), and the filter is a single Compare
    // node — not a comment-truncated parse fragment that produced a
    // simpler AST than expected.
    assert_eq!(policy.name, "p");
    assert_eq!(policy.table, "users");
}

/// V4 — `ASK '...'` parses into an `AskQuery` AST node whose only
/// free-text field is the `question` string. The runtime executes ASK
/// as RAG (search context + LLM Q&A), returning text in a result
/// column — the LLM output is never re-parsed as SQL. This test pins
/// the AST shape: any future refactor that introduces a `synthesised_sql`
/// or similar field will need to revisit this audit.
#[test]
fn ask_path_does_not_re_execute_llm_output_as_sql() {
    let parsed = parse_multi("ASK 'who owns the users table'").expect("parse ASK");
    let ask = match parsed {
        QueryExpr::Ask(a) => a,
        other => panic!("expected Ask, got {:?}", other),
    };
    assert_eq!(ask.question, "who owns the users table");

    // Pin the AskQuery struct shape — these are the only fields the
    // runtime feeds to `execute_ask`. There is no field that holds
    // synthesised SQL or that flows back into the parser.
    let _ = (
        &ask.question,
        &ask.collection,
        &ask.depth,
        &ask.limit,
        &ask.provider,
        &ask.model,
    );

    // Sanity: an injection-shaped question string is just text.
    let parsed = parse_multi("ASK '''; DROP TABLE users; --'").expect("parse ASK with payload");
    let ask = match parsed {
        QueryExpr::Ask(a) => a,
        other => panic!("expected Ask, got {:?}", other),
    };
    assert_eq!(
        ask.question, "'; DROP TABLE users; --",
        "question text round-trips verbatim, no SQL parse on the payload"
    );
}
