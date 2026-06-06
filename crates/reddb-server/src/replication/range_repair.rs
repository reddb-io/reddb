//! Full-rebootstrap range repair tracer (issue #1022).
//!
//! A damaged local range copy is quarantined before replacement. The repair
//! then installs a physical range snapshot from a healthy owner and catches up
//! through the range-indexed logical WAL stream before reporting the replica
//! healthy again.

use std::io;
use std::path::Path;

use crate::replication::{
    MoveRangeCatchUp, MoveRangeError, MoveRangeRequest, MoveRangeTargetState, MoveRangeTracer,
};
use crate::storage::{ClusterRangeLayout, RangeMetadata, RangeQuarantine, RangeSnapshot};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeRepairReason {
    Corrupt {
        detail: String,
    },
    TooStale {
        local_lsn: u64,
        retention_floor_lsn: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeReplicaHealth {
    Healthy { owner_epoch: u64, applied_lsn: u64 },
    RepairRequired { reason: RangeRepairReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeReplicaRepairState {
    pub range_id: String,
    pub metadata: RangeMetadata,
    pub owner_epoch: u64,
    pub applied_lsn: u64,
    pub health: RangeReplicaHealth,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeRepairRequest {
    pub range_id: String,
    pub required_watermark_lsn: u64,
    pub source_owner_epoch: u64,
    pub repaired_owner_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeRepairOutcome {
    pub range_id: String,
    pub quarantine: RangeQuarantine,
    pub installed_metadata: RangeMetadata,
    pub catch_up: MoveRangeCatchUp,
    pub healthy: RangeReplicaHealth,
}

#[derive(Debug)]
pub enum RangeRepairError {
    NotMarkedForRepair {
        range_id: String,
    },
    WrongLocalRange {
        expected: String,
        actual: String,
    },
    WrongSnapshotRange {
        expected: String,
        actual: String,
    },
    Quarantine(io::Error),
    InstallSnapshot {
        quarantine: RangeQuarantine,
        source: io::Error,
    },
    CatchUp {
        quarantine: RangeQuarantine,
        source: io::Error,
    },
    Cutover {
        quarantine: RangeQuarantine,
        source: MoveRangeError,
    },
}

impl std::fmt::Display for RangeRepairError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotMarkedForRepair { range_id } => {
                write!(f, "range {range_id} is not marked for repair")
            }
            Self::WrongLocalRange { expected, actual } => {
                write!(f, "range repair expected local {expected}, got {actual}")
            }
            Self::WrongSnapshotRange { expected, actual } => {
                write!(
                    f,
                    "range repair expected snapshot for {expected}, got {actual}"
                )
            }
            Self::Quarantine(err) => write!(f, "failed to quarantine range: {err}"),
            Self::InstallSnapshot { source, .. } => {
                write!(f, "failed to install repair snapshot: {source}")
            }
            Self::CatchUp { source, .. } => {
                write!(f, "failed to catch up repaired range: {source}")
            }
            Self::Cutover { source, .. } => {
                write!(f, "failed to mark repaired range healthy: {source}")
            }
        }
    }
}

impl std::error::Error for RangeRepairError {}

pub struct RangeRepairTracer;

impl RangeRepairTracer {
    pub fn mark_corrupt(
        metadata: RangeMetadata,
        owner_epoch: u64,
        applied_lsn: u64,
        detail: impl Into<String>,
    ) -> RangeReplicaRepairState {
        Self::mark_for_repair(
            metadata,
            owner_epoch,
            applied_lsn,
            RangeRepairReason::Corrupt {
                detail: detail.into(),
            },
        )
    }

    pub fn mark_too_stale(
        metadata: RangeMetadata,
        owner_epoch: u64,
        local_lsn: u64,
        retention_floor_lsn: u64,
    ) -> RangeReplicaRepairState {
        Self::mark_for_repair(
            metadata,
            owner_epoch,
            local_lsn,
            RangeRepairReason::TooStale {
                local_lsn,
                retention_floor_lsn,
            },
        )
    }

    pub fn repair_from_healthy_owner(
        target_layout: &ClusterRangeLayout,
        state: &mut RangeReplicaRepairState,
        snapshot: &RangeSnapshot,
        source_wal_path: impl AsRef<Path>,
        request: RangeRepairRequest,
    ) -> Result<RangeRepairOutcome, RangeRepairError> {
        if !matches!(state.health, RangeReplicaHealth::RepairRequired { .. }) {
            return Err(RangeRepairError::NotMarkedForRepair {
                range_id: state.range_id.clone(),
            });
        }
        if state.range_id != request.range_id {
            return Err(RangeRepairError::WrongLocalRange {
                expected: request.range_id.clone(),
                actual: state.range_id.clone(),
            });
        }
        if snapshot.metadata.logical_range_id != request.range_id {
            return Err(RangeRepairError::WrongSnapshotRange {
                expected: request.range_id.clone(),
                actual: snapshot.metadata.logical_range_id.clone(),
            });
        }

        let reason = match &state.health {
            RangeReplicaHealth::RepairRequired { reason } => reason.quarantine_label(),
            RangeReplicaHealth::Healthy { .. } => unreachable!(),
        };
        let quarantine = target_layout
            .quarantine_range(&state.metadata, reason)
            .map_err(RangeRepairError::Quarantine)?;
        let installed_metadata =
            target_layout
                .install_range_snapshot(snapshot)
                .map_err(|source| RangeRepairError::InstallSnapshot {
                    quarantine: quarantine.clone(),
                    source,
                })?;
        let mut target_state =
            MoveRangeTargetState::from_installed_snapshot(snapshot, installed_metadata.clone());
        let catch_up = MoveRangeTracer::catch_up_from_wal(
            &mut target_state,
            source_wal_path,
            request.required_watermark_lsn,
        )
        .map_err(|source| RangeRepairError::CatchUp {
            quarantine: quarantine.clone(),
            source,
        })?;
        MoveRangeTracer::cutover(
            &MoveRangeRequest {
                range_id: request.range_id.clone(),
                required_watermark_lsn: request.required_watermark_lsn,
                source_owner_epoch: request.source_owner_epoch,
                target_owner_epoch: request.repaired_owner_epoch,
            },
            &target_state,
        )
        .map_err(|source| RangeRepairError::Cutover {
            quarantine: quarantine.clone(),
            source,
        })?;

        state.metadata = installed_metadata.clone();
        state.owner_epoch = request.repaired_owner_epoch;
        state.applied_lsn = target_state.applied_lsn;
        state.health = RangeReplicaHealth::Healthy {
            owner_epoch: request.repaired_owner_epoch,
            applied_lsn: target_state.applied_lsn,
        };

        Ok(RangeRepairOutcome {
            range_id: request.range_id,
            quarantine,
            installed_metadata,
            catch_up,
            healthy: state.health.clone(),
        })
    }

    fn mark_for_repair(
        metadata: RangeMetadata,
        owner_epoch: u64,
        applied_lsn: u64,
        reason: RangeRepairReason,
    ) -> RangeReplicaRepairState {
        RangeReplicaRepairState {
            range_id: metadata.logical_range_id.clone(),
            metadata,
            owner_epoch,
            applied_lsn,
            health: RangeReplicaHealth::RepairRequired { reason },
        }
    }
}

impl RangeRepairReason {
    fn quarantine_label(&self) -> &'static str {
        match self {
            Self::Corrupt { .. } => "corrupt",
            Self::TooStale { .. } => "too-stale",
        }
    }
}
