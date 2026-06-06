//! Move-range snapshot catch-up tracer (issue #1021).
//!
//! This module models the first range move flow over the storage pieces that
//! already exist: a physical range-directory checkpoint plus range-stamped
//! logical WAL batches. Live cluster transport can wrap these primitives later.

use std::io;
use std::path::Path;

use crate::storage::wal::{WalReader, WalRecord};
use crate::storage::{RangeMetadata, RangeSnapshot};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeIndexedRecord {
    pub lsn: u64,
    pub range_id: String,
    pub actions: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveRangeRequest {
    pub range_id: String,
    pub required_watermark_lsn: u64,
    pub source_owner_epoch: u64,
    pub target_owner_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveRangeTargetState {
    pub range_id: String,
    pub owner_epoch: u64,
    pub snapshot_lsn: u64,
    pub applied_lsn: u64,
    pub metadata: RangeMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveRangeCatchUp {
    pub range_id: String,
    pub started_after_lsn: u64,
    pub applied_lsn: u64,
    pub applied_batches: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveRangeCutover {
    pub range_id: String,
    pub owner_epoch: u64,
    pub reached_lsn: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeOwnership {
    pub range_id: String,
    pub owner_node_id: String,
    pub owner_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveRangeError {
    WrongRange {
        expected: String,
        actual: String,
    },
    StaleTargetSnapshot {
        range_id: String,
        snapshot_epoch: u64,
        required_epoch: u64,
    },
    CatchUpRequired {
        range_id: String,
        reached_lsn: u64,
        required_lsn: u64,
    },
    StaleOwnerEpoch {
        range_id: String,
        attempted_epoch: u64,
        current_epoch: u64,
    },
}

impl std::fmt::Display for MoveRangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongRange { expected, actual } => {
                write!(f, "range move expected {expected}, got {actual}")
            }
            Self::StaleTargetSnapshot {
                range_id,
                snapshot_epoch,
                required_epoch,
            } => write!(
                f,
                "range {range_id} target snapshot epoch {snapshot_epoch} is stale; required {required_epoch}",
            ),
            Self::CatchUpRequired {
                range_id,
                reached_lsn,
                required_lsn,
            } => write!(
                f,
                "range {range_id} cutover blocked at LSN {reached_lsn}; required {required_lsn}",
            ),
            Self::StaleOwnerEpoch {
                range_id,
                attempted_epoch,
                current_epoch,
            } => write!(
                f,
                "range {range_id} write epoch {attempted_epoch} is stale; current owner epoch {current_epoch}",
            ),
        }
    }
}

impl std::error::Error for MoveRangeError {}

impl MoveRangeTargetState {
    pub fn from_installed_snapshot(snapshot: &RangeSnapshot, metadata: RangeMetadata) -> Self {
        Self {
            range_id: metadata.logical_range_id.clone(),
            owner_epoch: snapshot.owner_epoch,
            snapshot_lsn: snapshot.snapshot_lsn,
            applied_lsn: snapshot.snapshot_lsn,
            metadata,
        }
    }
}

impl RangeOwnership {
    pub fn new(
        range_id: impl Into<String>,
        owner_node_id: impl Into<String>,
        owner_epoch: u64,
    ) -> Self {
        Self {
            range_id: range_id.into(),
            owner_node_id: owner_node_id.into(),
            owner_epoch,
        }
    }

    pub fn check_write_epoch(&self, attempted_epoch: u64) -> Result<(), MoveRangeError> {
        if attempted_epoch == self.owner_epoch {
            Ok(())
        } else {
            Err(MoveRangeError::StaleOwnerEpoch {
                range_id: self.range_id.clone(),
                attempted_epoch,
                current_epoch: self.owner_epoch,
            })
        }
    }
}

pub struct MoveRangeTracer;

impl MoveRangeTracer {
    pub fn read_range_indexed_wal(
        wal_path: impl AsRef<Path>,
        range_id: &str,
        since_lsn: u64,
    ) -> io::Result<Vec<RangeIndexedRecord>> {
        let reader = WalReader::open(wal_path)?;
        let mut records = Vec::new();
        for entry in reader.iter() {
            let (lsn, record) = entry?;
            let WalRecord::RangeCommitBatch {
                range_id: record_range_id,
                actions,
                ..
            } = record
            else {
                continue;
            };
            if record_range_id == range_id && lsn > since_lsn {
                records.push(RangeIndexedRecord {
                    lsn,
                    range_id: record_range_id,
                    actions,
                });
            }
        }
        Ok(records)
    }

    pub fn catch_up_from_wal(
        target: &mut MoveRangeTargetState,
        wal_path: impl AsRef<Path>,
        required_watermark_lsn: u64,
    ) -> io::Result<MoveRangeCatchUp> {
        let started_after_lsn = target.applied_lsn;
        let records = Self::read_range_indexed_wal(wal_path, &target.range_id, target.applied_lsn)?;
        for record in &records {
            target.applied_lsn = target.applied_lsn.max(record.lsn);
        }
        target.applied_lsn = target.applied_lsn.min(required_watermark_lsn);
        Ok(MoveRangeCatchUp {
            range_id: target.range_id.clone(),
            started_after_lsn,
            applied_lsn: target.applied_lsn,
            applied_batches: records.len(),
        })
    }

    pub fn cutover(
        req: &MoveRangeRequest,
        target: &MoveRangeTargetState,
    ) -> Result<MoveRangeCutover, MoveRangeError> {
        if target.range_id != req.range_id {
            return Err(MoveRangeError::WrongRange {
                expected: req.range_id.clone(),
                actual: target.range_id.clone(),
            });
        }
        if target.owner_epoch != req.source_owner_epoch {
            return Err(MoveRangeError::StaleTargetSnapshot {
                range_id: req.range_id.clone(),
                snapshot_epoch: target.owner_epoch,
                required_epoch: req.source_owner_epoch,
            });
        }
        if target.applied_lsn < req.required_watermark_lsn {
            return Err(MoveRangeError::CatchUpRequired {
                range_id: req.range_id.clone(),
                reached_lsn: target.applied_lsn,
                required_lsn: req.required_watermark_lsn,
            });
        }
        Ok(MoveRangeCutover {
            range_id: req.range_id.clone(),
            owner_epoch: req.target_owner_epoch,
            reached_lsn: target.applied_lsn,
        })
    }
}
