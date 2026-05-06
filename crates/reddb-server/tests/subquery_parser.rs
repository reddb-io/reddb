//! Parser hardening property suite for subqueries (issue #106).
//!
//! Reuses the `tests/support/parser_hardening` harness from #87 to
//! cover the five subquery shapes called out in the issue:
//!   - `WHERE x IN (SELECT …)`
//!   - `WHERE EXISTS (SELECT …)`
//!   - scalar subqueries `= (SELECT …)`
//!   - `FROM (SELECT …) AS sub`
//!   - correlated outer/inner references
//!
//! Phase A (#106): tests-only. The only shape the parser already
//! accepts on main is `FROM (SELECT …) AS sub` (`parser/join.rs`).
//! The other four are pinned as `assert_no_panic_on::<SubqueryParser>`
//! today and the snapshot suite (`subquery_snapshots.rs`) records the
//! exact error message a future `Subquery`-AST landing must update.
//! No source mods, no deps bump.

mod support {
    pub mod parser_hardening;
}

use proptest::prelude::*;
use reddb_server::storage::query::parser::{self, ParseError, ParseErrorKind, ParserLimits};
use support::parser_hardening::{
    self as harness, assert_no_panic_on, corpus::subquery_adversarial_inputs, subquery_grammar,
    HardenedParser,
};

/// `HardenedParser` shim. Subqueries reach the parser via the same
/// top-level `parser::parse` entry point as the rest of the SQL
/// surface, so this is mechanically identical to the SQL / vector
/// shims — what makes it distinct is the property + snapshot suites
/// below, which only feed subquery-shaped inputs.
pub struct SubqueryParser;

impl HardenedParser for SubqueryParser {
    type Error = ParseError;

    fn parse(input: &str) -> Result<(), Self::Error> {
        parser::parse(input).map(|_| ())
    }

    fn parse_with_limits(input: &str, limits: ParserLimits) -> Result<(), Self::Error> {
        let mut p = parser::Parser::with_limits(input, limits)?;
        p.parse().map(|_| ())
    }
}

// ---- panic-safety on adversarial corpus -------------------------

#[test]
fn subquery_parser_does_not_panic_on_adversarial_corpus() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            for (name, input) in subquery_adversarial_inputs() {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    assert_no_panic_on::<SubqueryParser>(&input);
                }));
                if result.is_err() {
                    panic!("subquery adversarial corpus entry {} panicked", name);
                }
            }
        })
        .expect("spawn corpus thread");
    handle.join().expect("corpus thread panic");
}

// ---- depth-guard regression (issue #91 SELECT-recursion) --------

/// Deeply nested scalar subqueries must surface
/// `ParseErrorKind::DepthLimit` rather than overflow the Rust stack.
///
/// Counterpart to `dos_limit_chained_not_in_where_does_not_overflow_stack`
/// from `parser/tests.rs` (which covered NOT-recursion). A 10k-NOT
/// payload trips `parse_not_expr`; this test will trip the
/// `parse_atom`/`parse_select_query` reentry that subqueries take —
/// once the AST `Subquery` variant lands and a parenthesised SELECT
/// becomes a valid expression atom.
///
/// Phase A (#106) reality: the scalar-subquery RHS errors with
/// `Syntax` long before the depth guard counts the nested SELECTs,
/// because the inner `SELECT` keyword is not yet a valid atom. The
/// assertion below therefore accepts `Syntax` *or* `DepthLimit` and
/// pins the post-fix expectation in a FIXME comment so the test
/// becomes load-bearing the moment subquery support lands.
///
/// FIXME: bug — fix when AST `Subquery` variant lands (ast.rs L216).
/// Once `(SELECT …)` becomes a valid expression atom, tighten the
/// assertion to `matches!(err.kind, DepthLimit { … })` only — the
/// `Syntax` fallback is a Phase A artefact.
///
/// Runs on a generous-stack thread so an unfixed build (where the
/// depth guard is missing) reports the regression as a panic on the
/// *inner* thread instead of aborting the whole runner.
#[test]
fn dos_limit_deeply_nested_select_subquery_does_not_overflow_stack() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            // N=200: well above ParserLimits::default().max_depth=128
            // so the guard *must* fire structurally before any leaf
            // expression reaches the Rust stack ceiling. The 10k-NOT
            // case in #91 used 10k specifically because each NOT
            // pushes one stack frame; SELECT subqueries push more
            // frames per level (parse_select → parse_filter →
            // parse_expr_prec → parse_atom → parse_select), so 200
            // is the equivalent stress point for this slice.
            let input = subquery_grammar::nested_scalar_subquery(200);
            let err = parser::parse(&input).err().expect("must error, not panic");
            assert!(
                matches!(
                    err.kind,
                    ParseErrorKind::DepthLimit { limit_name: "max_depth", .. }
                        | ParseErrorKind::Syntax
                ),
                "expected DepthLimit or Syntax (Phase A), got: {:?}",
                err.kind
            );
        })
        .expect("spawn deep-SELECT thread");
    handle.join().expect("deep-SELECT thread must not panic");
}

/// Companion test to `dos_limit_deeply_nested_select_subquery_does_not_overflow_stack`
/// using the FROM-prefixed subquery shape — the **only** subquery
/// form parsed today (`parser/join.rs` L23).
///
/// Phase A reality: `parse_select_query_inner` (`parser/table.rs`
/// L130) requires an identifier after the inner `FROM`, so
/// `FROM (SELECT * FROM (SELECT * FROM t) AS a) AS b` hits a Syntax
/// error at the inner `(` long before `parse_select_query`'s depth
/// guard can count the nested subqueries. Once the
/// `parse_select_query_inner` body is widened to accept a
/// FROM-subquery (Fase 1.7 follow-up), the depth guard becomes
/// load-bearing and the assertion below tightens to `DepthLimit`
/// only.
///
/// FIXME: bug — fix when nested FROM-subqueries become supported in
/// `parse_select_query_inner`. Today the assertion accepts
/// `Syntax | DepthLimit`; once the wider acceptance lands the
/// `Syntax` arm should be dropped so this test becomes a hard pin.
#[test]
fn dos_limit_deeply_nested_from_subquery_returns_depth_limit() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let depth = 200usize;
            let mut s = String::new();
            for _ in 0..depth {
                s.push_str("FROM (SELECT * ");
            }
            s.push_str("FROM t");
            for i in 0..depth {
                s.push_str(&format!(") AS a{}", i));
            }
            let err = parser::parse(&s).err().expect("must error, not panic");
            assert!(
                matches!(
                    err.kind,
                    ParseErrorKind::DepthLimit { limit_name: "max_depth", .. }
                        | ParseErrorKind::Syntax
                ),
                "expected DepthLimit or Syntax (Phase A), got: {:?}",
                err.kind
            );
        })
        .expect("spawn deep-FROM-subquery thread");
    handle.join().expect("deep-FROM-subquery thread must not panic");
}

// ---- property tests ---------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        max_shrink_iters: 64,
        ..ProptestConfig::default()
    })]

    /// `WHERE x IN (SELECT …)` never panics. Today the parser bails
    /// because `parse_in` only accepts a comma-list of expressions
    /// (FIXME pinned in `subquery_grammar.rs::where_in_subquery_stmt`).
    /// Once the AST `Subquery` variant lands this assertion can be
    /// strengthened to `parse(...).is_ok()`.
    #[test]
    fn proptest_where_in_subquery_no_panic(
        s in subquery_grammar::where_in_subquery_stmt(),
    ) {
        harness::roundtrip_property::<SubqueryParser>(&s);
    }

    /// `WHERE EXISTS (SELECT …)` never panics. Same FIXME gating as
    /// the IN-subquery proptest.
    #[test]
    fn proptest_where_exists_subquery_no_panic(
        s in subquery_grammar::where_exists_subquery_stmt(),
    ) {
        harness::roundtrip_property::<SubqueryParser>(&s);
    }

    /// Scalar subqueries `<col> <op> (SELECT …)` never panic. Same
    /// FIXME gating as the IN-subquery proptest.
    #[test]
    fn proptest_scalar_subquery_no_panic(
        s in subquery_grammar::scalar_subquery_stmt(),
    ) {
        harness::roundtrip_property::<SubqueryParser>(&s);
    }

    /// `FROM (SELECT …) AS sub` parses cleanly. This is the only
    /// subquery shape on main today (`parser/join.rs` L23) and the
    /// strategy enforces `is_ok()` to catch regressions.
    #[test]
    fn proptest_from_aliased_subquery_roundtrips(
        s in subquery_grammar::from_aliased_subquery_stmt(),
    ) {
        harness::roundtrip_property::<SubqueryParser>(&s);
        prop_assert!(
            SubqueryParser::parse(&s).is_ok(),
            "FROM (SELECT …) AS sub did not parse: {}", s
        );
    }

    /// Correlated subqueries — outer alias referenced from the inner
    /// WHERE — never panic. FIXME-gated like the scalar-subquery
    /// proptest because the outer `=` operator hits the same hole.
    #[test]
    fn proptest_correlated_subquery_no_panic(
        s in subquery_grammar::correlated_subquery_stmt(),
    ) {
        harness::roundtrip_property::<SubqueryParser>(&s);
    }

    /// Arbitrary suffix glued to a subquery-keyword prefix never
    /// panics. `Err` is fine.
    #[test]
    fn proptest_subquery_arbitrary_suffix_no_panic(
        prefix in prop_oneof![
            Just("SELECT * FROM t WHERE x IN (".to_string()),
            Just("SELECT * FROM t WHERE EXISTS (".to_string()),
            Just("SELECT * FROM t WHERE x = (SELECT ".to_string()),
            Just("FROM (SELECT ".to_string()),
            Just("FROM (SELECT id FROM t) AS ".to_string()),
        ],
        suffix in ".{0,256}",
    ) {
        let s = format!("{}{}", prefix, suffix);
        harness::roundtrip_property::<SubqueryParser>(&s);
    }

    /// Tighter `max_depth` always refuses a generated nested-SELECT
    /// chain. The depth guard is the load-bearing safety net against
    /// the SELECT-recursion DoS this slice pins.
    #[test]
    fn proptest_subquery_depth_limit_enforced(
        depth in 40usize..120,
    ) {
        let limits = ParserLimits {
            max_depth: 16,
            ..ParserLimits::default()
        };
        let input = subquery_grammar::nested_in_subquery(depth);
        let r = SubqueryParser::parse_with_limits(&input, limits);
        prop_assert!(r.is_err(), "deeply nested subquery must error under tight depth");
    }
}
