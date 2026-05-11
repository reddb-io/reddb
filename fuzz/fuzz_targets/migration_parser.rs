//! cargo-fuzz target for the migration DSL parser (issue #88).
//!
//! Mirrors `sql_parser.rs`. Feeds arbitrary bytes prefixed with a
//! migration keyword into `reddb_server::storage::query::parser::parse`
//! and asserts the parser never panics.
//!
//! The migration grammar lives behind the same top-level entry as
//! the SQL surface, so the prefix nudges libFuzzer toward inputs
//! that exercise the migration-specific entry points
//! (`parse_create_migration_body`, `parse_apply_migration`,
//! `parse_rollback_migration_after_keyword`,
//! `parse_explain_migration_after_keyword`).
//!
//! Run locally:
//!   cargo +nightly fuzz run migration_parser -- -max_total_time=10
//!
//! Run for the CI 5-minute window:
//!   cargo +nightly fuzz run migration_parser -- -max_total_time=300
//!
//! Reproduce a crash from a saved corpus entry:
//!   cargo +nightly fuzz run migration_parser fuzz/corpus/migration_parser/<id>

#![no_main]

use libfuzzer_sys::fuzz_target;
use reddb_server::storage::query::parser;

const PREFIXES: &[&str] = &[
    "CREATE MIGRATION ",
    "APPLY MIGRATION ",
    "ROLLBACK MIGRATION ",
    "EXPLAIN MIGRATION ",
];

fuzz_target!(|data: &[u8]| {
    // The parser only accepts UTF-8 strings. Anything else is
    // out-of-domain — return early.
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    // First byte (mod 4) selects which migration prefix to prepend;
    // the rest of the input is fed verbatim. This keeps libFuzzer
    // exploring the migration entry points instead of rediscovering
    // the SQL surface that `sql_parser.rs` already covers.
    let (prefix_byte, tail) = match s.as_bytes().split_first() {
        Some((b, rest)) => (*b, rest),
        None => return,
    };
    let prefix = PREFIXES[prefix_byte as usize % PREFIXES.len()];
    let Ok(tail_str) = std::str::from_utf8(tail) else {
        return;
    };
    let input = format!("{}{}", prefix, tail_str);

    // Single safety invariant: parsing must terminate with Ok or
    // Err, never panic. The DoS limits in `ParserLimits::default()`
    // bound recursion / input size / identifier length; anything
    // else surfaces as `ParseError`.
    let _ = parser::parse(&input);

    // Also feed the raw input — ensures the fuzzer can still reach
    // shapes where the migration keyword appears mid-string (e.g.
    // inside a CTE prelude or following a comment).
    let _ = parser::parse(s);
});
