//! Parser hardening test suite for the migration DSL (issue #88).
//!
//! Reuses the `tests/support/parser_hardening` harness from #87 to
//! cover the `CREATE MIGRATION`, `APPLY MIGRATION`,
//! `ROLLBACK MIGRATION`, and `EXPLAIN MIGRATION` shapes. The
//! migration parser is reached through the standard
//! `reddb_server::storage::query::parser::parse` entry point, so
//! `ParserLimits` (max_depth / max_input_bytes /
//! max_identifier_chars) cascade automatically — this test file
//! pins the contract.

mod support {
    pub mod parser_hardening;
}

use proptest::prelude::*;
use reddb_server::storage::query::parser::{self, ParseError, ParserLimits};
use support::parser_hardening::{
    self as harness, assert_no_panic_on, corpus::migration_adversarial_inputs, migration_grammar,
    HardenedParser,
};

/// `HardenedParser` shim around the migration DSL surface. The
/// migration parser shares the top-level entry point with the rest
/// of the SQL grammar, so the shim simply funnels into
/// `parser::parse` — what makes this distinct from the SQL shim is
/// the property + snapshot suites below, which only feed migration-
/// shaped inputs.
pub struct MigrationParser;

impl HardenedParser for MigrationParser {
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
fn migration_parser_does_not_panic_on_adversarial_corpus() {
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            for (name, input) in migration_adversarial_inputs() {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    assert_no_panic_on::<MigrationParser>(&input);
                }));
                if result.is_err() {
                    panic!("migration adversarial corpus entry {} panicked", name);
                }
            }
        })
        .expect("spawn corpus thread");
    handle.join().expect("corpus thread panic");
}

// ---- property tests ---------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 128,
        max_shrink_iters: 64,
        ..ProptestConfig::default()
    })]

    /// Generated CREATE MIGRATION shapes parse cleanly.
    #[test]
    fn proptest_create_migration_roundtrips(s in migration_grammar::create_migration_stmt()) {
        harness::roundtrip_property::<MigrationParser>(&s);
        prop_assert!(
            MigrationParser::parse(&s).is_ok(),
            "create migration did not parse: {}", s
        );
    }

    /// Generated APPLY MIGRATION shapes parse cleanly.
    #[test]
    fn proptest_apply_migration_roundtrips(s in migration_grammar::apply_migration_stmt()) {
        harness::roundtrip_property::<MigrationParser>(&s);
        prop_assert!(
            MigrationParser::parse(&s).is_ok(),
            "apply migration did not parse: {}", s
        );
    }

    /// Generated ROLLBACK MIGRATION shapes parse cleanly.
    #[test]
    fn proptest_rollback_migration_roundtrips(s in migration_grammar::rollback_migration_stmt()) {
        harness::roundtrip_property::<MigrationParser>(&s);
        prop_assert!(
            MigrationParser::parse(&s).is_ok(),
            "rollback migration did not parse: {}", s
        );
    }

    /// Generated EXPLAIN MIGRATION shapes parse cleanly.
    #[test]
    fn proptest_explain_migration_roundtrips(s in migration_grammar::explain_migration_stmt()) {
        harness::roundtrip_property::<MigrationParser>(&s);
        prop_assert!(
            MigrationParser::parse(&s).is_ok(),
            "explain migration did not parse: {}", s
        );
    }

    /// Arbitrary bytes prefixed with a migration keyword never
    /// panic — Err is fine, panic is not.
    #[test]
    fn proptest_migration_arbitrary_suffix_no_panic(
        prefix in prop_oneof![
            Just("CREATE MIGRATION ".to_string()),
            Just("APPLY MIGRATION ".to_string()),
            Just("ROLLBACK MIGRATION ".to_string()),
            Just("EXPLAIN MIGRATION ".to_string()),
        ],
        suffix in ".{0,512}",
    ) {
        let s = format!("{}{}", prefix, suffix);
        harness::roundtrip_property::<MigrationParser>(&s);
    }

    /// Tighter limits always refuse oversized migration bodies.
    #[test]
    fn proptest_migration_input_size_limit_enforced(len in 200usize..2000) {
        let limits = ParserLimits {
            max_input_bytes: 64,
            ..ParserLimits::default()
        };
        let body = "x".repeat(len);
        let input = format!("CREATE MIGRATION m1 AS {}", body);
        let r = MigrationParser::parse_with_limits(&input, limits);
        prop_assert!(r.is_err(), "oversized migration body must error");
    }
}

// ---- happy-path regression tests for issue #92 ------------------
//
// `DEPENDS ON`, `FOR TENANT`, and the `MIGRATION` keyword in
// `APPLY MIGRATION` were each broken by `consume_ident_ci` calls
// against reserved-keyword tokens (or by silent-on-miss semantics).
// These tests pin the post-fix behaviour: the formerly-broken shapes
// now parse, and `APPLY <name>` without `MIGRATION` errors instead of
// silently succeeding.

use reddb_server::storage::query::ast::{ApplyMigrationTarget, QueryExpr};

fn parse_query(input: &str) -> QueryExpr {
    parser::parse(input)
        .unwrap_or_else(|e| panic!("expected ok for {input:?}, got error: {e}"))
        .query
}

#[test]
fn create_migration_with_single_dependency_parses() {
    let q = parse_query("CREATE MIGRATION m1 DEPENDS ON m0 AS CREATE TABLE t (id INTEGER)");
    match q {
        QueryExpr::CreateMigration(cm) => {
            assert_eq!(cm.name, "m1");
            assert_eq!(cm.depends_on, vec!["m0".to_string()]);
        }
        other => panic!("expected CreateMigration, got {other:?}"),
    }
}

#[test]
fn create_migration_with_multiple_dependencies_parses() {
    let q =
        parse_query("CREATE MIGRATION m1 DEPENDS ON m0, m_alpha AS CREATE TABLE t (id INTEGER)");
    match q {
        QueryExpr::CreateMigration(cm) => {
            assert_eq!(cm.name, "m1");
            assert_eq!(
                cm.depends_on,
                vec!["m0".to_string(), "m_alpha".to_string()]
            );
        }
        other => panic!("expected CreateMigration, got {other:?}"),
    }
}

#[test]
fn apply_migration_for_tenant_parses_with_tenant_scope() {
    let q = parse_query("APPLY MIGRATION m1 FOR TENANT 't1'");
    match q {
        QueryExpr::ApplyMigration(am) => {
            assert!(matches!(am.target, ApplyMigrationTarget::Named(ref n) if n == "m1"));
            assert_eq!(am.for_tenant.as_deref(), Some("t1"));
        }
        other => panic!("expected ApplyMigration, got {other:?}"),
    }
}

#[test]
fn apply_without_migration_keyword_returns_parse_error() {
    let result = parser::parse("APPLY m1");
    let err = result.expect_err("APPLY <name> without MIGRATION must error");
    assert!(
        matches!(err.kind, parser::ParseErrorKind::Syntax),
        "expected Syntax error, got {:?}",
        err.kind
    );
    let msg = err.to_string();
    assert!(
        msg.contains("MIGRATION"),
        "error should mention the missing MIGRATION keyword, got: {msg}"
    );
}
