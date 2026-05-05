//! Parser hardening test suite (issue #87).
//!
//! Property-based, snapshot, and panic-safety tests for the SQL
//! parser. Reuses the harness in `tests/support/parser_hardening`
//! so subsequent slices (#88, #89, #90) plug in their own parsers
//! against the same scaffolding.

mod support {
    pub mod parser_hardening;
}

use proptest::prelude::*;
use reddb_server::storage::query::parser::{self, ParseError, ParserLimits};
use support::parser_hardening::{
    self as harness, assert_no_panic_on, corpus::adversarial_inputs, sql_grammar, HardenedParser,
};

/// Concrete `HardenedParser` shim around the SQL parser.
pub struct SqlParser;

impl HardenedParser for SqlParser {
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
fn parser_does_not_panic_on_adversarial_corpus() {
    // Use a thread with a generous stack so even pathological
    // inputs that allocate large parse-state strings (e.g. the
    // very-long-string-lit fixture) don't trip the test runner's
    // 2 MiB default stack. Recursion itself is bounded by the
    // harness's `test_safe_limits` (max_depth=32).
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            for (name, input) in adversarial_inputs() {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    assert_no_panic_on::<SqlParser>(&input);
                }));
                if result.is_err() {
                    panic!("adversarial corpus entry {} panicked", name);
                }
            }
        })
        .expect("spawn corpus thread");
    handle.join().expect("corpus thread panic");
}

// ---- property tests ---------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        max_shrink_iters: 64,
        ..ProptestConfig::default()
    })]

    /// Generated SELECT shapes parse cleanly.
    #[test]
    fn proptest_select_roundtrips(s in sql_grammar::select_stmt()) {
        harness::roundtrip_property::<SqlParser>(&s);
        // Valid shape — must succeed.
        prop_assert!(SqlParser::parse(&s).is_ok(), "select did not parse: {}", s);
    }

    /// Generated INSERT shapes parse cleanly.
    #[test]
    fn proptest_insert_roundtrips(s in sql_grammar::insert_stmt()) {
        harness::roundtrip_property::<SqlParser>(&s);
        prop_assert!(SqlParser::parse(&s).is_ok(), "insert did not parse: {}", s);
    }

    /// Generated UPDATE shapes parse cleanly.
    #[test]
    fn proptest_update_roundtrips(s in sql_grammar::update_stmt()) {
        harness::roundtrip_property::<SqlParser>(&s);
        prop_assert!(SqlParser::parse(&s).is_ok(), "update did not parse: {}", s);
    }

    /// Generated DELETE shapes parse cleanly.
    #[test]
    fn proptest_delete_roundtrips(s in sql_grammar::delete_stmt()) {
        harness::roundtrip_property::<SqlParser>(&s);
        prop_assert!(SqlParser::parse(&s).is_ok(), "delete did not parse: {}", s);
    }

    /// Arbitrary bytes never panic — Err is fine, panic is not.
    #[test]
    fn proptest_arbitrary_bytes_no_panic(s in ".{0,2048}") {
        harness::roundtrip_property::<SqlParser>(&s);
    }

    /// Tighter limits always refuse oversized inputs (structured Err).
    #[test]
    fn proptest_input_size_limit_enforced(
        len in 100usize..1000,
    ) {
        let limits = ParserLimits {
            max_input_bytes: 64,
            ..ParserLimits::default()
        };
        let input = "a".repeat(len);
        let r = SqlParser::parse_with_limits(&input, limits);
        prop_assert!(r.is_err(), "oversized input must error");
    }
}
