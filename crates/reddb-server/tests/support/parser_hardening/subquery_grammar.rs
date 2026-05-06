//! Proptest strategies that emit subquery-shaped SQL strings (issue #106).
//!
//! Mirrors the layout of `sql_grammar.rs`, `graph_dsl_grammar.rs`,
//! and `vector_search_grammar.rs`: each strategy returns a `String`
//! that, when fed back through `parser::parse`, must not panic.
//! Whether the string *succeeds* depends on which subquery shape the
//! parser already supports — see the per-strategy notes below.
//!
//! Surface covered:
//!   - `WHERE x IN (SELECT …)` — Strategy 1 (`where_in_subquery_stmt`)
//!   - `WHERE EXISTS (SELECT …)` — Strategy 2 (`where_exists_subquery_stmt`)
//!   - scalar `= (SELECT …)` — Strategy 3 (`scalar_subquery_stmt`)
//!   - `FROM (SELECT …) AS sub` — Strategy 4 (`from_aliased_subquery_stmt`)
//!   - correlated outer/inner refs — Strategy 5 (`correlated_subquery_stmt`)
//!
//! Composition with #99 (graph) and #100 (vector) strategies on main
//! is intentionally light: the subquery surface is SQL-only today —
//! `parse_from_query` rejects anything that does not start with
//! `SELECT` (`crates/.../parser/join.rs` L27). Strategies 4 + 5 reuse
//! `sql_grammar::ident` and `sql_grammar::literal` to keep the token
//! shape consistent with the rest of the SQL hardening corpus.
//!
//! Phase A (#106) is tests-only. Strategies 1, 2, 3 generate inputs
//! that today's parser *errors* on rather than parses (the AST
//! `Subquery` variant is scheduled for Fase 2 Week 3 — see
//! `ast.rs` L216). The property tests in `tests/subquery_*.rs` only
//! assert no-panic for those three; once the parser starts accepting
//! them, the snapshot suite will detect the change and a follow-up
//! issue widens the strategies to assert `parse(...).is_ok()`.

use proptest::prelude::*;

use super::sql_grammar;

/// A simple identifier — same shape as `sql_grammar::ident()`.
/// Re-exported here so that the subquery strategies read locally
/// without forcing every callsite to reach into `sql_grammar::`.
pub fn ident() -> impl Strategy<Value = String> {
    sql_grammar::ident()
}

/// A literal value (int, string, boolean, NULL).
pub fn literal() -> impl Strategy<Value = String> {
    sql_grammar::literal()
}

/// A small inner SELECT body: `SELECT <col> FROM <table> [WHERE …]`.
/// Single-column projection because the WHERE-IN and scalar shapes
/// require it semantically; the FROM-aliased shape is tolerant of a
/// wider projection but sticks with single-column for shrinking
/// economy.
pub fn inner_select() -> impl Strategy<Value = String> {
    (
        ident(),
        ident(),
        proptest::option::of((ident(), literal())),
    )
        .prop_map(|(col, table, wh)| {
            let mut s = format!("SELECT {} FROM {}", col, table);
            if let Some((wcol, wval)) = wh {
                s.push_str(&format!(" WHERE {} = {}", wcol, wval));
            }
            s
        })
}

// ============================================================
// Strategy 1: `WHERE x IN (SELECT …)`
// ============================================================

/// `SELECT * FROM <outer> WHERE <col> [NOT] IN (SELECT …)`.
///
/// FIXME: bug — fix when AST `Subquery` variant lands (ast.rs L216
/// scheduled for Fase 2 Week 3). Today `parse_in` only accepts a
/// comma-list of expressions (`expr.rs` L694–L714) and bails on the
/// inner `SELECT` token. The strategy is wired so that, once the
/// subquery branch lands, the property test can flip from "no-panic"
/// to `parse(...).is_ok()` with no string changes.
pub fn where_in_subquery_stmt() -> impl Strategy<Value = String> {
    (ident(), ident(), any::<bool>(), inner_select()).prop_map(
        |(outer, col, negated, inner)| {
            let kw = if negated { "NOT IN" } else { "IN" };
            format!("SELECT * FROM {} WHERE {} {} ({})", outer, col, kw, inner)
        },
    )
}

// ============================================================
// Strategy 2: `WHERE EXISTS (SELECT …)`
// ============================================================

/// `SELECT * FROM <outer> WHERE [NOT] EXISTS (SELECT …)`.
///
/// FIXME: bug — fix when AST `Subquery` variant lands. `EXISTS` is a
/// recognised lexer token (`lexer.rs` L121, L1145) but only the DDL
/// `IF EXISTS` / `IF NOT EXISTS` branches consume it (`ddl.rs`
/// L643/L654). The Pratt expression parser does not handle it as a
/// prefix unary, so today this input errors before reaching the inner
/// SELECT.
pub fn where_exists_subquery_stmt() -> impl Strategy<Value = String> {
    (ident(), any::<bool>(), inner_select()).prop_map(|(outer, negated, inner)| {
        let kw = if negated { "NOT EXISTS" } else { "EXISTS" };
        format!("SELECT * FROM {} WHERE {} ({})", outer, kw, inner)
    })
}

// ============================================================
// Strategy 3: scalar subquery `= (SELECT …)`
// ============================================================

/// `SELECT * FROM <outer> WHERE <col> <op> (SELECT …)`.
///
/// Comparison op covers `=`, `!=`, `<`, `>`, `<=`, `>=` so each Pratt
/// branch sees a parenthesised SELECT on the RHS.
///
/// FIXME: bug — fix when AST `Subquery` variant lands. Today the
/// parenthesised RHS reaches `parse_atom`, which descends into an
/// expression — the bare `SELECT` keyword is not a valid expression
/// atom and surfaces a Syntax error.
pub fn scalar_subquery_stmt() -> impl Strategy<Value = String> {
    let op = prop_oneof![
        Just("="),
        Just("!="),
        Just("<"),
        Just(">"),
        Just("<="),
        Just(">="),
    ];
    (ident(), ident(), op, inner_select()).prop_map(|(outer, col, o, inner)| {
        format!("SELECT * FROM {} WHERE {} {} ({})", outer, col, o, inner)
    })
}

// ============================================================
// Strategy 4: `FROM (SELECT …) AS sub`
// ============================================================

/// `FROM (SELECT …) AS <alias>` — the FROM-prefixed query form
/// recognised by `parse_from_query` (`parser/join.rs` L12). This is
/// the **only** subquery shape the parser already accepts on main
/// today: the SELECT-prefixed form (`SELECT * FROM (SELECT …)`) calls
/// `parse_select_query_inner` which expects an identifier after FROM
/// and rejects the LParen — see `parser/table.rs` L130–L137.
///
/// Generated strings must parse cleanly — the property test enforces
/// `is_ok()`.
///
/// `AS` is emitted explicitly (rather than the bare-identifier alias
/// path that join.rs also accepts) so the strategy stays robust to
/// future tightenings of the alias-keyword exclusion list.
pub fn from_aliased_subquery_stmt() -> impl Strategy<Value = String> {
    (inner_select(), ident()).prop_map(|(inner, alias)| {
        format!("FROM ({}) AS {}", inner, alias)
    })
}

// ============================================================
// Strategy 5: correlated outer/inner reference
// ============================================================

/// Correlated subquery: the inner WHERE references an outer alias —
/// `SELECT * FROM <outer> o WHERE o.<col> = (SELECT <col> FROM <inner> i WHERE i.<col> = o.<col>)`.
///
/// Phase A only asserts panic-safety. The qualified-column reference
/// (`o.col`) reaches the parser as a `column.qualifier` shape, which
/// the SQL parser does parse for non-correlated joins; whether the
/// outer-alias resolution survives semantic analysis is a Fase 2
/// concern and is *not* exercised here.
///
/// FIXME: bug — same gating as Strategies 1–3. The outer `=` branch
/// trips the same scalar-subquery hole described in
/// `scalar_subquery_stmt`.
pub fn correlated_subquery_stmt() -> impl Strategy<Value = String> {
    (ident(), ident(), ident(), ident()).prop_map(|(outer, inner, col, jcol)| {
        format!(
            "SELECT * FROM {outer} o WHERE o.{col} = \
             (SELECT {col} FROM {inner} i WHERE i.{jcol} = o.{jcol})",
            outer = outer,
            inner = inner,
            col = col,
            jcol = jcol,
        )
    })
}

// ============================================================
// Top-level union (mirrors `sql_grammar::any_stmt`)
// ============================================================

/// Any of the five subquery shapes covered above. Useful for the
/// arbitrary-bytes / no-panic property that wants a uniform mix.
pub fn any_subquery_stmt() -> impl Strategy<Value = String> {
    prop_oneof![
        where_in_subquery_stmt(),
        where_exists_subquery_stmt(),
        scalar_subquery_stmt(),
        from_aliased_subquery_stmt(),
        correlated_subquery_stmt(),
    ]
}

// ============================================================
// Depth-stress generators (bug pin for #91 SELECT recursion)
// ============================================================

/// Build an N-deep nested scalar-subquery shape:
///   `SELECT * FROM t WHERE x = (SELECT x FROM t WHERE x = (SELECT … ))`
/// where the innermost SELECT has no further nesting.
///
/// The harness uses this to feed N=200 into `parser::parse_with_limits`
/// and pin that the parser returns `ParseErrorKind::DepthLimit`
/// rather than overflowing the Rust stack — the SELECT-recursion
/// counterpart to the 10k-NOT regression already in the parser
/// tests (issue #91, `parser/tests.rs::dos_limit_chained_not_in_where_does_not_overflow_stack`).
pub fn nested_scalar_subquery(depth: usize) -> String {
    let mut s = String::new();
    for _ in 0..depth {
        s.push_str("SELECT x FROM t WHERE x = (");
    }
    // Innermost atom — a literal so the inner-most parens close on a
    // valid expression atom rather than another SELECT.
    s.push_str("SELECT x FROM t");
    for _ in 0..depth {
        s.push(')');
    }
    format!("SELECT * FROM outer_t WHERE x = ({})", s)
}

/// Same shape as `nested_scalar_subquery` but using `WHERE x IN
/// (SELECT …)` as the recursive constructor. Exercises the
/// `parse_in` → `parse_atom` path rather than the Pratt-RHS path.
pub fn nested_in_subquery(depth: usize) -> String {
    let mut s = String::new();
    for _ in 0..depth {
        s.push_str("SELECT x FROM t WHERE x IN (");
    }
    s.push_str("SELECT x FROM t");
    for _ in 0..depth {
        s.push(')');
    }
    s
}
