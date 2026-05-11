//! cargo-fuzz target for the SQL parser (issue #87).
//!
//! Feeds arbitrary bytes into `reddb_server::storage::query::parser::parse`
//! and asserts the parser never panics. `Err` is the expected
//! outcome for the vast majority of random inputs; the only
//! observable failure is an unwind panic.
//!
//! Run locally:
//!   cargo +nightly fuzz run sql_parser -- -max_total_time=10
//!
//! Run for the CI 5-minute window:
//!   cargo +nightly fuzz run sql_parser -- -max_total_time=300
//!
//! Reproduce a crash from a saved corpus entry:
//!   cargo +nightly fuzz run sql_parser fuzz/corpus/sql_parser/<id>

#![no_main]

use libfuzzer_sys::fuzz_target;
use reddb_server::storage::query::parser;

fuzz_target!(|data: &[u8]| {
    // The parser only accepts UTF-8 strings. Anything else is
    // out-of-domain — return early. (`from_utf8` is cheap; rejecting
    // here keeps the corpus focused on text that exercises the
    // grammar instead of bytes the lexer would refuse trivially.)
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    // Single safety invariant: parsing must terminate with Ok or
    // Err, never panic. The DoS limits in `ParserLimits::default()`
    // bound recursion / input size / identifier length; anything
    // else surfaces as `ParseError`.
    let _ = parser::parse(s);
});
