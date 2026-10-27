//! A paged database must read a small working set without restoring every cold payload.
use reddb_server::storage::memory_pools::MemoryPool;
use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};
use reddb_types::Value;
use std::{path::Path, time::Instant};

const READER_BUDGET_BYTES: u64 = 2 * 1024 * 1024;
const ROWS: usize = 2048;
const PAYLOAD_BYTES: usize = 4096;

fn body(id: usize) -> String {
    let mut state = id as u64 + 1;
    (0..PAYLOAD_BYTES)
        .map(|_| {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            char::from(b'a' + ((state >> 24) % 26) as u8)
        })
        .collect()
}

fn options(path: &Path, budget_bytes: u64) -> RedDBOptions {
    RedDBOptions::persistent(path)
        .with_memory_budget(budget_bytes)
        .with_storage_profile(StorageDeployPreset::Serverless.selection())
        .expect("native paged profile")
}

fn seed(path: &Path, rows: usize) {
    let writer = RedDBRuntime::with_options(options(path, 128 * 1024 * 1024))
        .expect("fixture writer fits its explicit budget");
    writer
        .execute_query(
            "CREATE TABLE cold_rows (id INTEGER PRIMARY KEY,body TEXT) WITH context_index = false",
        )
        .expect("fixture table");
    for first in (0..rows).step_by(32) {
        let values = (first..first + 32)
            .map(|id| format!("({id},'{}')", body(id)))
            .collect::<Vec<_>>()
            .join(",");
        writer
            .execute_query(&format!("INSERT INTO cold_rows (id,body) VALUES {values}"))
            .expect("fixture batch");
    }
    writer.db().flush_local_only().expect("durable fixture");
}

fn assert_working_set(reader: &RedDBRuntime, count: usize) {
    for id in [0, count / 2, count - 1] {
        let rows = reader
            .execute_query(&format!("SELECT body FROM cold_rows WHERE id={id}"))
            .expect("bounded indexed read")
            .result
            .records;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("body"), Some(&Value::text(body(id))));
    }
    let rows = reader
        .execute_query("SELECT COUNT(*) AS n FROM cold_rows")
        .expect("complete table remains visible")
        .result
        .records;
    assert_eq!(
        rows[0].get("n").expect("count").to_string(),
        count.to_string()
    );
}

#[test]
fn paged_open_keeps_cold_payloads_within_the_reader_budget() {
    let directory = tempfile::tempdir().expect("isolated fixture directory");
    let empty_path = directory.path().join("empty.rdb");
    let empty = RedDBRuntime::with_options(options(&empty_path, READER_BUDGET_BYTES))
        .expect("the same reader budget admits an empty paged runtime");
    assert!(empty.db().store().pager().is_some());
    empty.refresh_memory_accounting();
    let empty_bytes = empty.memory_accounting().total_used_bytes();
    assert!(empty_bytes < READER_BUDGET_BYTES);
    drop(empty);

    assert!(ROWS as u64 * PAYLOAD_BYTES as u64 > READER_BUDGET_BYTES * 3);
    for row_count in [32, ROWS] {
        let path = directory.path().join(format!("cold_{row_count}.rdb"));
        seed(&path, row_count);
        for opening in 1..=2 {
            let start = Instant::now();
            let reader = RedDBRuntime::with_options(options(&path, READER_BUDGET_BYTES))
                .expect("a valid paged database remains readable with a small working set");
            assert!(reader.db().store().pager().is_some());
            reader.refresh_memory_accounting();
            let accounting = reader.memory_accounting();
            let used_bytes = accounting.total_used_bytes();
            eprintln!(
            "opening={opening} rows={row_count} payload_bytes={} budget_bytes={READER_BUDGET_BYTES} \
             empty_bytes={empty_bytes} used_before_query_bytes={used_bytes} \
             segment_bytes={} page_cache_bytes={} index_bytes={} open_ms={}",
            row_count * PAYLOAD_BYTES,
            accounting.used_bytes(MemoryPool::SegmentArena),
            accounting.used_bytes(MemoryPool::PageCache),
            accounting.used_bytes(MemoryPool::IndexMemory),
            start.elapsed().as_millis(),
        );
            assert!(
            used_bytes <= READER_BUDGET_BYTES,
            "paged open restored cold data before any query: {used_bytes} > {READER_BUDGET_BYTES}"
        );

            assert_working_set(&reader, row_count);
            reader.refresh_memory_accounting();
            assert!(reader.memory_accounting().total_used_bytes() <= READER_BUDGET_BYTES);
        }
    }
}
