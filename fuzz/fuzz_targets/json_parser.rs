//! cargo-fuzz target for the in-house JSON parser.
//!
//! `reddb_types::utils::json` parses HTTP request bodies on
//! *unauthenticated* endpoints (`/auth/login`, `/auth/bootstrap`), so a
//! panic here is a pre-auth crash. It is a hand-written recursive-descent
//! parser, which is exactly the shape that grows stack-overflow and
//! slice-index bugs; the nesting cap it now carries is one such bug that a
//! fuzzer would have found.
//!
//! Run locally:
//!   cargo +nightly fuzz run json_parser -- -max_total_time=60

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };
    // The only invariant: parsing terminates with Ok or Err, never a panic
    // and never a stack overflow.
    if let Ok(value) = reddb_types::utils::json::parse_json(s) {
        // Re-serialising a parsed document must also not panic; the writer
        // walks the same recursive structure the parser built.
        let _ = value.to_string();
    }
});
