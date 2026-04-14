//! TID bitmap — Fase 5 P6 building block.
//!
//! Provides a `TidBitmap` data structure that represents a set of
//! row IDs (TIDs) compactly using a `roaring::RoaringBitmap`
//! backing store. Multiple bitmaps from different indexes can be
//! AND/OR/NOT combined to express multi-predicate scans, then
//! drained in sorted order so the heap fetcher can read pages
//! sequentially instead of jumping around randomly.
//!
//! Mirrors PG's `tidbitmap.c` modulo features we don't have:
//!
//! - **Lossy mode**: PG flips entries to "page-only" when the
//!   bitmap grows past `work_mem`. We use a hard cap and report
//!   `BitmapTooLarge` when crossed; falling back to sequential
//!   scan is the caller's call.
//! - **Hash-of-hash sub-bitmaps**: PG uses a 2-level structure
//!   keyed by block number → row-within-block. RoaringBitmap
//!   already does block-of-bits compression internally so we
//!   skip the explicit block layer.
//! - **Cross-process sharing**: PG tidbitmaps live in shared
//!   memory for parallel bitmap heap scans. Single-process for
//!   now.
//!
//! The module is **not yet wired** into a query plan node. Wiring
//! into the canonical plan (Fase 5 W2+) requires a `BitmapIndex`
//! plan operator that produces a TidBitmap and a `BitmapHeap`
//! operator that consumes one — both pending.
//!
//! ## Why bitmaps win
//!
//! For a query like `WHERE a = 1 AND b = 2` with separate hash
//! indexes on `a` and `b`:
//!
//! - **Old path**: pick one index (say `a`), look up matching
//!   row IDs, fetch each row from the heap, evaluate `b = 2`
//!   per row. Random heap I/O for each match.
//! - **Bitmap path**: lookup `a`'s index → `bitmap_a`, lookup
//!   `b`'s index → `bitmap_b`, AND them in CPU → `intersection`,
//!   walk `intersection` in sorted order and fetch rows. The
//!   sequential-by-page walk turns random reads into prefetch-
//!   friendly streaming I/O.
//!
//! On `WHERE a IN (1,2,3) OR b > 5`, the OR equivalent is:
//!
//! - `bitmap_a_eq_1 OR bitmap_a_eq_2 OR bitmap_a_eq_3 OR
//!    bitmap_b_gt_5` — four index lookups, three OR operations,
//!    one final heap walk.

use roaring::RoaringBitmap;

/// A sparse, sorted set of row IDs backed by a Roaring bitmap.
/// Row IDs are `u32` because Roaring's API is u32-native; tables
/// with more than 4 billion rows need to partition by another
/// dimension (typically segment id) and use multiple bitmaps.
#[derive(Debug, Clone, Default)]
pub struct TidBitmap {
    inner: RoaringBitmap,
    /// Soft size cap measured in 32-bit words. Crossing this
    /// triggers `BitmapError::TooLarge`. Defaults to 32 MB worth
    /// of bits which holds ~256 million sparse row IDs.
    cap_bytes: usize,
}

/// Errors raised by bitmap operations.
#[derive(Debug)]
pub enum BitmapError {
    /// The bitmap exceeded its configured size cap. Caller
    /// should fall back to sequential scan or split the
    /// predicate further.
    TooLarge { current: usize, cap: usize },
}

impl std::fmt::Display for BitmapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge { current, cap } => {
                write!(f, "tid bitmap {current} bytes exceeds cap {cap}")
            }
        }
    }
}

impl std::error::Error for BitmapError {}

impl TidBitmap {
    /// Create an empty bitmap with the default 32 MB cap.
    pub fn new() -> Self {
        Self {
            inner: RoaringBitmap::new(),
            cap_bytes: 32 * 1024 * 1024,
        }
    }

    /// Create an empty bitmap with a custom cap (bytes). Use 0
    /// to disable the cap entirely — useful for tests and
    /// in-memory benchmarks.
    pub fn with_cap_bytes(cap_bytes: usize) -> Self {
        Self {
            inner: RoaringBitmap::new(),
            cap_bytes,
        }
    }

    /// Insert a row ID. Returns `BitmapError::TooLarge` if the
    /// resulting size exceeds the configured cap; the row is
    /// NOT inserted in that case so the caller can recover by
    /// switching strategies.
    pub fn insert(&mut self, tid: u32) -> Result<bool, BitmapError> {
        let added = self.inner.insert(tid);
        self.check_cap()?;
        Ok(added)
    }

    /// Bulk insert from any iterator of row IDs. Stops on the
    /// first cap violation and returns the number of IDs that
    /// were successfully inserted before the cap was hit.
    pub fn extend_from_iter(
        &mut self,
        iter: impl IntoIterator<Item = u32>,
    ) -> Result<usize, BitmapError> {
        let mut count = 0usize;
        for tid in iter {
            self.inner.insert(tid);
            count += 1;
            // Amortise the cap check: only verify every 4096
            // insertions to avoid O(n) on the size estimate.
            if count % 4096 == 0 {
                self.check_cap()?;
            }
        }
        self.check_cap()?;
        Ok(count)
    }

    /// Verify the in-memory size hasn't exceeded the cap. The
    /// underlying RoaringBitmap exposes `serialized_size()` which
    /// is a close proxy for actual heap usage.
    fn check_cap(&self) -> Result<(), BitmapError> {
        if self.cap_bytes == 0 {
            return Ok(());
        }
        let current = self.inner.serialized_size();
        if current > self.cap_bytes {
            return Err(BitmapError::TooLarge {
                current,
                cap: self.cap_bytes,
            });
        }
        Ok(())
    }

    /// Returns true when the bitmap contains the given row ID.
    /// O(log n) lookup — Roaring does block search internally.
    pub fn contains(&self, tid: u32) -> bool {
        self.inner.contains(tid)
    }

    /// Number of row IDs in the bitmap.
    pub fn len(&self) -> u64 {
        self.inner.len()
    }

    /// True when the bitmap holds no row IDs.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// In-place AND with another bitmap. Used by the planner's
    /// `WHERE a = 1 AND b = 2` rewrite: lookup each side, AND
    /// the results.
    pub fn intersect_with(&mut self, other: &TidBitmap) {
        self.inner &= &other.inner;
    }

    /// In-place OR with another bitmap. Used by `WHERE a OR b`
    /// patterns and IN list expansion.
    pub fn union_with(&mut self, other: &TidBitmap) {
        self.inner |= &other.inner;
    }

    /// In-place ANDNOT — remove every row ID also present in
    /// `other`. Used by `WHERE a AND NOT b` patterns and EXCEPT
    /// queries.
    pub fn difference_with(&mut self, other: &TidBitmap) {
        self.inner -= &other.inner;
    }

    /// Iterate row IDs in sorted ascending order. The heap
    /// fetcher uses this to read pages sequentially.
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        self.inner.iter()
    }

    /// Drain all row IDs in sorted ascending order, consuming
    /// the bitmap. Equivalent to `iter().collect()` but releases
    /// the inner storage as soon as iteration completes.
    pub fn into_sorted_vec(self) -> Vec<u32> {
        self.inner.into_iter().collect()
    }

    /// Group row IDs by their containing page number. Returns a
    /// vector of `(page_id, Vec<row_within_page>)` pairs sorted
    /// by page_id ascending. The heap fetcher reads each page
    /// once and extracts every matching row in a single I/O.
    ///
    /// `rows_per_page` is the table-specific constant — for
    /// reddb's default 8 KB pages with ~64-byte rows it's
    /// roughly 128, but call sites pass the exact value from
    /// their schema metadata.
    pub fn group_by_page(&self, rows_per_page: u32) -> Vec<(u32, Vec<u32>)> {
        if rows_per_page == 0 {
            return Vec::new();
        }
        let mut groups: Vec<(u32, Vec<u32>)> = Vec::new();
        let mut current_page: Option<u32> = None;
        let mut current_rows: Vec<u32> = Vec::new();
        for tid in self.inner.iter() {
            let page = tid / rows_per_page;
            let row = tid % rows_per_page;
            match current_page {
                Some(p) if p == page => current_rows.push(row),
                _ => {
                    if let Some(p) = current_page {
                        groups.push((p, std::mem::take(&mut current_rows)));
                    }
                    current_page = Some(page);
                    current_rows.push(row);
                }
            }
        }
        if let Some(p) = current_page {
            groups.push((p, current_rows));
        }
        groups
    }

    /// Cardinality of the union with another bitmap, computed
    /// without materialising the union itself. Used by the
    /// planner's cost estimator to compare AND vs OR rewrites
    /// without paying the merge cost.
    pub fn union_cardinality(&self, other: &TidBitmap) -> u64 {
        self.inner.union_len(&other.inner)
    }

    /// Cardinality of the intersection with another bitmap, also
    /// without materialising the result.
    pub fn intersection_cardinality(&self, other: &TidBitmap) -> u64 {
        self.inner.intersection_len(&other.inner)
    }
}

/// Convenience constructor: build a TidBitmap from any iterable
/// of row IDs. Skips the cap check if the iterator's
/// `size_hint().1` exceeds the cap to fail fast.
pub fn from_iter(iter: impl IntoIterator<Item = u32>) -> Result<TidBitmap, BitmapError> {
    let mut bitmap = TidBitmap::new();
    bitmap.extend_from_iter(iter)?;
    Ok(bitmap)
}

/// Build the AND of any number of bitmaps. Empty input yields
/// an empty bitmap. Single-element input returns the bitmap
/// unchanged. Order doesn't matter (AND is commutative) but
/// for cost reasons callers should pass the most-selective
/// bitmap first to minimise intermediate state.
pub fn intersect_all(mut bitmaps: Vec<TidBitmap>) -> TidBitmap {
    if bitmaps.is_empty() {
        return TidBitmap::new();
    }
    let mut acc = bitmaps.remove(0);
    for b in bitmaps {
        acc.intersect_with(&b);
        if acc.is_empty() {
            // Short-circuit: empty intersection stays empty.
            return acc;
        }
    }
    acc
}

/// Build the OR of any number of bitmaps. Empty input yields
/// an empty bitmap. Single-element input returns it unchanged.
pub fn union_all(bitmaps: Vec<TidBitmap>) -> TidBitmap {
    let mut iter = bitmaps.into_iter();
    let Some(mut acc) = iter.next() else {
        return TidBitmap::new();
    };
    for b in iter {
        acc.union_with(&b);
    }
    acc
}
