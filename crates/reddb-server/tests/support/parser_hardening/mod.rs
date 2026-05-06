//! Reusable parser-hardening test harness (issue #87).
//!
//! See `README.md` in this directory for the consumer story.
//!
//! The harness is intentionally generic: it talks to a parser via
//! a trait so subsequent slices (#88, #89, #90) plug in their own
//! parsers without touching the harness internals.

#![allow(dead_code)]

use std::panic::{catch_unwind, AssertUnwindSafe};

use reddb_server::storage::query::parser::ParserLimits;

pub mod ask_grammar;
pub mod corpus;
pub mod geo_grammar;
pub mod migration_grammar;
pub mod secret_fixture_gen;
pub mod secret_redactor;
pub mod sql_grammar;
pub mod timeseries_grammar;

/// Parser-agnostic interface every consumer of the harness
/// implements. The associated `Error` type lets callers preserve
/// rich error information for snapshot assertions while keeping
/// the harness generic.
pub trait HardenedParser {
    type Error: std::fmt::Debug + std::fmt::Display;

    /// Parse input under default DoS limits.
    fn parse(input: &str) -> Result<(), Self::Error>;

    /// Parse input under explicit DoS limits. Used by the
    /// limit-enforcement property tests.
    fn parse_with_limits(input: &str, limits: ParserLimits) -> Result<(), Self::Error>;
}

/// Test-only limits that keep recursion well below the test
/// thread's default 2 MiB stack. Production limits ship in
/// `ParserLimits::default()` and rely on a larger thread stack.
pub fn test_safe_limits() -> ParserLimits {
    ParserLimits {
        max_depth: 32,
        max_input_bytes: 1024 * 1024,
        max_identifier_chars: 256,
    }
}

/// Run `parse` on `input` and verify it does not panic. Either
/// `Ok` or `Err` is acceptable; only an `unwind_panic` is a
/// regression. This is the single safety invariant the fuzzer and
/// the property suite both lean on.
pub fn assert_no_panic_on<P: HardenedParser>(input: &str) {
    let result = catch_unwind(AssertUnwindSafe(|| {
        P::parse_with_limits(input, test_safe_limits())
    }));
    if let Err(panic) = result {
        let msg = panic
            .downcast_ref::<&'static str>()
            .map(|s| (*s).to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        panic!(
            "parser panicked on input ({} bytes): {}\n\nfirst 256 bytes: {:?}",
            input.len(),
            msg,
            &input.chars().take(256).collect::<String>(),
        );
    }
}

/// Round-trip property: parse(input) must not panic and must
/// terminate. Strict AST-equality round-trip would require a
/// `Display` impl on every AST node which the production code
/// does not yet ship; that is tracked separately. The parsing-
/// stable invariant captured here is sufficient to catch grammar
/// holes the table-driven tests miss.
pub fn roundtrip_property<P: HardenedParser>(input: &str) {
    assert_no_panic_on::<P>(input);
}

/// Run `parse` and confirm the parser refuses inputs that
/// exceed the supplied DoS limits. Returns the formatted error
/// for snapshot assertions.
pub fn parse_under_limits<P: HardenedParser>(
    input: &str,
    limits: ParserLimits,
) -> Result<(), String> {
    P::parse_with_limits(input, limits).map_err(|e| format!("{}", e))
}

/// Reusable snapshot helper. Wraps `insta::assert_snapshot!` with
/// a parser-agnostic shape so each consumer of the harness gets
/// uniform formatting.
#[macro_export]
macro_rules! snapshot_parse_error {
    ($parser:ty, $name:expr, $input:expr) => {{
        let input: &str = $input;
        let formatted =
            match <$parser as $crate::support::parser_hardening::HardenedParser>::parse(input) {
                Ok(()) => format!("UNEXPECTED OK\ninput: {:?}\n", input),
                Err(e) => format!("input: {:?}\nerror: {}\n", input, e),
            };
        insta::assert_snapshot!($name, formatted);
    }};
}

pub mod ask_grammar;
pub mod graph_dsl_grammar;
pub mod queue_grammar;

pub mod timeseries_grammar;

pub mod vector_search_grammar;

pub mod probabilistic_grammar;
pub mod subquery_grammar;
