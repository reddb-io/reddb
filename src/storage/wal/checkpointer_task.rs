//! Checkpointer task — Post-MVP credibility item.
//!
//! Pairs with `cache/bgwriter.rs` to give reddb the
//! Postgres-style three-tier I/O setup:
//!
//! 1. **bgwriter** — flushes dirty pages on a cadence so
//!    eviction is fast.
//! 2. **checkpointer** — periodically writes a checkpoint
//!    record to the WAL, fsyncs every dirty page, and
//!    advances the recovery floor (the LSN below which the
//!    WAL can be truncated).
//! 3. **walwriter** — flushes the WAL itself (already in
//!    `wal/group_commit.rs`).
//!
//! Mirrors PG's `checkpointer.c`. Each checkpoint:
//!
//! 1. Records start LSN.
//! 2. Walks every dirty page, calling pager flush.
//! 3. Records end LSN.
//! 4. Writes a `Checkpoint` WAL record with both LSNs.
//! 5. Truncates the WAL up to the previous checkpoint's
//!    redo pointer.
//!
//! Crash recovery starts from the most recent checkpoint
//! record's redo pointer instead of replaying the entire
//! WAL — bounded by checkpoint_timeout (default 5 min) so
//! recovery wall time is bounded too.
//!
//! ## Why
//!
//! Without checkpoints, the WAL grows unbounded and crash
//! recovery has to replay everything since database creation.
//! With checkpoints every N minutes / M MB of WAL written,
//! recovery is bounded to "WAL since last checkpoint".
//!
//! ## Wiring
//!
//! Phase post-MVP wiring spawns this task during
//! `Database::open` alongside the bgwriter. Stops when the
//! database is dropped.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Checkpoint cadence configuration.
#[derive(Debug, Clone, Copy)]
pub struct CheckpointConfig {
    /// Maximum time between checkpoints. PG default 5 min.
    pub timeout: Duration,
    /// Maximum WAL bytes between checkpoints. PG default 1 GB.
    pub max_wal_bytes: u64,
    /// Throttle: if a checkpoint runs faster than this, sleep
    /// before the next round to avoid I/O storms.
    pub min_completion_target_ratio: f64,
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(300),
            max_wal_bytes: 1024 * 1024 * 1024,
            min_completion_target_ratio: 0.9,
        }
    }
}

/// Checkpoint diagnostic counters.
#[derive(Debug, Default)]
pub struct CheckpointStats {
    pub checkpoints_completed: AtomicU64,
    pub pages_flushed_total: AtomicU64,
    pub wal_truncated_bytes: AtomicU64,
    pub last_checkpoint_lsn: AtomicU64,
    pub last_checkpoint_duration_ms: AtomicU64,
}

impl CheckpointStats {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn snapshot(&self) -> CheckpointStatsSnapshot {
        CheckpointStatsSnapshot {
            checkpoints_completed: self.checkpoints_completed.load(Ordering::Relaxed),
            pages_flushed_total: self.pages_flushed_total.load(Ordering::Relaxed),
            wal_truncated_bytes: self.wal_truncated_bytes.load(Ordering::Relaxed),
            last_checkpoint_lsn: self.last_checkpoint_lsn.load(Ordering::Relaxed),
            last_checkpoint_duration_ms: self.last_checkpoint_duration_ms.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CheckpointStatsSnapshot {
    pub checkpoints_completed: u64,
    pub pages_flushed_total: u64,
    pub wal_truncated_bytes: u64,
    pub last_checkpoint_lsn: u64,
    pub last_checkpoint_duration_ms: u64,
}

/// Trait the database must implement so the checkpointer can
/// drive the actual checkpoint without depending on the full
/// `Database` struct.
pub trait CheckpointDriver: Send + Sync {
    /// Current WAL byte position. Used to decide when WAL has
    /// grown enough to trigger a checkpoint.
    fn current_wal_bytes(&self) -> u64;

    /// Last completed checkpoint's WAL byte position.
    fn last_checkpoint_wal_bytes(&self) -> u64;

    /// Run a full checkpoint:
    /// 1. Walk every dirty page, flush via pager.
    /// 2. Write a Checkpoint WAL record.
    /// 3. Truncate WAL up to the previous checkpoint's redo.
    /// Returns (pages_flushed, new_redo_lsn, truncated_bytes).
    fn run_checkpoint(&self) -> CheckpointResult;
}

/// Result of a single checkpoint pass.
#[derive(Debug, Clone, Copy)]
pub struct CheckpointResult {
    pub pages_flushed: u64,
    pub new_redo_lsn: u64,
    pub wal_truncated_bytes: u64,
}

/// Shutdown handle returned by `spawn`.
pub struct CheckpointerHandle {
    stop: Arc<AtomicBool>,
    pub stats: Arc<CheckpointStats>,
}

impl CheckpointerHandle {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
}

impl Drop for CheckpointerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Spawn the checkpointer thread. Wakes on a 1-second cadence
/// to check both the timeout and the WAL byte threshold; runs
/// a checkpoint as soon as either trips.
pub fn spawn(driver: Arc<dyn CheckpointDriver>, config: CheckpointConfig) -> CheckpointerHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let stats = CheckpointStats::new();
    let stop_clone = Arc::clone(&stop);
    let stats_clone = Arc::clone(&stats);

    std::thread::spawn(move || {
        let mut last_checkpoint_at = Instant::now();
        loop {
            if stop_clone.load(Ordering::Acquire) {
                break;
            }

            let elapsed = last_checkpoint_at.elapsed();
            let wal_grown = driver.current_wal_bytes() - driver.last_checkpoint_wal_bytes();

            let trigger = elapsed >= config.timeout || wal_grown >= config.max_wal_bytes;
            if trigger {
                let start = Instant::now();
                let result = driver.run_checkpoint();
                let duration = start.elapsed();

                stats_clone
                    .checkpoints_completed
                    .fetch_add(1, Ordering::Relaxed);
                stats_clone
                    .pages_flushed_total
                    .fetch_add(result.pages_flushed, Ordering::Relaxed);
                stats_clone
                    .wal_truncated_bytes
                    .fetch_add(result.wal_truncated_bytes, Ordering::Relaxed);
                stats_clone
                    .last_checkpoint_lsn
                    .store(result.new_redo_lsn, Ordering::Relaxed);
                stats_clone
                    .last_checkpoint_duration_ms
                    .store(duration.as_millis() as u64, Ordering::Relaxed);

                last_checkpoint_at = Instant::now();
            }

            // Wake up every second to check.
            std::thread::sleep(Duration::from_secs(1));
        }
    });

    CheckpointerHandle { stop, stats }
}
