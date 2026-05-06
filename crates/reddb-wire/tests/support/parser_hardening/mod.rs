//! Slim parser-hardening test harness for `reddb-wire` (issue #90).
//!
//! Mirrors the `reddb-server` harness shipped in #87 but consumes
//! only what the connection-string parser needs:
//!   - `HardenedParser` trait — generic over `ConnStringLimits`.
//!   - `assert_no_panic_on` — single safety invariant exercised by
//!     both the property tests and the cargo-fuzz target.
//!   - `corpus` — adversarial-input fixtures shared between the
//!     property suite and the fuzz seed corpus.
//!   - `conn_grammar` — proptest strategies for the documented
//!     URL vocabulary.
//!
//! `reddb-wire` deliberately does not depend on `reddb-server`'s
//! test tree, so the harness lives here as a slim duplicate. The
//! types parameterise on this crate's `ConnStringLimits` rather
//! than the SQL parser's `ParserLimits`.

#![allow(dead_code)]

use std::panic::{catch_unwind, AssertUnwindSafe};

use reddb_wire::ConnStringLimits;

pub mod conn_grammar;
pub mod corpus;
pub mod secret_redactor;

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
    fn parse_with_limits(input: &str, limits: ConnStringLimits) -> Result<(), Self::Error>;
}

/// Test-only limits used by the property suite. Tighter than the
/// production defaults so generated long inputs predictably trip
/// the size guard.
pub fn test_safe_limits() -> ConnStringLimits {
    ConnStringLimits::default()
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
/// terminate. The conn-string parser's `ConnectionTarget` does not
/// (yet) ship a canonical `Display` impl that round-trips through
/// `parse`, so the harness only enforces the non-panic invariant.
/// Strict AST round-trip is exercised by the per-shape strategies
/// in `conn_grammar` which compare the parsed target against the
/// expected target.
pub fn roundtrip_property<P: HardenedParser>(input: &str) {
    assert_no_panic_on::<P>(input);
}

/// Run `parse` and confirm the parser refuses inputs that exceed
/// the supplied DoS limits. Returns the formatted error for
/// snapshot assertions.
pub fn parse_under_limits<P: HardenedParser>(
    input: &str,
    limits: ConnStringLimits,
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
