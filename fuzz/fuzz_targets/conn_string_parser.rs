//! cargo-fuzz target for the connection-string parser (issue #90).
//!
//! Feeds arbitrary bytes into `reddb_wire::parse` and asserts the
//! parser never panics. `Err` is the expected outcome for the vast
//! majority of random inputs; the only observable failure is an
//! unwind panic.
//!
//! The conn-string parser is the only entry point an attacker can
//! reach BEFORE auth, so fuzzing it for panic-safety matters more
//! than anywhere else in the codebase.
//!
//! Run locally:
//!   cargo +nightly fuzz run conn_string_parser -- -max_total_time=10
//!
//! Run for the CI 5-minute window:
//!   cargo +nightly fuzz run conn_string_parser -- -max_total_time=300
//!
//! Reproduce a crash from a saved corpus entry:
//!   cargo +nightly fuzz run conn_string_parser fuzz/corpus/conn_string_parser/<id>

#![no_main]

use libfuzzer_sys::fuzz_target;
use reddb_wire::parse;

fuzz_target!(|data: &[u8]| {
    // The parser only accepts UTF-8 strings. Anything else is
    // out-of-domain — return early. (`from_utf8` is cheap; rejecting
    // here keeps the corpus focused on text that exercises the
    // grammar instead of bytes the URL crate would refuse trivially.)
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    // Single safety invariant: parsing must terminate with Ok or
    // Err, never panic. The DoS limits in `ConnStringLimits::default()`
    // bound input size, query-param count, and cluster-host count;
    // anything else surfaces as `ParseError`.
    let _ = parse(s);
});
