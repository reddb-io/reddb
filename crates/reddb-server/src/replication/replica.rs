//! Replica-side replication: connects to primary, consumes WAL records.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use crate::json::Value as JsonValue;
use crate::telemetry::admin_intent_log::{
    AdminIntentLog, IntentArgs, IntentHandle, IntentLogError, IntentOp, IntentPhase,
    IntentProgress, IntentSummary,
};

/// Replica replication state.
pub struct ReplicaReplication {
    pub primary_addr: String,
    pub last_applied_lsn: u64,
    pub poll_interval: Duration,
    pub connected: bool,
}

/// Result of staging one basebackup response from `replication_snapshot`.
#[derive(Debug, Clone)]
pub struct StagedBaseBackupChunk {
    pub manifest: reddb_file::PrimaryReplicaBaseBackupManifest,
    pub chunk_ordinal: Option<u32>,
    pub snapshot_offset: u64,
    pub next_snapshot_offset: Option<u64>,
    pub snapshot_complete: bool,
}

#[derive(Debug)]
pub enum ReplicaBaseBackupError {
    MissingField(&'static str),
    InvalidField(&'static str),
    Decode(String),
    File(reddb_file::RdbFileError),
    Io(std::io::Error),
}

impl fmt::Display for ReplicaBaseBackupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingField(field) => write!(f, "missing basebackup field {field}"),
            Self::InvalidField(field) => write!(f, "invalid basebackup field {field}"),
            Self::Decode(err) => write!(f, "decode basebackup payload: {err}"),
            Self::File(err) => write!(f, "{err}"),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for ReplicaBaseBackupError {}

impl From<reddb_file::RdbFileError> for ReplicaBaseBackupError {
    fn from(value: reddb_file::RdbFileError) -> Self {
        Self::File(value)
    }
}

impl From<std::io::Error> for ReplicaBaseBackupError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

/// Stage the basebackup chunk carried by one `replication_snapshot` response.
///
/// The wire payload carries a binary manifest plus at most one matching chunk
/// for the requested snapshot offset. This helper centralizes the validation:
/// manifest checksum, relative paths, per-chunk CRC, and atomic chunk write.
pub fn stage_basebackup_snapshot_chunk(
    payload: &reddb_wire::replication::BaseBackupChunk,
    parts_root: impl AsRef<Path>,
) -> Result<Option<StagedBaseBackupChunk>, ReplicaBaseBackupError> {
    if !payload.basebackup_available {
        return Ok(None);
    }

    let manifest_bytes =
        payload
            .basebackup_manifest
            .as_deref()
            .ok_or(ReplicaBaseBackupError::MissingField(
                "basebackup_manifest_hex",
            ))?;
    let manifest = reddb_file::PrimaryReplicaBaseBackupManifest::decode(&manifest_bytes)?;
    manifest.validate()?;

    let snapshot_offset = payload.snapshot_offset;
    let next_snapshot_offset = payload.next_snapshot_offset;
    let snapshot_complete = payload.snapshot_complete;

    let chunk_ordinal = match (payload.basebackup_chunk_ordinal, &payload.basebackup_chunk) {
        (Some(ordinal), Some(bytes)) => {
            if !manifest.chunks.iter().any(|chunk| chunk.ordinal == ordinal) {
                return Err(ReplicaBaseBackupError::InvalidField(
                    "basebackup_chunk_ordinal",
                ));
            }
            manifest.stage_chunk_part(parts_root.as_ref(), ordinal, bytes)?;
            Some(ordinal)
        }
        (None, None) => None,
        _ => {
            return Err(ReplicaBaseBackupError::InvalidField(
                "basebackup_chunk_ordinal/basebackup_chunk_hex",
            ));
        }
    };

    Ok(Some(StagedBaseBackupChunk {
        manifest,
        chunk_ordinal,
        snapshot_offset,
        next_snapshot_offset,
        snapshot_complete,
    }))
}

pub fn recover_staged_basebackup_chunks(
    manifest: &reddb_file::PrimaryReplicaBaseBackupManifest,
    parts_root: impl AsRef<Path>,
) -> Result<std::collections::BTreeSet<u32>, ReplicaBaseBackupError> {
    manifest
        .recover_staged_chunk_parts(parts_root)
        .map_err(Into::into)
}

impl ReplicaReplication {
    pub fn new(primary_addr: String, poll_interval_ms: u64) -> Self {
        Self {
            primary_addr,
            last_applied_lsn: 0,
            poll_interval: Duration::from_millis(poll_interval_ms),
            connected: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Bootstrap resumability via AdminIntentLog
// ---------------------------------------------------------------------------

/// Resume point recovered from a previously checkpointed bootstrap intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumePoint {
    pub last_applied_lsn: u64,
    pub snapshot_token: Option<String>,
    pub snapshot_offset: u64,
}

/// Manages bootstrap lifecycle using [`AdminIntentLog`] for crash-resumability.
///
/// # Single-resumer policy
///
/// Each node only resumes its own intents (`args.replica_id == node_id`).
/// If multiple unfinished intents exist for this node (unexpected), none is
/// resumed — a fresh bootstrap is started and the dangling intents are left for
/// operator investigation via [`crate::telemetry::operator_event::OperatorEvent::DanglingAdminIntent`].
pub struct ReplicaBootstrapper {
    node_id: String,
}

impl ReplicaBootstrapper {
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
        }
    }

    /// Scan `log` for unfinished bootstrap intents.
    ///
    /// Calls [`AdminIntentLog::scan_and_report`] first — this emits a
    /// `DanglingAdminIntent` operator event for every unfinished intent.
    /// Then applies the single-resumer policy: returns a [`ResumePoint`] only
    /// if exactly one unfinished `ReplicaBootstrap` intent for this `node_id`
    /// exists with at least one checkpoint record carrying `last_applied_lsn`.
    pub fn scan_for_resume(&self, log: &AdminIntentLog) -> Option<ResumePoint> {
        log.scan_and_report();
        let mut mine: Vec<_> = log
            .list_unfinished()
            .into_iter()
            .filter(|u| {
                u.op == IntentOp::ReplicaBootstrap
                    && u.args.get("replica_id").and_then(|v| v.as_str())
                        == Some(self.node_id.as_str())
            })
            .collect();

        if mine.len() != 1 {
            return None;
        }

        let item = mine.remove(0);
        item.last_progress
            .as_ref()
            .and_then(resume_point_from_progress)
    }

    /// Resume one unfinished bootstrap intent and return a handle that appends
    /// progress to the original intent id.
    pub fn resume<'a>(
        &self,
        log: &'a AdminIntentLog,
    ) -> Option<(ResumePoint, BootstrapHandle<'a>)> {
        log.scan_and_report();

        let mut mine: Vec<_> = log
            .list_unfinished()
            .into_iter()
            .filter(|u| {
                u.op == IntentOp::ReplicaBootstrap
                    && u.args.get("replica_id").and_then(|v| v.as_str())
                        == Some(self.node_id.as_str())
            })
            .collect();

        if mine.len() != 1 {
            return None;
        }

        let item = mine.remove(0);
        let progress = item.last_progress.as_ref()?;
        let resume = resume_point_from_progress(progress)?;
        let checkpoint_n = match item.last_phase {
            IntentPhase::Checkpoint(n) => n,
            _ => 0,
        };
        let handle = log.resume_unfinished(&item);

        Some((
            resume.clone(),
            BootstrapHandle {
                handle,
                checkpoint_n,
                last_applied_lsn: resume.last_applied_lsn,
            },
        ))
    }
}

fn resume_point_from_progress(
    progress: &crate::json::Map<String, JsonValue>,
) -> Option<ResumePoint> {
    let lsn = progress
        .get("last_applied_lsn")
        .and_then(|v| v.as_f64())
        .map(|f| f as u64)
        .unwrap_or(0);
    let snapshot_token = progress
        .get("snapshot_cursor")
        .or_else(|| progress.get("snapshot_token"))
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned);
    let snapshot_offset = progress
        .get("snapshot_offset")
        .and_then(|v| v.as_f64())
        .map(|f| f as u64)
        .unwrap_or(0);

    Some(ResumePoint {
        last_applied_lsn: lsn,
        snapshot_token,
        snapshot_offset,
    })
}

impl ReplicaBootstrapper {
    /// Begin a fresh bootstrap intent.
    ///
    /// `source_lsn`: LSN at the primary when bootstrap starts.
    /// `target_lsn_hint`: expected completion LSN (informational).
    pub fn begin<'a>(
        &self,
        log: &'a AdminIntentLog,
        source_lsn: u64,
        target_lsn_hint: u64,
    ) -> Result<BootstrapHandle<'a>, IntentLogError> {
        let args = IntentArgs::new()
            .insert("replica_id", JsonValue::String(self.node_id.clone()))
            .insert("source_lsn", JsonValue::Number(source_lsn as f64))
            .insert("target_lsn_hint", JsonValue::Number(target_lsn_hint as f64));
        let handle = log.begin(IntentOp::ReplicaBootstrap, &self.node_id, args)?;
        Ok(BootstrapHandle {
            handle,
            checkpoint_n: 0,
            last_applied_lsn: 0,
        })
    }
}

/// Active bootstrap handle. Call [`BootstrapHandle::checkpoint`] periodically
/// during catchup. Call [`BootstrapHandle::complete`] on success.
///
/// Dropping without calling `complete` writes `aborted` to the intent log
/// (guaranteed by [`IntentHandle`]'s `Drop` impl).
pub struct BootstrapHandle<'a> {
    handle: IntentHandle<'a>,
    checkpoint_n: u32,
    last_applied_lsn: u64,
}

impl<'a> BootstrapHandle<'a> {
    pub fn last_applied_lsn(&self) -> u64 {
        self.last_applied_lsn
    }

    /// Write a checkpoint with current progress. Checkpoint number auto-increments.
    pub fn checkpoint(
        &mut self,
        last_applied_lsn: u64,
        batches_applied: u64,
    ) -> Result<(), IntentLogError> {
        self.checkpoint_n += 1;
        let progress = IntentProgress::new()
            .insert(
                "last_applied_lsn",
                JsonValue::Number(last_applied_lsn as f64),
            )
            .insert("batches_applied", JsonValue::Number(batches_applied as f64));
        self.handle.checkpoint(self.checkpoint_n, Some(progress))?;
        self.last_applied_lsn = last_applied_lsn;
        Ok(())
    }

    /// Checkpoint an in-flight snapshot transfer so an interrupted bootstrap
    /// can resume from the last persisted byte offset instead of restarting
    /// from zero (issue #830).
    ///
    /// The snapshot token is stored under `snapshot_cursor` because
    /// [`AdminIntentLog`] redacts progress keys containing `token`; the public
    /// [`ResumePoint`] still surfaces it as `snapshot_token` to callers, which
    /// also read the legacy `snapshot_token` key as a fallback.
    pub fn checkpoint_snapshot_transfer(
        &mut self,
        snapshot_token: impl Into<String>,
        snapshot_offset: u64,
        last_applied_lsn: u64,
        batches_applied: u64,
    ) -> Result<(), IntentLogError> {
        self.checkpoint_n += 1;
        let progress = IntentProgress::new()
            .insert("snapshot_cursor", JsonValue::String(snapshot_token.into()))
            .insert("snapshot_offset", JsonValue::Number(snapshot_offset as f64))
            .insert(
                "last_applied_lsn",
                JsonValue::Number(last_applied_lsn as f64),
            )
            .insert("batches_applied", JsonValue::Number(batches_applied as f64));
        self.handle.checkpoint(self.checkpoint_n, Some(progress))?;
        self.last_applied_lsn = last_applied_lsn;
        Ok(())
    }

    /// Mark bootstrap complete. Consumes the handle.
    pub fn complete(self, total_records: u64, duration_ms: u64) -> Result<(), IntentLogError> {
        let summary = IntentSummary::new()
            .insert("total_records", JsonValue::Number(total_records as f64))
            .insert("duration_ms", JsonValue::Number(duration_ms as f64));
        self.handle.complete(Some(summary))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmp_path(label: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "reddb-bootstrap-{}-{}.log",
            label,
            std::process::id()
        ));
        p
    }

    fn open_log(path: &PathBuf) -> AdminIntentLog {
        AdminIntentLog::open(path).expect("open intent log")
    }

    #[test]
    fn rebootstrap_ready_marker_write_is_atomic_and_readable() {
        let mut data_path = std::env::temp_dir();
        data_path.push(format!(
            "reddb-rebootstrap-marker-{}.rdb",
            std::process::id()
        ));
        let marker_path = reddb_file::layout::rebootstrap_ready_marker_path(&data_path);
        let tmp_path = reddb_file::layout::atomic_temp_path(&marker_path);
        let pending_path = reddb_file::layout::rebootstrap_pending_path(&data_path);
        let _ = fs::remove_file(&marker_path);
        let _ = fs::remove_file(&tmp_path);

        reddb_file::write_rebootstrap_ready_marker(
            &data_path,
            &reddb_file::ReplicaRebootstrapReadyMarker {
                pending_path: pending_path.clone(),
                checkpoint_lsn: 7,
                timeline: reddb_file::TimelineId::initial(),
            },
        )
        .expect("write marker");
        let ready = reddb_file::read_rebootstrap_ready_marker(&data_path).expect("read marker");
        assert_eq!(ready.checkpoint_lsn, 7);
        assert_eq!(ready.pending_path, pending_path);

        fs::write(&tmp_path, b"stale tmp").expect("write stale tmp");
        reddb_file::write_rebootstrap_ready_marker(
            &data_path,
            &reddb_file::ReplicaRebootstrapReadyMarker {
                pending_path: reddb_file::layout::rebootstrap_pending_path(&data_path),
                checkpoint_lsn: 8,
                timeline: reddb_file::TimelineId(3),
            },
        )
        .expect("replace marker");
        let ready =
            reddb_file::read_rebootstrap_ready_marker(&data_path).expect("read replaced marker");
        assert_eq!(ready.checkpoint_lsn, 8);
        assert_eq!(ready.timeline, reddb_file::TimelineId(3));

        let _ = fs::remove_file(&marker_path);
        let _ = fs::remove_file(&tmp_path);
    }

    // -----------------------------------------------------------------------
    // 1. From-scratch: no unfinished intent → scan_for_resume returns None
    // -----------------------------------------------------------------------
    #[test]
    fn bootstrap_from_scratch_when_no_unfinished_intent() {
        let path = tmp_path("fresh");
        let log = open_log(&path);
        let bootstrapper = ReplicaBootstrapper::new("replica-1");

        assert!(bootstrapper.scan_for_resume(&log).is_none());

        let handle = bootstrapper.begin(&log, 0, 1000).unwrap();
        handle.complete(500, 100).unwrap();

        // Completed intent → no resume point on next boot
        let log2 = open_log(&path);
        assert!(bootstrapper.scan_for_resume(&log2).is_none());

        let _ = std::fs::remove_file(&path);
    }

    // -----------------------------------------------------------------------
    // 2. Crash mid-catchup (mem::forget simulates no-Drop) → resume from lsn
    // -----------------------------------------------------------------------
    #[test]
    fn resume_from_checkpoint_after_crash() {
        let path = tmp_path("resume");
        let bootstrapper = ReplicaBootstrapper::new("replica-A");

        // Phase 1: start, checkpoint at lsn=500, then "crash" (no Drop)
        {
            let log = open_log(&path);
            let mut handle = bootstrapper.begin(&log, 0, 1000).unwrap();
            handle.checkpoint(500, 10).unwrap();
            std::mem::forget(handle);
        }

        // Phase 2: restart — resume at lsn=500, then continue to completion
        {
            let log2 = open_log(&path);
            let resume = bootstrapper.scan_for_resume(&log2).expect("should resume");
            assert_eq!(resume.last_applied_lsn, 500);

            let mut handle = bootstrapper.begin(&log2, 500, 1000).unwrap();
            handle.checkpoint(1000, 20).unwrap();
            handle.complete(1000, 250).unwrap();
        }

        let _ = std::fs::remove_file(&path);
    }

    // -----------------------------------------------------------------------
    // 3. Multi-replica isolation: each node sees only its own intent
    // -----------------------------------------------------------------------
    #[test]
    fn multi_replica_isolation() {
        let path = tmp_path("multi");
        let log = open_log(&path);

        let b1 = ReplicaBootstrapper::new("replica-1");
        let b2 = ReplicaBootstrapper::new("replica-2");
        let b3 = ReplicaBootstrapper::new("replica-3");

        let mut h1 = b1.begin(&log, 0, 1000).unwrap();
        h1.checkpoint(300, 5).unwrap();
        std::mem::forget(h1);

        let mut h2 = b2.begin(&log, 0, 2000).unwrap();
        h2.checkpoint(700, 12).unwrap();
        std::mem::forget(h2);

        let log2 = open_log(&path);
        let r1 = b1.scan_for_resume(&log2).map(|r| r.last_applied_lsn);
        let r2 = b2.scan_for_resume(&log2).map(|r| r.last_applied_lsn);
        let r3 = b3.scan_for_resume(&log2);

        assert_eq!(r1, Some(300), "replica-1 resumes at 300");
        assert_eq!(r2, Some(700), "replica-2 resumes at 700");
        assert!(r3.is_none(), "replica-3 has no intent");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn resume_from_snapshot_transfer_checkpoint_after_crash() {
        let path = tmp_path("snapshot-resume");
        let bootstrapper = ReplicaBootstrapper::new("replica-snapshot");

        {
            let log = open_log(&path);
            let mut handle = bootstrapper.begin(&log, 10, 1000).unwrap();
            handle
                .checkpoint_snapshot_transfer("snapshot-token-10", 4096, 10, 0)
                .unwrap();
            std::mem::forget(handle);
        }

        {
            let log2 = open_log(&path);
            let resume = bootstrapper.scan_for_resume(&log2).expect("should resume");
            assert_eq!(resume.last_applied_lsn, 10);
            assert_eq!(resume.snapshot_token.as_deref(), Some("snapshot-token-10"));
            assert_eq!(resume.snapshot_offset, 4096);
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn resume_snapshot_transfer_completes_original_intent() {
        let path = tmp_path("snapshot-resume-complete");
        let bootstrapper = ReplicaBootstrapper::new("replica-snapshot-complete");

        {
            let log = open_log(&path);
            let mut handle = bootstrapper.begin(&log, 10, 1000).unwrap();
            handle
                .checkpoint_snapshot_transfer("snapshot-token-10", 4096, 10, 1)
                .unwrap();
            std::mem::forget(handle);
        }

        {
            let log = open_log(&path);
            let (resume, mut handle) = bootstrapper.resume(&log).expect("resume handle");
            assert_eq!(resume.last_applied_lsn, 10);
            assert_eq!(resume.snapshot_token.as_deref(), Some("snapshot-token-10"));
            assert_eq!(resume.snapshot_offset, 4096);

            handle
                .checkpoint_snapshot_transfer("snapshot-token-10", 8192, 10, 2)
                .unwrap();
            handle.complete(2, 25).unwrap();
        }

        let log = open_log(&path);
        assert!(
            log.list_unfinished().is_empty(),
            "resumed handle must complete the original dangling intent"
        );

        let _ = std::fs::remove_file(&path);
    }

    // -----------------------------------------------------------------------
    // 4. Drop without complete → aborted (terminal) → list_unfinished empty
    // -----------------------------------------------------------------------
    #[test]
    fn drop_without_complete_writes_aborted() {
        let path = tmp_path("abort");
        let log = open_log(&path);
        let bootstrapper = ReplicaBootstrapper::new("replica-X");

        {
            let mut handle = bootstrapper.begin(&log, 0, 1000).unwrap();
            handle.checkpoint(100, 2).unwrap();
            // drop → aborted written by IntentHandle::Drop
        }

        let log2 = open_log(&path);
        assert_eq!(log2.list_unfinished().len(), 0, "aborted is terminal");

        let _ = std::fs::remove_file(&path);
    }

    // -----------------------------------------------------------------------
    // 5. Success path: complete writes completed phase → no unfinished intents
    // -----------------------------------------------------------------------
    #[test]
    fn bootstrap_success_completes_intent() {
        let path = tmp_path("success");
        let log = open_log(&path);
        let bootstrapper = ReplicaBootstrapper::new("replica-Y");

        let mut handle = bootstrapper.begin(&log, 0, 500).unwrap();
        handle.checkpoint(250, 5).unwrap();
        handle.checkpoint(500, 10).unwrap();
        handle.complete(1000, 300).unwrap();

        let log2 = open_log(&path);
        assert_eq!(log2.list_unfinished().len(), 0, "completed is terminal");

        let _ = std::fs::remove_file(&path);
    }

    // -----------------------------------------------------------------------
    // 6. No resume when intent crashed before any checkpoint
    // -----------------------------------------------------------------------
    #[test]
    fn no_resume_when_no_checkpoint_progress() {
        let path = tmp_path("no-progress");
        let log = open_log(&path);
        let bootstrapper = ReplicaBootstrapper::new("replica-Z");

        // Crash before any checkpoint — no progress in the intent log
        let handle = bootstrapper.begin(&log, 0, 1000).unwrap();
        std::mem::forget(handle);

        let log2 = open_log(&path);
        let resume = bootstrapper.scan_for_resume(&log2);
        assert!(resume.is_none(), "no checkpoint → no resume point");

        let _ = std::fs::remove_file(&path);
    }
}
