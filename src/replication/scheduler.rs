//! Backup Scheduler — automatic periodic snapshots with optional remote upload.
//!
//! Runs as a background thread, configurable via `red_config`:
//! - `red.backup.enabled` — enable/disable
//! - `red.backup.interval_secs` — backup interval (default 3600 = 1 hour)
//! - `red.backup.retention_count` — snapshots to keep

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Result of a backup operation.
#[derive(Debug, Clone)]
pub struct BackupResult {
    /// Snapshot identifier
    pub snapshot_id: u64,
    /// Whether the snapshot was uploaded to remote backend
    pub uploaded: bool,
    /// Duration of the backup in milliseconds
    pub duration_ms: u64,
    /// When the backup was taken (unix ms)
    pub timestamp: u64,
}

/// Backup history and status.
#[derive(Debug, Clone)]
pub struct BackupStatus {
    /// Whether the scheduler is running
    pub running: bool,
    /// Interval in seconds between backups
    pub interval_secs: u64,
    /// Last backup result
    pub last_backup: Option<BackupResult>,
    /// Total backups completed since start
    pub total_backups: u64,
    /// Total backup failures since start
    pub total_failures: u64,
    /// Recent backup history
    pub history: Vec<BackupResult>,
}

/// Backup scheduler that runs periodic snapshots in a background thread.
pub struct BackupScheduler {
    running: Arc<AtomicBool>,
    interval_secs: Arc<RwLock<u64>>,
    last_backup: Arc<RwLock<Option<BackupResult>>>,
    total_backups: Arc<RwLock<u64>>,
    total_failures: Arc<RwLock<u64>>,
    history: Arc<RwLock<Vec<BackupResult>>>,
    max_history: usize,
}

impl BackupScheduler {
    /// Create a new scheduler (not yet started).
    pub fn new(interval_secs: u64) -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            interval_secs: Arc::new(RwLock::new(interval_secs)),
            last_backup: Arc::new(RwLock::new(None)),
            total_backups: Arc::new(RwLock::new(0)),
            total_failures: Arc::new(RwLock::new(0)),
            history: Arc::new(RwLock::new(Vec::new())),
            max_history: 50,
        }
    }

    /// Start the scheduler background thread.
    /// The `backup_fn` is called each interval to perform the actual backup.
    pub fn start<F>(&self, backup_fn: F)
    where
        F: Fn() -> Result<BackupResult, String> + Send + 'static,
    {
        if self.running.load(Ordering::SeqCst) {
            return; // Already running
        }
        self.running.store(true, Ordering::SeqCst);

        let running = Arc::clone(&self.running);
        let interval = Arc::clone(&self.interval_secs);
        let last_backup = Arc::clone(&self.last_backup);
        let total_backups = Arc::clone(&self.total_backups);
        let total_failures = Arc::clone(&self.total_failures);
        let history = Arc::clone(&self.history);
        let max_history = self.max_history;

        std::thread::Builder::new()
            .name("reddb-backup-scheduler".into())
            .spawn(move || {
                while running.load(Ordering::SeqCst) {
                    let secs = *interval.read().unwrap_or_else(|e| e.into_inner());
                    std::thread::sleep(Duration::from_secs(secs));

                    if !running.load(Ordering::SeqCst) {
                        break;
                    }

                    match backup_fn() {
                        Ok(result) => {
                            *last_backup.write().unwrap_or_else(|e| e.into_inner()) =
                                Some(result.clone());
                            *total_backups.write().unwrap_or_else(|e| e.into_inner()) += 1;
                            let mut hist = history.write().unwrap_or_else(|e| e.into_inner());
                            hist.push(result);
                            if hist.len() > max_history {
                                hist.remove(0);
                            }
                        }
                        Err(_) => {
                            *total_failures.write().unwrap_or_else(|e| e.into_inner()) += 1;
                        }
                    }
                }
            })
            .ok();
    }

    /// Stop the scheduler.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// Update the backup interval.
    pub fn set_interval(&self, secs: u64) {
        *self
            .interval_secs
            .write()
            .unwrap_or_else(|e| e.into_inner()) = secs;
    }

    /// Record a manual backup result.
    pub fn record_backup(&self, result: BackupResult) {
        *self.last_backup.write().unwrap_or_else(|e| e.into_inner()) = Some(result.clone());
        *self
            .total_backups
            .write()
            .unwrap_or_else(|e| e.into_inner()) += 1;
        let mut hist = self.history.write().unwrap_or_else(|e| e.into_inner());
        hist.push(result);
        if hist.len() > self.max_history {
            hist.remove(0);
        }
    }

    /// Get current status.
    pub fn status(&self) -> BackupStatus {
        BackupStatus {
            running: self.running.load(Ordering::SeqCst),
            interval_secs: *self.interval_secs.read().unwrap_or_else(|e| e.into_inner()),
            last_backup: self
                .last_backup
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            total_backups: *self.total_backups.read().unwrap_or_else(|e| e.into_inner()),
            total_failures: *self
                .total_failures
                .read()
                .unwrap_or_else(|e| e.into_inner()),
            history: self
                .history
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        }
    }

    /// Check if scheduler is running.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

impl Default for BackupScheduler {
    fn default() -> Self {
        Self::new(3600)
    }
}
