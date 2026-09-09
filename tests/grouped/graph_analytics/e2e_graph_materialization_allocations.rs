use reddb::storage::UnifiedEntity;
use reddb::{RedDBRuntime, RuntimeQueryResult};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static TRACK_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
    static ALLOCATED_BYTES: Cell<usize> = const { Cell::new(0) };
    static LARGEST_ALLOCATION: Cell<usize> = const { Cell::new(0) };
}

struct QueryAllocator;

fn record_allocation(bytes: usize) {
    if TRACK_ALLOCATIONS.try_with(Cell::get).unwrap_or(false) {
        ALLOCATED_BYTES.with(|total| total.set(total.get() + bytes));
        LARGEST_ALLOCATION.with(|largest| largest.set(largest.get().max(bytes)));
    }
}

unsafe impl GlobalAlloc for QueryAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record_allocation(size);
        unsafe { System.realloc(ptr, layout, size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static QUERY_ALLOCATOR: QueryAllocator = QueryAllocator;

const GRAPH_QUERY: &str = "MATCH (a)-[:step]->(b) RETURN b.name AS name";
const GRAPH_CALL: &str = "CALL materialized_names()";

fn mixed_graph_fixture(sealed: bool, vector_count: usize) -> RedDBRuntime {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    for query in [
        "SET CONFIG runtime.result_cache.enabled = false",
        "INSERT INTO mixed_graph NODE (label, name) VALUES ('alice', 'Alice')",
        "INSERT INTO mixed_graph NODE (label, name) VALUES ('bob', 'Bob')",
        "INSERT INTO mixed_graph EDGE (label, from, to) VALUES ('step', 'alice', 'bob')",
    ] {
        runtime.execute_query(query).expect("graph fixture");
    }
    // Fixture installation uses real IDs in the same unified collection as the graph.
    // The measured operation is the public MATCH/CALL read, not vector ingestion.
    let store = runtime.db().store();
    for _ in 0..vector_count {
        let id = store.next_entity_id();
        store
            .insert(
                "mixed_graph",
                UnifiedEntity::vector(id, "mixed_graph", vec![1.0; 16_384]),
            )
            .expect("fixture vector");
    }
    let manager = store
        .get_collection("mixed_graph")
        .expect("mixed collection");
    assert_eq!(
        manager.stats().sealed_count,
        0,
        "fixture stays in one segment"
    );
    if sealed {
        manager.force_seal().expect("seal fixture");
        assert_eq!(manager.stats().sealed_count, 1);
    }
    runtime.execute_query(&format!(
        "CREATE FUNCTION materialized_names() RETURNS TABLE (name TEXT) EFFECT READ AS '{GRAPH_QUERY}'"
    )).expect("function fixture");
    runtime
}

fn assert_graph_result(result: &RuntimeQueryResult) {
    assert_eq!(result.result.records.len(), 1);
    assert_eq!(
        result.result.records[0]
            .get("name")
            .and_then(|value| value.as_text()),
        Some("Bob")
    );
}

fn measured_query(runtime: &RedDBRuntime, query: &str) -> (usize, usize) {
    struct AllocationScope;
    impl Drop for AllocationScope {
        fn drop(&mut self) {
            TRACK_ALLOCATIONS.with(|enabled| enabled.set(false));
        }
    }
    ALLOCATED_BYTES.with(|total| total.set(0));
    LARGEST_ALLOCATION.with(|largest| largest.set(0));
    TRACK_ALLOCATIONS.with(|enabled| enabled.set(true));
    let scope = AllocationScope;
    let result = runtime.execute_query(query);
    drop(scope);
    assert_graph_result(&result.expect("graph read"));
    (
        ALLOCATED_BYTES.with(Cell::get),
        LARGEST_ALLOCATION.with(Cell::get),
    )
}

#[test]
fn graph_reads_do_not_copy_vector_payloads_from_mixed_collections() {
    let mut measurements = Vec::new();
    // At most one sealed segment keeps the scan on the calling thread, so the
    // thread-local allocator observes the entire scan without counting other tests.
    for sealed in [false, true] {
        let runtime = mixed_graph_fixture(sealed, 64);
        for query in [GRAPH_QUERY, GRAPH_CALL] {
            assert_graph_result(&runtime.execute_query(query).expect("warm query"));
            let (allocated_bytes, largest) = measured_query(&runtime, query);
            println!(
                "sealed={sealed} query={query} allocated_bytes={allocated_bytes} largest={largest}"
            );
            measurements.push((allocated_bytes, largest));
        }
    }
    for (allocated_bytes, largest) in measurements {
        // Each unrelated vector occupies 64 KiB; copying all 64 twice costs 8 MiB.
        assert!(
            largest < 64 * 1024,
            "copied unrelated payload: {largest} bytes"
        );
        assert!(
            allocated_bytes < 512 * 1024,
            "graph read allocated {allocated_bytes} bytes"
        );
    }
}

#[test]
fn graph_budget_still_counts_non_graph_scan_candidates() {
    let runtime = mixed_graph_fixture(false, 32);
    runtime
        .execute_query("SET CONFIG functions.execution.work_max = 20")
        .expect("tight budget");
    let error = runtime
        .execute_query(GRAPH_CALL)
        .expect_err("scan must exhaust budget");
    assert!(
        error.to_string().contains("execution work_max exceeded"),
        "{error}"
    );
    assert_graph_result(
        &runtime
            .execute_query(GRAPH_QUERY)
            .expect("ordinary query after failure"),
    );
}

#[test]
fn graph_budget_skips_segments_without_graph_items() {
    for same_collection in [false, true] {
        for sealed in [false, true] {
            let runtime = mixed_graph_fixture(false, 0);
            runtime
                .execute_query("SET CONFIG functions.execution.work_max = 512")
                .expect("graph budget");
            assert_graph_result(
                &runtime
                    .execute_query(GRAPH_CALL)
                    .expect("graph-only control"),
            );
            let store = runtime.db().store();
            store
                .get_collection("mixed_graph")
                .expect("graph")
                .force_seal()
                .expect("seal graph");
            let collection = if same_collection {
                "mixed_graph"
            } else {
                "embeddings"
            };
            let vectors = (0..1024)
                .map(|_| UnifiedEntity::vector(store.next_entity_id(), collection, vec![1.0]))
                .collect();
            store
                .bulk_insert(collection, vectors)
                .expect("vector-only segment");
            if sealed {
                store
                    .get_collection(collection)
                    .expect("vectors")
                    .force_seal()
                    .expect("seal vectors");
            }
            assert_graph_result(
                &runtime
                    .execute_query(GRAPH_CALL)
                    .expect("irrelevant segments must not consume entity work"),
            );
            assert_graph_result(
                &runtime
                    .execute_query(GRAPH_QUERY)
                    .expect("ordinary graph read"),
            );
        }
    }
}

#[test]
fn graph_pruning_rebuilds_after_reopen_and_preserves_rollback() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("graph.rdb");
    for reopening in [false, true] {
        let runtime =
            RedDBRuntime::with_options(reddb::RedDBOptions::persistent(&path)).expect("open");
        runtime
            .execute_query("SET CONFIG runtime.result_cache.enabled = false")
            .expect("disable cache");
        if !reopening {
            for query in [
                "INSERT INTO mixed_graph NODE (label, name) VALUES ('alice', 'Alice')",
                "INSERT INTO mixed_graph NODE (label, name) VALUES ('bob', 'Bob')",
                "INSERT INTO mixed_graph EDGE (label, from, to) VALUES ('step', 'alice', 'bob')",
            ] {
                runtime.execute_query(query).expect("persistent graph");
            }
        }
        runtime
            .db()
            .store()
            .get_collection("mixed_graph")
            .expect("graph")
            .force_seal()
            .expect("seal graph");
        assert_graph_result(&runtime.execute_query(GRAPH_QUERY).expect("read graph"));
        runtime.execute_query("BEGIN").expect("begin");
        runtime
            .execute_query("UPDATE mixed_graph NODES SET name = 'Changed' WHERE name = 'Bob'")
            .expect("update node");
        let changed = runtime.execute_query(GRAPH_QUERY).expect("read own write");
        assert_eq!(changed.result.records.len(), 1);
        assert_eq!(
            changed.result.records[0].get("name"),
            Some(&reddb_types::Value::text("Changed"))
        );
        runtime.execute_query("ROLLBACK").expect("rollback");
        assert_graph_result(
            &runtime
                .execute_query(GRAPH_QUERY)
                .expect("rolled back graph"),
        );
    }
}
