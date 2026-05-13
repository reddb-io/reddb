#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = reddb_wire::query_with_params::decode_query_with_params(data);
});
