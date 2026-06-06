//! Operational local backup/restore tracer.
//!
//! This is deliberately narrower than the remote PITR archiver: it
//! checkpoints a live local/primary store, copies the checkpointed
//! database file plus the retained logical WAL suffix, and restores by
//! validating all checksums before opening the copied store.

use std::path::{Path, PathBuf};

use crate::json::{Map, Value as JsonValue};
use crate::replication::cdc::ChangeRecord;
use crate::replication::logical::{ApplyMode, ApplyOutcome, LogicalChangeApplier};
use crate::replication::primary::LogicalWalSpool;
use crate::storage::backend::BackendError;
use crate::{RedDBOptions, RedDBRuntime};

use super::{sha256_file_hex, SnapshotManifest};

const MANIFEST_FILE: &str = "MANIFEST.json";
const MANIFEST_SHA_FILE: &str = "MANIFEST.sha256";
const DATA_FILE: &str = "data.rdb";
const LOGICAL_WAL_FILE: &str = "logical.wal";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationalBackupFile {
    pub role: String,
    pub relative_path: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationalBackupManifest {
    pub version: u32,
    pub checkpoint_lsn: u64,
    pub wal_retention_floor_lsn: u64,
    pub wal_start_lsn: u64,
    pub current_lsn: u64,
    pub files: Vec<OperationalBackupFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationalBackupResult {
    pub manifest_path: PathBuf,
    pub manifest_sha256: String,
    pub checkpoint_lsn: u64,
    pub wal_start_lsn: u64,
    pub current_lsn: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationalRestoreResult {
    pub manifest_sha256: String,
    pub target_lsn: u64,
    pub recovered_to_lsn: u64,
    pub records_applied: u64,
}

pub fn create_operational_backup(
    runtime: &RedDBRuntime,
    backup_dir: &Path,
) -> Result<OperationalBackupResult, BackendError> {
    let db = runtime.db();
    let source_path = db.path().ok_or_else(|| {
        BackendError::Config("operational backup requires a persistent local store".to_string())
    })?;

    runtime
        .checkpoint()
        .map_err(|err| BackendError::Internal(format!("checkpoint before backup failed: {err}")))?;

    std::fs::create_dir_all(backup_dir).map_err(|err| {
        BackendError::Transport(format!(
            "create backup directory {} failed: {err}",
            backup_dir.display()
        ))
    })?;

    let replication = db.replication.as_ref();
    let current_lsn = replication
        .map(|primary| {
            primary
                .logical_wal_spool
                .as_ref()
                .map(|spool| spool.current_lsn())
                .unwrap_or_else(|| primary.wal_buffer.current_lsn())
        })
        .unwrap_or(0);
    let checkpoint_lsn = current_lsn;
    let wal_start_lsn = checkpoint_lsn;

    let mut files = Vec::new();
    let data_dest = backup_dir.join(DATA_FILE);
    copy_file(source_path, &data_dest)?;
    files.push(file_entry("data", DATA_FILE, &data_dest)?);

    if let Some(spool) = replication.and_then(|primary| primary.logical_wal_spool.as_ref()) {
        let records = spool.read_since(wal_start_lsn, usize::MAX).map_err(|err| {
            BackendError::Internal(format!("read retained logical WAL failed: {err}"))
        })?;
        let wal_dest = backup_dir.join(LOGICAL_WAL_FILE);
        let backup_spool = LogicalWalSpool::open(&wal_dest).map_err(|err| {
            BackendError::Internal(format!("create backup logical WAL failed: {err}"))
        })?;
        for (lsn, bytes) in records {
            backup_spool.append(lsn, &bytes).map_err(|err| {
                BackendError::Internal(format!(
                    "write backup logical WAL at lsn {lsn} failed: {err}"
                ))
            })?;
        }
        if !wal_dest.exists() {
            std::fs::File::create(&wal_dest).map_err(|err| {
                BackendError::Transport(format!(
                    "create empty backup logical WAL {} failed: {err}",
                    wal_dest.display()
                ))
            })?;
        }
        files.push(file_entry("logical_wal", LOGICAL_WAL_FILE, &wal_dest)?);
    }

    let manifest = OperationalBackupManifest {
        version: 1,
        checkpoint_lsn,
        wal_retention_floor_lsn: wal_start_lsn,
        wal_start_lsn,
        current_lsn,
        files,
    };
    let manifest_path = backup_dir.join(MANIFEST_FILE);
    write_manifest(&manifest, &manifest_path)?;
    let manifest_sha256 = SnapshotManifest::compute_snapshot_sha256(&manifest_path)?;
    std::fs::write(
        backup_dir.join(MANIFEST_SHA_FILE),
        format!("{manifest_sha256}\n"),
    )
    .map_err(|err| BackendError::Transport(format!("write manifest checksum failed: {err}")))?;

    Ok(OperationalBackupResult {
        manifest_path,
        manifest_sha256,
        checkpoint_lsn,
        wal_start_lsn,
        current_lsn,
    })
}

pub fn restore_operational_backup_to_lsn(
    backup_dir: &Path,
    dest_path: &Path,
    target_lsn: u64,
) -> Result<OperationalRestoreResult, BackendError> {
    let manifest_path = backup_dir.join(MANIFEST_FILE);
    let manifest_sha_path = backup_dir.join(MANIFEST_SHA_FILE);
    let expected_manifest_sha = std::fs::read_to_string(&manifest_sha_path).map_err(|err| {
        BackendError::Transport(format!(
            "read manifest checksum {} failed: {err}",
            manifest_sha_path.display()
        ))
    })?;
    let expected_manifest_sha = expected_manifest_sha.trim();
    let actual_manifest_sha = SnapshotManifest::compute_snapshot_sha256(&manifest_path)?;
    if !actual_manifest_sha.eq_ignore_ascii_case(expected_manifest_sha) {
        return Err(BackendError::Internal(format!(
            "manifest checksum mismatch: expected {expected_manifest_sha}, computed {actual_manifest_sha}"
        )));
    }

    let manifest = read_manifest(&manifest_path)?;
    for file in &manifest.files {
        let source = backup_dir.join(&file.relative_path);
        let actual = sha256_file_hex(&source)?;
        if !actual.eq_ignore_ascii_case(&file.sha256) {
            return Err(BackendError::Internal(format!(
                "backup file checksum mismatch for '{}': manifest sha256 {} != computed sha256 {}",
                file.relative_path, file.sha256, actual
            )));
        }
    }

    let data_file = manifest
        .files
        .iter()
        .find(|file| file.role == "data")
        .ok_or_else(|| BackendError::Internal("backup manifest missing data file".to_string()))?;
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            BackendError::Transport(format!(
                "create restore destination directory {} failed: {err}",
                parent.display()
            ))
        })?;
    }
    copy_file(&backup_dir.join(&data_file.relative_path), dest_path)?;

    let db = crate::storage::RedDB::open_with_options(&RedDBOptions::persistent(dest_path))
        .map_err(|err| BackendError::Internal(format!("open restored store failed: {err}")))?;
    let mut records_applied = 0;
    let mut recovered_to_lsn = manifest.checkpoint_lsn;

    if target_lsn > manifest.checkpoint_lsn {
        if let Some(wal_file) = manifest
            .files
            .iter()
            .find(|file| file.role == "logical_wal")
        {
            let spool = LogicalWalSpool::open(&backup_dir.join(&wal_file.relative_path)).map_err(
                |err| BackendError::Internal(format!("open backup logical WAL failed: {err}")),
            )?;
            let applier = LogicalChangeApplier::new(manifest.checkpoint_lsn);
            for (lsn, bytes) in spool
                .read_since(manifest.checkpoint_lsn, usize::MAX)
                .map_err(|err| BackendError::Internal(format!("read backup WAL failed: {err}")))?
            {
                if lsn > target_lsn {
                    break;
                }
                let record = ChangeRecord::decode(&bytes).map_err(|err| {
                    BackendError::Internal(format!(
                        "decode backup WAL record lsn={lsn} failed: {err}"
                    ))
                })?;
                match applier.apply(&db, &record, ApplyMode::Restore) {
                    Ok(ApplyOutcome::Applied) => {
                        records_applied += 1;
                        recovered_to_lsn = recovered_to_lsn.max(record.lsn);
                    }
                    Ok(ApplyOutcome::Idempotent) | Ok(ApplyOutcome::Skipped) => {}
                    Err(err) => {
                        return Err(BackendError::Internal(format!(
                            "restore apply failed at lsn {}: {}",
                            record.lsn, err
                        )));
                    }
                }
            }
        }
    }

    db.flush().map_err(|err| {
        BackendError::Internal(format!("flush restored operational backup failed: {err}"))
    })?;

    Ok(OperationalRestoreResult {
        manifest_sha256: actual_manifest_sha,
        target_lsn,
        recovered_to_lsn,
        records_applied,
    })
}

fn copy_file(source: &Path, dest: &Path) -> Result<(), BackendError> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            BackendError::Transport(format!(
                "create directory {} failed: {err}",
                parent.display()
            ))
        })?;
    }
    std::fs::copy(source, dest).map_err(|err| {
        BackendError::Transport(format!(
            "copy {} to {} failed: {err}",
            source.display(),
            dest.display()
        ))
    })?;
    Ok(())
}

fn file_entry(
    role: &str,
    relative_path: &str,
    path: &Path,
) -> Result<OperationalBackupFile, BackendError> {
    let size_bytes = std::fs::metadata(path)
        .map_err(|err| BackendError::Transport(format!("stat {} failed: {err}", path.display())))?
        .len();
    Ok(OperationalBackupFile {
        role: role.to_string(),
        relative_path: relative_path.to_string(),
        size_bytes,
        sha256: sha256_file_hex(path)?,
    })
}

fn write_manifest(manifest: &OperationalBackupManifest, path: &Path) -> Result<(), BackendError> {
    std::fs::write(path, manifest.to_json_value().to_string_pretty()).map_err(|err| {
        BackendError::Transport(format!("write manifest {} failed: {err}", path.display()))
    })
}

fn read_manifest(path: &Path) -> Result<OperationalBackupManifest, BackendError> {
    let text = std::fs::read_to_string(path).map_err(|err| {
        BackendError::Transport(format!("read manifest {} failed: {err}", path.display()))
    })?;
    let value: JsonValue = crate::json::from_str(&text).map_err(|err| {
        BackendError::Internal(format!(
            "parse backup manifest {} failed: {err}",
            path.display()
        ))
    })?;
    OperationalBackupManifest::from_json_value(&value)
}

impl OperationalBackupManifest {
    pub fn wal_retention_floor_lsn(&self) -> u64 {
        self.wal_retention_floor_lsn
    }

    fn to_json_value(&self) -> JsonValue {
        let mut object = Map::new();
        object.insert(
            "version".to_string(),
            JsonValue::Number(self.version as f64),
        );
        object.insert(
            "checkpoint_lsn".to_string(),
            JsonValue::Number(self.checkpoint_lsn as f64),
        );
        object.insert(
            "wal_retention_floor_lsn".to_string(),
            JsonValue::Number(self.wal_retention_floor_lsn as f64),
        );
        object.insert(
            "wal_start_lsn".to_string(),
            JsonValue::Number(self.wal_start_lsn as f64),
        );
        object.insert(
            "current_lsn".to_string(),
            JsonValue::Number(self.current_lsn as f64),
        );
        object.insert(
            "files".to_string(),
            JsonValue::Array(
                self.files
                    .iter()
                    .map(OperationalBackupFile::to_json_value)
                    .collect(),
            ),
        );
        JsonValue::Object(object)
    }

    fn from_json_value(value: &JsonValue) -> Result<Self, BackendError> {
        let files = value
            .get("files")
            .and_then(JsonValue::as_array)
            .ok_or_else(|| BackendError::Internal("backup manifest missing files".to_string()))?
            .iter()
            .map(OperationalBackupFile::from_json_value)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            version: value
                .get("version")
                .and_then(JsonValue::as_u64)
                .unwrap_or(1) as u32,
            checkpoint_lsn: value
                .get("checkpoint_lsn")
                .and_then(JsonValue::as_u64)
                .ok_or_else(|| {
                    BackendError::Internal("backup manifest missing checkpoint_lsn".to_string())
                })?,
            wal_retention_floor_lsn: value
                .get("wal_retention_floor_lsn")
                .and_then(JsonValue::as_u64)
                .or_else(|| value.get("wal_start_lsn").and_then(JsonValue::as_u64))
                .ok_or_else(|| {
                    BackendError::Internal(
                        "backup manifest missing wal_retention_floor_lsn".to_string(),
                    )
                })?,
            wal_start_lsn: value
                .get("wal_start_lsn")
                .and_then(JsonValue::as_u64)
                .ok_or_else(|| {
                    BackendError::Internal("backup manifest missing wal_start_lsn".to_string())
                })?,
            current_lsn: value
                .get("current_lsn")
                .and_then(JsonValue::as_u64)
                .ok_or_else(|| {
                    BackendError::Internal("backup manifest missing current_lsn".to_string())
                })?,
            files,
        })
    }
}

impl OperationalBackupFile {
    fn to_json_value(&self) -> JsonValue {
        let mut object = Map::new();
        object.insert("role".to_string(), JsonValue::String(self.role.clone()));
        object.insert(
            "relative_path".to_string(),
            JsonValue::String(self.relative_path.clone()),
        );
        object.insert(
            "size_bytes".to_string(),
            JsonValue::Number(self.size_bytes as f64),
        );
        object.insert("sha256".to_string(), JsonValue::String(self.sha256.clone()));
        JsonValue::Object(object)
    }

    fn from_json_value(value: &JsonValue) -> Result<Self, BackendError> {
        Ok(Self {
            role: required_str(value, "role")?.to_string(),
            relative_path: required_str(value, "relative_path")?.to_string(),
            size_bytes: value
                .get("size_bytes")
                .and_then(JsonValue::as_u64)
                .ok_or_else(|| {
                    BackendError::Internal("backup file missing size_bytes".to_string())
                })?,
            sha256: required_str(value, "sha256")?.to_string(),
        })
    }
}

fn required_str<'a>(value: &'a JsonValue, field: &str) -> Result<&'a str, BackendError> {
    value
        .get(field)
        .and_then(JsonValue::as_str)
        .ok_or_else(|| BackendError::Internal(format!("backup manifest missing {field}")))
}
