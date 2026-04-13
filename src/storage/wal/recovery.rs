//! Point-in-Time Recovery (PITR) built on top of logical WAL segments.

use std::path::Path;
use std::sync::Arc;

use super::{load_archived_change_records, load_backup_head, load_snapshot_manifest};
use crate::replication::logical::{ApplyMode, LogicalChangeApplier};
use crate::storage::backend::{BackendError, RemoteBackend};
use crate::storage::RedDB;

/// A point to which the database can be restored.
#[derive(Debug, Clone)]
pub struct RestorePoint {
    pub snapshot_id: u64,
    pub snapshot_time: u64,
    pub wal_segment_count: usize,
    pub latest_recoverable_time: u64,
}

/// Result of a PITR operation.
#[derive(Debug, Clone)]
pub struct RecoveryResult {
    pub snapshot_used: u64,
    pub wal_segments_replayed: usize,
    pub records_applied: u64,
    pub recovered_to_lsn: u64,
    pub recovered_to_time: u64,
}

#[derive(Debug, Clone)]
pub struct RestorePlan {
    pub timeline_id: String,
    pub snapshot_key: String,
    pub snapshot_id: u64,
    pub snapshot_time: u64,
    pub base_lsn: u64,
    pub target_time: u64,
    pub wal_segments: Vec<String>,
}

#[derive(Debug, Clone)]
struct SnapshotDescriptor {
    key: String,
    snapshot_id: u64,
    snapshot_time: u64,
    timeline_id: String,
    base_lsn: u64,
}

#[derive(Debug, Clone)]
struct WalSegmentDescriptor {
    key: String,
    lsn_start: u64,
    lsn_end: u64,
}

/// Point-in-Time Recovery engine.
pub struct PointInTimeRecovery {
    backend: Arc<dyn RemoteBackend>,
    snapshot_prefix: String,
    wal_prefix: String,
}

impl PointInTimeRecovery {
    pub fn new(
        backend: Arc<dyn RemoteBackend>,
        snapshot_prefix: impl Into<String>,
        wal_prefix: impl Into<String>,
    ) -> Self {
        Self {
            backend,
            snapshot_prefix: snapshot_prefix.into(),
            wal_prefix: wal_prefix.into(),
        }
    }

    pub fn plan_restore(&self, target_time: u64) -> Result<RestorePlan, BackendError> {
        let snapshots = self.list_snapshots()?;
        let selected = snapshots
            .iter()
            .filter(|snapshot| snapshot.snapshot_time <= target_time || target_time == 0)
            .max_by_key(|snapshot| snapshot.snapshot_time)
            .ok_or_else(|| {
                BackendError::NotFound(format!(
                    "no snapshot available at or before target timestamp {target_time}"
                ))
            })?;

        let wal_segments = self
            .list_wal_segments()?
            .into_iter()
            .filter(|segment| segment.lsn_end > selected.base_lsn)
            .map(|segment| segment.key)
            .collect();

        Ok(RestorePlan {
            timeline_id: selected.timeline_id.clone(),
            snapshot_key: selected.key.clone(),
            snapshot_id: selected.snapshot_id,
            snapshot_time: selected.snapshot_time,
            base_lsn: selected.base_lsn,
            target_time,
            wal_segments,
        })
    }

    pub fn restore_to(
        &self,
        target_time: u64,
        dest_path: &Path,
    ) -> Result<RecoveryResult, BackendError> {
        let plan = self.plan_restore(target_time)?;
        self.execute_restore(&plan, dest_path)
    }

    pub fn execute_restore(
        &self,
        plan: &RestorePlan,
        dest_path: &Path,
    ) -> Result<RecoveryResult, BackendError> {
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                BackendError::Transport(format!(
                    "create restore destination directory failed: {err}"
                ))
            })?;
        }

        let downloaded = self.backend.download(&plan.snapshot_key, dest_path)?;
        if !downloaded {
            return Err(BackendError::NotFound(format!(
                "snapshot '{}' disappeared during restore",
                plan.snapshot_key
            )));
        }

        let db = RedDB::open(dest_path).map_err(|err| {
            BackendError::Internal(format!("open restore database failed: {err}"))
        })?;

        let mut wal_segments_replayed = 0usize;
        let mut records_applied = 0u64;
        let mut recovered_to_lsn = plan.base_lsn;
        let mut recovered_to_time = plan.snapshot_time;

        for segment_key in &plan.wal_segments {
            let records = load_archived_change_records(self.backend.as_ref(), segment_key)?;
            let mut segment_applied = false;
            for record in records {
                if record.lsn <= plan.base_lsn {
                    continue;
                }
                if plan.target_time != 0 && record.timestamp > plan.target_time {
                    continue;
                }
                LogicalChangeApplier::apply_record(&db, &record, ApplyMode::Restore)
                    .map_err(|err| BackendError::Internal(err.to_string()))?;
                recovered_to_lsn = recovered_to_lsn.max(record.lsn);
                recovered_to_time = recovered_to_time.max(record.timestamp);
                records_applied += 1;
                segment_applied = true;
            }
            if segment_applied {
                wal_segments_replayed += 1;
            }
        }

        db.flush().map_err(|err| {
            BackendError::Internal(format!("flush restored database failed: {err}"))
        })?;

        Ok(RecoveryResult {
            snapshot_used: plan.snapshot_id,
            wal_segments_replayed,
            records_applied,
            recovered_to_lsn,
            recovered_to_time,
        })
    }

    pub fn list_restore_points(&self) -> Result<Vec<RestorePoint>, BackendError> {
        let snapshots = self.list_snapshots()?;
        let wal_segments = self.list_wal_segments()?;
        let mut out = Vec::new();

        for snapshot in snapshots {
            let wal_segment_count = wal_segments
                .iter()
                .filter(|segment| segment.lsn_end > snapshot.base_lsn)
                .count();
            out.push(RestorePoint {
                snapshot_id: snapshot.snapshot_id,
                snapshot_time: snapshot.snapshot_time,
                wal_segment_count,
                latest_recoverable_time: snapshot.snapshot_time,
            });
        }

        out.sort_by_key(|point| point.snapshot_time);
        Ok(out)
    }

    fn list_snapshots(&self) -> Result<Vec<SnapshotDescriptor>, BackendError> {
        let snapshots = self.backend.list(&self.snapshot_prefix)?;
        let mut out = Vec::new();
        for key in snapshots {
            let Some(file_name) = std::path::Path::new(&key)
                .file_name()
                .and_then(|s| s.to_str())
            else {
                continue;
            };
            let Some(base) = file_name.strip_suffix(".snapshot") else {
                continue;
            };
            let Some((snapshot_id, snapshot_time)) = base.split_once('-') else {
                continue;
            };
            let (Ok(snapshot_id), Ok(snapshot_time)) =
                (snapshot_id.parse::<u64>(), snapshot_time.parse::<u64>())
            else {
                continue;
            };
            let manifest = load_snapshot_manifest(self.backend.as_ref(), &key)?;
            let (timeline_id, base_lsn) = manifest
                .map(|manifest| (manifest.timeline_id, manifest.base_lsn))
                .or_else(|| {
                    self.load_current_head()
                        .filter(|head| head.snapshot_id == snapshot_id)
                        .map(|head| (head.timeline_id, head.current_lsn))
                })
                .unwrap_or_else(|| ("main".to_string(), 0));

            out.push(SnapshotDescriptor {
                key,
                snapshot_id,
                snapshot_time,
                timeline_id,
                base_lsn,
            });
        }
        out.sort_by_key(|snapshot| snapshot.snapshot_time);
        Ok(out)
    }

    fn list_wal_segments(&self) -> Result<Vec<WalSegmentDescriptor>, BackendError> {
        let keys = self.backend.list(&self.wal_prefix)?;
        let mut out = Vec::new();
        for key in keys {
            let Some(file_name) = std::path::Path::new(&key)
                .file_name()
                .and_then(|s| s.to_str())
            else {
                continue;
            };
            let Some((start, end)) = file_name
                .strip_suffix(".wal")
                .and_then(|base| base.split_once('-'))
            else {
                continue;
            };
            let (Ok(lsn_start), Ok(lsn_end)) = (start.parse::<u64>(), end.parse::<u64>()) else {
                continue;
            };
            out.push(WalSegmentDescriptor {
                key,
                lsn_start,
                lsn_end,
            });
        }
        out.sort_by_key(|segment| segment.lsn_start);
        Ok(out)
    }

    fn load_current_head(&self) -> Option<super::BackupHead> {
        let snapshot_root = self.snapshot_prefix.trim_end_matches('/');
        let parent = Path::new(snapshot_root).parent()?;
        let parent = parent.to_string_lossy().trim_end_matches('/').to_string();
        let head_key = if parent.is_empty() {
            "manifests/head.json".to_string()
        } else {
            format!("{parent}/manifests/head.json")
        };
        load_backup_head(self.backend.as_ref(), &head_key)
            .ok()
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::backend::LocalBackend;
    use crate::storage::wal::{publish_snapshot_manifest, SnapshotManifest};

    #[test]
    fn restore_to_downloads_latest_snapshot_before_target() {
        let temp_dir =
            std::env::temp_dir().join(format!("reddb_pitr_restore_{}_{}", std::process::id(), 1));
        let snapshot_dir = temp_dir.join("snapshots");
        let restore_path = temp_dir.join("restore").join("data.rdb");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&snapshot_dir).unwrap();

        let snapshot1 = snapshot_dir.join("1-100.snapshot");
        let snapshot2 = snapshot_dir.join("2-200.snapshot");
        RedDB::open(&snapshot1).unwrap().flush().unwrap();
        RedDB::open(&snapshot2).unwrap().flush().unwrap();
        publish_snapshot_manifest(
            &LocalBackend,
            &SnapshotManifest {
                timeline_id: "main".to_string(),
                snapshot_key: snapshot1.to_string_lossy().to_string(),
                snapshot_id: 1,
                snapshot_time: 100,
                base_lsn: 0,
                schema_version: crate::api::REDDB_FORMAT_VERSION,
                format_version: crate::api::REDDB_FORMAT_VERSION,
            },
        )
        .unwrap();
        publish_snapshot_manifest(
            &LocalBackend,
            &SnapshotManifest {
                timeline_id: "main".to_string(),
                snapshot_key: snapshot2.to_string_lossy().to_string(),
                snapshot_id: 2,
                snapshot_time: 200,
                base_lsn: 0,
                schema_version: crate::api::REDDB_FORMAT_VERSION,
                format_version: crate::api::REDDB_FORMAT_VERSION,
            },
        )
        .unwrap();

        let recovery = PointInTimeRecovery::new(
            Arc::new(LocalBackend),
            snapshot_dir.to_string_lossy().to_string(),
            temp_dir.join("wal").to_string_lossy().to_string(),
        );

        let result = recovery.restore_to(150, &restore_path).unwrap();
        assert_eq!(result.snapshot_used, 1);
        assert_eq!(result.recovered_to_time, 100);
        assert!(restore_path.exists());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
