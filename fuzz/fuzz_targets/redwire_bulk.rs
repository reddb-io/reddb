//! cargo-fuzz target for the RedWire bulk payload decoders.
//!
//! These run on an authenticated session but *before* the write privilege
//! check, and they read row and column counts straight off the wire. A
//! decoder that pre-allocates from an unchecked `u32` turns a 20-byte frame
//! into a multi-gigabyte allocation, which aborts the process rather than
//! erroring the connection — the bound they now carry is what this target
//! keeps honest.
//!
//! Run locally:
//!   cargo +nightly fuzz run redwire_bulk -- -max_total_time=60

#![no_main]

use libfuzzer_sys::fuzz_target;
use reddb_wire::redwire::bulk_binary::BulkBinaryFlavor;
use reddb_wire::redwire::{bulk_binary, bulk_json, bulk_stream};

fuzz_target!(|data: &[u8]| {
    for flavor in [BulkBinaryFlavor::Binary, BulkBinaryFlavor::Prevalidated] {
        let _ = bulk_binary::decode_bulk_binary_payload(data, flavor);
    }
    let _ = bulk_json::decode_bulk_json_payload(data);
    // The row decoder takes the column count from a previously decoded
    // start frame; exercise a few widths, including the degenerate zero.
    for column_count in [0usize, 1, 4, 64] {
        let _ = bulk_stream::decode_bulk_stream_rows_payload(data, column_count);
    }
    let _ = bulk_stream::decode_bulk_stream_start_payload(data);
});
