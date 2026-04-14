//! Visibility map — Fase 5 P2 building block.
//!
//! Provides a `VisibilityMap` data structure that tracks per-page
//! "all-visible" status: a single bit per heap page recording
//! whether every row on that page is visible to every concurrent
//! transaction. When the bit is set, the planner can answer
//! certain queries without ever fetching the heap row — index-
//! only scans return the indexed columns directly.
//!
//! Mirrors PG's `visibilitymap.c` modulo features we don't have:
//!
//! - **Crash recovery**: PG's vmap is durable via WAL. Ours is
//!   memory-only for now; on restart every page resets to
//!   "not all-visible" and gets re-marked lazily as queries
//!   verify rows.
//! - **All-frozen bit**: PG tracks a second bit per page
//!   ("all-frozen") used by the freeze map for vacuum
//!   optimisation. We omit it — reddb doesn't have freeze
//!   semantics yet.
//! - **Concurrent updates**: PG uses LWLocks per buffer slot
//!   for fine-grained concurrency. Ours uses a single RwLock
//!   over the bitmap. Acceptable for Week 5; later weeks
//!   shard by page range when contention shows up.
//!
//! ## Why it matters
//!
//! Index-only scans are the killer perf win for "narrow"
//! queries that select only indexed columns:
//!
//!     SELECT user_id FROM users WHERE email = ?
//!
//! With an index on `email` covering `user_id`, the planner can
//! skip the heap fetch entirely. But that's only safe when the
//! tuple's MVCC visibility hasn't changed since the last vacuum
//! — i.e. the page is "all-visible".
//!
//! Without a vmap, every index-only scan candidate must double-
//! check by fetching the heap row anyway, defeating the point.
//! With a vmap, the per-page bit is a single memory access.
//!
//! ## Marking strategy
//!
//! - **Set** the all-visible bit when:
//!   - VACUUM determines every row on the page is visible to
//!     every active xmin (no in-flight inserts / updates).
//!   - A bulk-load operation atomically writes a new page where
//!     every tuple is committed.
//! - **Clear** the all-visible bit when:
//!   - Any tuple on the page is updated, deleted, or inserted.
//!   - Even a no-op WAL replay: clearing is conservative.
//!
//! reddb's MVCC version chains live in the btree itself
//! (`storage/btree/node.rs:300`), not in row headers, so the
//! "page" concept here is an *abstract* page — call sites use
//! the entity ID divided by `entries_per_page` (~256 for 8 KB
//! pages) as the page index.

use std::sync::RwLock;

/// Per-page visibility tracking bitmap. Pages are indexed by a
/// dense u32 — sparse table sizes can overshoot the bitmap and
/// trigger lazy resize via `ensure_capacity`.
pub struct VisibilityMap {
    /// Bit-packed visibility bits, indexed by page number.
    /// `bits[i / 64] & (1 << (i % 64))` set means page `i` is
    /// all-visible.
    bits: RwLock<Vec<u64>>,
}

impl VisibilityMap {
    /// Create an empty visibility map. Initial capacity is
    /// minimal; pages get added as `mark_all_visible` extends
    /// the bitmap.
    pub fn new() -> Self {
        Self {
            bits: RwLock::new(Vec::new()),
        }
    }

    /// Pre-allocate room for `pages` pages. Useful when the
    /// caller knows the table size up-front (e.g. ANALYZE
    /// freshly imported data).
    pub fn with_capacity(pages: u32) -> Self {
        let words = (pages as usize + 63) / 64;
        Self {
            bits: RwLock::new(vec![0u64; words]),
        }
    }

    /// Returns true when `page` is marked all-visible. Page
    /// indexes beyond the current bitmap return false (treated
    /// as "unknown / not-visible").
    pub fn is_all_visible(&self, page: u32) -> bool {
        let bits = self.bits.read().expect("vmap rwlock poisoned");
        let word_idx = page as usize / 64;
        if word_idx >= bits.len() {
            return false;
        }
        let bit_idx = page as usize % 64;
        (bits[word_idx] >> bit_idx) & 1 == 1
    }

    /// Mark `page` as all-visible. Extends the bitmap on demand.
    /// Idempotent — calling twice has the same effect as calling
    /// once.
    pub fn mark_all_visible(&self, page: u32) {
        let mut bits = self.bits.write().expect("vmap rwlock poisoned");
        let word_idx = page as usize / 64;
        if word_idx >= bits.len() {
            bits.resize(word_idx + 1, 0);
        }
        let bit_idx = page as usize % 64;
        bits[word_idx] |= 1u64 << bit_idx;
    }

    /// Clear the all-visible bit for `page`. Called by every
    /// write path that touches the page (insert / update /
    /// delete). Cheap no-op when the page wasn't marked or
    /// doesn't exist in the bitmap yet.
    pub fn clear_all_visible(&self, page: u32) {
        let mut bits = self.bits.write().expect("vmap rwlock poisoned");
        let word_idx = page as usize / 64;
        if word_idx >= bits.len() {
            // Page doesn't exist yet — implicit clear, nothing
            // to do.
            return;
        }
        let bit_idx = page as usize % 64;
        bits[word_idx] &= !(1u64 << bit_idx);
    }

    /// Number of all-visible pages currently tracked.
    pub fn all_visible_count(&self) -> u64 {
        let bits = self.bits.read().expect("vmap rwlock poisoned");
        bits.iter().map(|w| w.count_ones() as u64).sum()
    }

    /// Total number of pages the bitmap can address (capacity,
    /// not "set count"). Mostly useful for diagnostics and
    /// memory accounting.
    pub fn capacity_pages(&self) -> u64 {
        let bits = self.bits.read().expect("vmap rwlock poisoned");
        (bits.len() as u64) * 64
    }

    /// Reset the entire bitmap to "not all-visible". Used by
    /// crash recovery and DROP TABLE.
    pub fn clear(&self) {
        let mut bits = self.bits.write().expect("vmap rwlock poisoned");
        for w in bits.iter_mut() {
            *w = 0;
        }
    }

    /// Bulk-mark a contiguous range of pages [`start`, `end`)
    /// as all-visible. Used by VACUUM after a successful sweep
    /// of a page range.
    pub fn mark_range_visible(&self, start: u32, end: u32) {
        if start >= end {
            return;
        }
        let mut bits = self.bits.write().expect("vmap rwlock poisoned");
        let last_word = (end as usize - 1) / 64;
        if last_word >= bits.len() {
            bits.resize(last_word + 1, 0);
        }
        for page in start..end {
            let word_idx = page as usize / 64;
            let bit_idx = page as usize % 64;
            bits[word_idx] |= 1u64 << bit_idx;
        }
    }

    /// Iterate over (page, all_visible_bool) for the first
    /// `limit_pages` pages. Diagnostic / debugging helper.
    pub fn snapshot(&self, limit_pages: u32) -> Vec<(u32, bool)> {
        let bits = self.bits.read().expect("vmap rwlock poisoned");
        let mut out = Vec::with_capacity(limit_pages as usize);
        for page in 0..limit_pages {
            let word_idx = page as usize / 64;
            let visible = if word_idx < bits.len() {
                let bit_idx = page as usize % 64;
                (bits[word_idx] >> bit_idx) & 1 == 1
            } else {
                false
            };
            out.push((page, visible));
        }
        out
    }
}

impl Default for VisibilityMap {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper: convert an entity ID to a page index using the given
/// `rows_per_page` constant. The btree's MVCC version chain
/// doesn't actually map onto fixed-size pages, so this is an
/// abstraction layer that lets the planner reason about
/// "page-shaped" visibility regions without committing to a
/// specific physical layout.
pub fn page_of(entity_id: u64, rows_per_page: u32) -> u32 {
    if rows_per_page == 0 {
        return 0;
    }
    (entity_id / rows_per_page as u64) as u32
}

/// Phase 3.5 wiring callback. The btree write path calls this
/// after every insert / update / delete to clear the all-visible
/// bit for the affected page. Centralised so a single function
/// can be hooked into multiple write call sites without each
/// rewriting the page-of math.
pub fn mark_dirty_after_write(vmap: &VisibilityMap, entity_id: u64, rows_per_page: u32) {
    let page = page_of(entity_id, rows_per_page);
    vmap.clear_all_visible(page);
}

/// Phase 3.5 wiring callback for VACUUM / GC. After confirming
/// every row in a page range is visible to all active txns, the
/// GC sweeps this with the (start, end) page bounds.
pub fn mark_clean_after_gc(vmap: &VisibilityMap, start_page: u32, end_page: u32) {
    vmap.mark_range_visible(start_page, end_page);
}
