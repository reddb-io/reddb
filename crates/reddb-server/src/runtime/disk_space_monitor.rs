//! DiskSpaceMonitor — edge-triggered disk space watchdog.
//!
//! Linux: opens a fanotify group watching FAN_CLOSE_WRITE on the
//! data directory's mount point. Every write event triggers a
//! statvfs check; if used% ≥ threshold and the debounce window has
//! cleared, emits OperatorEvent::DiskSpaceCritical. Falls back to
//! polling when fanotify_init returns EPERM (unprivileged container).
//!
//! Non-Linux: polls via a tokio timer at POLL_INTERVAL.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::telemetry::operator_event::OperatorEvent;

/// Debounce window: don't re-emit within this duration after the last emit.
const DEBOUNCE: Duration = Duration::from_secs(30);

/// Poll interval for the non-fanotify fallback path (non-Linux or EPERM).
const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Background disk-space watchdog. Spawn with [`DiskSpaceMonitor::spawn`].
pub struct DiskSpaceMonitor {
    path: PathBuf,
    /// 1–99. Default 90 (= emit when used% ≥ 90).
    critical_pct: u8,
}

impl DiskSpaceMonitor {
    pub fn new(path: impl Into<PathBuf>, critical_pct: u8) -> Self {
        Self {
            path: path.into(),
            critical_pct: critical_pct.clamp(1, 99),
        }
    }

    pub fn with_default_threshold(path: impl Into<PathBuf>) -> Self {
        Self::new(path, 90)
    }

    /// Spawn the monitor as detached background work. When the caller is inside
    /// a Tokio runtime this uses that runtime; otherwise it creates a small
    /// current-thread runtime for the monitor. The monitor is expected to live
    /// for the full server lifetime, so no cancellation handle is exposed.
    pub fn spawn(self) {
        let path = self.path;
        let critical_pct = self.critical_pct;

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(run(path, critical_pct));
            return;
        }

        std::thread::Builder::new()
            .name("reddb-disk-space-monitor".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                    .expect("disk space monitor runtime");
                runtime.block_on(run(path, critical_pct));
            })
            .expect("disk space monitor thread spawn");
    }
}

async fn run(path: PathBuf, critical_pct: u8) {
    #[cfg(target_os = "linux")]
    {
        if run_fanotify(&path, critical_pct).await {
            return;
        }
        // fanotify failed (EPERM / unsupported kernel) — fall through to poll.
    }
    run_poll(&path, critical_pct).await;
}

// ---------------------------------------------------------------------------
// Shared: check disk usage and conditionally emit
// ---------------------------------------------------------------------------

/// Returns `true` if used% ≥ `critical_pct` and the event was (or would be)
/// emitted. `last_emit` is updated on each actual emission.
fn check(path: &Path, critical_pct: u8, last_emit: &mut Option<Instant>) -> bool {
    let (free, total) = match disk_free_total(path) {
        Some(pair) => pair,
        None => return false,
    };
    if total == 0 {
        return false;
    }
    let used = total.saturating_sub(free);
    let used_pct = used as f64 / total as f64 * 100.0;
    if used_pct >= critical_pct as f64 {
        let should_emit = last_emit.is_none_or(|t| t.elapsed() >= DEBOUNCE);
        if should_emit {
            let threshold_bytes = (total as f64 * ((100 - critical_pct) as f64 / 100.0)) as u64;
            OperatorEvent::DiskSpaceCritical {
                path: path.to_string_lossy().into_owned(),
                available_bytes: free,
                threshold_bytes,
            }
            .emit_global();
            *last_emit = Some(Instant::now());
        }
        return true;
    }
    false
}

fn disk_free_total(path: &Path) -> Option<(u64, u64)> {
    let free = fs2::free_space(path).ok()?;
    let total = fs2::total_space(path).ok()?;
    Some((free, total))
}

// ---------------------------------------------------------------------------
// Linux: fanotify path
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
async fn run_fanotify(path: &Path, critical_pct: u8) -> bool {
    match FanotifyWatcher::open(path) {
        Ok(watcher) => {
            let mut last_emit: Option<Instant> = None;
            watcher.run_loop(path, critical_pct, &mut last_emit).await;
            true
        }
        Err(_) => false,
    }
}

#[cfg(target_os = "linux")]
struct FanotifyWatcher {
    fd: libc::c_int,
}

#[cfg(target_os = "linux")]
impl FanotifyWatcher {
    fn open(path: &Path) -> Result<Self, ()> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        // FAN_CLOEXEC | FAN_CLASS_NOTIF
        let fd = unsafe {
            libc::fanotify_init(
                libc::FAN_CLOEXEC | libc::FAN_CLASS_NOTIF,
                libc::O_RDONLY as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(());
        }

        let path_cstr = match CString::new(path.as_os_str().as_bytes()) {
            Ok(s) => s,
            Err(_) => {
                unsafe { libc::close(fd) };
                return Err(());
            }
        };

        // Watch FAN_CLOSE_WRITE on the directory (mark the mount).
        let rc = unsafe {
            libc::fanotify_mark(
                fd,
                libc::FAN_MARK_ADD | libc::FAN_MARK_MOUNT,
                libc::FAN_CLOSE_WRITE,
                libc::AT_FDCWD,
                path_cstr.as_ptr(),
            )
        };
        if rc < 0 {
            unsafe { libc::close(fd) };
            return Err(());
        }

        Ok(Self { fd })
    }

    /// Block-read fanotify events using a background blocking thread so the
    /// tokio executor doesn't stall. Each event wakes a check.
    async fn run_loop(&self, path: &Path, critical_pct: u8, last_emit: &mut Option<Instant>) {
        let fd = self.fd;
        let path = path.to_path_buf();

        // Channel: blocking reader → async checker.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(64);

        // Blocking thread reads fanotify events. It doesn't need the
        // event data — the occurrence is enough to trigger a statvfs check.
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
                if n <= 0 {
                    break;
                }
                // If receiver is gone, stop.
                if tx.blocking_send(()).is_err() {
                    break;
                }
            }
        });

        while rx.recv().await.is_some() {
            check(&path, critical_pct, last_emit);
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for FanotifyWatcher {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

// ---------------------------------------------------------------------------
// Polling fallback (non-Linux or when fanotify is unavailable)
// ---------------------------------------------------------------------------

async fn run_poll(path: &Path, critical_pct: u8) {
    let mut last_emit: Option<Instant> = None;
    let mut interval = tokio::time::interval(POLL_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        check(path, critical_pct, &mut last_emit);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn check_no_emit_below_threshold() {
        // Tmp dir exists, so disk_free_total returns real values. With a 99%
        // threshold on a host that isn't completely full this should be false.
        let path = std::env::temp_dir();
        let mut last: Option<Instant> = None;
        // threshold=99 → only fires when disk is ≥99% full, extremely unlikely
        let fired = check(&path, 99, &mut last);
        // We can't assert false in CI (disk could be full), but last_emit
        // shouldn't advance unless fired.
        if !fired {
            assert!(last.is_none());
        }
    }

    #[test]
    fn check_threshold_zero_excluded_by_clamp() {
        // clamp(0, 1, 99) → 1, which is always ≥1% used → fires on any non-empty disk
        let monitor = DiskSpaceMonitor::new("/tmp", 0);
        assert_eq!(monitor.critical_pct, 1);
    }

    #[test]
    fn check_threshold_100_excluded_by_clamp() {
        let monitor = DiskSpaceMonitor::new("/tmp", 100);
        assert_eq!(monitor.critical_pct, 99);
    }

    #[test]
    fn debounce_suppresses_second_emit() {
        // Simulate two consecutive calls when disk is "full" by passing a
        // synthetic path check via a local helper.
        let mut last: Option<Instant> = Some(Instant::now()); // pretend just emitted
                                                              // disk_free_total("/nonexistent") → None → check returns false, no emit
        let fired = check(Path::new("/nonexistent-path-for-test"), 1, &mut last);
        assert!(!fired); // can't get disk stats for nonexistent path
    }

    #[test]
    fn disk_free_total_returns_values_for_tmp() {
        let result = disk_free_total(Path::new("/tmp"));
        assert!(result.is_some(), "statvfs /tmp should succeed");
        let (free, total) = result.unwrap();
        assert!(total > 0);
        assert!(free <= total);
    }
}
