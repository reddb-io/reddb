//! Pinned subquery parse-error snapshots and happy-path regressions
//! (issue #106).
//!
//! Mirrors `vector_search_snapshots.rs` (#100) and the rest of the
//! parser-hardening snapshot suites. Each error snapshot installs the
//! shared secret-redactor (#98) before calling
//! `insta::assert_snapshot!`, so any future generator-fed input can
//! never accidentally pin a credential into a `*.snap` file.
//!
//! Phase A (#106): the `WHERE-IN`, `WHERE-EXISTS`, scalar, and
//! correlated paths all *error* on main today — the parser's
//! `Subquery` AST variant is scheduled for Fase 2 Week 3
//! (`ast.rs` L216). Each snapshot below is **FIXME-pinned**: when the
//! subquery branch lands, the recorded error string changes and the
//! diff forces the test author to widen the happy-path block instead
//! of letting a silent regression slip through.
//!
//! Workflow:
//!   - First run: `cargo insta accept` records the new outputs.
//!   - Reviewing changes: `cargo insta review`.
//!   - CI: snapshots must match exactly.

mod support {
    pub mod parser_hardening;
}

use reddb_server::storage::query::ast::{Expr, Filter, QueryExpr};
use reddb_server::storage::query::parser;
use support::parser_hardening::secret_redactor;

/// Parse `input` and format the resulting error for snapshotting.
/// Successful parses render as `UNEXPECTED OK` so a missing error
/// path is visible in the diff — and so the moment the parser stops
/// rejecting one of the FIXME-pinned shapes the snapshot updates
/// instead of silently passing.
fn fmt_parse_error(input: &str) -> String {
    match parser::parse(input) {
        Ok(_) => format!("UNEXPECTED OK\ninput: {:?}\n", input),
        Err(e) => format!("input: {:?}\nkind:  {:?}\nerror: {}\n", input, e.kind, e),
    }
}

/// Wrapper that pins both the install_redactions guard and the
/// snapshot name. Every snapshot test below uses this macro so the
/// redaction guard can never be accidentally omitted.
macro_rules! snap_redacted {
    ($name:ident, $input:expr) => {
        #[test]
        fn $name() {
            let _g = secret_redactor::install_redactions();
            insta::assert_snapshot!(stringify!($name), fmt_parse_error($input));
        }
    };
}

// ============================================================
// WHERE x IN (SELECT …) error scenarios
// ============================================================
//
// FIXME: bug — fix when AST `Subquery` variant lands. `parse_in`
// (`expr.rs` L694–L714) only accepts a comma-list of expressions,
// so the bare `SELECT` token errors. When the subquery branch lands,
// the snapshot text below changes and the diff forces a happy-path
// upgrade.

snap_redacted!(subq_in_eof_after_lparen, "SELECT * FROM t WHERE id IN (");
snap_redacted!(
    subq_in_unterminated_inner,
    "SELECT * FROM t WHERE id IN (SELECT id FROM u"
);
snap_redacted!(
    subq_not_in_subquery,
    "SELECT * FROM t WHERE id NOT IN (SELECT id FROM u)"
);

// ============================================================
// WHERE EXISTS (SELECT …) error scenarios
// ============================================================
//
// FIXME: bug — fix when AST `Subquery` variant lands. `EXISTS` is a
// recognised lexer token (`lexer.rs` L121) but the Pratt expression
// parser does not handle it as a prefix unary, so this errors before
// reaching the inner SELECT.

snap_redacted!(
    subq_exists_basic,
    "SELECT * FROM t WHERE EXISTS (SELECT 1 FROM u)"
);
snap_redacted!(
    subq_exists_eof_after_keyword,
    "SELECT * FROM t WHERE EXISTS"
);
snap_redacted!(
    subq_not_exists_basic,
    "SELECT * FROM t WHERE NOT EXISTS (SELECT 1 FROM u)"
);

// ============================================================
// scalar subquery `= (SELECT …)` error scenarios
// ============================================================
//
// FIXME: bug — fix when AST `Subquery` variant lands. The
// parenthesised RHS reaches `parse_atom`, which descends into an
// expression — the bare `SELECT` keyword is not a valid atom and
// surfaces a Syntax error.

snap_redacted!(
    subq_scalar_eq_unterminated,
    "SELECT * FROM t WHERE x = (SELECT"
);

// ============================================================
// FROM (SELECT …) AS sub — error scenarios
// ============================================================
//
// These shapes round-trip on the happy path (see
// `happy_from_aliased_subquery_basic` below); the snapshots here pin
// the malformed surface so a future tightening of the FROM-subquery
// recogniser surfaces in the diff.

snap_redacted!(subq_from_eof_after_lparen, "SELECT * FROM (");
snap_redacted!(
    subq_from_inner_not_select,
    "SELECT * FROM (DELETE FROM t) AS x"
);
snap_redacted!(
    subq_from_unterminated,
    "SELECT * FROM (SELECT id FROM t AS sub"
);

// ============================================================
// correlated outer/inner reference error scenarios
// ============================================================
//
// FIXME: bug — same gating as the scalar-subquery snapshots. The
// outer `=` branch hits `parse_atom` on the inner `SELECT` keyword.

snap_redacted!(
    subq_correlated_eq_outer_dot_col,
    "SELECT * FROM users u WHERE u.id = \
     (SELECT user_id FROM orders o WHERE o.user_id = u.id)"
);
snap_redacted!(
    subq_correlated_in_outer_dot_col,
    "SELECT * FROM users u WHERE u.id IN \
     (SELECT user_id FROM orders o WHERE o.user_id = u.id)"
);
snap_redacted!(
    subq_correlated_dangling_dot,
    "SELECT * FROM users u WHERE u.id IN \
     (SELECT user_id FROM orders o WHERE o. = u.id)"
);

// ============================================================
// Happy-path regression tests
// ============================================================
//
// These are not snapshots — they assert the AST shape of correct
// inputs so a parser change that silently breaks the only
// already-supported subquery shape (`FROM (SELECT …) AS sub`) trips
// a precise assertion message instead of a snapshot diff. Mirrors
// the post-#92 happy-path coverage in `migration_parser.rs` and the
// happy-path block in `vector_search_snapshots.rs`.

fn parse_query(input: &str) -> QueryExpr {
    parser::parse(input)
        .unwrap_or_else(|e| panic!("expected ok for {input:?}, got error: {e}"))
        .query
}

fn expr_contains_subquery(expr: &Expr) -> bool {
    match expr {
        Expr::Subquery { .. } => true,
        Expr::BinaryOp { lhs, rhs, .. } => {
            expr_contains_subquery(lhs) || expr_contains_subquery(rhs)
        }
        Expr::UnaryOp { operand, .. } | Expr::Cast { inner: operand, .. } => {
            expr_contains_subquery(operand)
        }
        Expr::FunctionCall { args, .. } => args.iter().any(expr_contains_subquery),
        Expr::Case {
            branches, else_, ..
        } => {
            branches.iter().any(|(condition, value)| {
                expr_contains_subquery(condition) || expr_contains_subquery(value)
            }) || else_.as_deref().is_some_and(expr_contains_subquery)
        }
        Expr::IsNull { operand, .. } => expr_contains_subquery(operand),
        Expr::InList { target, values, .. } => {
            expr_contains_subquery(target) || values.iter().any(expr_contains_subquery)
        }
        Expr::Between {
            target, low, high, ..
        } => {
            expr_contains_subquery(target)
                || expr_contains_subquery(low)
                || expr_contains_subquery(high)
        }
        Expr::Literal { .. } | Expr::Column { .. } | Expr::Parameter { .. } => false,
    }
}

fn filter_contains_subquery(filter: &Filter) -> bool {
    match filter {
        Filter::CompareExpr { lhs, rhs, .. } => {
            expr_contains_subquery(lhs) || expr_contains_subquery(rhs)
        }
        Filter::And(left, right) | Filter::Or(left, right) => {
            filter_contains_subquery(left) || filter_contains_subquery(right)
        }
        Filter::Not(inner) => filter_contains_subquery(inner),
        Filter::Compare { .. }
        | Filter::CompareFields { .. }
        | Filter::IsNull(_)
        | Filter::IsNotNull(_)
        | Filter::In { .. }
        | Filter::Between { .. }
        | Filter::Like { .. }
        | Filter::StartsWith { .. }
        | Filter::EndsWith { .. }
        | Filter::Contains { .. } => false,
    }
}

fn assert_table_where_contains_subquery(input: &str) {
    match parse_query(input) {
        QueryExpr::Table(t) => {
            let where_has_subquery = t.where_expr.as_ref().is_some_and(expr_contains_subquery)
                || t.filter.as_ref().is_some_and(filter_contains_subquery);
            assert!(
                where_has_subquery,
                "WHERE clause must preserve a Subquery expression for {input:?}"
            );
        }
        other => panic!("expected QueryExpr::Table, got {other:?}"),
    }
}

#[test]
fn happy_from_aliased_subquery_basic() {
    // The FROM-prefixed form (no `SELECT *` outer) is the entry
    // point that `parse_from_query` (`join.rs` L12) handles. The
    // `SELECT * FROM (...)` form is rejected today because
    // `parse_select_query_inner` (`table.rs` L130) requires an
    // identifier after FROM. The SELECT-prefixed shape is pinned in
    // the snapshot block above for the same FIXME-gating reason.
    let q = parse_query("FROM (SELECT id FROM users) AS sub");
    match q {
        QueryExpr::Table(t) => {
            assert!(t.source.is_some(), "FROM-subquery must populate source");
            assert_eq!(t.alias.as_deref(), Some("sub"));
        }
        other => panic!("expected QueryExpr::Table, got {other:?}"),
    }
}

#[test]
fn happy_from_aliased_subquery_with_inner_where() {
    let q = parse_query("FROM (SELECT id FROM users WHERE active = TRUE) AS active_users");
    match q {
        QueryExpr::Table(t) => {
            assert!(t.source.is_some());
            assert_eq!(t.alias.as_deref(), Some("active_users"));
        }
        other => panic!("expected QueryExpr::Table, got {other:?}"),
    }
}

#[test]
fn happy_from_aliased_subquery_bare_alias_no_as() {
    // `parse_from_query` accepts a bare-identifier alias when the
    // following token is not a join/clause keyword (`join.rs` L36).
    let q = parse_query("FROM (SELECT id FROM t) sub");
    match q {
        QueryExpr::Table(t) => {
            assert!(t.source.is_some());
            assert_eq!(t.alias.as_deref(), Some("sub"));
        }
        other => panic!("expected QueryExpr::Table, got {other:?}"),
    }
}

#[test]
fn happy_from_aliased_subquery_no_alias() {
    // FROM-subquery alias is optional in `parse_from_query`; the
    // resulting TableQuery has `alias: None` but `source` populated.
    let q = parse_query("FROM (SELECT id FROM t)");
    match q {
        QueryExpr::Table(t) => {
            assert!(t.source.is_some(), "FROM-subquery must populate source");
            assert!(t.alias.is_none(), "alias is optional on FROM-subquery");
        }
        other => panic!("expected QueryExpr::Table, got {other:?}"),
    }
}

#[test]
fn happy_from_aliased_subquery_with_outer_where() {
    let q = parse_query("FROM (SELECT id FROM users) AS sub WHERE sub.id = 1");
    match q {
        QueryExpr::Table(t) => {
            assert!(t.source.is_some());
            assert_eq!(t.alias.as_deref(), Some("sub"));
            assert!(
                t.where_expr.is_some() || t.filter.is_some(),
                "outer WHERE must populate where_expr or filter"
            );
        }
        other => panic!("expected QueryExpr::Table, got {other:?}"),
    }
}

#[test]
fn happy_from_aliased_subquery_with_outer_limit() {
    let q = parse_query("FROM (SELECT id FROM users) AS sub LIMIT 10");
    match q {
        QueryExpr::Table(t) => {
            assert_eq!(t.limit, Some(10));
            assert_eq!(t.alias.as_deref(), Some("sub"));
        }
        other => panic!("expected QueryExpr::Table, got {other:?}"),
    }
}

#[test]
fn happy_where_in_subquery_after_subquery_ast_lands() {
    assert_table_where_contains_subquery("SELECT * FROM t WHERE id IN (SELECT id FROM u)");
}

// FIXME: bug — same gating as the IN-subquery happy path. Drop
// `#[ignore]` once `EXISTS (SELECT …)` is wired through the Pratt
// expression parser.
#[test]
#[ignore = "blocked: WHERE EXISTS (SELECT …) needs Pratt-prefix support for Token::Exists"]
fn happy_where_exists_subquery_after_subquery_ast_lands() {
    let _q = parse_query("SELECT * FROM t WHERE EXISTS (SELECT 1 FROM u)");
}

#[test]
fn happy_scalar_subquery_after_subquery_ast_lands() {
    assert_table_where_contains_subquery("SELECT * FROM t WHERE x = (SELECT MAX(y) FROM u)");
}

#[test]
fn happy_scalar_lt_subquery_after_subquery_ast_lands() {
    assert_table_where_contains_subquery("SELECT * FROM t WHERE x < (SELECT MAX(y) FROM u)");
}
