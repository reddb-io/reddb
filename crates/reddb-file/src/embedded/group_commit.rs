//! Synchronous group commit: all members wait for both WAL publication barriers.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Duration, Instant};

use super::{EmbeddedRdbArtifact, EmbeddedRdbOpen, RdbFileError, RdbFileResult};

#[derive(Clone, Copy)]
pub struct EmbeddedGroupCommitOptions {
    pub window: Duration,
    pub max_statements: usize,
    pub max_wal_bytes: u64,
}

type Completion = Arc<Mutex<Option<RdbFileResult<EmbeddedRdbOpen>>>>;

struct PendingAppend {
    payloads: Vec<Vec<u8>>,
    bytes: u64,
    completion: Completion,
    options: EmbeddedGroupCommitOptions,
    deadline: Instant,
}

#[derive(Default)]
struct GroupState {
    pending: VecDeque<PendingAppend>,
    pending_bytes: u64,
    flushing: bool,
    failure: Option<RdbFileError>,
}

#[derive(Default)]
struct GroupCommit {
    state: Mutex<GroupState>,
    changed: Condvar,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn copy_error(error: &RdbFileError) -> RdbFileError {
    match error {
        RdbFileError::ReadLimitExceeded {
            required_bytes,
            limit_bytes,
        } => RdbFileError::ReadLimitExceeded {
            required_bytes: *required_bytes,
            limit_bytes: *limit_bytes,
        },
        RdbFileError::InvalidOperation(message) => RdbFileError::InvalidOperation(message.clone()),
        RdbFileError::ZoneUnrecoverable { zone, path } => RdbFileError::ZoneUnrecoverable {
            zone,
            path: path.clone(),
        },
        RdbFileError::Io(error) => RdbFileError::Io(match error.raw_os_error() {
            Some(code) => std::io::Error::from_raw_os_error(code),
            None => std::io::Error::new(error.kind(), error.to_string()),
        }),
    }
}

static GROUPS: OnceLock<Mutex<HashMap<PathBuf, Weak<GroupCommit>>>> = OnceLock::new();

fn coordinator(path: &Path) -> Arc<GroupCommit> {
    let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut groups = lock(GROUPS.get_or_init(Mutex::default));
    if let Some(group) = groups.get(&key).and_then(Weak::upgrade) {
        return group;
    }
    // No worker thread or path entry survives the last active caller.
    groups.retain(|_, group| group.strong_count() != 0);
    let group = Arc::new(GroupCommit::default());
    groups.insert(key, Arc::downgrade(&group));
    group
}

struct FlushBatch<'a> {
    group: &'a GroupCommit,
    requests: Vec<PendingAppend>,
    completed: bool,
}

impl FlushBatch<'_> {
    fn complete(&mut self, mut result: RdbFileResult<EmbeddedRdbOpen>) {
        let mut state = lock(&self.group.state);
        if result.is_ok() {
            if let Some(error) = &state.failure {
                result = Err(copy_error(error));
            }
        }
        if let Err(error) = &result {
            if !matches!(error, RdbFileError::InvalidOperation(message) if message.contains("embedded wal region full"))
            {
                state.failure = Some(copy_error(error));
            }
        }
        for request in &self.requests {
            *lock(&request.completion) = Some(match &result {
                Ok(open) => Ok(open.clone()),
                Err(error) => Err(copy_error(error)),
            });
        }
        state.flushing = false;
        self.completed = true;
        self.group.changed.notify_all();
    }
}

impl Drop for FlushBatch<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.complete(Err(RdbFileError::Io(std::io::Error::other(
                "embedded group commit interrupted before durability confirmation",
            ))));
        }
    }
}

impl GroupCommit {
    #[cfg(test)]
    fn append(
        &self,
        payloads: &[Vec<u8>],
        options: EmbeddedGroupCommitOptions,
        after_enqueue: impl FnOnce(),
        write: impl FnMut(&[Vec<u8>]) -> RdbFileResult<EmbeddedRdbOpen>,
    ) -> RdbFileResult<EmbeddedRdbOpen> {
        let completion = self.enqueue(payloads, options)?;
        after_enqueue();
        self.wait(&completion, write)
    }

    fn enqueue(
        &self,
        payloads: &[Vec<u8>],
        options: EmbeddedGroupCommitOptions,
    ) -> RdbFileResult<Completion> {
        let deadline = Instant::now().checked_add(options.window).ok_or_else(|| {
            RdbFileError::InvalidOperation("embedded group commit window is too large".into())
        })?;
        let bytes = EmbeddedRdbArtifact::wal_payloads_encoded_len(payloads)?;
        let completion = Arc::new(Mutex::new(None));
        let mut state = lock(&self.state);
        if let Some(error) = &state.failure {
            return Err(copy_error(error));
        }
        // Bound queued work. One indivisible oversized operation is permitted
        // only on an empty queue; an operation is never split across commits.
        while !state.pending.is_empty()
            && (state.pending.len() >= options.max_statements.max(1)
                || state.pending_bytes.saturating_add(bytes) > options.max_wal_bytes.max(1))
        {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(error) = &state.failure {
                return Err(copy_error(error));
            }
        }
        state.pending_bytes = state.pending_bytes.checked_add(bytes).ok_or_else(|| {
            RdbFileError::InvalidOperation("embedded group commit byte count overflow".into())
        })?;
        state.pending.push_back(PendingAppend {
            payloads: payloads.to_vec(),
            bytes,
            completion: Arc::clone(&completion),
            options,
            deadline,
        });
        self.changed.notify_all();
        Ok(completion)
    }

    fn wait(
        &self,
        completion: &Completion,
        mut write: impl FnMut(&[Vec<u8>]) -> RdbFileResult<EmbeddedRdbOpen>,
    ) -> RdbFileResult<EmbeddedRdbOpen> {
        loop {
            let mut state = lock(&self.state);
            if let Some(result) = lock(completion).as_ref() {
                return match result {
                    Ok(open) => Ok(open.clone()),
                    Err(error) => Err(copy_error(error)),
                };
            }
            if let Some(error) = state.failure.as_ref().map(copy_error) {
                for pending in state.pending.drain(..) {
                    *lock(&pending.completion) = Some(Err(copy_error(&error)));
                }
                state.pending_bytes = 0;
                self.changed.notify_all();
                continue;
            }
            if state.flushing {
                drop(
                    self.changed
                        .wait(state)
                        .unwrap_or_else(|poisoned| poisoned.into_inner()),
                );
                continue;
            }
            state.flushing = true;
            let first = state
                .pending
                .front()
                .expect("unfinished caller must be queued");
            let options = first.options;
            let deadline = first.deadline;
            let mut batch = FlushBatch {
                group: self,
                requests: Vec::new(),
                completed: false,
            };
            // Enqueueing wakes the elected caller early when either bound is
            // reached. A zero window still coalesces already queued requests.
            while state.pending.len() < options.max_statements.max(1)
                && state.pending_bytes < options.max_wal_bytes.max(1)
            {
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    break;
                };
                let (next, timeout) = self
                    .changed
                    .wait_timeout(state, remaining)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state = next;
                if timeout.timed_out() {
                    break;
                }
            }
            let mut batch_bytes = 0_u64;
            while let Some(front) = state.pending.front() {
                // An empty member fences earlier work; it must not inherit a
                // retryable full-region error from a data batch. Process it
                // alone, after earlier requests complete. Sticky IO failures
                // still reject it through GroupState::failure above.
                if !batch.requests.is_empty()
                    && (front.payloads.is_empty()
                        || batch.requests[0].payloads.is_empty()
                        || batch.requests.len() >= options.max_statements.max(1)
                        || batch_bytes.saturating_add(front.bytes) > options.max_wal_bytes.max(1))
                {
                    break;
                }
                let request = state.pending.pop_front().expect("pending front exists");
                batch_bytes = batch_bytes.saturating_add(request.bytes);
                state.pending_bytes -= request.bytes;
                batch.requests.push(request);
            }
            self.changed.notify_all();
            drop(state);
            let payloads: Vec<Vec<u8>> = batch
                .requests
                .iter_mut()
                .flat_map(|request| std::mem::take(&mut request.payloads))
                .collect();
            batch.complete(write(&payloads));
            // Relinquish leadership after one bounded batch. This caller can
            // return once its own batch is durable even under sustained load.
        }
    }
}

/// A queued write is not an acknowledgement. Call wait() outside publication
/// locks; only its successful return confirms the WAL and superblock syncs.
pub struct PendingEmbeddedAppend {
    group: Arc<GroupCommit>,
    completion: Completion,
    path: PathBuf,
}

/// Keep a failed publication fenced until its owning database is closed.
/// A new store must reload the durable image before accepting more writes.
pub struct EmbeddedGroupCommitSession {
    group: Arc<GroupCommit>,
    path: PathBuf,
}

impl EmbeddedGroupCommitSession {
    pub fn fence_interrupted_write(&self) {
        let mut state = lock(&self.group.state);
        if state.failure.is_none() {
            state.failure = Some(RdbFileError::Io(std::io::Error::other(
                "logical write interrupted before durability confirmation",
            )));
        }
        self.group.changed.notify_all();
    }
    pub fn check(&self) -> RdbFileResult<()> {
        let failure = lock(&self.group.state).failure.as_ref().map(copy_error);
        if let Some(error) = failure {
            return Err(RdbFileError::Io(std::io::Error::other(format!(
                "a prior embedded WAL publication failed; close and reopen the database before writing: {error}"
            ))));
        }
        Ok(())
    }

    pub fn enqueue(
        &self,
        payloads: &[Vec<u8>],
        options: EmbeddedGroupCommitOptions,
    ) -> RdbFileResult<PendingEmbeddedAppend> {
        self.check()?;
        let completion = self.group.enqueue(payloads, options)?;
        Ok(PendingEmbeddedAppend {
            group: Arc::clone(&self.group),
            completion,
            path: self.path.clone(),
        })
    }

    pub fn append(
        &self,
        payloads: &[Vec<u8>],
        options: EmbeddedGroupCommitOptions,
    ) -> RdbFileResult<EmbeddedRdbOpen> {
        self.enqueue(payloads, options)?.wait()
    }
}

impl PendingEmbeddedAppend {
    pub fn wait(&self) -> RdbFileResult<EmbeddedRdbOpen> {
        self.group.wait(&self.completion, |batch| {
            EmbeddedRdbArtifact::append_wal_payloads(&self.path, batch)
        })
    }
}

pub(super) fn wait_for_pending_appends(path: &Path) -> RdbFileResult<()> {
    // An empty FIFO member is a publication fence, not a new WAL frame. Use
    // unbounded enqueue limits so the checkpoint itself can drain a full queue;
    // every flush still uses the first member's configured batch limits.
    let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let group = {
        let groups = lock(GROUPS.get_or_init(Mutex::default));
        groups.get(&key).and_then(Weak::upgrade)
    };
    let Some(group) = group else {
        return Ok(());
    };
    {
        let state = lock(&group.state);
        if let Some(error) = &state.failure {
            return Err(copy_error(error));
        }
        if state.pending.is_empty() && !state.flushing {
            return Ok(());
        }
    }
    let completion = group.enqueue(
        &[],
        EmbeddedGroupCommitOptions {
            window: Duration::ZERO,
            max_statements: usize::MAX,
            max_wal_bytes: u64::MAX,
        },
    )?;
    group
        .wait(&completion, |batch| {
            EmbeddedRdbArtifact::append_wal_payloads(path, batch)
        })
        .map(|_| ())
}

impl EmbeddedRdbArtifact {
    pub fn group_commit_session(path: impl AsRef<Path>) -> EmbeddedGroupCommitSession {
        let path = path.as_ref();
        let path = path
            .canonicalize()
            .or_else(|_| std::path::absolute(path))
            .unwrap_or_else(|_| path.to_path_buf());
        EmbeddedGroupCommitSession {
            group: coordinator(&path),
            path,
        }
    }
    pub fn enqueue_wal_payloads_grouped(
        path: impl AsRef<Path>,
        payloads: &[Vec<u8>],
        options: EmbeddedGroupCommitOptions,
    ) -> RdbFileResult<PendingEmbeddedAppend> {
        let path = path.as_ref();
        let group = coordinator(path);
        let completion = group.enqueue(payloads, options)?;
        Ok(PendingEmbeddedAppend {
            group,
            completion,
            path: path.to_path_buf(),
        })
    }
    /// Concurrent requests share WAL + alternate-superblock publication. Every
    /// successful caller waits for the same two syncs as an ungrouped append.
    pub fn append_wal_payloads_grouped(
        path: impl AsRef<Path>,
        payloads: &[Vec<u8>],
        options: EmbeddedGroupCommitOptions,
    ) -> RdbFileResult<EmbeddedRdbOpen> {
        let path = path.as_ref();
        if payloads.is_empty() {
            return Self::open(path);
        }
        Self::enqueue_wal_payloads_grouped(path, payloads, options)?.wait()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Barrier;

    fn options() -> EmbeddedGroupCommitOptions {
        EmbeddedGroupCommitOptions {
            window: Duration::ZERO,
            max_statements: 32,
            max_wal_bytes: 1024 * 1024,
        }
    }

    #[test]
    fn grouped_crash_recovers_whole_publications() {
        const CHILD: &str = "REDDB_GROUP_CRASH_PATH";
        if let Some(path) = std::env::var_os(CHILD) {
            EmbeddedRdbArtifact::append_wal_payloads_grouped(
                Path::new(&path),
                &[b"replacement".to_vec(), b"delete previous".to_vec()],
                options(),
            )
            .expect("injected crash exits before returning");
            panic!("crash injection did not fire");
        }
        for point in [
            "wal_after_frame_write",
            "wal_after_frame_sync",
            "wal_after_superblock_write",
        ] {
            let directory = tempfile::tempdir().expect("directory");
            let path = directory.path().join("crash.rdb");
            EmbeddedRdbArtifact::create(&path).expect("artifact");
            EmbeddedRdbArtifact::append_wal_payloads(&path, &[b"original".to_vec()]).expect("seed");
            let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
                .args([
                    "--exact",
                    "embedded::group_commit::tests::grouped_crash_recovers_whole_publications",
                    "--nocapture",
                ])
                .env(CHILD, &path)
                .env(super::super::CRASH_INJECT_ENV, point)
                .status()
                .expect("crash child");
            assert_eq!(status.code(), Some(173), "{point}");
            let open = EmbeddedRdbArtifact::open(&path).expect("recover");
            let payloads = EmbeddedRdbArtifact::read_wal_payloads(&open).expect("replay");
            assert!(
                payloads == vec![b"original".to_vec()]
                    || payloads
                        == vec![
                            b"original".to_vec(),
                            b"replacement".to_vec(),
                            b"delete previous".to_vec()
                        ],
                "partial publication at {point}: {payloads:?}"
            );
        }
    }

    #[test]
    fn concurrent_group_members_wait_for_one_durable_publication() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("group.rdb");
        let initial = EmbeddedRdbArtifact::create(&path).expect("artifact");
        let group = GroupCommit::default();
        let enqueued = Barrier::new(8);
        let writes = AtomicUsize::new(0);
        let generations = std::thread::scope(|scope| {
            let readers: Vec<_> = (0_u8..8)
                .map(|value| {
                    let path = &path;
                    let group = &group;
                    let enqueued = &enqueued;
                    let writes = &writes;
                    scope.spawn(move || {
                        group
                            .append(
                                &[vec![value]],
                                options(),
                                || {
                                    enqueued.wait();
                                },
                                |payloads| {
                                    assert_eq!(payloads.len(), 8);
                                    writes.fetch_add(1, Ordering::Relaxed);
                                    EmbeddedRdbArtifact::append_wal_payloads(path, payloads)
                                },
                            )
                            .expect("durable append")
                            .selected_superblock
                            .generation
                    })
                })
                .collect();
            readers
                .into_iter()
                .map(|reader| reader.join().expect("caller"))
                .collect::<Vec<_>>()
        });
        assert_eq!(writes.load(Ordering::Relaxed), 1);
        assert!(generations
            .iter()
            .all(|generation| *generation == initial.selected_superblock.generation + 1));
        let reopened = EmbeddedRdbArtifact::open(&path).expect("reopen");
        let mut payloads = EmbeddedRdbArtifact::read_wal_payloads(&reopened).expect("payloads");
        payloads.sort();
        assert_eq!(
            payloads,
            (0_u8..8).map(|value| vec![value]).collect::<Vec<_>>()
        );
    }

    #[test]
    fn failed_publication_rejects_every_group_member_and_poisoned_retries() {
        let group = GroupCommit::default();
        let enqueued = Barrier::new(8);
        std::thread::scope(|scope| {
            let readers: Vec<_> = (0_u8..8)
                .map(|value| {
                    let group = &group;
                    let enqueued = &enqueued;
                    scope.spawn(move || {
                        group.append(
                            &[vec![value]],
                            options(),
                            || {
                                enqueued.wait();
                            },
                            |_| {
                                Err(RdbFileError::Io(std::io::Error::new(
                                    std::io::ErrorKind::PermissionDenied,
                                    "injected publication failure",
                                )))
                            },
                        )
                    })
                })
                .collect();
            for reader in readers {
                let error = reader
                    .join()
                    .expect("caller")
                    .expect_err("no durable acknowledgement");
                assert!(
                    matches!(error, RdbFileError::Io(error) if error.kind() == std::io::ErrorKind::PermissionDenied)
                );
            }
        });
        assert!(!lock(&group.state).flushing);
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("retry.rdb");
        EmbeddedRdbArtifact::create(&path).expect("artifact");
        assert!(group.enqueue(&[b"retry".to_vec()], options()).is_err());
        GroupCommit::default()
            .append(
                &[b"retry".to_vec()],
                options(),
                || {},
                |payloads| EmbeddedRdbArtifact::append_wal_payloads(&path, payloads),
            )
            .expect("retry");
    }

    #[test]
    fn checkpoint_drains_queued_publications_before_replacing_wal() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("checkpoint.rdb");
        EmbeddedRdbArtifact::create(&path).expect("artifact");
        let pending = EmbeddedRdbArtifact::enqueue_wal_payloads_grouped(
            &path,
            &[b"before snapshot".to_vec()],
            options(),
        )
        .expect("enqueue");
        let checkpoint = EmbeddedRdbArtifact::write_snapshot_with_wal_capacity(
            &path,
            b"RDST-snapshot includes pending write",
            1024,
        )
        .expect("checkpoint drains the queue itself");
        let committed = pending.wait().expect("durable publication");
        assert!(committed.selected_superblock.wal_live_bytes > 0);
        assert!(
            committed.selected_superblock.generation < checkpoint.selected_superblock.generation
        );
        assert!(EmbeddedRdbArtifact::read_wal_payloads(&checkpoint)
            .expect("wal")
            .is_empty());
    }

    #[test]
    fn checkpoint_fence_drains_retryable_full_requests_without_inheriting_their_error() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("full-checkpoint.rdb");
        let open = EmbeddedRdbArtifact::create(&path).expect("artifact");
        let header = EmbeddedRdbArtifact::wal_payloads_encoded_len(&[vec![]]).expect("header");
        EmbeddedRdbArtifact::append_wal_payloads(
            &path,
            &[vec![1; (open.manifest.wal_region_bytes - header) as usize]],
        )
        .expect("fill WAL");
        let session = EmbeddedRdbArtifact::group_commit_session(&path);
        let pending = session
            .enqueue(&[vec![2; 1024]], options())
            .expect("queued staged write");
        let checkpoint = EmbeddedRdbArtifact::write_snapshot_with_wal_capacity(
            &path,
            b"RDST-image includes the staged write",
            1024 * 1024,
        )
        .expect("fence drains full requests before checkpointing");
        assert!(
            matches!(pending.wait(), Err(RdbFileError::InvalidOperation(message)) if message.contains("embedded wal region full"))
        );
        session
            .check()
            .expect("capacity overflow must not poison the session");
        assert_eq!(checkpoint.manifest.wal_region_bytes, 1024 * 1024);
        assert!(EmbeddedRdbArtifact::read_wal_payloads(&checkpoint)
            .expect("reclaimed WAL")
            .is_empty());
        session
            .append(&[vec![3]], options())
            .expect("durable retry after checkpoint");
    }

    #[test]
    fn writer_panic_wakes_waiters_and_rejects_pending_work() {
        let group = GroupCommit::default();
        let first = group.enqueue(&[vec![1]], options()).expect("first");
        let second = group.enqueue(&[vec![2]], options()).expect("second");
        let panic = std::panic::catch_unwind(|| {
            let _ = group.wait(&first, |_| panic!("injected IO panic"));
        });
        assert!(panic.is_err());
        assert!(group
            .wait(&second, |_| panic!("failed group must not write"))
            .is_err());
        assert!(!lock(&group.state).flushing);
    }

    #[test]
    fn batch_limits_preserve_indivisible_operations() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("bounds.rdb");
        EmbeddedRdbArtifact::create(&path).expect("artifact");
        let group = GroupCommit::default();
        let mut limits = options();
        limits.max_statements = 2;
        // Queue with loose admission limits; the first member determines each flush.
        let first = group.enqueue(&[vec![1], vec![2]], limits).expect("first");
        let second = group.enqueue(&[vec![3]], options()).expect("second");
        let third = group.enqueue(&[vec![4]], options()).expect("third");
        group
            .wait(&first, |payloads| {
                assert_eq!(payloads, &[vec![1], vec![2], vec![3]]);
                EmbeddedRdbArtifact::append_wal_payloads(&path, payloads)
            })
            .expect("bounded first batch");
        group
            .wait(&second, |_| panic!("already completed"))
            .expect("second durable");
        group
            .wait(&third, |payloads| {
                assert_eq!(payloads, &[vec![4]]);
                EmbeddedRdbArtifact::append_wal_payloads(&path, payloads)
            })
            .expect("remaining batch");
    }

    #[test]
    fn byte_limit_allows_one_oversized_operation_without_splitting_it() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("byte-bound.rdb");
        EmbeddedRdbArtifact::create(&path).expect("artifact");
        let group = GroupCommit::default();
        let mut limits = options();
        limits.max_wal_bytes = 1;
        let first = group
            .enqueue(&[vec![1; 100], vec![2; 100]], limits)
            .expect("indivisible oversized write");
        let second = group
            .enqueue(&[vec![3]], options())
            .expect("queued follower");
        group
            .wait(&first, |payloads| {
                assert_eq!(payloads, &[vec![1; 100], vec![2; 100]]);
                EmbeddedRdbArtifact::append_wal_payloads(&path, payloads)
            })
            .expect("first batch");
        group
            .wait(&second, |payloads| {
                assert_eq!(payloads, &[vec![3]]);
                EmbeddedRdbArtifact::append_wal_payloads(&path, payloads)
            })
            .expect("second batch");
    }

    #[test]
    fn uncertain_publication_fences_later_writes_and_checkpoints_until_session_closes() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("uncertain.rdb");
        EmbeddedRdbArtifact::create(&path).expect("artifact");
        let session = EmbeddedRdbArtifact::group_commit_session(&path);
        let group = coordinator(&path);
        let completion = group
            .enqueue(&[b"possibly committed".to_vec()], options())
            .expect("enqueue");
        let result = group.wait(&completion, |payloads| {
            EmbeddedRdbArtifact::append_wal_payloads(&path, payloads)?;
            Err(RdbFileError::Io(std::io::Error::other(
                "injected uncertain result after publication",
            )))
        });
        assert!(result.is_err());
        drop(group);
        assert!(session.check().is_err());
        assert!(EmbeddedRdbArtifact::append_wal_payloads_grouped(
            &path,
            &[b"must not ACK".to_vec()],
            options()
        )
        .is_err());
        assert!(EmbeddedRdbArtifact::write_snapshot(&path, b"RDST-rolled back image").is_err());
        drop(session);
        let reopened = EmbeddedRdbArtifact::open(&path).expect("reload durable outcome");
        assert_eq!(
            EmbeddedRdbArtifact::read_wal_payloads(&reopened).expect("outcome"),
            vec![b"possibly committed".to_vec()]
        );
        let session = EmbeddedRdbArtifact::group_commit_session(&path);
        session.check().expect("new session after reload");
        EmbeddedRdbArtifact::append_wal_payloads_grouped(
            &path,
            &[b"after reload".to_vec()],
            options(),
        )
        .expect("new durable write");
    }

    #[test]
    fn a_fenced_session_cannot_ack_a_concurrent_publication() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("interrupted.rdb");
        EmbeddedRdbArtifact::create(&path).expect("artifact");
        let session = EmbeddedRdbArtifact::group_commit_session(&path);
        let group = coordinator(&path);
        let completion = group
            .enqueue(&[b"in flight".to_vec()], options())
            .expect("enqueue");
        let result = group.wait(&completion, |payloads| {
            let published = EmbeddedRdbArtifact::append_wal_payloads(&path, payloads)?;
            session.fence_interrupted_write();
            Ok(published)
        });
        assert!(
            result.is_err(),
            "a concurrent fence forbids a successful acknowledgment"
        );
        assert!(session.check().is_err());
    }
}
