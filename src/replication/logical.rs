//! Logical replication helpers shared by replica apply and point-in-time restore.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::api::{RedDBError, RedDBResult};
use crate::application::entity::metadata_from_json;
use crate::replication::cdc::{ChangeOperation, ChangeRecord};
use crate::storage::{EntityId, RedDB, UnifiedStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyMode {
    Replica,
    Restore,
}

/// PLAN.md Phase 11.5 — counters the replica apply loop bumps when an
/// invariant breaks. Surfaced via `reddb_replica_apply_errors_total`.
/// Decode errors aren't strictly apply errors but they share the same
/// observability lane so dashboards alert on "replica is ingesting
/// trash from primary regardless of cause".
#[derive(Debug, Default)]
pub struct ReplicaApplyMetrics {
    pub gap_total: std::sync::atomic::AtomicU64,
    pub divergence_total: std::sync::atomic::AtomicU64,
    pub apply_error_total: std::sync::atomic::AtomicU64,
    pub decode_error_total: std::sync::atomic::AtomicU64,
}

impl ReplicaApplyMetrics {
    pub fn record(&self, kind: ApplyErrorKind) {
        use std::sync::atomic::Ordering::Relaxed;
        match kind {
            ApplyErrorKind::Gap => {
                self.gap_total.fetch_add(1, Relaxed);
            }
            ApplyErrorKind::Divergence => {
                self.divergence_total.fetch_add(1, Relaxed);
            }
            ApplyErrorKind::Apply => {
                self.apply_error_total.fetch_add(1, Relaxed);
            }
            ApplyErrorKind::Decode => {
                self.decode_error_total.fetch_add(1, Relaxed);
            }
        }
    }

    pub fn snapshot(&self) -> [(ApplyErrorKind, u64); 4] {
        use std::sync::atomic::Ordering::Relaxed;
        [
            (ApplyErrorKind::Gap, self.gap_total.load(Relaxed)),
            (ApplyErrorKind::Divergence, self.divergence_total.load(Relaxed)),
            (ApplyErrorKind::Apply, self.apply_error_total.load(Relaxed)),
            (ApplyErrorKind::Decode, self.decode_error_total.load(Relaxed)),
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyErrorKind {
    Gap,
    Divergence,
    Apply,
    Decode,
}

impl ApplyErrorKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Gap => "gap",
            Self::Divergence => "divergence",
            Self::Apply => "apply",
            Self::Decode => "decode",
        }
    }
}

impl LogicalApplyError {
    pub fn kind(&self) -> ApplyErrorKind {
        match self {
            Self::Gap { .. } => ApplyErrorKind::Gap,
            Self::Divergence { .. } => ApplyErrorKind::Divergence,
            Self::Apply { .. } => ApplyErrorKind::Apply,
        }
    }
}

/// Outcome of a single `apply` call. `Applied` advances the chain;
/// `Idempotent` and `Skipped` are no-ops (we already saw an
/// equal-or-newer LSN). `Gap` and `Divergence` (returned via
/// `LogicalApplyError`) are fail-closed — callers (replica fetcher,
/// restore loop) should mark the instance unhealthy and stop applying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Normal monotonic advance.
    Applied,
    /// Same LSN as last applied with same payload hash — log + skip.
    Idempotent,
    /// Older LSN than what we already have — log + skip.
    Skipped,
}

#[derive(Debug)]
pub enum LogicalApplyError {
    Gap {
        last: u64,
        next: u64,
    },
    Divergence {
        lsn: u64,
        expected: String,
        got: String,
    },
    Apply {
        lsn: u64,
        source: RedDBError,
    },
}

impl std::fmt::Display for LogicalApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gap { last, next } => write!(f, "LSN gap on apply: last={last} next={next}"),
            Self::Divergence {
                lsn,
                expected,
                got,
            } => write!(
                f,
                "LSN divergence on apply at lsn={lsn}: expected payload hash {expected}, got {got}"
            ),
            Self::Apply { lsn, source } => write!(f, "apply error at lsn={lsn}: {source}"),
        }
    }
}

impl std::error::Error for LogicalApplyError {}

/// Shared logical change applier so replica replay and PITR converge on the
/// same semantics. Stateful (PLAN.md Phase 11.5): tracks the last applied
/// LSN + payload hash so duplicates / older LSNs / gaps / divergences are
/// detected explicitly.
pub struct LogicalChangeApplier {
    last_applied_lsn: AtomicU64,
    last_payload_hash: Mutex<Option<[u8; 32]>>,
}

impl LogicalChangeApplier {
    /// Build a fresh applier. `starting_lsn` is the LSN already
    /// covered by the snapshot (or `0` for an empty replica). The
    /// next acceptable record is any positive LSN; from there the
    /// chain advances by 1.
    pub fn new(starting_lsn: u64) -> Self {
        Self {
            last_applied_lsn: AtomicU64::new(starting_lsn),
            last_payload_hash: Mutex::new(None),
        }
    }

    pub fn last_applied_lsn(&self) -> u64 {
        self.last_applied_lsn.load(Ordering::Acquire)
    }

    /// Apply one logical change record. The state machine:
    /// - first record after `starting_lsn == 0` → apply, anchor.
    /// - `lsn == last + 1` → apply, advance.
    /// - `lsn == last` && payload hash equal → idempotent skip.
    /// - `lsn == last` && payload hash differs → `Divergence` (fail closed).
    /// - `lsn < last` → older replay, skip with debug log.
    /// - `lsn > last + 1` → `Gap` (fail closed; caller marks unhealthy).
    pub fn apply(
        &self,
        db: &RedDB,
        record: &ChangeRecord,
        mode: ApplyMode,
    ) -> Result<ApplyOutcome, LogicalApplyError> {
        let last = self.last_applied_lsn.load(Ordering::Acquire);
        let payload_hash = record_payload_hash(record);

        if last == 0 && record.lsn > 0 {
            self.do_apply(db, record, mode)?;
            self.last_applied_lsn.store(record.lsn, Ordering::Release);
            *self.last_payload_hash.lock().expect("payload hash mutex") = Some(payload_hash);
            return Ok(ApplyOutcome::Applied);
        }

        if record.lsn == last {
            let prior = self
                .last_payload_hash
                .lock()
                .expect("payload hash mutex")
                .clone();
            return match prior {
                Some(p) if p == payload_hash => Ok(ApplyOutcome::Idempotent),
                Some(p) => Err(LogicalApplyError::Divergence {
                    lsn: record.lsn,
                    expected: hex_digest(&p),
                    got: hex_digest(&payload_hash),
                }),
                None => Ok(ApplyOutcome::Idempotent),
            };
        }
        if record.lsn < last {
            return Ok(ApplyOutcome::Skipped);
        }
        if record.lsn > last + 1 {
            return Err(LogicalApplyError::Gap {
                last,
                next: record.lsn,
            });
        }

        self.do_apply(db, record, mode)?;
        self.last_applied_lsn.store(record.lsn, Ordering::Release);
        *self.last_payload_hash.lock().expect("payload hash mutex") = Some(payload_hash);
        Ok(ApplyOutcome::Applied)
    }

    fn do_apply(
        &self,
        db: &RedDB,
        record: &ChangeRecord,
        mode: ApplyMode,
    ) -> Result<(), LogicalApplyError> {
        Self::apply_record(db, record, mode).map_err(|err| LogicalApplyError::Apply {
            lsn: record.lsn,
            source: err,
        })
    }

    /// Stateless apply — applies the record without monotonicity
    /// checks. Kept for callers that don't yet thread the stateful
    /// applier through. New code should prefer
    /// `LogicalChangeApplier::new()` + `apply()`.
    pub fn apply_record(db: &RedDB, record: &ChangeRecord, _mode: ApplyMode) -> RedDBResult<()> {
        let store = db.store();
        match record.operation {
            ChangeOperation::Delete => {
                let _ = store.delete(&record.collection, EntityId::new(record.entity_id));
            }
            ChangeOperation::Insert | ChangeOperation::Update => {
                let Some(bytes) = &record.entity_bytes else {
                    return Err(RedDBError::Internal(
                        "replication record missing entity payload".to_string(),
                    ));
                };
                let entity = UnifiedStore::deserialize_entity(bytes, store.format_version())
                    .map_err(|err| RedDBError::Internal(err.to_string()))?;
                let exists = store
                    .get(&record.collection, EntityId::new(record.entity_id))
                    .is_some();
                if exists {
                    let manager = store
                        .get_collection(&record.collection)
                        .ok_or_else(|| RedDBError::NotFound(record.collection.clone()))?;
                    manager
                        .update(entity.clone())
                        .map_err(|err| RedDBError::Internal(err.to_string()))?;
                } else {
                    store
                        .insert_auto(&record.collection, entity.clone())
                        .map_err(|err| RedDBError::Internal(err.to_string()))?;
                }
                if let Some(metadata_json) = &record.metadata {
                    let metadata = metadata_from_json(metadata_json)
                        .map_err(|err| RedDBError::Internal(err.to_string()))?;
                    store
                        .set_metadata(&record.collection, entity.id, metadata)
                        .map_err(|err| RedDBError::Internal(err.to_string()))?;
                }
                store
                    .context_index()
                    .index_entity(&record.collection, &entity);
            }
        }
        Ok(())
    }
}

fn record_payload_hash(record: &ChangeRecord) -> [u8; 32] {
    let mut hasher = crate::crypto::sha256::Sha256::new();
    hasher.update(&record.lsn.to_le_bytes());
    hasher.update(&[record.operation as u8]);
    hasher.update(record.collection.as_bytes());
    hasher.update(&record.entity_id.to_le_bytes());
    if let Some(bytes) = &record.entity_bytes {
        hasher.update(bytes);
    }
    hasher.finalize()
}

fn hex_digest(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replication::cdc::ChangeOperation;
    use crate::storage::{EntityId, RedDB, UnifiedEntity, UnifiedStore};

    fn open_db() -> (RedDB, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "reddb_logical_apply_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        let db = RedDB::open(&path).unwrap();
        (db, path)
    }

    fn record(lsn: u64, payload: &[u8]) -> ChangeRecord {
        let entity = UnifiedEntity::new(EntityId::new(lsn), payload.to_vec());
        ChangeRecord {
            lsn,
            timestamp: 100 + lsn,
            operation: ChangeOperation::Insert,
            collection: "users".to_string(),
            entity_id: lsn,
            entity_kind: "row".to_string(),
            entity_bytes: Some(UnifiedStore::serialize_entity(&entity, crate::api::REDDB_FORMAT_VERSION)),
            metadata: None,
        }
    }

    #[test]
    fn apply_advances_on_monotonic_lsn() {
        let (db, path) = open_db();
        let applier = LogicalChangeApplier::new(0);
        assert_eq!(
            applier.apply(&db, &record(1, b"a"), ApplyMode::Replica).unwrap(),
            ApplyOutcome::Applied
        );
        assert_eq!(applier.last_applied_lsn(), 1);
        assert_eq!(
            applier.apply(&db, &record(2, b"b"), ApplyMode::Replica).unwrap(),
            ApplyOutcome::Applied
        );
        assert_eq!(applier.last_applied_lsn(), 2);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn apply_idempotent_on_duplicate_lsn_same_payload() {
        let (db, path) = open_db();
        let applier = LogicalChangeApplier::new(0);
        let r = record(5, b"same");
        applier.apply(&db, &r, ApplyMode::Replica).unwrap();
        assert_eq!(
            applier.apply(&db, &r, ApplyMode::Replica).unwrap(),
            ApplyOutcome::Idempotent
        );
        assert_eq!(applier.last_applied_lsn(), 5);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn apply_fails_closed_on_lsn_collision_diff_payload() {
        let (db, path) = open_db();
        let applier = LogicalChangeApplier::new(0);
        applier.apply(&db, &record(7, b"first"), ApplyMode::Replica).unwrap();
        let err = applier
            .apply(&db, &record(7, b"different"), ApplyMode::Replica)
            .unwrap_err();
        assert!(
            matches!(err, LogicalApplyError::Divergence { lsn: 7, .. }),
            "got {err:?}"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn apply_skips_older_lsn() {
        let (db, path) = open_db();
        let applier = LogicalChangeApplier::new(0);
        applier.apply(&db, &record(1, b"a"), ApplyMode::Replica).unwrap();
        applier.apply(&db, &record(2, b"b"), ApplyMode::Replica).unwrap();
        assert_eq!(
            applier.apply(&db, &record(1, b"a"), ApplyMode::Replica).unwrap(),
            ApplyOutcome::Skipped
        );
        assert_eq!(applier.last_applied_lsn(), 2);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn apply_returns_gap_on_future_lsn() {
        let (db, path) = open_db();
        let applier = LogicalChangeApplier::new(0);
        applier.apply(&db, &record(1, b"a"), ApplyMode::Replica).unwrap();
        let err = applier
            .apply(&db, &record(5, b"e"), ApplyMode::Replica)
            .unwrap_err();
        assert!(
            matches!(err, LogicalApplyError::Gap { last: 1, next: 5 }),
            "got {err:?}"
        );
        assert_eq!(applier.last_applied_lsn(), 1);
        let _ = std::fs::remove_file(path);
    }
}
