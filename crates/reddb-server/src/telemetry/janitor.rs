//! Log-file retention janitor.
//!
//! `tracing-appender` rotates files daily but never deletes the old
//! ones — after a month of uptime you have 30+ files eating disk.
//! This module spawns a lightweight task that wakes every hour,
//! lists files matching `{prefix}.*` in the log dir, and removes
//! anything older than `keep_days`.
//!
//! Best-effort: filesystem errors are logged at debug level and the
//! janitor keeps going. Never panics — a broken janitor must not
//! take down the server.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// Spawn a tokio task that runs the cleanup loop in the background.
///
/// If no tokio runtime is active (e.g. embedded / one-shot CLI), we
/// silently do nothing — a disk-capped short-lived process doesn't
/// need retention.
pub fn spawn(dir: PathBuf, file_prefix: String, keep_days: u16) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    handle.spawn(async move {
        // One immediate sweep so the first rotation after a long
        // downtime cleans up before we accumulate another day.
        cleanup_once(&dir, &file_prefix, keep_days);
        let mut ticker = tokio::time::interval(Duration::from_secs(3600));
        // First tick fires immediately — consume and let the sleep
        // happen before the second tick.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            cleanup_once(&dir, &file_prefix, keep_days);
        }
    });
}

fn cleanup_once(dir: &std::path::Path, file_prefix: &str, keep_days: u16) {
    let cutoff =
        match SystemTime::now().checked_sub(Duration::from_secs(u64::from(keep_days) * 86_400)) {
            Some(t) => t,
            None => return,
        };

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) => {
            tracing::debug!(dir = %dir.display(), err = %err, "log janitor: read_dir failed");
            return;
        }
    };

    let mut removed = 0usize;
    let mut skipped = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        // Match `{prefix}` or `{prefix}.<date>` — daily rotation
        // names files as `reddb.log.2026-04-17`. Current-day file
        // has the bare prefix and must never be removed.
        if name == file_prefix || !name.starts_with(file_prefix) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            skipped += 1;
            continue;
        };
        let Ok(mtime) = meta.modified() else {
            skipped += 1;
            continue;
        };
        if mtime < cutoff {
            if std::fs::remove_file(&path).is_ok() {
                removed += 1;
            } else {
                skipped += 1;
            }
        }
    }

    if removed > 0 {
        tracing::info!(
            dir = %dir.display(),
            removed,
            skipped,
            keep_days,
            "log janitor: purged old rotated logs"
        );
    }
}
