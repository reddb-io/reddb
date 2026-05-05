//! Buffer access strategies for the page cache.
//!
//! Mirrors PostgreSQL's `BufferAccessStrategy` (src/backend/storage/buffer/freelist.c)
//! — a hint passed by callers to tell the cache that a particular access
//! pattern (sequential scan, bulk read, bulk write) should NOT pollute
//! the main hot pool. Strategy-tagged accesses go through a small
//! dedicated ring instead.
//!
//! The `Normal` strategy is the default and uses the main SIEVE pool
//! exactly as before. Sequential scans, full table scans, vector batch
//! scans, timeseries chunk iteration, and backup/export should pass
//! `SequentialScan` / `BulkRead` / `BulkWrite` to spare the main pool.
//!
//! See `src/storage/cache/README.md` § Invariants 4 for the rules.

/// How a caller intends to access the page cache.
///
/// `Normal` is the default — pages flow through the main SIEVE pool.
/// Other variants route through small dedicated rings sized to keep
/// scan workloads out of the main pool's working set.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum BufferAccessStrategy {
    /// Use the main SIEVE pool. Default for OLTP-style point access.
    #[default]
    Normal,
    /// Sequential scan over a known-large relation. 16-page ring.
    SequentialScan,
    /// Bulk read (vector batch, timeseries chunk iter, backup export).
    /// 32-page ring.
    BulkRead,
    /// Bulk write (initial load, restore). 32-page ring; dirty pages
    /// flushed through the pager on eviction.
    BulkWrite,
}

impl BufferAccessStrategy {
    /// Ring capacity for non-`Normal` strategies, or `None` when the
    /// caller should use the main pool directly.
    pub fn ring_size(self) -> Option<usize> {
        match self {
            Self::Normal => None,
            Self::SequentialScan => Some(16),
            Self::BulkRead | Self::BulkWrite => Some(32),
        }
    }

    /// True iff the strategy uses a ring buffer (i.e. is non-`Normal`).
    pub fn is_ring(self) -> bool {
        self.ring_size().is_some()
    }

    /// True iff the strategy expects writes that must flush dirty
    /// pages on eviction. Currently only `BulkWrite`.
    pub fn is_write(self) -> bool {
        matches!(self, Self::BulkWrite)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_normal() {
        assert_eq!(
            BufferAccessStrategy::default(),
            BufferAccessStrategy::Normal
        );
    }

    #[test]
    fn ring_size_matches_strategy() {
        assert_eq!(BufferAccessStrategy::Normal.ring_size(), None);
        assert_eq!(BufferAccessStrategy::SequentialScan.ring_size(), Some(16));
        assert_eq!(BufferAccessStrategy::BulkRead.ring_size(), Some(32));
        assert_eq!(BufferAccessStrategy::BulkWrite.ring_size(), Some(32));
    }

    #[test]
    fn is_ring_true_for_non_normal() {
        assert!(!BufferAccessStrategy::Normal.is_ring());
        assert!(BufferAccessStrategy::SequentialScan.is_ring());
        assert!(BufferAccessStrategy::BulkRead.is_ring());
        assert!(BufferAccessStrategy::BulkWrite.is_ring());
    }

    #[test]
    fn is_write_only_for_bulk_write() {
        assert!(!BufferAccessStrategy::Normal.is_write());
        assert!(!BufferAccessStrategy::SequentialScan.is_write());
        assert!(!BufferAccessStrategy::BulkRead.is_write());
        assert!(BufferAccessStrategy::BulkWrite.is_write());
    }
}
