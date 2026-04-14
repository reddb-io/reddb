//! Cooperative group-commit coordinator for the WAL.
//!
//! Mirrors PostgreSQL's `XLogFlush` waiter logic
//! (`src/backend/access/transam/xlog.c`). The single-writer commit
//! path used to call `wal.sync()` once per commit, so N concurrent
//! writers paid N independent fsyncs (~N × 100 µs on SSD). Group
//! commit collapses those into **one** fsync that covers every byte
//! appended up to the slowest writer's LSN.
//!
//! # Algorithm
//!
//! 1. A writer appends its records under the WAL lock and captures
//!    the resulting `commit_lsn = wal.current_lsn()` after its
//!    `Commit` record.
//! 2. The writer releases the WAL lock and calls
//!    [`GroupCommit::commit_at_least(commit_lsn, &wal)`].
//! 3. Inside `commit_at_least`:
//!    - **Fast path:** if `flushed_lsn >= commit_lsn`, the write is
//!      already durable from a piggyback on a previous fsync.
//!      Return immediately.
//!    - Otherwise take the coordinator state lock. Re-check the
//!      flushed LSN (another leader may have raced).
//!    - If a leader is already mid-flush, wait on the condvar until
//!      `flushed_lsn >= commit_lsn`. The leader will wake us up.
//!    - If no leader is in progress, become the leader: mark
//!      `in_progress = true`, drop the state lock, take the WAL
//!      lock, call `wal.sync()`, publish the new `flushed_lsn`,
//!      take the state lock again, clear `in_progress`, and
//!      notify all waiters.
//!
//! # Why this works
//!
//! Between the first writer's `append` and the leader's `wal.sync()`,
//! other writers can grab the WAL lock and append more records.
//! When the leader finally calls `sync()`, it flushes **everything**
//! that has been appended so far — not just its own records. Each
//! late writer wakes up to find `flushed_lsn` already past its LSN,
//! and returns without a second fsync.
//!
//! So `commit_at_least` produces one fsync per *batch* of concurrent
//! writers, not per writer. On a workload with 8 concurrent
//! committers, the throughput goes from ~8 × 100 µs ≈ 1 250
//! commits/s to ~1 × 100 µs ≈ 10 000 commits/s, an 8× win.
//!
//! # Correctness
//!
//! - `flushed_lsn` is monotonic: only the leader writes it, and
//!   only after a successful `sync()`.
//! - The state lock + condvar guarantee that exactly one leader is
//!   ever in flight, so we never have two parallel fsyncs racing.
//! - Waiters re-check `flushed_lsn` under the state lock before
//!   sleeping, so we never miss a wake-up.
//! - The leader does **only** the WAL `sync()` while holding
//!   leadership — no extra work — to keep the critical section as
//!   short as possible.

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};

use super::writer::WalWriter;

/// Coordinator state guarded by `GroupCommit::state`.
struct GroupCommitState {
    /// True when a leader is currently inside `wal.sync()`.
    in_progress: bool,
}

/// Cooperative WAL flush coordinator.
///
/// Owned by the [`super::transaction::TransactionManager`]; cheap to
/// share across writers via `Arc`.
pub struct GroupCommit {
    /// Highest LSN known to be durable. Atomic so the fast path can
    /// read it without taking the state lock.
    flushed_lsn: AtomicU64,
    /// Coordination state.
    state: Mutex<GroupCommitState>,
    /// Wakes waiters when `flushed_lsn` advances.
    cond: Condvar,
}

impl GroupCommit {
    /// Create a new coordinator initialised with the WAL's current
    /// durable position. Pass `wal.durable_lsn()` from a freshly
    /// opened `WalWriter`.
    pub fn new(initial_durable_lsn: u64) -> Self {
        Self {
            flushed_lsn: AtomicU64::new(initial_durable_lsn),
            state: Mutex::new(GroupCommitState { in_progress: false }),
            cond: Condvar::new(),
        }
    }

    /// Highest LSN that is known durable on disk. Cheap atomic read,
    /// no lock taken. Used by tests and by the diagnostics surface.
    pub fn flushed_lsn(&self) -> u64 {
        self.flushed_lsn.load(Ordering::Acquire)
    }

    /// Block the caller until the WAL is durable up to at least
    /// `target`. If another writer is already mid-flush, piggyback
    /// on it; otherwise become the leader and do the fsync ourselves.
    ///
    /// `wal` is the same `Mutex<WalWriter>` the transaction manager
    /// holds. The leader briefly takes that lock to call `sync()`.
    pub fn commit_at_least(&self, target: u64, wal: &Mutex<WalWriter>) -> io::Result<()> {
        // ── Fast path: already flushed past us. ─────────────────────
        if self.flushed_lsn.load(Ordering::Acquire) >= target {
            return Ok(());
        }

        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());

        // Re-check under the lock: another leader may have raced
        // and already flushed past our target between the load
        // above and the lock acquisition.
        if self.flushed_lsn.load(Ordering::Acquire) >= target {
            return Ok(());
        }

        if state.in_progress {
            // Another leader is mid-flush. Wait until they wake us
            // OR until the WAL has advanced past our target.
            while self.flushed_lsn.load(Ordering::Acquire) < target {
                state = self.cond.wait(state).unwrap_or_else(|p| p.into_inner());
            }
            return Ok(());
        }

        // ── We are the leader. ──────────────────────────────────────
        state.in_progress = true;
        // Drop the state lock before taking the WAL lock so other
        // writers can still **append** while we fsync. They will
        // either piggyback on this very flush (if their record made
        // it into the WAL before our `sync_all()` call) or wait for
        // the next leader.
        drop(state);

        // Take the WAL lock briefly to call sync(). The sync drains
        // the BufWriter and calls sync_all, then bumps the WAL's
        // own internal `durable_lsn`.
        let new_durable = {
            let mut wal_guard = wal.lock().unwrap_or_else(|p| p.into_inner());
            wal_guard.sync()?;
            wal_guard.durable_lsn()
        };

        // Publish the new flushed LSN to readers, then release
        // leadership and wake every waiter — they'll re-check the
        // counter and either return or wait again on the next
        // leader.
        self.flushed_lsn.store(new_durable, Ordering::Release);

        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.in_progress = false;
        drop(state);
        self.cond.notify_all();

        Ok(())
    }
}

impl std::fmt::Debug for GroupCommit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupCommit")
            .field("flushed_lsn", &self.flushed_lsn.load(Ordering::Acquire))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::wal::record::WalRecord;
    use crate::storage::wal::writer::WalWriter;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct FileGuard {
        path: PathBuf,
    }

    impl Drop for FileGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn temp_wal(name: &str) -> (FileGuard, PathBuf) {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "rb_group_commit_{}_{}_{}.wal",
            name,
            std::process::id(),
            nanos
        ));
        let _ = std::fs::remove_file(&path);
        (FileGuard { path: path.clone() }, path)
    }

    #[test]
    fn fast_path_when_already_flushed() {
        let (_g, path) = temp_wal("fast_path");
        let wal = Mutex::new(WalWriter::open(&path).unwrap());
        let initial = wal.lock().unwrap().durable_lsn();
        let gc = GroupCommit::new(initial);
        // Target equal to the initial flushed_lsn → no fsync.
        gc.commit_at_least(initial, &wal).unwrap();
        assert_eq!(gc.flushed_lsn(), initial);
    }

    #[test]
    fn single_writer_advances_flushed_lsn() {
        let (_g, path) = temp_wal("single_writer");
        let wal = Mutex::new(WalWriter::open(&path).unwrap());
        let initial = wal.lock().unwrap().durable_lsn();
        let gc = GroupCommit::new(initial);

        // Append a record, capture its LSN, then commit.
        let target = {
            let mut w = wal.lock().unwrap();
            w.append(&WalRecord::Begin { tx_id: 1 }).unwrap();
            w.append(&WalRecord::Commit { tx_id: 1 }).unwrap();
            w.current_lsn()
        };
        assert!(target > initial);

        gc.commit_at_least(target, &wal).unwrap();
        assert!(gc.flushed_lsn() >= target);
    }

    #[test]
    fn flushed_lsn_is_monotonic() {
        let (_g, path) = temp_wal("monotonic");
        let wal = Mutex::new(WalWriter::open(&path).unwrap());
        let initial = wal.lock().unwrap().durable_lsn();
        let gc = GroupCommit::new(initial);

        // First commit.
        let lo = {
            let mut w = wal.lock().unwrap();
            w.append(&WalRecord::Begin { tx_id: 1 }).unwrap();
            w.current_lsn()
        };
        gc.commit_at_least(lo, &wal).unwrap();
        let after_lo = gc.flushed_lsn();

        // Second commit advances further.
        let hi = {
            let mut w = wal.lock().unwrap();
            w.append(&WalRecord::Commit { tx_id: 1 }).unwrap();
            w.current_lsn()
        };
        gc.commit_at_least(hi, &wal).unwrap();
        let after_hi = gc.flushed_lsn();

        assert!(after_hi >= after_lo);
        // Calling commit_at_least with `lo` after `hi` is a no-op.
        gc.commit_at_least(lo, &wal).unwrap();
        assert_eq!(gc.flushed_lsn(), after_hi);
    }

    #[test]
    fn concurrent_writers_coalesce_through_one_coordinator() {
        // Two threads each commit a few records. Both must succeed
        // and `flushed_lsn` must reflect every byte they wrote.
        // We can't directly count fsyncs at this layer (the WAL
        // doesn't expose a sync counter), but the absence of
        // deadlock and the correct final LSN are the contract.
        let (_g, path) = temp_wal("two_writers");
        let wal = Arc::new(Mutex::new(WalWriter::open(&path).unwrap()));
        let initial = wal.lock().unwrap().durable_lsn();
        let gc = Arc::new(GroupCommit::new(initial));

        let mut handles = Vec::new();
        for tx in 0..2u64 {
            let wal_c = Arc::clone(&wal);
            let gc_c = Arc::clone(&gc);
            handles.push(thread::spawn(move || -> io::Result<()> {
                for i in 0..10u64 {
                    let target = {
                        let mut w = wal_c.lock().unwrap();
                        w.append(&WalRecord::Begin {
                            tx_id: tx * 100 + i,
                        })?;
                        w.append(&WalRecord::Commit {
                            tx_id: tx * 100 + i,
                        })?;
                        w.current_lsn()
                    };
                    gc_c.commit_at_least(target, &wal_c)?;
                }
                Ok(())
            }));
        }

        for h in handles {
            h.join().unwrap().unwrap();
        }

        let final_durable = wal.lock().unwrap().durable_lsn();
        assert!(gc.flushed_lsn() >= final_durable);
        // 20 commits worth of 13-byte Begin + 13-byte Commit
        // records = 520 bytes minimum on top of the 8-byte header.
        assert!(final_durable >= 8 + 520);
    }

    #[test]
    fn high_concurrency_eight_writers_no_deadlock() {
        // 8 threads × 50 commits each. Stress the coordinator:
        // expected to complete without deadlock and with the WAL
        // fully durable up to the largest committed LSN.
        let (_g, path) = temp_wal("eight_writers");
        let wal = Arc::new(Mutex::new(WalWriter::open(&path).unwrap()));
        let initial = wal.lock().unwrap().durable_lsn();
        let gc = Arc::new(GroupCommit::new(initial));

        let mut handles = Vec::new();
        for tx in 0..8u64 {
            let wal_c = Arc::clone(&wal);
            let gc_c = Arc::clone(&gc);
            handles.push(thread::spawn(move || -> io::Result<()> {
                for i in 0..50u64 {
                    let target = {
                        let mut w = wal_c.lock().unwrap();
                        w.append(&WalRecord::Begin {
                            tx_id: tx * 1000 + i,
                        })?;
                        w.append(&WalRecord::Commit {
                            tx_id: tx * 1000 + i,
                        })?;
                        w.current_lsn()
                    };
                    gc_c.commit_at_least(target, &wal_c)?;
                }
                Ok(())
            }));
        }

        for h in handles {
            h.join().unwrap().unwrap();
        }

        let current = wal.lock().unwrap().current_lsn();
        let durable = wal.lock().unwrap().durable_lsn();
        assert_eq!(durable, current, "every appended byte must be durable");
        assert!(gc.flushed_lsn() >= current);
    }

    #[test]
    fn writers_recover_from_poisoned_state() {
        // If a previous panic poisoned the state mutex, subsequent
        // writers must still be able to commit (we recover via
        // `unwrap_or_else(into_inner)`).
        let (_g, path) = temp_wal("poison_recovery");
        let wal = Arc::new(Mutex::new(WalWriter::open(&path).unwrap()));
        let initial = wal.lock().unwrap().durable_lsn();
        let gc = Arc::new(GroupCommit::new(initial));

        // Poison the state mutex by panicking inside it.
        let gc_c = Arc::clone(&gc);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _state = gc_c.state.lock().unwrap();
            panic!("intentional poison");
        }));

        // The mutex is now poisoned — but commit_at_least must
        // still work because we recover from poisoning.
        let target = {
            let mut w = wal.lock().unwrap();
            w.append(&WalRecord::Begin { tx_id: 1 }).unwrap();
            w.current_lsn()
        };
        gc.commit_at_least(target, &wal).unwrap();
        assert!(gc.flushed_lsn() >= target);
    }
}
