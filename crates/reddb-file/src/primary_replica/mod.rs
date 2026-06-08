//! Primary-replica file planning.
//!
//! The primary-replica profile treats WAL as a shipped, retained timeline.
//! Base backups provide bounded catch-up points; replicas then consume WAL
//! segments after the backup LSN.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::embedded::{RdbFileError, RdbFileResult};

mod basebackup;
mod rebootstrap;
mod relay;
mod slots;
mod timeline;
mod wal;

pub use basebackup::*;
pub use rebootstrap::*;
pub use relay::*;
pub use slots::*;
pub use timeline::*;
pub use wal::*;

const PRIMARY_WAL_SEGMENT_MAGIC: &[u8; 8] = b"RDPWAL01";
const PRIMARY_BASEBACKUP_MAGIC: &[u8; 8] = b"RDBASE01";
const REPLICATION_SLOT_CATALOG_MAGIC: &[u8; 8] = b"RDSLOT01";
const RELAY_LOG_MANIFEST_MAGIC: &[u8; 8] = b"RDRELAY1";
const RELAY_LOG_SEGMENT_MAGIC: &[u8; 8] = b"RDRSEG01";
const TIMELINE_HISTORY_MAGIC: &[u8; 8] = b"RDTLINE1";
const PRIMARY_REPLICA_ARTIFACT_VERSION: u16 = 1;
const WAL_RECORD_MAGIC: &[u8; 8] = b"RDWREC01";
const WAL_RECORD_HEADER_BYTES: usize = 8 + 2 + 2 + 8 + 8 + 4 + 4 + 4 + 4;
const CHECKSUM_LEN: usize = 4;
const PRIMARY_REPLICA_CRASH_INJECT_ENV: &str = "REDDB_PRIMARY_REPLICA_CRASH_AT";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimaryReplicaFilePlan {
    pub root: PathBuf,
    pub timeline: TimelineId,
    pub segment_bytes: u64,
    pub retention: WalRetentionPolicy,
}

impl PrimaryReplicaFilePlan {
    pub fn new(root: impl Into<PathBuf>, timeline: TimelineId) -> Self {
        Self {
            root: root.into(),
            timeline,
            segment_bytes: 64 * 1024 * 1024,
            retention: WalRetentionPolicy::default(),
        }
    }

    pub fn with_segment_bytes(mut self, bytes: u64) -> Self {
        self.segment_bytes = bytes.max(1024 * 1024);
        self
    }

    pub fn with_retention(mut self, retention: WalRetentionPolicy) -> Self {
        self.retention = retention;
        self
    }

    pub fn timeline_dir(&self) -> PathBuf {
        self.root.join(format!("timeline-{:020}", self.timeline.0))
    }

    pub fn wal_dir(&self) -> PathBuf {
        self.timeline_dir().join("wal")
    }

    pub fn basebackup_dir(&self) -> PathBuf {
        self.timeline_dir().join("basebackup")
    }

    pub fn slots_path(&self) -> PathBuf {
        self.timeline_dir().join("replication-slots.redslots")
    }

    pub fn relay_dir(&self, replica_id: &str) -> PathBuf {
        self.timeline_dir().join("relay").join(replica_id)
    }

    pub fn relay_manifest_path(&self, replica_id: &str) -> PathBuf {
        self.relay_dir(replica_id).join("relay.redmanifest")
    }

    pub fn timeline_history_path(&self) -> PathBuf {
        self.root.join("timeline-history.redtimeline")
    }

    pub fn wal_segment_index(&self, lsn: u64) -> u64 {
        lsn / self.segment_bytes
    }

    pub fn wal_segment_start_lsn(&self, segment_index: u64) -> u64 {
        segment_index.saturating_mul(self.segment_bytes)
    }

    pub fn wal_segment_end_lsn(&self, segment_index: u64) -> u64 {
        segment_index
            .saturating_add(1)
            .saturating_mul(self.segment_bytes)
    }

    pub fn wal_segment_path(&self, lsn: u64) -> PathBuf {
        self.wal_dir()
            .join(crate::layout::primary_wal_segment_file_name(
                self.wal_segment_index(lsn),
            ))
    }

    pub fn basebackup_path(&self, backup: &BaseBackupPlan) -> PathBuf {
        self.basebackup_dir().join(format!(
            "base-{:020}-{:020}.redbase",
            backup.start_lsn, backup.checkpoint_lsn
        ))
    }
}

fn write_bytes_atomically(path: &Path, bytes: &[u8]) -> RdbFileResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp_path = crate::layout::atomic_temp_path(path);
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp_path)?;
        file.write_all(bytes)?;
        crash_inject("atomic_after_tmp_write");
        file.sync_all()?;
        crash_inject("atomic_after_tmp_sync");
    }
    fs::rename(&tmp_path, path)?;
    crash_inject("atomic_after_rename");
    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    crash_inject("atomic_after_dir_sync");
    Ok(())
}

fn crash_inject(point: &str) {
    if std::env::var(PRIMARY_REPLICA_CRASH_INJECT_ENV)
        .ok()
        .as_deref()
        == Some(point)
    {
        std::process::exit(173);
    }
}

fn now_unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn verify_checksum(bytes: &[u8], artifact: &str) -> RdbFileResult<()> {
    let Some(checksum_offset) = bytes.len().checked_sub(CHECKSUM_LEN) else {
        return Err(RdbFileError::InvalidOperation(format!(
            "{artifact} is too short"
        )));
    };
    let stored = u32::from_le_bytes(bytes[checksum_offset..].try_into().unwrap());
    let computed = crc32(&bytes[..checksum_offset]);
    if stored != computed {
        return Err(RdbFileError::InvalidOperation(format!(
            "{artifact} checksum mismatch: stored {stored:#010x}, computed {computed:#010x}"
        )));
    }
    Ok(())
}

fn crc32(data: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(data);
    hasher.finalize()
}

fn expect_magic(
    bytes: &[u8],
    cursor: &mut usize,
    payload_end: usize,
    magic: &[u8],
    artifact: &str,
) -> RdbFileResult<()> {
    let actual = take_bytes(bytes, cursor, payload_end, magic.len())?;
    if actual != magic {
        return Err(RdbFileError::InvalidOperation(format!(
            "invalid {artifact} magic"
        )));
    }
    Ok(())
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u128(out: &mut Vec<u8>, value: u128) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_string(out: &mut Vec<u8>, value: &str) {
    put_u32(out, value.len() as u32);
    out.extend_from_slice(value.as_bytes());
}

fn put_optional_string(out: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            out.push(1);
            put_string(out, value);
        }
        None => out.push(0),
    }
}

fn put_optional_u128(out: &mut Vec<u8>, value: Option<u128>) {
    match value {
        Some(value) => {
            out.push(1);
            put_u128(out, value);
        }
        None => out.push(0),
    }
}

fn take_bytes<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
    payload_end: usize,
    len: usize,
) -> RdbFileResult<&'a [u8]> {
    let end = cursor
        .checked_add(len)
        .ok_or_else(|| RdbFileError::InvalidOperation("primary-replica cursor overflow".into()))?;
    if end > payload_end {
        return Err(RdbFileError::InvalidOperation(
            "primary-replica artifact truncated".into(),
        ));
    }
    let value = &bytes[*cursor..end];
    *cursor = end;
    Ok(value)
}

fn take_u16(bytes: &[u8], cursor: &mut usize, payload_end: usize) -> RdbFileResult<u16> {
    Ok(u16::from_le_bytes(
        take_bytes(bytes, cursor, payload_end, 2)?
            .try_into()
            .unwrap(),
    ))
}

fn take_u8(bytes: &[u8], cursor: &mut usize, payload_end: usize) -> RdbFileResult<u8> {
    Ok(take_bytes(bytes, cursor, payload_end, 1)?[0])
}

fn take_u32(bytes: &[u8], cursor: &mut usize, payload_end: usize) -> RdbFileResult<u32> {
    Ok(u32::from_le_bytes(
        take_bytes(bytes, cursor, payload_end, 4)?
            .try_into()
            .unwrap(),
    ))
}

fn take_u64(bytes: &[u8], cursor: &mut usize, payload_end: usize) -> RdbFileResult<u64> {
    Ok(u64::from_le_bytes(
        take_bytes(bytes, cursor, payload_end, 8)?
            .try_into()
            .unwrap(),
    ))
}

fn take_u128(bytes: &[u8], cursor: &mut usize, payload_end: usize) -> RdbFileResult<u128> {
    Ok(u128::from_le_bytes(
        take_bytes(bytes, cursor, payload_end, 16)?
            .try_into()
            .unwrap(),
    ))
}

fn take_string(bytes: &[u8], cursor: &mut usize, payload_end: usize) -> RdbFileResult<String> {
    let len = take_u32(bytes, cursor, payload_end)? as usize;
    let raw = take_bytes(bytes, cursor, payload_end, len)?;
    std::str::from_utf8(raw)
        .map(|value| value.to_string())
        .map_err(|err| RdbFileError::InvalidOperation(format!("invalid utf-8 string: {err}")))
}

fn take_optional_string(
    bytes: &[u8],
    cursor: &mut usize,
    payload_end: usize,
) -> RdbFileResult<Option<String>> {
    match take_u8(bytes, cursor, payload_end)? {
        0 => Ok(None),
        1 => take_string(bytes, cursor, payload_end).map(Some),
        tag => Err(RdbFileError::InvalidOperation(format!(
            "invalid optional string tag {tag}"
        ))),
    }
}

fn take_optional_u128(
    bytes: &[u8],
    cursor: &mut usize,
    payload_end: usize,
) -> RdbFileResult<Option<u128>> {
    match take_u8(bytes, cursor, payload_end)? {
        0 => Ok(None),
        1 => take_u128(bytes, cursor, payload_end).map(Some),
        tag => Err(RdbFileError::InvalidOperation(format!(
            "invalid optional u128 tag {tag}"
        ))),
    }
}

fn reject_trailing_bytes(
    bytes: &[u8],
    cursor: usize,
    payload_end: usize,
    artifact: &str,
) -> RdbFileResult<()> {
    if cursor != payload_end || payload_end + CHECKSUM_LEN != bytes.len() {
        return Err(RdbFileError::InvalidOperation(format!(
            "{artifact} has trailing bytes"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wal_segment_paths_are_timeline_scoped() {
        let plan = PrimaryReplicaFilePlan::new("/var/lib/reddb", TimelineId(3))
            .with_segment_bytes(1024 * 1024);
        assert_eq!(
            plan.wal_segment_path(2_500_000),
            PathBuf::from(
                "/var/lib/reddb/timeline-00000000000000000003/wal/00000000000000000002.redwal"
            )
        );
    }

    #[test]
    fn catchup_mode_uses_retention_floor() {
        let plan = PrimaryReplicaFilePlan::new("/var/lib/reddb", TimelineId::initial());
        assert_eq!(plan.catchup_mode(1_000, 1_000), ReplicaCatchupMode::WalOnly);
        assert_eq!(
            plan.catchup_mode(1_000, 999),
            ReplicaCatchupMode::BaseBackupThenWal
        );
    }

    #[test]
    fn catchup_mode_uses_basebackup_or_reclone_when_wal_gap_exists() {
        let plan = PrimaryReplicaFilePlan::new("/var/lib/reddb", TimelineId(1));
        let stale = PrimaryReplicaBaseBackupManifest::new(
            BaseBackupPlan::new(TimelineId(1), 100, 900),
            "stale.rdb",
            10,
            0x01,
        )
        .expect("stale backup");
        let usable = PrimaryReplicaBaseBackupManifest::new(
            BaseBackupPlan::new(TimelineId(1), 100, 1_100),
            "usable.rdb",
            10,
            0x02,
        )
        .expect("usable backup");
        let wrong_timeline = PrimaryReplicaBaseBackupManifest::new(
            BaseBackupPlan::new(TimelineId(2), 100, 2_000),
            "wrong.rdb",
            10,
            0x03,
        )
        .expect("wrong timeline backup");

        assert_eq!(
            plan.catchup_mode_with_basebackups(1_000, 1_000, &[usable.clone()]),
            ReplicaCatchupMode::WalOnly
        );
        assert_eq!(
            plan.catchup_mode_with_basebackups(
                1_000,
                999,
                &[stale.clone(), usable.clone(), wrong_timeline]
            ),
            ReplicaCatchupMode::BaseBackupThenWal
        );
        assert_eq!(
            plan.select_basebackup_for_catchup(1_000, &[stale.clone(), usable.clone()]),
            Some(&usable)
        );
        assert_eq!(
            plan.catchup_mode_with_basebackups(1_000, 999, &[stale]),
            ReplicaCatchupMode::Reclone
        );
    }

    #[test]
    fn timeline_history_round_trips_and_tracks_forks() {
        let mut history = TimelineHistory::new(10);
        history
            .fork(TimelineId(2), TimelineId(1), 1_500, 20, "promote replica-a")
            .expect("fork timeline");

        assert_eq!(history.current(), Some(TimelineId(2)));
        assert_eq!(history.ancestor_lsn(TimelineId(2)), Some(1_500));

        let decoded = TimelineHistory::decode(&history.encode()).expect("decode timeline history");
        assert_eq!(decoded, history);
    }

    #[test]
    fn timeline_history_rejects_semantically_invalid_but_checksummed_payloads() {
        let mut empty = Vec::new();
        empty.extend_from_slice(TIMELINE_HISTORY_MAGIC);
        put_u16(&mut empty, PRIMARY_REPLICA_ARTIFACT_VERSION);
        put_u32(&mut empty, 0);
        let empty_crc = crc32(&empty);
        put_u32(&mut empty, empty_crc);
        assert!(TimelineHistory::decode(&empty)
            .expect_err("empty timeline history must be rejected")
            .to_string()
            .contains("initial entry"));

        let mut wrong_initial = Vec::new();
        wrong_initial.extend_from_slice(TIMELINE_HISTORY_MAGIC);
        put_u16(&mut wrong_initial, PRIMARY_REPLICA_ARTIFACT_VERSION);
        put_u32(&mut wrong_initial, 1);
        put_u64(&mut wrong_initial, 2);
        wrong_initial.push(0);
        put_u64(&mut wrong_initial, 0);
        put_u64(&mut wrong_initial, 0);
        put_u64(&mut wrong_initial, 10);
        put_string(&mut wrong_initial, "not initial");
        let wrong_initial_crc = crc32(&wrong_initial);
        put_u32(&mut wrong_initial, wrong_initial_crc);
        assert!(TimelineHistory::decode(&wrong_initial)
            .expect_err("timeline history must start at timeline 1")
            .to_string()
            .contains("initial timeline"));

        let mut non_increasing = TimelineHistory::new(10).encode();
        non_increasing.truncate(non_increasing.len() - CHECKSUM_LEN);
        non_increasing[10..14].copy_from_slice(&2u32.to_le_bytes());
        put_u64(&mut non_increasing, 1);
        non_increasing.push(1);
        put_u64(&mut non_increasing, 1);
        put_u64(&mut non_increasing, 42);
        put_u64(&mut non_increasing, 11);
        put_string(&mut non_increasing, "bad fork");
        let non_increasing_crc = crc32(&non_increasing);
        put_u32(&mut non_increasing, non_increasing_crc);
        assert!(TimelineHistory::decode(&non_increasing)
            .expect_err("child timeline must advance")
            .to_string()
            .contains("must be greater"));
    }

    #[test]
    fn timeline_history_writes_and_reads_from_disk() {
        let root = temp_root("timeline-history");
        let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(2));
        let mut history = TimelineHistory::new(10);
        history
            .fork(TimelineId(2), TimelineId(1), 1_500, 20, "promote replica-a")
            .expect("fork timeline");

        history
            .write_to_path(plan.timeline_history_path())
            .expect("write timeline history");
        assert_eq!(
            TimelineHistory::read_from_path(plan.timeline_history_path())
                .expect("read timeline history"),
            history
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn promotion_candidate_selects_most_applied_replica() {
        let acks = vec![
            ReplicaAck::with_positions("replica-b", TimelineId(1), 500, 500, 500, 300)
                .expect("ack"),
            ReplicaAck::with_positions("replica-a", TimelineId(1), 700, 650, 650, 450)
                .expect("ack"),
            ReplicaAck::with_positions("replica-c", TimelineId(2), 900, 900, 900, 900)
                .expect("ack"),
        ];

        let candidate =
            PromotionCandidate::select(TimelineId(1), &acks).expect("promotion candidate");
        assert_eq!(candidate.replica_id, "replica-a");
        assert_eq!(candidate.applied_lsn, 450);
    }

    #[test]
    fn promotion_history_forks_from_candidate_apply_lsn() {
        let history = TimelineHistory::new(10);
        let candidate = PromotionCandidate {
            replica_id: "replica-a".into(),
            timeline: TimelineId(1),
            received_lsn: 700,
            flushed_lsn: 650,
            applied_lsn: 450,
        };

        let promoted = history
            .promotion_history(&candidate, TimelineId(2), 20)
            .expect("promotion history");
        assert_eq!(promoted.current(), Some(TimelineId(2)));
        assert_eq!(promoted.ancestor_lsn(TimelineId(2)), Some(450));
    }

    #[test]
    fn timeline_history_returns_descendant_chain_after_multiple_promotions() {
        let mut history = TimelineHistory::new(10);
        history
            .fork(TimelineId(2), TimelineId(1), 1_000, 20, "promote replica-a")
            .expect("first promotion");
        history
            .fork(TimelineId(3), TimelineId(2), 1_500, 30, "promote replica-b")
            .expect("second promotion");

        let from_initial = history
            .descendant_chain_from(TimelineId(1))
            .expect("chain from initial timeline");
        assert_eq!(from_initial.len(), 2);
        assert_eq!(from_initial[0].timeline, TimelineId(2));
        assert_eq!(from_initial[0].fork_lsn, 1_000);
        assert_eq!(from_initial[1].timeline, TimelineId(3));
        assert_eq!(from_initial[1].fork_lsn, 1_500);

        let from_second = history
            .descendant_chain_from(TimelineId(2))
            .expect("chain from second timeline");
        assert_eq!(from_second.len(), 1);
        assert_eq!(from_second[0].timeline, TimelineId(3));
        assert_eq!(
            history.descendant_chain_from(TimelineId(3)),
            Some(Vec::new())
        );
        assert_eq!(history.descendant_chain_from(TimelineId(99)), None);
    }

    #[test]
    fn rejoin_decision_handles_rewind_follow_and_reclone() {
        let mut history = TimelineHistory::new(10);
        history
            .fork(TimelineId(2), TimelineId(1), 1_000, 20, "promote replica-a")
            .expect("fork timeline");

        assert_eq!(
            history.rejoin_decision(TimelineId(2), 1_200, 900),
            RejoinDecision::AlreadyCurrent
        );
        assert_eq!(
            history.rejoin_decision(TimelineId(1), 1_200, 900),
            RejoinDecision::Rewind {
                target_timeline: TimelineId(2),
                rewind_to_lsn: 1_000
            }
        );
        assert_eq!(
            history.rejoin_decision(TimelineId(1), 950, 900),
            RejoinDecision::FollowNewTimeline {
                target_timeline: TimelineId(2),
                start_lsn: 950
            }
        );
        assert_eq!(
            history.rejoin_decision(TimelineId(1), 850, 900),
            RejoinDecision::Reclone
        );
    }

    #[test]
    fn rejoin_decision_targets_current_timeline_across_multiple_promotions() {
        let mut history = TimelineHistory::new(10);
        history
            .fork(TimelineId(2), TimelineId(1), 1_000, 20, "promote replica-a")
            .expect("first promotion");
        history
            .fork(TimelineId(3), TimelineId(2), 1_500, 30, "promote replica-b")
            .expect("second promotion");

        assert_eq!(
            history.rejoin_decision(TimelineId(1), 1_200, 900),
            RejoinDecision::Rewind {
                target_timeline: TimelineId(3),
                rewind_to_lsn: 1_000
            },
            "old timeline node rewinds at the first fork but rejoins the current timeline"
        );
        assert_eq!(
            history.rejoin_decision(TimelineId(1), 950, 900),
            RejoinDecision::FollowNewTimeline {
                target_timeline: TimelineId(3),
                start_lsn: 950
            },
            "node before the first fork follows WAL toward the current timeline"
        );
        assert_eq!(
            history.rejoin_decision(TimelineId(2), 1_600, 900),
            RejoinDecision::Rewind {
                target_timeline: TimelineId(3),
                rewind_to_lsn: 1_500
            },
            "intermediate timeline node uses the fork from its own timeline"
        );
    }

    #[test]
    fn basebackup_names_include_start_and_checkpoint_lsn() {
        let plan = PrimaryReplicaFilePlan::new("/var/lib/reddb", TimelineId(2));
        let backup = BaseBackupPlan::new(TimelineId(2), 10, 50);
        assert!(backup.is_valid());
        assert_eq!(
            plan.basebackup_path(&backup),
            PathBuf::from(
                "/var/lib/reddb/timeline-00000000000000000002/basebackup/base-00000000000000000010-00000000000000000050.redbase"
            )
        );
    }

    #[test]
    fn wal_segment_round_trips_with_crc_chain() {
        let mut segment = PrimaryReplicaWalSegment::new(TimelineId(7), 3, 1_000);
        segment
            .push(PrimaryReplicaWalRecord::new(10, 1_000, b"first".to_vec()))
            .expect("push first record");
        segment
            .push(PrimaryReplicaWalRecord::new(11, 1_005, b"second".to_vec()))
            .expect("push second record");

        let encoded = segment.encode().expect("encode segment");
        let decoded = PrimaryReplicaWalSegment::decode(&encoded).expect("decode segment");
        assert_eq!(decoded, segment);

        let mut corrupt = encoded;
        let last_payload_byte = corrupt.len() - CHECKSUM_LEN - 1;
        corrupt[last_payload_byte] ^= 0x01;
        let err =
            PrimaryReplicaWalSegment::decode(&corrupt).expect_err("checksum catches corruption");
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
    }

    #[test]
    fn wal_segment_rejects_non_monotonic_sequence() {
        let mut segment = PrimaryReplicaWalSegment::new(TimelineId::initial(), 0, 0);
        segment
            .push(PrimaryReplicaWalRecord::new(1, 0, b"a".to_vec()))
            .expect("push first record");
        let err = segment
            .push(PrimaryReplicaWalRecord::new(3, 1, b"b".to_vec()))
            .expect_err("sequence gap rejected");
        assert!(err.to_string().contains("does not follow"), "{err}");
    }

    #[test]
    fn wal_segment_writes_and_reads_from_disk() {
        let root = temp_root("primary-wal");
        let plan =
            PrimaryReplicaFilePlan::new(&root, TimelineId(5)).with_segment_bytes(1024 * 1024);
        let mut segment = PrimaryReplicaWalSegment::new(TimelineId(5), 0, 0);
        segment
            .push(PrimaryReplicaWalRecord::new(1, 0, b"commit".to_vec()))
            .expect("push wal record");

        let path = plan.wal_segment_path(0);
        segment.write_to_path(&path).expect("write wal segment");

        assert_eq!(
            PrimaryReplicaWalSegment::read_from_path(&path).expect("read wal segment"),
            segment
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn wal_segment_accepts_logical_lsn_sequence_independent_of_payload_size() {
        let mut segment = PrimaryReplicaWalSegment::new(TimelineId::initial(), 0, 0);
        segment
            .push(PrimaryReplicaWalRecord::new(
                0,
                1,
                b"payload-larger-than-one".to_vec(),
            ))
            .expect("push first logical lsn");
        segment
            .push(PrimaryReplicaWalRecord::new(1, 2, b"next".to_vec()))
            .expect("push next logical lsn");

        assert_eq!(segment.end_lsn, 3);
        assert_eq!(segment.records.len(), 2);
    }

    #[test]
    fn file_plan_appends_logical_wal_records_to_existing_segment() {
        let root = temp_root("primary-wal-append");
        let plan =
            PrimaryReplicaFilePlan::new(&root, TimelineId(1)).with_segment_bytes(1024 * 1024);

        let path = plan
            .append_wal_record(1, b"payload-larger-than-one")
            .expect("append first record");
        assert_eq!(
            path,
            plan.wal_segment_path(1),
            "append returns the segment path"
        );
        plan.append_wal_record(2, b"next")
            .expect("append second record");

        let segment = PrimaryReplicaWalSegment::read_from_path(plan.wal_segment_path(1))
            .expect("read appended wal segment");
        assert_eq!(segment.timeline, TimelineId(1));
        assert_eq!(segment.records.len(), 2);
        assert_eq!(segment.records[0].sequence, 0);
        assert_eq!(segment.records[0].lsn, 1);
        assert_eq!(segment.records[1].sequence, 1);
        assert_eq!(segment.records[1].lsn, 2);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn wal_retention_prunes_segments_below_slot_floor_and_keeps_min_segments() {
        let root = temp_root("primary-wal-retention");
        let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(1))
            .with_segment_bytes(1024 * 1024)
            .with_retention(WalRetentionPolicy {
                min_segments: 2,
                max_bytes: 1024 * 1024 * 1024,
                keep_until_replicas_ack: true,
            });
        for index in 0..5 {
            let path = plan.wal_segment_path(index * plan.segment_bytes);
            write_bytes_atomically(&path, &[index as u8]).expect("write fake redwal");
        }
        let mut catalog = ReplicationSlotCatalog::new(TimelineId(1));
        let mut slot = ReplicationSlot::new("replica-a", TimelineId(1), 0);
        slot.update_ack(
            &ReplicaAck::with_positions(
                "replica-a",
                TimelineId(1),
                5 * plan.segment_bytes,
                5 * plan.segment_bytes,
                3 * plan.segment_bytes,
                3 * plan.segment_bytes,
            )
            .expect("ack"),
        )
        .expect("update slot");
        catalog.upsert(slot).expect("upsert slot");

        let current_lsn = 5 * plan.segment_bytes;
        let retention = plan
            .plan_wal_retention(&catalog, current_lsn)
            .expect("plan retention");
        assert_eq!(retention.oldest_required_lsn, Some(3 * plan.segment_bytes));
        assert_eq!(retention.retained_bytes_before_prune, 5);
        assert_eq!(retention.retained_bytes_after_prune, 2);
        assert_eq!(retention.removable_segments.len(), 3);

        let pruned = plan
            .prune_wal_segments(&catalog, current_lsn)
            .expect("prune wal");
        assert_eq!(pruned.removed_segments, retention.removable_segments);
        assert_eq!(pruned.retained_bytes_before_prune, 5);
        assert_eq!(pruned.retained_bytes_after_prune, 2);
        for index in 0..3 {
            assert!(
                !plan.wal_segment_path(index * plan.segment_bytes).exists(),
                "segment {index} should be pruned"
            );
        }
        for index in 3..5 {
            assert!(
                plan.wal_segment_path(index * plan.segment_bytes).exists(),
                "segment {index} should be retained"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn wal_retention_keeps_segments_needed_by_slowest_replica() {
        let root = temp_root("primary-wal-retention-slow");
        let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(1))
            .with_segment_bytes(1024 * 1024)
            .with_retention(WalRetentionPolicy {
                min_segments: 1,
                max_bytes: 1024 * 1024 * 1024,
                keep_until_replicas_ack: true,
            });
        for index in 0..4 {
            let path = plan.wal_segment_path(index * plan.segment_bytes);
            write_bytes_atomically(&path, &[index as u8]).expect("write fake redwal");
        }
        let mut catalog = ReplicationSlotCatalog::new(TimelineId(1));
        let mut fast = ReplicationSlot::new("fast", TimelineId(1), 0);
        fast.update_ack(
            &ReplicaAck::with_positions(
                "fast",
                TimelineId(1),
                4 * plan.segment_bytes,
                4 * plan.segment_bytes,
                4 * plan.segment_bytes,
                4 * plan.segment_bytes,
            )
            .expect("fast ack"),
        )
        .expect("update fast");
        let mut slow = ReplicationSlot::new("slow", TimelineId(1), 0);
        slow.update_ack(
            &ReplicaAck::with_positions(
                "slow",
                TimelineId(1),
                4 * plan.segment_bytes,
                4 * plan.segment_bytes,
                plan.segment_bytes,
                plan.segment_bytes,
            )
            .expect("slow ack"),
        )
        .expect("update slow");
        catalog.upsert(fast).expect("upsert fast");
        catalog.upsert(slow).expect("upsert slow");

        let retention = plan
            .plan_wal_retention(&catalog, 4 * plan.segment_bytes)
            .expect("plan retention");
        assert_eq!(retention.oldest_required_lsn, Some(plan.segment_bytes));
        assert_eq!(retention.retained_bytes_before_prune, 4);
        assert_eq!(retention.retained_bytes_after_prune, 3);
        assert_eq!(retention.removable_segments, vec![plan.wal_segment_path(0)]);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn wal_retention_max_bytes_prunes_slot_released_segments_inside_min_window() {
        let root = temp_root("primary-wal-retention-max-bytes");
        let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(1))
            .with_segment_bytes(1024 * 1024)
            .with_retention(WalRetentionPolicy {
                min_segments: 6,
                max_bytes: 3,
                keep_until_replicas_ack: true,
            });
        for index in 0..5 {
            let path = plan.wal_segment_path(index * plan.segment_bytes);
            write_bytes_atomically(&path, &[index as u8]).expect("write fake redwal");
        }
        let mut catalog = ReplicationSlotCatalog::new(TimelineId(1));
        let mut slot = ReplicationSlot::new("replica-a", TimelineId(1), 0);
        slot.update_ack(
            &ReplicaAck::with_positions(
                "replica-a",
                TimelineId(1),
                5 * plan.segment_bytes,
                5 * plan.segment_bytes,
                5 * plan.segment_bytes,
                5 * plan.segment_bytes,
            )
            .expect("ack"),
        )
        .expect("update slot");
        catalog.upsert(slot).expect("upsert slot");

        let retention = plan
            .plan_wal_retention(&catalog, 5 * plan.segment_bytes)
            .expect("plan retention");
        assert_eq!(retention.oldest_required_lsn, Some(5 * plan.segment_bytes));
        assert_eq!(retention.retained_bytes_before_prune, 5);
        assert_eq!(retention.retained_bytes_after_prune, 3);
        assert_eq!(
            retention.removable_segments,
            vec![
                plan.wal_segment_path(0),
                plan.wal_segment_path(plan.segment_bytes)
            ]
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn basebackup_manifest_round_trips_with_checksum() {
        let backup = BaseBackupPlan::new(TimelineId(9), 100, 200);
        let manifest =
            PrimaryReplicaBaseBackupManifest::new(backup, "snapshot.rdb", 4096, 0x1122_3344)
                .expect("manifest");

        let encoded = manifest.encode();
        assert_eq!(
            PrimaryReplicaBaseBackupManifest::decode(&encoded).expect("decode manifest"),
            manifest
        );

        let mut corrupt = encoded;
        corrupt[16] ^= 0x01;
        let err = PrimaryReplicaBaseBackupManifest::decode(&corrupt)
            .expect_err("checksum catches corruption");
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
    }

    #[test]
    fn basebackup_manifest_writes_and_reads_from_disk() {
        let root = temp_root("primary-basebackup");
        let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(4));
        let backup = BaseBackupPlan::new(TimelineId(4), 10, 50);
        let manifest =
            PrimaryReplicaBaseBackupManifest::new(backup, "snapshot.rdb", 8192, 0xCAFE_BABE)
                .expect("manifest");

        let path = plan.basebackup_path(&backup);
        manifest.write_to_path(&path).expect("write manifest");
        assert_eq!(
            PrimaryReplicaBaseBackupManifest::read_from_path(&path).expect("read manifest"),
            manifest
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn basebackup_list_reads_manifests_sorted_by_checkpoint_lsn() {
        let root = temp_root("primary-basebackup-list");
        let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(4));
        for checkpoint in [90, 50] {
            let backup = BaseBackupPlan::new(TimelineId(4), 10, checkpoint);
            let manifest = PrimaryReplicaBaseBackupManifest::new(
                backup,
                format!("snapshot-{checkpoint}.rdb"),
                128,
                checkpoint as u32,
            )
            .expect("manifest");
            manifest
                .write_to_path(plan.basebackup_path(&backup))
                .expect("write manifest");
        }

        let manifests = plan.list_basebackups().expect("list basebackups");
        assert_eq!(
            manifests
                .iter()
                .map(|manifest| manifest.checkpoint_lsn)
                .collect::<Vec<_>>(),
            vec![50, 90]
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn basebackup_writes_chunked_snapshot_parts_and_validates_them() {
        let root = temp_root("primary-basebackup-chunked");
        let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(4));
        let backup = BaseBackupPlan::new(TimelineId(4), 10, 50);
        let snapshot = b"abcdefghijklmnopqrstuvwxyz";

        let manifest = plan
            .write_basebackup_snapshot_parts(backup, snapshot, 8)
            .expect("write chunked snapshot");
        assert_eq!(manifest.snapshot_bytes, snapshot.len() as u64);
        assert_eq!(manifest.chunks.len(), 4);
        assert_eq!(manifest.chunks[0].snapshot_offset, 0);
        assert_eq!(manifest.chunks[1].snapshot_offset, 8);
        assert_eq!(manifest.chunks[2].snapshot_offset, 16);
        assert_eq!(manifest.chunks[3].snapshot_offset, 24);
        manifest
            .verify_snapshot_parts(plan.basebackup_dir())
            .expect("verify parts");
        assert_eq!(
            manifest
                .read_snapshot_parts(plan.basebackup_dir())
                .expect("read snapshot parts"),
            snapshot
        );

        let path = plan.basebackup_path(&backup);
        manifest.write_to_path(&path).expect("write manifest");
        assert_eq!(
            PrimaryReplicaBaseBackupManifest::read_from_path(&path).expect("read manifest"),
            manifest
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn basebackup_chunk_validation_catches_corrupt_part() {
        let root = temp_root("primary-basebackup-corrupt-chunk");
        let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(4));
        let backup = BaseBackupPlan::new(TimelineId(4), 10, 50);
        let mut manifest = plan
            .write_basebackup_snapshot_parts(backup, b"chunk-one-chunk-two", 9)
            .expect("write chunked snapshot");

        let corrupt_path = plan
            .basebackup_dir()
            .join(&manifest.chunks[1].relative_path);
        std::fs::write(&corrupt_path, b"corrupt!!").expect("overwrite chunk");
        let err = manifest
            .verify_snapshot_parts(plan.basebackup_dir())
            .expect_err("corrupt chunk rejected");
        assert!(err.to_string().contains("checksum mismatch"), "{err}");

        manifest.chunks[1].relative_path = PathBuf::from("../escape.redchunk");
        let err = manifest.validate().expect_err("path escape rejected");
        assert!(err.to_string().contains("parent components"), "{err}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn basebackup_staged_chunk_parts_recover_and_prune_corruption() {
        let root = temp_root("primary-basebackup-stage-chunk");
        let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(4));
        let backup = BaseBackupPlan::new(TimelineId(4), 10, 50);
        let manifest = PrimaryReplicaBaseBackupManifest::incremental(
            backup,
            PathBuf::from("base-00000000000000000010-00000000000000000050.snapshot"),
            19,
            crc32(b"chunk-one-chunk-two"),
            vec![
                BaseBackupChunkRef::new(
                    0,
                    0,
                    9,
                    crc32(b"chunk-one"),
                    plan.basebackup_chunk_relative_path(&backup, 0),
                ),
                BaseBackupChunkRef::new(
                    1,
                    9,
                    9,
                    crc32(b"-chunk-tw"),
                    plan.basebackup_chunk_relative_path(&backup, 1),
                ),
                BaseBackupChunkRef::new(
                    2,
                    18,
                    1,
                    crc32(b"o"),
                    plan.basebackup_chunk_relative_path(&backup, 2),
                ),
            ],
        )
        .expect("manifest");

        manifest
            .stage_chunk_part(plan.basebackup_dir(), 0, b"chunk-one")
            .expect("stage chunk 0");
        manifest
            .stage_chunk_part(plan.basebackup_dir(), 1, b"-chunk-tw")
            .expect("stage chunk 1");

        let recovered = manifest
            .recover_staged_chunk_parts(plan.basebackup_dir())
            .expect("recover staged chunks");
        assert_eq!(recovered.into_iter().collect::<Vec<_>>(), vec![0, 1]);

        let corrupt_path = plan
            .basebackup_dir()
            .join(&manifest.chunks[1].relative_path);
        std::fs::write(&corrupt_path, b"corrupt!!").expect("overwrite chunk");
        let recovered = manifest
            .recover_staged_chunk_parts(plan.basebackup_dir())
            .expect("recover after corruption");
        assert_eq!(recovered.into_iter().collect::<Vec<_>>(), vec![0]);
        assert!(
            !corrupt_path.exists(),
            "corrupt staged basebackup chunk should be pruned"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn basebackup_manifest_rejects_incomplete_snapshot_parts() {
        let root = temp_root("primary-basebackup-incomplete");
        let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(4));
        let backup = BaseBackupPlan::new(TimelineId(4), 10, 50);
        let manifest = plan
            .write_basebackup_snapshot_parts(backup, b"chunk-one-chunk-two", 9)
            .expect("write chunked snapshot");

        let missing_path = plan
            .basebackup_dir()
            .join(&manifest.chunks[1].relative_path);
        std::fs::remove_file(&missing_path).expect("remove chunk");
        let err = manifest
            .verify_snapshot_parts(plan.basebackup_dir())
            .expect_err("missing chunk rejected");
        assert!(err.to_string().contains("No such file"), "{err}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn replication_durability_requires_selected_ack_stage() {
        let acks = vec![
            ReplicaAck::with_positions("r1", TimelineId(1), 120, 120, 120, 90).expect("ack"),
            ReplicaAck::with_positions("r2", TimelineId(1), 120, 120, 80, 80).expect("ack"),
        ];

        assert!(ReplicationDurability::Async.is_satisfied(100, &[]));
        assert!(ReplicationDurability::RemoteWrite { quorum: 2 }.is_satisfied(100, &acks));
        assert!(!ReplicationDurability::RemoteFlush { quorum: 2 }.is_satisfied(100, &acks));
        assert!(!ReplicationDurability::RemoteApply { quorum: 1 }.is_satisfied(100, &acks));
        assert!(ReplicationDurability::RemoteApply { quorum: 1 }.is_satisfied(80, &acks));
    }

    #[test]
    fn replication_slot_catalog_round_trips_and_tracks_retention_floor() {
        let mut catalog = ReplicationSlotCatalog::new(TimelineId(1));
        let mut slot_a = ReplicationSlot::new("replica-a", TimelineId(1), 0);
        slot_a
            .update_ack(
                &ReplicaAck::with_positions("replica-a", TimelineId(1), 500, 500, 400, 300)
                    .expect("ack"),
            )
            .expect("update slot");
        let mut slot_b = ReplicationSlot::new("replica-b", TimelineId(1), 0);
        slot_b
            .update_ack(
                &ReplicaAck::with_positions("replica-b", TimelineId(1), 500, 500, 250, 250)
                    .expect("ack"),
            )
            .expect("update slot");

        catalog.upsert(slot_a).expect("upsert slot a");
        catalog.upsert(slot_b).expect("upsert slot b");

        assert_eq!(catalog.retention_floor_lsn(), Some(250));
        let decoded = ReplicationSlotCatalog::decode(&catalog.encode().expect("encode catalog"))
            .expect("decode catalog");
        assert_eq!(decoded, catalog);
    }

    #[test]
    fn replication_slot_catalog_writes_and_reads_from_disk() {
        let root = temp_root("primary-slots");
        let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(8));
        let mut catalog = ReplicationSlotCatalog::new(TimelineId(8));
        catalog
            .upsert(ReplicationSlot::new("replica-a", TimelineId(8), 100))
            .expect("upsert slot");

        catalog
            .write_to_path(plan.slots_path())
            .expect("write catalog");

        assert_eq!(
            ReplicationSlotCatalog::read_from_path(plan.slots_path()).expect("read catalog"),
            catalog
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn replication_slot_catalog_reads_and_writes_legacy_json_store() {
        let root = temp_root("primary-slots-legacy-json");
        let path = crate::layout::legacy_logical_slots_path(&root.join("db.rdb"));
        let mut catalog = ReplicationSlotCatalog::new(TimelineId::initial());
        let mut slot = ReplicationSlot::new("replica-a", TimelineId::initial(), 10);
        slot.confirmed_write_lsn = 25;
        slot.confirmed_flush_lsn = 10;
        slot.confirmed_apply_lsn = 10;
        slot.last_seen_at_unix_ms = 1234;
        catalog.upsert(slot).expect("upsert slot");

        catalog
            .write_legacy_json_to_path(&path)
            .expect("write legacy json");

        let decoded =
            ReplicationSlotCatalog::read_legacy_json_from_path(&path, 9999).expect("read legacy");
        assert_eq!(decoded, catalog);
        assert!(!crate::layout::legacy_logical_slots_temp_path(&path).exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn replication_slot_catalog_ignores_leftover_tmp_file_after_crash() {
        let root = temp_root("primary-slots-crash");
        let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(8));
        let mut catalog = ReplicationSlotCatalog::new(TimelineId(8));
        catalog
            .upsert(ReplicationSlot::new("replica-a", TimelineId(8), 100))
            .expect("upsert slot");
        catalog
            .write_to_path(plan.slots_path())
            .expect("write catalog");

        std::fs::write(crate::layout::atomic_temp_path(&plan.slots_path()), b"torn")
            .expect("write leftover tmp");
        assert_eq!(
            ReplicationSlotCatalog::read_from_path(plan.slots_path()).expect("read catalog"),
            catalog
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn relay_log_manifest_round_trips_and_builds_ack() {
        let mut manifest = ReplicaRelayLogManifest::new("replica-a", TimelineId(2));
        manifest
            .push_segment(
                RelayLogSegmentRef::new("00000000000000000000.redwal", 0, 100, 0xAA)
                    .expect("segment"),
            )
            .expect("push segment");
        manifest
            .push_segment(
                RelayLogSegmentRef::new("00000000000000000001.redwal", 100, 180, 0xBB)
                    .expect("segment"),
            )
            .expect("push segment");
        manifest.mark_applied(120).expect("mark applied");

        let ack = manifest.ack().expect("ack");
        assert_eq!(ack.received_lsn, 180);
        assert_eq!(ack.written_lsn, 180);
        assert_eq!(ack.flushed_lsn, 180);
        assert_eq!(ack.applied_lsn, 120);

        let decoded = ReplicaRelayLogManifest::decode(&manifest.encode().expect("encode manifest"))
            .expect("decode manifest");
        assert_eq!(decoded, manifest);
    }

    #[test]
    fn relay_log_segment_round_trips_and_checksums_payloads() {
        let segment = ReplicaRelayLogSegment::from_records(
            TimelineId(2),
            vec![
                ReplicaRelayLogRecord::new(10, b"ten".to_vec()),
                ReplicaRelayLogRecord::new(11, b"eleven".to_vec()),
            ],
        )
        .expect("relay segment");

        assert_eq!(segment.start_lsn, 10);
        assert_eq!(segment.end_lsn, 11);
        let checksum = segment.checksum().expect("checksum");
        assert_ne!(checksum, 0);

        let decoded = ReplicaRelayLogSegment::decode(&segment.encode().expect("encode segment"))
            .expect("decode segment");
        assert_eq!(decoded, segment);
        assert_eq!(decoded.checksum().expect("decoded checksum"), checksum);
    }

    #[test]
    fn relay_manifest_validates_referenced_segments() {
        let root = temp_root("primary-relay-validate-segments");
        let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(6));
        let relay_dir = plan.relay_dir("replica-a");
        let relative_path = PathBuf::from("relay-00000000000000000010-00000000000000000010.redwal");
        let segment = ReplicaRelayLogSegment::from_records(
            TimelineId(6),
            vec![ReplicaRelayLogRecord::new(10, b"ten".to_vec())],
        )
        .expect("relay segment");
        segment
            .write_to_path(relay_dir.join(&relative_path))
            .expect("write segment");

        let mut manifest = ReplicaRelayLogManifest::new("replica-a", TimelineId(6));
        manifest
            .push_segment(
                RelayLogSegmentRef::new(
                    relative_path,
                    segment.start_lsn,
                    segment.end_lsn,
                    segment.checksum().expect("checksum"),
                )
                .expect("segment ref"),
            )
            .expect("push segment");
        manifest
            .validate_segments(&relay_dir)
            .expect("validate segments");

        std::fs::write(
            relay_dir.join(&manifest.segments[0].relative_path),
            b"corrupt",
        )
        .expect("corrupt segment");
        assert!(
            manifest.validate_segments(&relay_dir).is_err(),
            "corrupt relay segment must fail validation"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn relay_log_manifest_writes_and_reads_from_disk() {
        let root = temp_root("primary-relay");
        let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(6));
        let mut manifest = ReplicaRelayLogManifest::new("replica-a", TimelineId(6));
        manifest
            .push_segment(
                RelayLogSegmentRef::new("00000000000000000000.redwal", 0, 32, 0xCC)
                    .expect("segment"),
            )
            .expect("push segment");

        manifest
            .write_to_path(plan.relay_manifest_path("replica-a"))
            .expect("write relay manifest");
        assert_eq!(
            ReplicaRelayLogManifest::read_from_path(plan.relay_manifest_path("replica-a"))
                .expect("read relay manifest"),
            manifest
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn relay_and_timeline_manifests_ignore_leftover_tmp_files_after_crash() {
        let root = temp_root("primary-relay-timeline-crash");
        let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(6));

        let mut relay = ReplicaRelayLogManifest::new("replica-a", TimelineId(6));
        relay
            .push_segment(
                RelayLogSegmentRef::new("00000000000000000000.redwal", 0, 32, 0xCC)
                    .expect("segment"),
            )
            .expect("push");
        relay.mark_applied(32).expect("mark applied");
        relay
            .write_to_path(plan.relay_manifest_path("replica-a"))
            .expect("write relay");
        std::fs::write(
            crate::layout::atomic_temp_path(&plan.relay_manifest_path("replica-a")),
            b"torn-relay",
        )
        .expect("write relay tmp");
        assert_eq!(
            ReplicaRelayLogManifest::read_from_path(plan.relay_manifest_path("replica-a"))
                .expect("read relay"),
            relay
        );

        let mut timeline = TimelineHistory::new(10);
        timeline
            .fork(TimelineId(2), TimelineId::initial(), 42, 11, "promote")
            .expect("fork timeline");
        timeline
            .write_to_path(plan.timeline_history_path())
            .expect("write timeline");
        std::fs::write(
            crate::layout::atomic_temp_path(&plan.timeline_history_path()),
            b"torn-timeline",
        )
        .expect("write timeline tmp");
        assert_eq!(
            TimelineHistory::read_from_path(plan.timeline_history_path()).expect("read timeline"),
            timeline
        );

        let _ = std::fs::remove_dir_all(root);
    }

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "reddb-file-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
}
