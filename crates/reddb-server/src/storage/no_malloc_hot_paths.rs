use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};

use super::cache::blob::{BlobCache, BlobCacheConfig, BlobCachePolicy, BlobCachePut, L1Admission};
use super::engine::page::{Page, PageType};
use super::engine::page_cache::PageCacheShard;
use super::query::ast::{Projection, QueryExpr, TableQuery};
use super::query::engine::binding::{Binding, Value as BindingValue, Var};
use super::query::executors::aggregation::write_group_key;
use super::query::executors::join::{
    extract_key as join_extract_key, key_hash as join_key_hash, key_matches as join_key_matches,
};
use super::query::planner::cache::{CachedPlan, PlanCache};
use super::query::planner::cost::PlanCost;
use super::query::planner::QueryPlan;
use super::unified::hash_index::HashIndex;
use super::unified::segment::{GrowingSegment, UnifiedSegment};
use super::unified::{EntityId, UnifiedEntity};
use super::wal::WalRecord;

thread_local! {
    static COUNTER: RefCell<AllocationCounter> = const { RefCell::new(AllocationCounter::new()) };
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        COUNTER.with(|counter| counter.borrow_mut().record_alloc());
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        COUNTER.with(|counter| counter.borrow_mut().record_dealloc());
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        COUNTER.with(|counter| counter.borrow_mut().record_alloc());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        COUNTER.with(|counter| counter.borrow_mut().record_alloc());
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AllocationCount {
    allocs: usize,
    deallocs: usize,
}

struct AllocationCounter {
    enabled: Cell<bool>,
    allocs: Cell<usize>,
    deallocs: Cell<usize>,
}

impl AllocationCounter {
    const fn new() -> Self {
        Self {
            enabled: Cell::new(false),
            allocs: Cell::new(0),
            deallocs: Cell::new(0),
        }
    }

    fn record_alloc(&mut self) {
        if self.enabled.get() {
            self.allocs.set(self.allocs.get() + 1);
        }
    }

    fn record_dealloc(&mut self) {
        if self.enabled.get() {
            self.deallocs.set(self.deallocs.get() + 1);
        }
    }

    fn begin(&self) {
        assert!(
            !self.enabled.replace(true),
            "allocation counter cannot be nested"
        );
        self.allocs.set(0);
        self.deallocs.set(0);
    }

    fn finish(&self) -> AllocationCount {
        let count = AllocationCount {
            allocs: self.allocs.get(),
            deallocs: self.deallocs.get(),
        };
        self.enabled.set(false);
        count
    }

    fn disable(&self) {
        self.enabled.set(false);
    }
}

struct CounterGuard;

impl Drop for CounterGuard {
    fn drop(&mut self) {
        COUNTER.with(|counter| counter.borrow().disable());
    }
}

fn measure_allocations<T>(operation: impl FnOnce() -> T) -> (T, AllocationCount) {
    COUNTER.with(|counter| counter.borrow().begin());
    let _guard = CounterGuard;
    let result = operation();
    let count = COUNTER.with(|counter| counter.borrow().finish());
    (result, count)
}

struct AllocationException {
    reason: &'static str,
    follow_up_issue: u64,
}

struct CoveredOperation {
    name: &'static str,
    allowed_allocs: usize,
    exception: Option<AllocationException>,
    measure: fn() -> AllocationCount,
}

const COVERED_OPERATIONS: &[CoveredOperation] = &[
    CoveredOperation {
        name: "hash-index-point-read-hit",
        allowed_allocs: 0,
        exception: None,
        measure: measure_hash_index_point_read_hit,
    },
    CoveredOperation {
        name: "growing-segment-flat-row-insert",
        allowed_allocs: 3,
        exception: Some(AllocationException {
            reason: "bulk_insert returns an allocated id vector and builds per-call flat insert bookkeeping",
            follow_up_issue: 1956,
        }),
        measure: measure_growing_segment_flat_row_insert,
    },
    CoveredOperation {
        name: "page-cache-hit",
        allowed_allocs: 0,
        exception: None,
        measure: measure_page_cache_hit,
    },
    CoveredOperation {
        name: "blob-cache-l1-hit",
        allowed_allocs: 0,
        exception: None,
        measure: measure_blob_cache_l1_hit,
    },
    CoveredOperation {
        name: "blob-cache-promote-on-second-hit-l1-hit",
        allowed_allocs: 0,
        exception: None,
        measure: measure_blob_cache_promote_on_second_hit_l1_hit,
    },
    CoveredOperation {
        name: "wal-record-encode-into-group-commit-buffer",
        allowed_allocs: 0,
        exception: None,
        measure: measure_wal_record_encode_into_group_commit_buffer,
    },
    // Ratchet additions from the #2011 structural hot-path sweep. Both paths
    // are allocation-free in steady state; see docs/perf for the baseline and
    // the `structural_hot_path_report` micro-measurement for the wider set.
    CoveredOperation {
        name: "group-by-row-into-existing-group-key-buffer",
        allowed_allocs: 0,
        exception: None,
        measure: measure_group_by_existing_group_key_write,
    },
    CoveredOperation {
        name: "plan-cache-hit",
        allowed_allocs: 0,
        exception: None,
        measure: measure_plan_cache_hit,
    },
    // Ratchet additions from the #2013 follow-up sweep. `encode_into` now
    // encodes straight out of the borrowed record, so a page payload is no
    // longer cloned into an owned file frame on every append.
    CoveredOperation {
        name: "wal-pagewrite-encode",
        allowed_allocs: 0,
        exception: None,
        measure: measure_wal_pagewrite_encode,
    },
    CoveredOperation {
        name: "wal-pagewrite-encode-compressed",
        allowed_allocs: 1,
        exception: Some(AllocationException {
            reason: "zstd::bulk::compress returns an owned buffer for pages at or above the compression threshold; the payload itself is no longer cloned",
            follow_up_issue: 2013,
        }),
        measure: measure_wal_pagewrite_encode_compressed,
    },
    CoveredOperation {
        name: "wal-tx-commit-batch-encode",
        allowed_allocs: 0,
        exception: None,
        measure: measure_wal_tx_commit_batch_encode,
    },
    CoveredOperation {
        name: "hash-join-probe-key-lookup",
        allowed_allocs: 0,
        exception: None,
        measure: measure_hash_join_probe_key_lookup,
    },
];

#[test]
fn counting_allocator_measures_a_known_allocating_fixture() {
    let ((), count) = measure_allocations(|| {
        let value = Box::new(7_u64);
        assert_eq!(*value, 7);
    });

    assert_eq!(
        count.allocs, 1,
        "Box::new should register exactly one allocation"
    );
    assert_eq!(
        count.deallocs, 1,
        "dropping the Box inside the measured closure should register exactly one deallocation"
    );
}

#[test]
fn covered_storage_hot_paths_do_not_exceed_the_manifest_floor() {
    for op in COVERED_OPERATIONS {
        if op.allowed_allocs == 0 {
            assert!(
                op.exception.is_none(),
                "{} has a zero floor and must not carry an exception",
                op.name
            );
        } else {
            let exception = op
                .exception
                .as_ref()
                .unwrap_or_else(|| panic!("{} has a nonzero floor without an exception", op.name));
            assert!(
                !exception.reason.is_empty() && exception.follow_up_issue > 0,
                "{} has an incomplete allocation-floor exception",
                op.name
            );
        }
        let count = (op.measure)();
        assert_eq!(
            count.allocs, op.allowed_allocs,
            "{} allocated {} times; manifest floor is {}",
            op.name, count.allocs, op.allowed_allocs
        );
    }
}

fn measure_hash_index_point_read_hit() -> AllocationCount {
    let mut index = HashIndex::new(false);
    index.insert(b"id:42".to_vec(), EntityId::new(42)).unwrap();
    let key = b"id:42";

    let (ids, count) = measure_allocations(|| index.get(key));

    assert_eq!(ids, &[EntityId::new(42)]);
    count
}

fn measure_growing_segment_flat_row_insert() -> AllocationCount {
    let mut segment = GrowingSegment::new(1, "orders");
    segment
        .bulk_insert(vec![UnifiedEntity::table_row(
            EntityId::new(1),
            "orders",
            1,
            Vec::new(),
        )])
        .unwrap();
    let entity = UnifiedEntity::table_row(EntityId::new(2), "orders", 2, Vec::new());

    let (ids, count) = measure_allocations(|| segment.bulk_insert(vec![entity]));
    let ids = ids.unwrap();

    assert_eq!(ids, vec![EntityId::new(2)]);
    count
}

fn measure_page_cache_hit() -> AllocationCount {
    let cache = PageCacheShard::new(8);
    let page = Page::new(PageType::BTreeLeaf, 7);
    cache.insert(7, page);

    let (hit, count) = measure_allocations(|| cache.get(7));

    assert!(hit.is_some());
    count
}

fn measure_blob_cache_l1_hit() -> AllocationCount {
    let cache = BlobCache::with_defaults();
    cache
        .put("sessions", "abc", BlobCachePut::new(b"payload".to_vec()))
        .unwrap();

    let (hit, count) = measure_allocations(|| cache.get("sessions", "abc"));

    assert_eq!(hit.unwrap().value(), b"payload");
    count
}

fn measure_blob_cache_promote_on_second_hit_l1_hit() -> AllocationCount {
    let tmp = tempfile::tempdir().expect("blob cache l2 tempdir");
    let cache = BlobCache::open_with_l2(
        BlobCacheConfig::default()
            .with_l1_bytes_max(128)
            .with_shard_count(1)
            .with_max_namespaces(4)
            .with_l2_path(tmp.path().join("cache.rdb")),
    )
    .expect("blob cache l2");
    cache
        .put(
            "sessions",
            "abc",
            BlobCachePut::new(b"payload".to_vec())
                .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
        )
        .unwrap();
    assert!(cache.get("sessions", "abc").is_some());
    assert!(cache.get("sessions", "abc").is_some());

    let (hit, count) = measure_allocations(|| cache.get("sessions", "abc"));

    assert_eq!(hit.unwrap().value(), b"payload");
    count
}

fn measure_wal_record_encode_into_group_commit_buffer() -> AllocationCount {
    let record = WalRecord::Commit { tx_id: 9 };
    let mut group_commit_buffer = Vec::with_capacity(128);

    let ((), count) = measure_allocations(|| record.encode_into(&mut group_commit_buffer));

    assert!(!group_commit_buffer.is_empty());
    count
}

/// A `PageWrite` append below the compression threshold: the frame is written
/// literally, straight out of the record's own payload buffer. Since #2013 the
/// page bytes are borrowed rather than cloned into an owned file frame, so a
/// warmed group-commit buffer absorbs the whole record with zero allocations.
fn measure_wal_pagewrite_encode() -> AllocationCount {
    let record = WalRecord::PageWrite {
        tx_id: 1,
        page_id: 2,
        // Below MAIN_WAL_DEFAULT_COMPRESS_THRESHOLD (256) → literal frame.
        data: vec![7u8; 200],
    };
    let mut group_commit_buffer = Vec::with_capacity(1024);

    let ((), count) = measure_allocations(|| record.encode_into(&mut group_commit_buffer));

    assert!(!group_commit_buffer.is_empty());
    count
}

/// The same append for a page at/above the compression threshold. The payload
/// clone is gone, but `zstd::bulk::compress` still hands back an owned buffer —
/// that single allocation is the documented residual floor.
fn measure_wal_pagewrite_encode_compressed() -> AllocationCount {
    let record = WalRecord::PageWrite {
        tx_id: 1,
        page_id: 2,
        data: vec![7u8; 4096],
    };
    let mut group_commit_buffer = Vec::with_capacity(8192);

    let ((), count) = measure_allocations(|| record.encode_into(&mut group_commit_buffer));

    assert!(!group_commit_buffer.is_empty());
    count
}

/// A logical commit batch append. Its action payloads are borrowed too, so the
/// record encodes into a warmed buffer without allocating.
fn measure_wal_tx_commit_batch_encode() -> AllocationCount {
    let record = WalRecord::TxCommitBatch {
        tx_id: 9,
        actions: vec![b"insert:orders:1".to_vec(), b"update:orders:2".to_vec()],
    };
    let mut group_commit_buffer = Vec::with_capacity(512);

    let ((), count) = measure_allocations(|| record.encode_into(&mut group_commit_buffer));

    assert!(!group_commit_buffer.is_empty());
    count
}

/// The hash-join probe hot path (#2013): hash the probe row's join key in place
/// and confirm the bucket match element-wise. Neither step clones a `Value`.
fn measure_hash_join_probe_key_lookup() -> AllocationCount {
    let binding = two_var_binding();
    let keys = [Var::new("x"), Var::new("y")];
    // The build side owns its key; only the probe side must stay allocation-free.
    let build_key = join_extract_key(&binding, &keys);
    // One seeded state per join, created outside the measured probe (as in
    // `hash_join` itself, where it is per-call, not per-row).
    let hash_state = std::collections::hash_map::RandomState::new();

    let (hit, count) = measure_allocations(|| {
        let hash = join_key_hash(&hash_state, &binding, &keys);
        (hash, join_key_matches(&build_key, &binding, &keys))
    });

    assert!(
        hit.1,
        "probe key must match the build key it was derived from"
    );
    count
}

/// Two-variable binding used across the structural hot-path fixtures.
fn two_var_binding() -> Binding {
    Binding::two(
        Var::new("x"),
        BindingValue::String("alpha".to_string()),
        Var::new("y"),
        BindingValue::Integer(42),
    )
}

/// Measures the *real* `executors::aggregation::write_group_key`: a row that
/// lands in an *existing* group reuses one scratch `String`, so no allocation
/// happens once the buffer has grown. This measures a steady-state row (buffer
/// already sized, cleared before the call).
fn measure_group_by_existing_group_key_write() -> AllocationCount {
    let binding = two_var_binding();
    let group_vars = [Var::new("x"), Var::new("y")];
    let mut key = String::with_capacity(64);
    // Warm the buffer so its backing allocation already exists.
    write_group_key(&binding, &group_vars, &mut key);

    let ((), count) = measure_allocations(|| {
        key.clear();
        write_group_key(&binding, &group_vars, &mut key);
    });

    assert!(!key.is_empty());
    count
}

/// Minimal `QueryPlan` fixture (parameter-free) used only as cache ballast.
fn tiny_query_plan() -> QueryPlan {
    fn table() -> QueryExpr {
        QueryExpr::Table(TableQuery {
            table: "t".to_string(),
            source: None,
            alias: None,
            select_items: Vec::new(),
            columns: vec![Projection::All],
            where_expr: None,
            filter: None,
            group_by_exprs: Vec::new(),
            group_by: Vec::new(),
            having_expr: None,
            having: None,
            order_by: vec![],
            limit: None,
            limit_param: None,
            offset: None,
            offset_param: None,
            expand: None,
            as_of: None,
            sessionize: None,
            distinct: false,
        })
    }
    QueryPlan::new(table(), table(), PlanCost::default())
}

/// A warm plan-cache lookup that hits an existing, active entry. After the
/// #2011 LRU rework the promotion is a recency-counter bump, so the hit path
/// allocates nothing (previously it scanned + rebuilt a `Vec<String>`).
fn measure_plan_cache_hit() -> AllocationCount {
    let mut cache = PlanCache::new(8);
    cache.insert("q".to_string(), CachedPlan::new(tiny_query_plan()));
    // Warm the entry so it is live and not first-touch.
    assert!(cache.get("q").is_some());

    let (hit, count) = measure_allocations(|| cache.get("q").is_some());

    assert!(hit);
    count
}

/// The *retired* probe-key shape: the values behind the join keys were cloned
/// into an owned key vector on every probe row. Kept as the before-number the
/// borrowed lookup (#2013) is compared against in the report.
#[cfg(test)]
fn measure_hash_join_probe_key_extract() -> AllocationCount {
    let binding = two_var_binding();
    let keys = [Var::new("x"), Var::new("y")];

    let (key, count) = measure_allocations(|| {
        keys.iter()
            .map(|v| binding.get(v).cloned())
            .collect::<Vec<Option<BindingValue>>>()
    });

    assert_eq!(key.len(), 2);
    count
}

/// Measures `Binding::merge` for one joined row (item 1). With `Var` interned
/// behind `Arc<str>`, cloning the keys during the merge is a refcount bump
/// rather than a fresh string allocation.
#[cfg(test)]
fn measure_binding_merge_per_joined_row() -> AllocationCount {
    let left = Binding::one(Var::new("x"), BindingValue::String("alpha".to_string()));
    let right = Binding::one(Var::new("y"), BindingValue::Integer(7));

    let (merged, count) = measure_allocations(|| left.merge(&right));

    assert!(merged.is_some());
    count
}

/// Measures the columnar transpose (item 5) under both the old per-cell clone
/// and the new move-based lockstep drain, over heap-owning cells.
#[cfg(test)]
fn measure_columnar_transpose_variants() -> (AllocationCount, AllocationCount) {
    fn columns() -> Vec<Vec<String>> {
        let row_count = 8;
        let col_count = 4;
        (0..col_count)
            .map(|c| (0..row_count).map(|r| format!("cell-{c}-{r}")).collect())
            .collect()
    }
    let row_count = 8;

    let clone_columns = columns();
    let ((), clone_count) = measure_allocations(|| {
        let mut out: Vec<Vec<String>> = Vec::new();
        for r in 0..row_count {
            let mut row = Vec::with_capacity(clone_columns.len());
            for column in &clone_columns {
                row.push(column[r].clone());
            }
            out.push(row);
        }
        assert_eq!(out.len(), row_count);
    });

    let move_columns = columns();
    let ((), move_count) = measure_allocations(|| {
        let mut out: Vec<Vec<String>> = Vec::new();
        let base = out.len();
        for _ in 0..row_count {
            out.push(Vec::with_capacity(4));
        }
        for column in move_columns {
            for (r, value) in column.into_iter().take(row_count).enumerate() {
                out[base + r].push(value);
            }
        }
        assert_eq!(out.len(), row_count);
    });

    (clone_count, move_count)
}

/// Informational micro-measurement for the #2011 structural hot-path sweep.
///
/// Prints allocations/op and ns/op for each covered operation. It never asserts
/// on timing (which is machine-dependent) — the load-bearing invariants are the
/// zero-alloc entries in [`COVERED_OPERATIONS`]. Run with:
/// `cargo test -p reddb-io-server structural_hot_path_report -- --nocapture`.
#[test]
fn structural_hot_path_report() {
    use std::time::Instant;

    fn time_ns(iters: u32, mut op: impl FnMut()) -> f64 {
        let start = Instant::now();
        for _ in 0..iters {
            op();
        }
        start.elapsed().as_nanos() as f64 / iters as f64
    }

    let iters = 2_000u32;
    eprintln!("\n[structural hot-path baseline #2011]");
    eprintln!("operation,allocations_per_op,ns_per_op,note");

    let merge = measure_binding_merge_per_joined_row();
    let binding = two_var_binding();
    let group_vars = [Var::new("x"), Var::new("y")];
    let mut scratch = String::with_capacity(64);
    write_group_key(&binding, &group_vars, &mut scratch);
    eprintln!(
        "binding-merge-per-joined-row,{},{:.1},item-1 Var interned (Arc<str>)",
        merge.allocs,
        time_ns(iters, || {
            let left = Binding::one(Var::new("x"), BindingValue::Integer(1));
            let right = Binding::one(Var::new("y"), BindingValue::Integer(2));
            let _ = std::hint::black_box(left.merge(&right));
        })
    );

    let probe = measure_hash_join_probe_key_extract();
    eprintln!(
        "hash-join-probe-key-extract(old),{},{:.1},#2013 before: key cloned into an owned HashKey (clone only; the map then hashed it too)",
        probe.allocs,
        time_ns(iters, || {
            let _ = std::hint::black_box(
                group_vars
                    .iter()
                    .map(|v| binding.get(v).cloned())
                    .collect::<Vec<Option<BindingValue>>>(),
            );
        })
    );

    let probe_lookup = measure_hash_join_probe_key_lookup();
    let build_key = join_extract_key(&binding, &group_vars);
    let hash_state = std::collections::hash_map::RandomState::new();
    eprintln!(
        "hash-join-probe-key-lookup(new),{},{:.1},#2013 after: borrowed hash + element-wise match — includes the hashing the row above excludes (0-alloc, ratcheted)",
        probe_lookup.allocs,
        time_ns(iters, || {
            let hash = join_key_hash(&hash_state, &binding, &group_vars);
            let hit = join_key_matches(&build_key, &binding, &group_vars);
            std::hint::black_box((hash, hit));
        })
    );

    let gbk = measure_group_by_existing_group_key_write();
    eprintln!(
        "group-by-existing-group-key-write,{},{:.1},steady-state buffer reuse (0-alloc, ratcheted)",
        gbk.allocs,
        time_ns(iters, || {
            scratch.clear();
            write_group_key(&binding, &group_vars, &mut scratch);
            std::hint::black_box(&scratch);
        })
    );

    let plan_hit = measure_plan_cache_hit();
    let mut cache = PlanCache::new(8);
    cache.insert("q".to_string(), CachedPlan::new(tiny_query_plan()));
    let _ = cache.get("q");
    eprintln!(
        "plan-cache-hit,{},{:.1},item-4 recency-counter LRU (0-alloc, ratcheted)",
        plan_hit.allocs,
        time_ns(iters, || {
            let _ = std::hint::black_box(cache.get("q").is_some());
        })
    );

    let wal = measure_wal_record_encode_into_group_commit_buffer();
    eprintln!(
        "wal-commit-encode-into,{},{:.1},0-alloc, ratcheted",
        wal.allocs,
        time_ns(iters, || {
            let record = WalRecord::Commit { tx_id: 9 };
            let mut buf = Vec::with_capacity(128);
            record.encode_into(&mut buf);
            std::hint::black_box(&buf);
        })
    );

    // #2013: the PageWrite append path no longer clones its page payload. The
    // literal frame is fully allocation-free; the compressed frame keeps one
    // allocation for zstd's owned output buffer (documented in the manifest).
    let page_literal = measure_wal_pagewrite_encode();
    eprintln!(
        "wal-pagewrite-encode(literal),{},{:.1},#2013 after: payload borrowed (0-alloc, ratcheted)",
        page_literal.allocs,
        time_ns(iters, || {
            let record = WalRecord::PageWrite {
                tx_id: 1,
                page_id: 2,
                data: vec![7u8; 200],
            };
            let mut buf = Vec::with_capacity(1024);
            record.encode_into(&mut buf);
            std::hint::black_box(&buf);
        })
    );
    let page_compressed = measure_wal_pagewrite_encode_compressed();
    eprintln!(
        "wal-pagewrite-encode(compressed),{},-,#2013 after: 1 residual alloc = zstd output buffer",
        page_compressed.allocs
    );
    let commit_batch = measure_wal_tx_commit_batch_encode();
    eprintln!(
        "wal-tx-commit-batch-encode,{},-,#2013 after: actions borrowed (0-alloc, ratcheted)",
        commit_batch.allocs
    );

    let (clone_count, move_count) = measure_columnar_transpose_variants();
    eprintln!(
        "columnar-transpose-clone(old),{},-,item-5 before: per-cell clone",
        clone_count.allocs
    );
    eprintln!(
        "columnar-transpose-move(new),{},-,item-5 after: move via lockstep drain",
        move_count.allocs
    );
    assert!(
        move_count.allocs < clone_count.allocs,
        "columnar transpose move ({}) must allocate less than clone ({})",
        move_count.allocs,
        clone_count.allocs
    );
}
