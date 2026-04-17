//! Background writer task — Post-MVP credibility item.
//!
//! Decouples dirty-page eviction from query execution. A
//! background tokio task scans the SIEVE buffer pool on a
//! cadence and writes dirty pages to disk via the pager,
//! freeing them for fast eviction by the next query that needs
//! a buffer.
//!
//! Mirrors PG's `bgwriter.c`:
//!
//! - **bgwriter_delay** — sleep between rounds (default 200 ms)
//! - **bgwriter_lru_maxpages** — max pages to flush per round
//! - **bgwriter_lru_multiplier** — adaptive scaling based on
//!   recent allocation rate
//!
//! ## Why
//!
//! Without a bgwriter, every query that needs a fresh buffer
//! pays the eviction cost: pick a dirty page, write it to
//! disk via the pager (which involves DWB + WAL flush
//! checkpoint), then return the cleaned slot. That write is
//! ~1-5 ms per page on spinning disk, ~100-300 µs on SSD.
//!
//! With a bgwriter, the writes happen on a separate task
//! during quiet moments. Query-side eviction finds clean
//! pages most of the time and just takes them.
//!
//! ## Wiring
//!
//! Phase post-MVP wiring spawns the bgwriter as a tokio task
//! during `Database::open`, parameterized by the config knobs
//! above. The task holds a `Weak<PageCache>` so it doesn't
//! prevent shutdown — when the cache is dropped, the next
//! `upgrade()` returns None and the task exits.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Configuration for the background writer task. All fields
/// have PG-equivalent defaults; production code can override
/// via `BgWriterConfig::with_*` builders.
#[derive(Debug, Clone, Copy)]
pub struct BgWriterConfig {
    /// Sleep between scan rounds.
    pub delay: Duration,
    /// Maximum dirty pages to flush per round. Soft limit —
    /// the writer stops at the first round that hits it.
    pub max_pages_per_round: usize,
    /// LRU adaptive multiplier. The writer estimates how
    /// many fresh buffers will be needed in the next round
    /// based on recent allocation rate, then flushes
    /// `multiplier × estimate`. Higher values flush more
    /// aggressively; lower values save I/O at the cost of
    /// query-side stall risk.
    pub lru_multiplier: f64,
    /// Soft cap on the dirty-page percentage. When the buffer
    /// pool's dirty fraction exceeds this, the writer scans
    /// every round regardless of `delay`.
    pub max_dirty_fraction: f64,
}

impl Default for BgWriterConfig {
    fn default() -> Self {
        Self {
            delay: Duration::from_millis(200),
            max_pages_per_round: 100,
            lru_multiplier: 2.0,
            max_dirty_fraction: 0.5,
        }
    }
}

/// Diagnostic counters published by the background writer for
/// monitoring / EXPLAIN ANALYZE-style introspection.
#[derive(Debug, Default)]
pub struct BgWriterStats {
    /// Total scan rounds executed since startup.
    pub rounds: AtomicU64,
    /// Total pages flushed since startup.
    pub pages_flushed: AtomicU64,
    /// Total times the writer exited a round early because
    /// it hit `max_pages_per_round`.
    pub max_round_hit: AtomicU64,
    /// Last reported dirty fraction (×1000 to keep it integer).
    pub last_dirty_fraction_milli: AtomicU64,
}

impl BgWriterStats {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Snapshot the counters as a plain struct for display.
    pub fn snapshot(&self) -> BgWriterStatsSnapshot {
        BgWriterStatsSnapshot {
            rounds: self.rounds.load(Ordering::Relaxed),
            pages_flushed: self.pages_flushed.load(Ordering::Relaxed),
            max_round_hit: self.max_round_hit.load(Ordering::Relaxed),
            last_dirty_fraction: self.last_dirty_fraction_milli.load(Ordering::Relaxed) as f64
                / 1000.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BgWriterStatsSnapshot {
    pub rounds: u64,
    pub pages_flushed: u64,
    pub max_round_hit: u64,
    pub last_dirty_fraction: f64,
}

/// Trait the buffer pool must implement so the writer can
/// scan and flush. Decoupled from the concrete `PageCache`
/// type so this module doesn't pull the SIEVE implementation
/// into its dep graph and so tests can plug a mock pool.
pub trait DirtyPageFlusher: Send + Sync {
    /// Estimated dirty fraction of the buffer pool, in [0, 1].
    fn dirty_fraction(&self) -> f64;
    /// Walk up to `max` dirty pages and flush them via the
    /// pager. Returns the number actually flushed.
    fn flush_some(&self, max: usize) -> usize;
}

/// Production `DirtyPageFlusher` wrapping the engine's `Pager`.
/// Holds the pager via `Weak` so dropping the database doesn't
/// keep the file alive via the background thread; the writer
/// exits at the next round when the upgrade fails.
pub struct PagerDirtyFlusher {
    pager: std::sync::Weak<crate::storage::engine::pager::Pager>,
}

impl PagerDirtyFlusher {
    pub fn new(pager: std::sync::Weak<crate::storage::engine::pager::Pager>) -> Self {
        Self { pager }
    }
}

impl DirtyPageFlusher for PagerDirtyFlusher {
    fn dirty_fraction(&self) -> f64 {
        match self.pager.upgrade() {
            Some(p) => p.dirty_fraction(),
            None => 0.0,
        }
    }

    fn flush_some(&self, max: usize) -> usize {
        let Some(p) = self.pager.upgrade() else {
            return 0;
        };
        match p.flush_some_dirty(max) {
            Ok(n) => n,
            Err(err) => {
                tracing::warn!(error = ?err, "bgwriter flush_some_dirty failed");
                0
            }
        }
    }
}

/// Shutdown handle returned by `spawn`. Drop the handle (or
/// call `stop()` explicitly) to signal the task to exit at
/// the start of its next round.
pub struct BgWriterHandle {
    stop: Arc<AtomicBool>,
    pub stats: Arc<BgWriterStats>,
}

impl BgWriterHandle {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
}

impl Drop for BgWriterHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Spawn the background writer as a std::thread that polls
/// the supplied flusher on a cadence. Returns a handle the
/// caller drops to stop the loop.
///
/// The implementation is intentionally tokio-free so it works
/// in the embedded-library use case where the user hasn't
/// brought up a runtime.
pub fn spawn(flusher: Arc<dyn DirtyPageFlusher>, config: BgWriterConfig) -> BgWriterHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let stats = BgWriterStats::new();
    let stop_clone = Arc::clone(&stop);
    let stats_clone = Arc::clone(&stats);

    std::thread::spawn(move || {
        loop {
            if stop_clone.load(Ordering::Acquire) {
                break;
            }
            // Estimate: based on allocation rate proxy
            // (lru_multiplier × max_pages_per_round / 4) plus
            // a forced sweep when dirty fraction is high.
            let dirty = flusher.dirty_fraction();
            stats_clone
                .last_dirty_fraction_milli
                .store((dirty * 1000.0) as u64, Ordering::Relaxed);

            let target_pages = if dirty > config.max_dirty_fraction {
                // Aggressive: flush the full budget every round.
                config.max_pages_per_round
            } else {
                // Adaptive: scale by lru_multiplier against a
                // baseline of max_pages / 4. Clamped to budget.
                ((config.max_pages_per_round as f64 / 4.0) * config.lru_multiplier) as usize
            };
            let target_pages = target_pages.min(config.max_pages_per_round);

            let flushed = flusher.flush_some(target_pages);
            stats_clone
                .pages_flushed
                .fetch_add(flushed as u64, Ordering::Relaxed);
            stats_clone.rounds.fetch_add(1, Ordering::Relaxed);
            if flushed >= config.max_pages_per_round {
                stats_clone.max_round_hit.fetch_add(1, Ordering::Relaxed);
            }

            std::thread::sleep(config.delay);
        }
    });

    BgWriterHandle { stop, stats }
}
