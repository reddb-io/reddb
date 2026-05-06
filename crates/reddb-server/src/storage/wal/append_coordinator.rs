//! Lock-free WAL append coordinator (Roadmap #2 / issue #157).
//!
//! Replaces the `Mutex<WalWriter>` held across `Begin + PageWrite×N +
//! Commit` append. Under 16-way concurrent writes the old mutex was
//! held ~13 µs per commit and produced a park-convoy that bottlenecked
//! the `concurrent` and `insert_sequential` benchmarks.
//!
//! # Architecture
//!
//! ```text
//!   writers ──▶ encode (no lock) ──▶ next_lsn.fetch_add (atomic)
//!                                         │
//!                                         ▼
//!                            SegQueue::push((lsn, bytes))
//!                                         │
//!                                         ▼
//!   first thread into commit_at_least becomes the leader, drains
//!   the queue in LSN order, takes the WAL file mutex briefly,
//!   writes contiguous bytes via `append_bytes` + `sync`, publishes
//!   `durable_lsn` atomically, unparks waiters.
//! ```
//!
//! # Why SegQueue + atomic LSN works
//!
//! Each writer reserves a contiguous LSN range with one atomic add.
//! The reserved ranges are non-overlapping by construction. The
//! leader pops entries (out of order is fine), sorts by LSN, then
//! writes a **contiguous prefix** starting at `written_lsn`. Any
//! non-contiguous tail is stashed in the leader's local pending
//! Vec and re-checked on the next iteration. A writer that has
//! reserved an LSN but not yet pushed its bytes is a transient
//! "hole" — the leader spin-waits up to `MAX_LEADER_SPIN_NS` for
//! the missing entry, then yields the leadership. The next leader
//! picks up from `written_lsn`, which has not advanced past the
//! gap.
//!
//! On-disk format: unchanged. `WalWriter::append_bytes` writes the
//! same byte sequence the old per-record `append` produced, in the
//! same LSN order. Recovery reads the file byte-by-byte and is
//! oblivious to whether the bytes arrived via one writer holding
//! a mutex or via N writers coordinated through this coordinator.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_queue::SegQueue;
use parking_lot::{Condvar, Mutex};

use super::writer::WalWriter;

/// Hard cap on time the leader will spin waiting for a missing
/// writer's push between `fetch_add` and `queue.push`. Past this,
/// the leader writes whatever contiguous prefix it has, releases
/// leadership, and parks. The next caller will retry.
///
/// 50 µs is two orders of magnitude longer than a typical
/// fetch_add → push gap (~50 ns) and well below an fsync (~100 µs),
/// so it never starves real progress.
const MAX_LEADER_SPIN_NS: u64 = 50_000;

/// Initial capacity for the leader's local pending Vec. 64 entries
/// is one typical concurrent-burst worth on a 16-core box; growth
/// past it costs one realloc and is unobservable in profiles.
const LEADER_PENDING_CAPACITY: usize = 64;

/// Lock-free coordinator sitting in front of a `WalWriter`.
///
/// Writers call [`reserve_and_enqueue`] to push their encoded
/// records, then [`commit_at_least`] to wait for durability.
/// The first thread into `commit_at_least` whose target is not
/// yet covered becomes the leader and drains the queue.
pub struct WalAppendCoordinator {
    /// Pending (lsn, bytes) tuples. `lsn` is the byte offset at which
    /// `bytes` should land in the WAL file.
    queue: SegQueue<(u64, Vec<u8>)>,
    /// Next LSN to hand out. Writers bump this with `fetch_add(len)`
    /// to atomically reserve a contiguous byte range.
    next_lsn: AtomicU64,
    /// Highest LSN that has been written to the BufWriter (not
    /// necessarily fsynced yet). Only the leader updates this; it is
    /// the watermark the leader walks from when looking for the
    /// contiguous prefix in the queue.
    written_lsn: AtomicU64,
    /// Highest LSN that has been `sync_all()`'d to disk. Waiters
    /// atomic-load this for the fast path before parking.
    durable_lsn: AtomicU64,
    /// Leadership flag. Set with CAS; cleared by the leader after
    /// publishing `durable_lsn` and notifying waiters.
    leader_in_progress: AtomicBool,
    /// Wakes waiters when `durable_lsn` advances. The mutex guards
    /// nothing of substance (parking_lot's Condvar still requires
    /// a mutex for park/unpark semantics) — its only role is to
    /// make the wait/notify pair correct.
    wait_lock: Mutex<()>,
    wait_cond: Condvar,
}

impl WalAppendCoordinator {
    /// Create a new coordinator pointing at the same byte space as
    /// `wal`. `wal` is left untouched here; subsequent writes flow
    /// through [`reserve_and_enqueue`] + [`commit_at_least`].
    pub fn new(initial_lsn: u64, initial_durable_lsn: u64) -> Self {
        Self {
            queue: SegQueue::new(),
            next_lsn: AtomicU64::new(initial_lsn),
            written_lsn: AtomicU64::new(initial_lsn),
            durable_lsn: AtomicU64::new(initial_durable_lsn),
            leader_in_progress: AtomicBool::new(false),
            wait_lock: Mutex::new(()),
            wait_cond: Condvar::new(),
        }
    }

    /// Highest LSN known durable. Cheap atomic read.
    pub fn durable_lsn(&self) -> u64 {
        self.durable_lsn.load(Ordering::Acquire)
    }

    /// Highest LSN that has been reserved (handed out to a writer).
    /// May exceed `written_lsn` if writers are mid-push.
    pub fn next_lsn(&self) -> u64 {
        self.next_lsn.load(Ordering::Acquire)
    }

    /// Reserve a contiguous LSN range covering `bytes.len()` bytes,
    /// push the encoded blob onto the queue, and return the
    /// **end-of-range** LSN (i.e. the LSN that
    /// [`commit_at_least`] should be called with for durability).
    ///
    /// The `(lsn, bytes)` push is unconditional — once `fetch_add`
    /// returns there is exactly one queue entry that owes its slice
    /// of the LSN range, and the leader will eventually drain it.
    pub fn reserve_and_enqueue(&self, bytes: Vec<u8>) -> u64 {
        let len = bytes.len() as u64;
        // Reserve the LSN range first. From this point onward the
        // leader is allowed to observe `next_lsn = lsn + len` while
        // we have not yet pushed our entry — that transient gap
        // is exactly what the leader's spin-loop tolerates.
        let lsn = self.next_lsn.fetch_add(len, Ordering::AcqRel);
        self.queue.push((lsn, bytes));
        lsn + len
    }

    /// Block until the WAL is durable up to at least `target`. If
    /// another leader is already mid-flush we park on the condvar;
    /// otherwise we become the leader and drive the drain ourselves.
    ///
    /// `wal` is the `parking_lot::Mutex<WalWriter>` shared with the
    /// transaction manager. We take it briefly (only the leader,
    /// only during drain + sync) — never on the writer fast path.
    pub fn commit_at_least(&self, target: u64, wal: &Mutex<WalWriter>) -> io::Result<()> {
        loop {
            // Fast path: another leader already covered us.
            if self.durable_lsn.load(Ordering::Acquire) >= target {
                return Ok(());
            }

            // Try to become the leader.
            if self
                .leader_in_progress
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                let pre_durable = self.durable_lsn.load(Ordering::Acquire);
                let result = self.drive_drain(wal);
                let post_durable = self.durable_lsn.load(Ordering::Acquire);
                self.leader_in_progress.store(false, Ordering::Release);
                // Wake every waiter — the loser of the leadership
                // CAS may have parked on the condvar, and the new
                // `durable_lsn` is what they were waiting for.
                {
                    let _g = self.wait_lock.lock();
                    self.wait_cond.notify_all();
                }
                result?;

                if post_durable >= target {
                    return Ok(());
                }

                if post_durable == pre_durable {
                    // Leader bailed without progress — most likely
                    // a writer is mid-push (fetch_add returned, but
                    // queue.push hasn't completed yet). Park very
                    // briefly to give that writer a chance to land
                    // its bytes before we retake leadership.
                    let mut guard = self.wait_lock.lock();
                    if self.durable_lsn.load(Ordering::Acquire) >= target {
                        return Ok(());
                    }
                    self.wait_cond
                        .wait_for(&mut guard, Duration::from_micros(50));
                }
                continue;
            }

            // Not the leader — park until durable_lsn moves, then
            // re-check. We use a short timeout so a missed wakeup
            // (impossible in theory, observed in practice on some
            // platforms when the leader bails very early) cannot
            // hang the writer indefinitely.
            let mut guard = self.wait_lock.lock();
            if self.durable_lsn.load(Ordering::Acquire) >= target {
                return Ok(());
            }
            // 1 ms is a soft upper bound on a typical group-fsync —
            // it just bounds the worst-case wakeup latency, real
            // wakeups come from `notify_all` above.
            self.wait_cond
                .wait_for(&mut guard, Duration::from_millis(1));
        }
    }

    /// Leader-side drain: pop the queue, sort by LSN, write the
    /// contiguous prefix starting at `written_lsn`, fsync, publish
    /// the new `durable_lsn`.
    ///
    /// The leader holds `wal` (the WAL file mutex) only during
    /// `append_bytes` + `sync`. Writers continue to push onto
    /// the queue throughout — their bytes will be picked up by
    /// the next drain (or the same one if they reach the queue
    /// before the leader stops popping).
    fn drive_drain(&self, wal: &Mutex<WalWriter>) -> io::Result<()> {
        let mut cursor = self.written_lsn.load(Ordering::Acquire);
        let mut writeable: Vec<(u64, Vec<u8>)> = Vec::with_capacity(LEADER_PENDING_CAPACITY);
        let spin_deadline = Instant::now() + Duration::from_nanos(MAX_LEADER_SPIN_NS);

        // Outer loop: drain the queue, sort, take contiguous prefix
        // from `cursor`. If after one drain we have NO progress and
        // there's still time on the spin deadline, yield and retry —
        // the missing writer may finish its push in the next slice.
        loop {
            let mut pending: Vec<(u64, Vec<u8>)> = Vec::with_capacity(LEADER_PENDING_CAPACITY);
            while let Some(entry) = self.queue.pop() {
                pending.push(entry);
            }

            if pending.is_empty() {
                // Nothing in the queue. If we've already taken some
                // bytes this round, write them and exit. Otherwise
                // there's nothing to do.
                break;
            }

            pending.sort_by_key(|(lsn, _)| *lsn);

            let mut idx = 0;
            // Skip stale entries below cursor (defensive — fetch_add
            // is monotonic so this shouldn't happen).
            while idx < pending.len() && pending[idx].0 < cursor {
                idx += 1;
            }

            // Take the contiguous prefix.
            while idx < pending.len() && pending[idx].0 == cursor {
                let (_, bytes) = std::mem::take(&mut pending[idx]);
                cursor += bytes.len() as u64;
                writeable.push((cursor - bytes.len() as u64, bytes));
                idx += 1;
            }

            // Re-push the non-contiguous tail — those entries belong
            // to writers further ahead, blocked on a missing entry
            // somewhere in [cursor, pending[idx].0).
            for (lsn, bytes) in pending.drain(idx..) {
                if !bytes.is_empty() {
                    self.queue.push((lsn, bytes));
                }
            }

            // If we made progress this iteration, try draining once
            // more — additional writers may have pushed while we
            // were taking the prefix.
            // If we made NO progress, decide whether to spin-wait
            // for the missing writer or bail. Capping by the
            // deadline guarantees the leader is never starved.
            if writeable.is_empty() && Instant::now() < spin_deadline {
                std::thread::yield_now();
                continue;
            }

            // Either we wrote something, or we ran out of patience.
            break;
        }

        if writeable.is_empty() {
            // Couldn't make any forward progress this round. Bail
            // and let the next leader try.
            return Ok(());
        }

        // ── Phase 1: take the WAL mutex, write all contiguous bytes,
        // capture target_lsn, then drain BufWriter into the kernel.
        let target_lsn = {
            let mut wal_guard = wal.lock();
            for (_lsn, bytes) in &writeable {
                wal_guard.append_bytes(bytes)?;
            }
            // Sync: drains BufWriter and fsyncs the file. We do this
            // under the WAL mutex to keep `durable_lsn` and the
            // file's actual on-disk position in lockstep with the
            // writer's bookkeeping.
            wal_guard.sync()?;
            wal_guard.current_lsn()
        };

        // Phase 2: publish.
        self.written_lsn.store(target_lsn, Ordering::Release);
        // durable_lsn is monotonic — we only ever raise it. fence
        // pairs with the Acquire load in the writer fast path.
        let prev = self.durable_lsn.load(Ordering::Acquire);
        if target_lsn > prev {
            self.durable_lsn.store(target_lsn, Ordering::Release);
        }

        Ok(())
    }

    /// After a `WalWriter::truncate()`, every byte counter resets to
    /// the header size. Mirror that on the coordinator: drop any
    /// queued entries, snap counters back to `next_lsn`. Used by the
    /// truncate-on-checkpoint path.
    pub fn reset(&self, next_lsn: u64) {
        // Drain the queue — any entries left over reference offsets
        // in the pre-truncate byte space and would corrupt the file
        // if we wrote them after the reset.
        while self.queue.pop().is_some() {}
        self.next_lsn.store(next_lsn, Ordering::Release);
        self.written_lsn.store(next_lsn, Ordering::Release);
        self.durable_lsn.store(next_lsn, Ordering::Release);
        let _g = self.wait_lock.lock();
        self.wait_cond.notify_all();
    }
}

impl std::fmt::Debug for WalAppendCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalAppendCoordinator")
            .field("next_lsn", &self.next_lsn.load(Ordering::Acquire))
            .field("written_lsn", &self.written_lsn.load(Ordering::Acquire))
            .field("durable_lsn", &self.durable_lsn.load(Ordering::Acquire))
            .field(
                "leader_in_progress",
                &self.leader_in_progress.load(Ordering::Acquire),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::wal::reader::WalReader;
    use crate::storage::wal::record::WalRecord;
    use crate::storage::wal::writer::WalWriter;
    use parking_lot::Mutex as PlMutex;
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
            "rb_wal_coord_{}_{}_{}.wal",
            name,
            std::process::id(),
            nanos
        ));
        let _ = std::fs::remove_file(&path);
        (FileGuard { path: path.clone() }, path)
    }

    /// Single writer end-to-end: reserve, enqueue, commit, recover.
    #[test]
    fn single_writer_round_trip() {
        let (_g, path) = temp_wal("single");
        let wal = WalWriter::open(&path).unwrap();
        let initial = wal.current_lsn();
        let durable = wal.durable_lsn();
        let wal = Arc::new(PlMutex::new(wal));
        let coord = WalAppendCoordinator::new(initial, durable);

        let mut blob = Vec::new();
        blob.extend_from_slice(&WalRecord::Begin { tx_id: 1 }.encode());
        blob.extend_from_slice(&WalRecord::Commit { tx_id: 1 }.encode());
        let target = coord.reserve_and_enqueue(blob);

        coord.commit_at_least(target, &wal).unwrap();

        assert!(coord.durable_lsn() >= target);
        // File contains exactly two records past the header.
        let recs: Vec<_> = WalReader::open(&path)
            .unwrap()
            .iter()
            .map(|r| r.unwrap().1)
            .collect();
        assert_eq!(recs.len(), 2);
    }

    /// Concurrent writers — every record must land in the WAL with
    /// no gaps, in the LSN order assigned by `next_lsn`.
    #[test]
    fn concurrent_writers_no_gaps_lsn_ordered() {
        let (_g, path) = temp_wal("concurrent_no_gaps");
        let wal = WalWriter::open(&path).unwrap();
        let initial = wal.current_lsn();
        let durable = wal.durable_lsn();
        let wal = Arc::new(PlMutex::new(wal));
        let coord = Arc::new(WalAppendCoordinator::new(initial, durable));

        const WRITERS: u64 = 16;
        const PER_WRITER: u64 = 50;

        let mut handles = Vec::new();
        for tx_base in 0..WRITERS {
            let wal_c = Arc::clone(&wal);
            let coord_c = Arc::clone(&coord);
            handles.push(thread::spawn(move || {
                for i in 0..PER_WRITER {
                    let tx_id = tx_base * 1000 + i;
                    let mut blob = Vec::new();
                    blob.extend_from_slice(&WalRecord::Begin { tx_id }.encode());
                    blob.extend_from_slice(&WalRecord::Commit { tx_id }.encode());
                    let target = coord_c.reserve_and_enqueue(blob);
                    coord_c.commit_at_least(target, &wal_c).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // Read every record back. Total count = WRITERS * PER_WRITER * 2
        // (Begin + Commit per loop iteration).
        let recs: Vec<_> = WalReader::open(&path)
            .unwrap()
            .iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(recs.len() as u64, WRITERS * PER_WRITER * 2);

        // LSNs must be strictly monotonically increasing — i.e. the
        // file is contiguous, no overlaps, no gaps.
        for w in recs.windows(2) {
            assert!(w[1].0 > w[0].0, "LSNs must be strictly increasing");
        }

        // Every (Begin tx_id, Commit tx_id) pair must appear with
        // Begin immediately followed by its matching Commit. (For
        // this test each writer pushes Begin+Commit as one blob, so
        // they end up adjacent in the file.)
        for chunk in recs.chunks_exact(2) {
            match (&chunk[0].1, &chunk[1].1) {
                (WalRecord::Begin { tx_id: a }, WalRecord::Commit { tx_id: b }) => {
                    assert_eq!(a, b, "Begin/Commit pair tx_id mismatch");
                }
                other => panic!("unexpected record pair: {other:?}"),
            }
        }
    }

    /// Property: reserved LSN equals the byte offset where the bytes
    /// actually land. Verified by reading back the file and matching
    /// the offset of the first record against the first reserved LSN.
    #[test]
    fn reserved_lsn_matches_on_disk_offset() {
        let (_g, path) = temp_wal("lsn_offset");
        let wal = WalWriter::open(&path).unwrap();
        let initial = wal.current_lsn();
        let durable = wal.durable_lsn();
        let wal = Arc::new(PlMutex::new(wal));
        let coord = WalAppendCoordinator::new(initial, durable);

        let blob = WalRecord::Begin { tx_id: 99 }.encode();
        let blob_len = blob.len() as u64;
        let target = coord.reserve_and_enqueue(blob);
        assert_eq!(target, initial + blob_len);
        coord.commit_at_least(target, &wal).unwrap();

        let recs: Vec<_> = WalReader::open(&path)
            .unwrap()
            .iter()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(recs[0].0, initial);
    }

    /// reset() drops queued entries and snaps counters back. The
    /// truncate-on-checkpoint path relies on this.
    #[test]
    fn reset_clears_queue_and_resets_counters() {
        let (_g, path) = temp_wal("reset");
        let wal = WalWriter::open(&path).unwrap();
        let initial = wal.current_lsn();
        let wal = Arc::new(PlMutex::new(wal));
        let coord = WalAppendCoordinator::new(initial, initial);

        // Reserve a few entries but DON'T commit them. They sit in
        // the queue.
        let _ = coord.reserve_and_enqueue(vec![1, 2, 3]);
        let _ = coord.reserve_and_enqueue(vec![4, 5, 6]);
        assert!(coord.next_lsn() > initial);

        coord.reset(initial);
        assert_eq!(coord.next_lsn(), initial);
        assert_eq!(coord.durable_lsn(), initial);
        // After reset, a fresh enqueue should start from initial again.
        let target = coord.reserve_and_enqueue(WalRecord::Begin { tx_id: 7 }.encode());
        coord.commit_at_least(target, &wal).unwrap();
        assert_eq!(coord.durable_lsn(), target);
    }

    /// Crash-injection: simulate a writer that successfully reserves
    /// an LSN range but is "killed" before pushing — i.e. an LSN
    /// gap that will never resolve. The leader must NOT advance
    /// `written_lsn` past the gap, and the file must contain only
    /// the bytes that were actually pushed (none in this case).
    ///
    /// We don't wait on `commit_at_least` here because, by design,
    /// a permanent gap means callers behind it can never reach
    /// durability — the system would deadlock if we did. The
    /// production failure mode is "process exits, all writers
    /// abandon their commits"; this test asserts the on-disk
    /// invariant under that scenario.
    #[test]
    fn writer_crash_between_reserve_and_push_keeps_file_clean() {
        let (_g, path) = temp_wal("writer_crash");
        let wal = WalWriter::open(&path).unwrap();
        let initial = wal.current_lsn();
        let wal = Arc::new(PlMutex::new(wal));
        let coord = Arc::new(WalAppendCoordinator::new(initial, initial));

        // Manually reserve an LSN range without pushing. This is the
        // "crashed-before-push" state — a permanent gap.
        let stuck_len = 10u64;
        let _stuck_lsn = coord.next_lsn.fetch_add(stuck_len, Ordering::AcqRel);

        // Try a single drain attempt by becoming the leader directly.
        // The drain bails because the contiguous prefix is empty.
        let acquired = coord
            .leader_in_progress
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        assert!(acquired, "test must own the leader flag");
        let _ = coord.drive_drain(&wal);
        coord.leader_in_progress.store(false, Ordering::Release);

        // durable_lsn stays at `initial` — no progress past the gap.
        assert_eq!(coord.durable_lsn(), initial);

        // The on-disk file is exactly the header — no garbage from
        // the gap, no half-written records.
        let on_disk_len = std::fs::metadata(&path).unwrap().len();
        assert_eq!(on_disk_len, initial);
    }

    /// Crash-injection: writer A reserves+pushes and waits; writer B
    /// reserves but "crashes" (never pushes); writer C
    /// reserves+pushes after B. The leader must write A's bytes
    /// (contiguous from initial) but stop at B's gap, leaving C's
    /// bytes in the queue. C's commit can never succeed (caller's
    /// problem), but A's commit MUST succeed.
    #[test]
    fn writer_a_succeeds_when_b_crashes_before_c() {
        let (_g, path) = temp_wal("abc_crash");
        let wal = WalWriter::open(&path).unwrap();
        let initial = wal.current_lsn();
        let wal = Arc::new(PlMutex::new(wal));
        let coord = Arc::new(WalAppendCoordinator::new(initial, initial));

        // Writer A: reserve + push.
        let blob_a = WalRecord::Begin { tx_id: 1 }.encode();
        let len_a = blob_a.len() as u64;
        let target_a = coord.reserve_and_enqueue(blob_a);

        // Writer B: reserve only ("crash" before push).
        let stuck_len = 13u64;
        let _stuck_lsn = coord.next_lsn.fetch_add(stuck_len, Ordering::AcqRel);

        // Writer C: reserve + push.
        let blob_c = WalRecord::Begin { tx_id: 3 }.encode();
        let _target_c = coord.reserve_and_enqueue(blob_c);

        // A's commit succeeds: leader takes A's bytes (contiguous
        // from initial), bails at B's gap, never reaches C.
        coord.commit_at_least(target_a, &wal).unwrap();
        assert_eq!(coord.durable_lsn(), initial + len_a);

        let on_disk_len = std::fs::metadata(&path).unwrap().len();
        assert_eq!(on_disk_len, initial + len_a);
    }
}
