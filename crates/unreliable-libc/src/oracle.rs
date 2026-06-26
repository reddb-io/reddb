//! The shared recovery-invariant assertion oracle (DST Fatia 0, #1351).
//!
//! After any injected fault — short write, `EIO`, or a `SIGKILL` power-cut — the
//! on-disk WAL + dual superblock must still satisfy the recovery invariants,
//! regardless of what the workload *thought* it had done. Later DST slices reuse
//! [`recover_and_check`] as their oracle.
//!
//! Invariants enforced:
//! * **Longest valid prefix** — recovery replays the maximal run of CRC-valid
//!   records from the header; the trailing torn record (if any) is discarded.
//! * **Monotonic LSN** — `Begin`/`Commit` transaction ids strictly increase; a
//!   `Commit` only follows its own `Begin`.
//! * **No torn/partial committed record visible** — only fully CRC-validated
//!   records count, and no valid record may appear after the torn boundary.
//! * **Intact dual superblocks** — once both slots have been written, at least
//!   one slot always survives a fault.
//! * **CRC/checksum integrity** — every recovered record passes its checksum
//!   (enforced by the engine decoder), and the recovered superblock never claims
//!   a commit beyond what the WAL durably holds.

use crate::superblock::{self, SUPERBLOCK_BYTES};
use crate::wal_workload::{SUPERBLOCK_FILE_NAME, WAL_FILE_NAME};
use reddb_file::wal_header::{decode_wal_file_header, WAL_FILE_HEADER_BYTES};
use reddb_file::wal_record::{decode_main_wal_record_frame, MainWalRecordFrame};
use std::fmt;
use std::io::Cursor;
use std::path::Path;

/// The state recovery reconstructed from the longest valid WAL prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Number of CRC-valid records replayed from the prefix.
    pub records_recovered: u64,
    /// Highest committed transaction id in the recovered prefix.
    pub last_committed_lsn: u64,
    /// Bytes discarded as a torn trailing record (0 if the WAL ended cleanly).
    pub torn_tail_bytes: u64,
    /// Generation of the recovered superblock, if any survived.
    pub superblock_generation: Option<u64>,
    /// Committed LSN recorded by the recovered superblock, if any.
    pub superblock_committed_lsn: Option<u64>,
}

/// A violated recovery invariant. The presence of any of these means recovery
/// would observe a corrupt or impossible state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryError {
    /// The WAL file could not be read.
    WalUnreadable(String),
    /// A `Begin`/`Commit` transaction id went backwards.
    NonMonotonicLsn { previous: u64, found: u64 },
    /// A `Commit`/`PageWrite` referenced a transaction with no open `Begin`.
    CommitWithoutBegin { tx_id: u64 },
    /// A nested `Begin` appeared before the prior transaction committed.
    NestedTransaction { open: u64, found: u64 },
    /// A `Checkpoint` recorded an LSN ahead of the last committed transaction.
    CheckpointAheadOfCommit { checkpoint: u64, committed: u64 },
    /// A fully-valid record was found after the torn boundary (resurrected gap).
    ValidRecordAfterTear { offset: u64 },
    /// Both superblock slots were corrupt after both had been written.
    SuperblockBothCorrupt,
    /// The recovered superblock claims a commit the WAL does not durably hold.
    SuperblockAheadOfWal { superblock: u64, wal: u64 },
}

impl fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WalUnreadable(e) => write!(f, "WAL unreadable: {e}"),
            Self::NonMonotonicLsn { previous, found } => {
                write!(f, "non-monotonic LSN: {found} followed {previous}")
            }
            Self::CommitWithoutBegin { tx_id } => {
                write!(f, "commit/page-write for tx {tx_id} with no open begin")
            }
            Self::NestedTransaction { open, found } => {
                write!(f, "nested begin {found} while tx {open} still open")
            }
            Self::CheckpointAheadOfCommit {
                checkpoint,
                committed,
            } => write!(
                f,
                "checkpoint lsn {checkpoint} ahead of committed {committed}"
            ),
            Self::ValidRecordAfterTear { offset } => {
                write!(f, "valid record resurrected after tear at offset {offset}")
            }
            Self::SuperblockBothCorrupt => write!(f, "both superblock slots corrupt"),
            Self::SuperblockAheadOfWal { superblock, wal } => write!(
                f,
                "superblock committed_lsn {superblock} ahead of WAL {wal}"
            ),
        }
    }
}

impl std::error::Error for RecoveryError {}

/// Recover the WAL + dual superblock in `dir` and assert every recovery
/// invariant. Returns the recovered [`RecoveryReport`] on success.
pub fn recover_and_check(dir: &Path) -> Result<RecoveryReport, RecoveryError> {
    let wal_path = dir.join(WAL_FILE_NAME);
    let wal_bytes = match std::fs::read(&wal_path) {
        Ok(bytes) => bytes,
        // A missing WAL means the workload died before creating it — a clean,
        // empty (genesis) state, which is trivially consistent.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => return Err(RecoveryError::WalUnreadable(err.to_string())),
    };

    let (records, last_committed, torn_tail) = scan_wal_prefix(&wal_bytes)?;

    let sb_bytes = read_optional(&dir.join(SUPERBLOCK_FILE_NAME));
    let recovered_sb = superblock::recover(&sb_bytes);

    // Once both slots have been written, a fault may corrupt at most one.
    if sb_bytes.len() >= SUPERBLOCK_BYTES && recovered_sb.is_none() {
        return Err(RecoveryError::SuperblockBothCorrupt);
    }
    // Durability: a recovered superblock can never be ahead of the WAL.
    if let Some(sb) = recovered_sb {
        if sb.committed_lsn > last_committed {
            return Err(RecoveryError::SuperblockAheadOfWal {
                superblock: sb.committed_lsn,
                wal: last_committed,
            });
        }
    }

    Ok(RecoveryReport {
        records_recovered: records,
        last_committed_lsn: last_committed,
        torn_tail_bytes: torn_tail,
        superblock_generation: recovered_sb.map(|s| s.generation),
        superblock_committed_lsn: recovered_sb.map(|s| s.committed_lsn),
    })
}

/// Scan the longest valid record prefix, returning
/// `(records_recovered, last_committed_lsn, torn_tail_bytes)`.
fn scan_wal_prefix(bytes: &[u8]) -> Result<(u64, u64, u64), RecoveryError> {
    if bytes.len() < WAL_FILE_HEADER_BYTES {
        // No durable header yet (or torn during the header write): genesis.
        return Ok((0, 0, bytes.len() as u64));
    }
    let mut header = [0u8; WAL_FILE_HEADER_BYTES];
    header.copy_from_slice(&bytes[..WAL_FILE_HEADER_BYTES]);
    let version = match decode_wal_file_header(&header) {
        Ok(h) => h.version,
        // A torn header means nothing durable was committed: genesis state.
        Err(_) => return Ok((0, 0, bytes.len() as u64)),
    };

    let mut cursor = Cursor::new(bytes);
    cursor.set_position(WAL_FILE_HEADER_BYTES as u64);

    let mut records = 0u64;
    let mut last_committed = 0u64;
    let mut prev_begin = 0u64;
    let mut open_tx: Option<u64> = None;
    let mut last_valid_end = WAL_FILE_HEADER_BYTES as u64;

    loop {
        match decode_main_wal_record_frame(&mut cursor, version, 1) {
            Ok(Some((_term, frame))) => {
                validate_frame(&frame, &mut prev_begin, &mut open_tx, &mut last_committed)?;
                last_valid_end = cursor.position();
                records += 1;
            }
            // Clean EOF exactly at a record boundary: nothing torn.
            Ok(None) => break,
            // Torn/corrupt record: the prefix ends here.
            Err(_) => break,
        }
    }

    let torn_tail = bytes.len() as u64 - last_valid_end;
    if torn_tail > 0 {
        ensure_no_record_after_tear(bytes, last_valid_end, version)?;
    }

    Ok((records, last_committed, torn_tail))
}

fn validate_frame(
    frame: &MainWalRecordFrame,
    prev_begin: &mut u64,
    open_tx: &mut Option<u64>,
    last_committed: &mut u64,
) -> Result<(), RecoveryError> {
    match frame {
        MainWalRecordFrame::Begin { tx_id } => {
            if let Some(open) = *open_tx {
                return Err(RecoveryError::NestedTransaction {
                    open,
                    found: *tx_id,
                });
            }
            if *tx_id <= *prev_begin {
                return Err(RecoveryError::NonMonotonicLsn {
                    previous: *prev_begin,
                    found: *tx_id,
                });
            }
            *prev_begin = *tx_id;
            *open_tx = Some(*tx_id);
        }
        MainWalRecordFrame::PageWrite { tx_id, .. } => {
            if *open_tx != Some(*tx_id) {
                return Err(RecoveryError::CommitWithoutBegin { tx_id: *tx_id });
            }
        }
        MainWalRecordFrame::Commit { tx_id } => {
            if *open_tx != Some(*tx_id) {
                return Err(RecoveryError::CommitWithoutBegin { tx_id: *tx_id });
            }
            if *tx_id <= *last_committed {
                return Err(RecoveryError::NonMonotonicLsn {
                    previous: *last_committed,
                    found: *tx_id,
                });
            }
            *last_committed = *tx_id;
            *open_tx = None;
        }
        MainWalRecordFrame::Checkpoint { lsn } => {
            if *lsn > *last_committed {
                return Err(RecoveryError::CheckpointAheadOfCommit {
                    checkpoint: *lsn,
                    committed: *last_committed,
                });
            }
        }
        // The workload never emits these, but they are valid frames; ignore.
        MainWalRecordFrame::Rollback { .. }
        | MainWalRecordFrame::TxCommitBatch { .. }
        | MainWalRecordFrame::FullPageImage { .. }
        | MainWalRecordFrame::VectorInsert { .. } => {}
    }
    Ok(())
}

/// Assert no fully-valid record can be decoded anywhere inside the torn tail —
/// proof there is no resurrected committed data after the tear. The tail is at
/// most one record long, so this scan is cheap.
fn ensure_no_record_after_tear(
    bytes: &[u8],
    tear_start: u64,
    version: u8,
) -> Result<(), RecoveryError> {
    let start = usize::try_from(tear_start).unwrap_or(0);
    // Offset 0 of the tail is the torn record itself (already known bad); probe
    // every later byte offset for an accidentally-valid record.
    for offset in (start + 1)..bytes.len() {
        let mut cursor = Cursor::new(&bytes[offset..]);
        if let Ok(Some(_)) = decode_main_wal_record_frame(&mut cursor, version, 1) {
            return Err(RecoveryError::ValidRecordAfterTear {
                offset: offset as u64,
            });
        }
    }
    Ok(())
}

fn read_optional(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wal_workload::run_wal_workload;

    #[test]
    fn fault_free_run_recovers_everything() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = run_wal_workload(dir.path(), 2024).unwrap();
        let report = recover_and_check(dir.path()).unwrap();
        assert_eq!(report.last_committed_lsn, outcome.last_committed_lsn);
        assert_eq!(report.torn_tail_bytes, 0);
        assert!(report.records_recovered > 0);
        assert_eq!(
            report.superblock_committed_lsn,
            Some(outcome.last_committed_lsn)
        );
    }

    #[test]
    fn empty_dir_is_clean_genesis() {
        let dir = tempfile::tempdir().unwrap();
        let report = recover_and_check(dir.path()).unwrap();
        assert_eq!(report.records_recovered, 0);
        assert_eq!(report.last_committed_lsn, 0);
        assert_eq!(report.superblock_generation, None);
    }

    #[test]
    fn torn_tail_is_discarded_and_prefix_holds() {
        let dir = tempfile::tempdir().unwrap();
        run_wal_workload(dir.path(), 55).unwrap();
        let wal_path = dir.path().join(WAL_FILE_NAME);
        let mut bytes = std::fs::read(&wal_path).unwrap();
        let full_committed = recover_and_check(dir.path()).unwrap().last_committed_lsn;
        // Simulate a power-cut mid-write: chop the trailing bytes.
        bytes.truncate(bytes.len() - 5);
        std::fs::write(&wal_path, &bytes).unwrap();
        let report = recover_and_check(dir.path()).unwrap();
        assert!(report.torn_tail_bytes > 0);
        // The recovered commit frontier never exceeds the original.
        assert!(report.last_committed_lsn <= full_committed);
    }

    #[test]
    fn superblock_ahead_of_wal_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        run_wal_workload(dir.path(), 9).unwrap();
        // Forge a superblock that claims a commit far beyond the WAL.
        let forged = crate::superblock::Superblock {
            generation: 999,
            committed_lsn: 1_000_000,
        }
        .encode();
        std::fs::write(dir.path().join(SUPERBLOCK_FILE_NAME), forged).unwrap();
        let err = recover_and_check(dir.path()).unwrap_err();
        assert!(matches!(err, RecoveryError::SuperblockAheadOfWal { .. }));
    }
}
