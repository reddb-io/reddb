//! Archive-backed range recovery for disaster failover.
//!
//! A compressed archive copy is not a hot mirror and cannot become a range owner
//! directly. This module models the explicit recovery path in between: restore
//! the archive payload, validate its checksum, prove which range commit
//! watermark the restored copy covers, and only then delegate the ownership move
//! to the forced-transition path that records the disaster-recovery audit.

use super::identity::NodeIdentity;
use super::ownership::{CollectionId, RangeId, ShardOwnershipCatalog};
use super::ownership_force::{
    force_transition, ForceTransitionCapability, ForcedTransitionAudit,
    ForcedTransitionDisposition, ForcedTransitionRequest, OperatorReason,
};
use super::ownership_transition::CommitWatermark;

/// Whether archive recovery must prove full watermark coverage or may proceed
/// while reporting an RPO gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveRecoveryMode {
    /// Refuse recovery unless the restored archive covers the latest committed
    /// watermark. Successful recovery is zero-RPO.
    RequireFullWatermark,
    /// Proceed through the forced ownership path even when the restored archive
    /// is behind, but report the skipped committed range as RPO evidence.
    ForceWithEvidence,
}

/// Compressed archive data for one range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedRangeReplica {
    collection: CollectionId,
    range_id: RangeId,
    archive_id: String,
    compressed_bytes: Vec<u8>,
    expected_checksum: u32,
    covered_watermark: CommitWatermark,
}

impl ArchivedRangeReplica {
    pub fn new(
        collection: CollectionId,
        range_id: RangeId,
        archive_id: impl Into<String>,
        compressed_bytes: Vec<u8>,
        expected_checksum: u32,
        covered_watermark: CommitWatermark,
    ) -> Self {
        Self {
            collection,
            range_id,
            archive_id: archive_id.into(),
            compressed_bytes,
            expected_checksum,
            covered_watermark,
        }
    }

    /// Restore the compressed payload and validate the checksum before it can be
    /// used as a recovered range seed.
    pub fn restore(&self) -> Result<RestoredArchiveReplica, ArchiveRecoveryError> {
        let seed_bytes = zstd::stream::decode_all(self.compressed_bytes.as_slice())
            .map_err(|err| ArchiveRecoveryError::RestoreFailed(err.to_string()))?;
        let computed_checksum = crc32fast::hash(&seed_bytes);
        if computed_checksum != self.expected_checksum {
            return Err(ArchiveRecoveryError::ChecksumMismatch {
                archive_id: self.archive_id.clone(),
                expected: self.expected_checksum,
                computed: computed_checksum,
            });
        }
        Ok(RestoredArchiveReplica {
            collection: self.collection.clone(),
            range_id: self.range_id,
            archive_id: self.archive_id.clone(),
            seed_bytes,
            checksum: computed_checksum,
            covered_watermark: self.covered_watermark,
        })
    }
}

/// A range seed that exists only after an explicit restore and checksum
/// validation step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoredArchiveReplica {
    collection: CollectionId,
    range_id: RangeId,
    archive_id: String,
    seed_bytes: Vec<u8>,
    checksum: u32,
    covered_watermark: CommitWatermark,
}

impl RestoredArchiveReplica {
    pub fn archive_id(&self) -> &str {
        &self.archive_id
    }

    pub fn seed_bytes(&self) -> &[u8] {
        &self.seed_bytes
    }

    pub fn checksum(&self) -> u32 {
        self.checksum
    }

    pub fn covered_watermark(&self) -> CommitWatermark {
        self.covered_watermark
    }
}

/// Request to recover a range from a restored archive seed and move ownership to
/// the recovered target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveRecoveryRequest {
    collection: CollectionId,
    range_id: RangeId,
    target: NodeIdentity,
    new_replicas: Vec<NodeIdentity>,
    latest_commit_watermark: CommitWatermark,
    mode: ArchiveRecoveryMode,
    capability: ForceTransitionCapability,
    reason: OperatorReason,
    restored: Option<RestoredArchiveReplica>,
}

impl ArchiveRecoveryRequest {
    pub fn new(
        collection: CollectionId,
        range_id: RangeId,
        target: NodeIdentity,
        latest_commit_watermark: CommitWatermark,
        mode: ArchiveRecoveryMode,
        capability: ForceTransitionCapability,
        reason: OperatorReason,
    ) -> Self {
        Self {
            collection,
            range_id,
            target,
            new_replicas: Vec::new(),
            latest_commit_watermark,
            mode,
            capability,
            reason,
            restored: None,
        }
    }

    pub fn with_replicas(mut self, replicas: impl IntoIterator<Item = NodeIdentity>) -> Self {
        self.new_replicas = replicas.into_iter().collect();
        self
    }

    pub fn with_restored_archive(mut self, restored: RestoredArchiveReplica) -> Self {
        self.restored = Some(restored);
        self
    }
}

/// The RPO gap proved by a forced archive recovery whose restored watermark is
/// behind the latest committed watermark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkippedDataEvidence {
    pub restored_watermark: CommitWatermark,
    pub latest_commit_watermark: CommitWatermark,
    pub skipped_lsn: u64,
}

/// Operator-facing RPO evidence: the restored archive watermark, latest known
/// committed watermark, and any forced recovery gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveRecoveryRpoEvidence {
    pub restored_watermark: CommitWatermark,
    pub latest_commit_watermark: CommitWatermark,
    pub skipped: Option<SkippedDataEvidence>,
}

impl ArchiveRecoveryRpoEvidence {
    pub fn is_zero_rpo(&self) -> bool {
        self.skipped.is_none()
    }

    pub fn rpo_lsn(&self) -> u64 {
        self.skipped.map(|skipped| skipped.skipped_lsn).unwrap_or(0)
    }
}

/// A successful archive recovery: the validated seed that may initialize the
/// recovered range, the ownership-force audit, and the RPO evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveRecoveryOutcome {
    pub seed: RestoredArchiveReplica,
    pub audit: ForcedTransitionAudit,
    pub evidence: ArchiveRecoveryRpoEvidence,
}

impl ArchiveRecoveryOutcome {
    pub fn is_zero_rpo(&self) -> bool {
        self.evidence.is_zero_rpo()
    }

    pub fn rpo_lsn(&self) -> u64 {
        self.evidence.rpo_lsn()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveRecoveryError {
    /// Ownership recovery was attempted without first attaching a restored seed.
    RestoreRequired,
    /// The compressed archive payload could not be decompressed.
    RestoreFailed(String),
    /// The restored bytes do not match the archive manifest checksum.
    ChecksumMismatch {
        archive_id: String,
        expected: u32,
        computed: u32,
    },
    /// The restored seed belongs to a different range than the recovery request.
    RestoredRangeMismatch {
        expected_collection: CollectionId,
        expected_range_id: RangeId,
        restored_collection: CollectionId,
        restored_range_id: RangeId,
    },
    /// The restored archive is behind the latest committed watermark and the
    /// request did not opt into forced RPO evidence.
    WatermarkGap {
        restored_watermark: CommitWatermark,
        latest_commit_watermark: CommitWatermark,
        skipped_lsn: u64,
    },
    /// The underlying forced ownership transition was denied or failed.
    ForcedOwnership(ForcedTransitionAudit),
}

impl std::fmt::Display for ArchiveRecoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RestoreRequired => write!(f, "archive recovery requires an explicit restore step"),
            Self::RestoreFailed(err) => write!(f, "archive restore failed: {err}"),
            Self::ChecksumMismatch {
                archive_id,
                expected,
                computed,
            } => write!(
                f,
                "archive {archive_id} checksum mismatch: expected {expected:#010x}, computed {computed:#010x}"
            ),
            Self::RestoredRangeMismatch {
                expected_collection,
                expected_range_id,
                restored_collection,
                restored_range_id,
            } => write!(
                f,
                "restored archive covers {restored_collection}/{restored_range_id}, expected {expected_collection}/{expected_range_id}"
            ),
            Self::WatermarkGap {
                restored_watermark,
                latest_commit_watermark,
                skipped_lsn,
            } => write!(
                f,
                "restored archive covers watermark term {} lsn {}, latest committed watermark is term {} lsn {}; forced recovery would skip {skipped_lsn} LSNs",
                restored_watermark.term,
                restored_watermark.lsn,
                latest_commit_watermark.term,
                latest_commit_watermark.lsn,
            ),
            Self::ForcedOwnership(audit) => write!(f, "{audit}"),
        }
    }
}

impl std::error::Error for ArchiveRecoveryError {}

/// Recover a range from a validated archive seed and move ownership to `target`.
///
/// The request must carry a [`RestoredArchiveReplica`], which can only be
/// obtained from [`ArchivedRangeReplica::restore`]. A restored seed that does not
/// cover the latest committed watermark is refused unless the caller explicitly
/// opts into [`ArchiveRecoveryMode::ForceWithEvidence`], in which case the
/// returned outcome reports the RPO gap.
pub fn recover_archive_replica(
    catalog: &mut ShardOwnershipCatalog,
    request: &ArchiveRecoveryRequest,
    now_ms: u64,
) -> Result<ArchiveRecoveryOutcome, ArchiveRecoveryError> {
    let restored = request
        .restored
        .clone()
        .ok_or(ArchiveRecoveryError::RestoreRequired)?;

    if restored.collection != request.collection || restored.range_id != request.range_id {
        return Err(ArchiveRecoveryError::RestoredRangeMismatch {
            expected_collection: request.collection.clone(),
            expected_range_id: request.range_id,
            restored_collection: restored.collection,
            restored_range_id: restored.range_id,
        });
    }

    let skipped = if watermark_covers(restored.covered_watermark, request.latest_commit_watermark) {
        None
    } else {
        let skipped_lsn = skipped_lsn(restored.covered_watermark, request.latest_commit_watermark);
        match request.mode {
            ArchiveRecoveryMode::RequireFullWatermark => {
                return Err(ArchiveRecoveryError::WatermarkGap {
                    restored_watermark: restored.covered_watermark,
                    latest_commit_watermark: request.latest_commit_watermark,
                    skipped_lsn,
                });
            }
            ArchiveRecoveryMode::ForceWithEvidence => Some(SkippedDataEvidence {
                restored_watermark: restored.covered_watermark,
                latest_commit_watermark: request.latest_commit_watermark,
                skipped_lsn,
            }),
        }
    };

    let force_request = ForcedTransitionRequest::new(
        request.collection.clone(),
        request.range_id,
        request.target.clone(),
    )
    .with_replicas(request.new_replicas.clone())
    .with_capability(request.capability.clone())
    .with_reason(request.reason.clone());
    let audit = force_transition(catalog, &force_request, now_ms);
    if !matches!(
        audit.disposition(),
        ForcedTransitionDisposition::Allowed { .. }
    ) {
        return Err(ArchiveRecoveryError::ForcedOwnership(audit));
    }

    Ok(ArchiveRecoveryOutcome {
        seed: restored.clone(),
        audit,
        evidence: ArchiveRecoveryRpoEvidence {
            restored_watermark: restored.covered_watermark,
            latest_commit_watermark: request.latest_commit_watermark,
            skipped,
        },
    })
}

fn watermark_covers(restored: CommitWatermark, latest: CommitWatermark) -> bool {
    restored.term > latest.term || (restored.term == latest.term && restored.lsn >= latest.lsn)
}

fn skipped_lsn(restored: CommitWatermark, latest: CommitWatermark) -> u64 {
    if restored.term == latest.term {
        latest.lsn.saturating_sub(restored.lsn)
    } else {
        latest.lsn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::ownership::{
        OwnershipEpoch, PlacementMetadata, RangeBounds, RangeOwnership, ShardKeyMode,
    };

    fn collection(name: &str) -> CollectionId {
        CollectionId::new(name).unwrap()
    }

    fn ident(cn: &str) -> NodeIdentity {
        NodeIdentity::from_certificate_subject(cn).unwrap()
    }

    fn catalog_with(owner: &str) -> (ShardOwnershipCatalog, CollectionId) {
        let orders = collection("orders");
        let mut catalog = ShardOwnershipCatalog::new();
        catalog
            .apply_update(RangeOwnership::establish(
                orders.clone(),
                RangeId::new(1),
                ShardKeyMode::Hash,
                RangeBounds::full(),
                ident(owner),
                Vec::new(),
                PlacementMetadata::with_replication_factor(3),
            ))
            .unwrap();
        (catalog, orders)
    }

    fn capability() -> ForceTransitionCapability {
        ForceTransitionCapability::granted_to(ident("CN=operator-root"))
    }

    fn reason() -> OperatorReason {
        OperatorReason::new("recover range from validated archive replica").unwrap()
    }

    fn compressed_archive(
        orders: &CollectionId,
        payload: &[u8],
        covered_watermark: CommitWatermark,
    ) -> ArchivedRangeReplica {
        let compressed = zstd::stream::encode_all(&payload[..], 0).unwrap();
        ArchivedRangeReplica::new(
            orders.clone(),
            RangeId::new(1),
            "archive-2026-06-29T08",
            compressed,
            crc32fast::hash(payload),
            covered_watermark,
        )
    }

    fn request(
        orders: &CollectionId,
        latest_commit_watermark: CommitWatermark,
        mode: ArchiveRecoveryMode,
    ) -> ArchiveRecoveryRequest {
        ArchiveRecoveryRequest::new(
            orders.clone(),
            RangeId::new(1),
            ident("CN=node-z"),
            latest_commit_watermark,
            mode,
            capability(),
            reason(),
        )
    }

    #[test]
    fn valid_restored_archive_moves_ownership_and_reports_zero_rpo() {
        let (mut catalog, orders) = catalog_with("CN=node-a");
        let archive = compressed_archive(&orders, b"range seed", CommitWatermark::new(7, 200));
        let restored = archive.restore().expect("restore validates checksum");
        let req = request(
            &orders,
            CommitWatermark::new(7, 200),
            ArchiveRecoveryMode::RequireFullWatermark,
        )
        .with_restored_archive(restored);

        let outcome = recover_archive_replica(&mut catalog, &req, 1_000)
            .expect("validated archive recovery should move ownership");

        assert_eq!(
            catalog.range(&orders, RangeId::new(1)).unwrap().owner(),
            &ident("CN=node-z")
        );
        assert_eq!(
            catalog
                .range(&orders, RangeId::new(1))
                .unwrap()
                .epoch()
                .value(),
            OwnershipEpoch::initial().value() + 1
        );
        assert!(outcome.audit.is_allowed());
        assert!(outcome.is_zero_rpo());
        assert_eq!(outcome.rpo_lsn(), 0);
        assert_eq!(outcome.seed.seed_bytes(), b"range seed");
        assert_eq!(
            outcome.evidence.restored_watermark,
            CommitWatermark::new(7, 200)
        );
    }

    #[test]
    fn recovery_requires_explicit_restore_before_ownership_moves() {
        let (mut catalog, orders) = catalog_with("CN=node-a");
        let req = request(
            &orders,
            CommitWatermark::new(7, 200),
            ArchiveRecoveryMode::RequireFullWatermark,
        );

        let err = recover_archive_replica(&mut catalog, &req, 1_000).unwrap_err();

        assert_eq!(err, ArchiveRecoveryError::RestoreRequired);
        assert_eq!(
            catalog.range(&orders, RangeId::new(1)).unwrap().owner(),
            &ident("CN=node-a")
        );
    }

    #[test]
    fn restored_archive_checksum_failure_cannot_seed_recovery() {
        let (_catalog, orders) = catalog_with("CN=node-a");
        let payload = b"range seed";
        let compressed = zstd::stream::encode_all(&payload[..], 0).unwrap();
        let archive = ArchivedRangeReplica::new(
            orders,
            RangeId::new(1),
            "archive-corrupt",
            compressed,
            crc32fast::hash(b"different seed"),
            CommitWatermark::new(7, 200),
        );

        let err = archive.restore().unwrap_err();

        match err {
            ArchiveRecoveryError::ChecksumMismatch {
                archive_id,
                expected,
                computed,
            } => {
                assert_eq!(archive_id, "archive-corrupt");
                assert_eq!(expected, crc32fast::hash(b"different seed"));
                assert_eq!(computed, crc32fast::hash(payload));
            }
            other => panic!("expected checksum mismatch, got {other:?}"),
        }
    }

    #[test]
    fn watermark_gap_without_force_refuses_recovery_and_leaves_owner() {
        let (mut catalog, orders) = catalog_with("CN=node-a");
        let archive = compressed_archive(&orders, b"range seed", CommitWatermark::new(7, 170));
        let restored = archive.restore().unwrap();
        let req = request(
            &orders,
            CommitWatermark::new(7, 200),
            ArchiveRecoveryMode::RequireFullWatermark,
        )
        .with_restored_archive(restored);

        let err = recover_archive_replica(&mut catalog, &req, 1_000).unwrap_err();

        assert_eq!(
            err,
            ArchiveRecoveryError::WatermarkGap {
                restored_watermark: CommitWatermark::new(7, 170),
                latest_commit_watermark: CommitWatermark::new(7, 200),
                skipped_lsn: 30,
            }
        );
        assert_eq!(
            catalog.range(&orders, RangeId::new(1)).unwrap().owner(),
            &ident("CN=node-a")
        );
    }

    #[test]
    fn forced_archive_recovery_reports_rpo_and_skipped_data_evidence() {
        let (mut catalog, orders) = catalog_with("CN=node-a");
        let archive = compressed_archive(&orders, b"range seed", CommitWatermark::new(7, 170));
        let restored = archive.restore().unwrap();
        let req = request(
            &orders,
            CommitWatermark::new(7, 200),
            ArchiveRecoveryMode::ForceWithEvidence,
        )
        .with_restored_archive(restored);

        let outcome = recover_archive_replica(&mut catalog, &req, 2_000)
            .expect("forced recovery should move ownership with evidence");

        assert_eq!(
            catalog.range(&orders, RangeId::new(1)).unwrap().owner(),
            &ident("CN=node-z")
        );
        assert!(!outcome.is_zero_rpo());
        assert_eq!(outcome.rpo_lsn(), 30);
        assert_eq!(
            outcome.evidence.skipped,
            Some(SkippedDataEvidence {
                restored_watermark: CommitWatermark::new(7, 170),
                latest_commit_watermark: CommitWatermark::new(7, 200),
                skipped_lsn: 30,
            })
        );
        assert!(outcome.audit.to_string().contains("ALLOWED"));
    }
}
