//! Bitmap heap scan — Fase 5 P6 consumer of `TidBitmap`.
//!
//! Implements the second half of the PG bitmap-index-scan
//! pipeline: given a `TidBitmap` produced by AND/OR-ing
//! per-index bitmaps, walk the target heap pages in sorted
//! order and fetch the rows corresponding to set bits.
//!
//! The win over a plain index scan is **sequential page
//! access**: bitmap entries are sorted by TID, so successive
//! fetches go to adjacent pages, giving the OS and buffer
//! pool a prefetch-friendly stream instead of random seeks.
//! For queries touching >1% of a large table the difference
//! is 5-20× on spinning disks and ~2-3× on SSDs.
//!
//! Mirrors PG's `nodeBitmapHeapscan.c` modulo features we
//! don't have:
//!
//! - **Lossy bitmap entries**: PG's tidbitmap can spill to
//!   page-level granularity when memory pressure mounts.
//!   `TidBitmap` doesn't, so the bitmap heap scan here
//!   always processes tuple-level entries.
//! - **Prefetch hints**: PG calls `PrefetchBuffer` for the
//!   next few pages while the current page is being
//!   processed. We rely on the OS readahead for now.
//! - **Parallel bitmap heap scans**: single-producer for now.
//!
//! This module is **not yet wired** into the canonical plan.
//! A `BitmapHeapScan` logical plan node and its executor
//! arm plug into `query_exec/table.rs` when the planner
//! learns to emit bitmap paths.

use crate::storage::index::tid_bitmap::TidBitmap;

/// Callback the bitmap scan uses to fetch a row by its TID.
/// The caller (typically the runtime executor) provides this
/// when invoking the scan because the row shape depends on
/// the target collection.
pub trait RowFetcher {
    type Row;
    type Error;
    /// Load the row at `(page, row_within_page)`. Returns
    /// `None` when the slot is empty (tombstone / deleted)
    /// so the scan can skip it without raising an error.
    fn fetch(
        &self,
        page: u32,
        row_within_page: u32,
    ) -> Result<Option<Self::Row>, Self::Error>;
}

/// Execute a bitmap heap scan over `bitmap`, invoking `fetcher`
/// for each surviving TID in sorted order. Returns the
/// materialised rows in the same TID order.
///
/// `rows_per_page` is the table's fixed row density — the
/// planner supplies this from schema metadata. For reddb's
/// default 8 KB pages with ~64-byte rows it's ~128.
///
/// Three-phase algorithm:
///
/// 1. **Group by page**: `bitmap.group_by_page(rows_per_page)`
///    returns `(page_id, Vec<row_within_page>)` sorted by
///    page. This turns the iteration into a sequential-read-
///    friendly pattern.
/// 2. **Fetch each page's rows**: for each page group, the
///    fetcher is called once per target row within that page.
///    The fetcher is responsible for caching the page's
///    buffer so repeated fetches within the same page don't
///    re-read the disk.
/// 3. **Materialise the output**: rows from all pages flow
///    into a single result Vec in their natural ascending
///    TID order.
pub fn execute_bitmap_scan<F: RowFetcher>(
    bitmap: &TidBitmap,
    fetcher: &F,
    rows_per_page: u32,
) -> Result<Vec<F::Row>, F::Error> {
    let groups = bitmap.group_by_page(rows_per_page);
    // Pre-allocate capacity for the expected output size —
    // bitmap::len() gives exact row count since the bitmap
    // is not lossy.
    let mut out: Vec<F::Row> = Vec::with_capacity(bitmap.len() as usize);
    for (page_id, rows_within) in groups {
        for row in rows_within {
            if let Some(row) = fetcher.fetch(page_id, row)? {
                out.push(row);
            }
        }
    }
    Ok(out)
}

/// Summary statistics the bitmap scan returns alongside its
/// output. Useful for `EXPLAIN ANALYZE`-style diagnostics and
/// for the planner's feedback loop when adjusting cost
/// estimates based on actual selectivity.
#[derive(Debug, Clone, Default)]
pub struct BitmapScanStats {
    /// Total candidate TIDs the bitmap contained before
    /// fetching.
    pub candidate_tids: u64,
    /// Rows actually returned (candidates minus tombstones).
    pub rows_returned: u64,
    /// Distinct pages touched during the scan. A good proxy
    /// for physical I/O: n pages × buffer-pool-hit ratio.
    pub pages_touched: u64,
}

impl BitmapScanStats {
    /// Returns the fraction of candidates that survived the
    /// tombstone check. Values near 1.0 mean the bitmap is
    /// well-pruned; values near 0.0 mean the index was stale
    /// and VACUUM should run.
    pub fn survival_ratio(&self) -> f64 {
        if self.candidate_tids == 0 {
            return 0.0;
        }
        self.rows_returned as f64 / self.candidate_tids as f64
    }
}

/// Variant of `execute_bitmap_scan` that also fills a
/// `BitmapScanStats` struct alongside the row output. Used
/// by `EXPLAIN ANALYZE` paths and by the runtime's
/// cardinality feedback loop.
pub fn execute_bitmap_scan_with_stats<F: RowFetcher>(
    bitmap: &TidBitmap,
    fetcher: &F,
    rows_per_page: u32,
) -> Result<(Vec<F::Row>, BitmapScanStats), F::Error> {
    let groups = bitmap.group_by_page(rows_per_page);
    let mut stats = BitmapScanStats {
        candidate_tids: bitmap.len(),
        rows_returned: 0,
        pages_touched: groups.len() as u64,
    };
    let mut out: Vec<F::Row> = Vec::with_capacity(bitmap.len() as usize);
    for (page_id, rows_within) in groups {
        for row in rows_within {
            if let Some(row) = fetcher.fetch(page_id, row)? {
                out.push(row);
                stats.rows_returned += 1;
            }
        }
    }
    Ok((out, stats))
}

/// Phase 3.6 wiring entry point. Combines a list of per-index
/// bitmaps via the requested boolean op and runs a single heap
/// fetch over the result. The planner uses this when WHERE has
/// multi-index AND/OR predicates.
///
/// `combine` controls the merge: `BitmapCombine::And` produces
/// the intersection (rows matching every index), `Or` produces
/// the union (rows matching any index).
///
/// Returns the scan rows in TID-sorted order. Caller can wrap
/// in `execute_bitmap_scan_with_stats` instead if it wants the
/// diagnostic counters.
pub fn execute_combined_bitmap_scan<F: RowFetcher>(
    bitmaps: Vec<TidBitmap>,
    combine: BitmapCombine,
    fetcher: &F,
    rows_per_page: u32,
) -> Result<Vec<F::Row>, F::Error> {
    let merged = match combine {
        BitmapCombine::And => crate::storage::index::tid_bitmap::intersect_all(bitmaps),
        BitmapCombine::Or => crate::storage::index::tid_bitmap::union_all(bitmaps),
    };
    execute_bitmap_scan(&merged, fetcher, rows_per_page)
}

/// Boolean combinator passed into `execute_combined_bitmap_scan`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitmapCombine {
    And,
    Or,
}
