//! Startup I/O must follow live roots, not the database file's logical extent.

#![cfg(target_os = "linux")]

use crate::storage::engine::pager::measure_pager_read_bytes;
use crate::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;
use std::os::unix::fs::MetadataExt;
use std::time::Instant;

const LOGICAL_LEN_50_GIB: u64 = 50 * 1024 * 1024 * 1024;

fn options(path: &std::path::Path) -> RedDBOptions {
    RedDBOptions::persistent(path)
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true)
}

fn count_rows(runtime: &RedDBRuntime) -> String {
    runtime
        .execute_query("SELECT COUNT(*) AS n FROM startup_rows")
        .expect("count persisted rows")
        .result
        .records[0]
        .get("n")
        .expect("count column")
        .display_string()
}

#[test]
fn disk_resident_startup_io_is_independent_of_sparse_logical_size() {
    let directory = tempfile::tempdir().expect("startup I/O fixture");
    let path = directory.path().join("startup.rdb");
    let options = options(&path);

    let seed = RedDBRuntime::with_options(options.clone()).expect("seed runtime");
    seed.execute_query("CREATE TABLE startup_rows (id INTEGER PRIMARY KEY, payload TEXT)")
        .expect("create startup table");
    for id in 0..64 {
        seed.execute_query_with_params(
            "INSERT INTO startup_rows (id, payload) VALUES ($1, $2)",
            &[Value::Integer(id), Value::text("payload".repeat(512))],
        )
        .expect("insert startup row");
    }
    seed.flush().expect("checkpoint startup fixture");
    drop(seed);

    // The first disk-resident reopen may publish missing derived roots. Keep
    // both measured opens on the same stable checkpoint generation.
    let prepared = RedDBRuntime::with_options(options.clone()).expect("prepare cold reopen");
    assert_eq!(count_rows(&prepared), "64");
    prepared.flush().expect("publish prepared roots");
    drop(prepared);

    let (small, small_read_bytes) = measure_pager_read_bytes(|| {
        let start = Instant::now();
        let runtime = RedDBRuntime::with_options(options.clone()).expect("open compact file");
        let ready = start.elapsed();
        assert_eq!(count_rows(&runtime), "64");
        eprintln!(
            "compact: ready={ready:?}, first_result={:?}",
            start.elapsed()
        );
        runtime
    });
    drop(small);

    let allocated_bytes_before = std::fs::metadata(&path)
        .expect("compact database metadata")
        .blocks()
        .checked_mul(512)
        .expect("allocated byte count fits u64");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open sparse fixture");
    file.set_len(LOGICAL_LEN_50_GIB)
        .expect("extend sparse logical length");
    drop(file);
    let sparse_metadata = std::fs::metadata(&path).expect("sparse database metadata");
    let allocated_bytes_after = sparse_metadata
        .blocks()
        .checked_mul(512)
        .expect("allocated byte count fits u64");
    assert_eq!(sparse_metadata.len(), LOGICAL_LEN_50_GIB);
    assert!(
        allocated_bytes_after <= allocated_bytes_before.saturating_add(4096),
        "50 GiB logical fixture allocated {} additional bytes",
        allocated_bytes_after.saturating_sub(allocated_bytes_before)
    );

    let (large, large_read_bytes) = measure_pager_read_bytes(|| {
        let start = Instant::now();
        let runtime = RedDBRuntime::with_options(options).expect("open 50 GiB logical file");
        let ready = start.elapsed();
        assert_eq!(count_rows(&runtime), "64");
        eprintln!(
            "sparse: ready={ready:?}, first_result={:?}",
            start.elapsed()
        );
        runtime
    });
    drop(large);
    let read_byte_margin = 8_u64.saturating_mul(
        u64::try_from(crate::storage::engine::PAGE_SIZE).expect("page size fits u64"),
    );
    assert!(
        large_read_bytes <= small_read_bytes.saturating_add(read_byte_margin),
        "startup and first-query data-file reads grew from {} to {} bytes",
        small_read_bytes,
        large_read_bytes
    );
}
