//! cargo-fuzz target for the RedWire frame codec.
//!
//! `decode_frame` is the first thing an unauthenticated peer's bytes reach
//! on the wire listener: it parses a 16-byte header, then optionally
//! zstd-decompresses the payload. Both the length arithmetic and the
//! decompression are attacker-influenced, so a panic or an unbounded
//! allocation here is reachable pre-auth.
//!
//! Run locally:
//!   cargo +nightly fuzz run redwire_frame -- -max_total_time=60

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Ok or Err, never a panic — and never an allocation large enough to
    // abort the process, which the decompression cap enforces.
    let _ = reddb_wire::redwire::codec::decode_frame(data);
});
