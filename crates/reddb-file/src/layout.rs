//! Canonical file and sidecar path derivation.
//!
//! These helpers are pure: they do not touch the filesystem. Runtime crates
//! use them so filename contracts live in `reddb-file` instead of being
//! reassembled at call sites.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const SUPPORT_DIR_SUFFIX: &str = "red";
pub const UNIFIED_WAL_EXTENSION: &str = "rdb-uwal";
pub const LOGICAL_WAL_SUFFIX: &str = "logical.wal";
pub const TEMP_EXTENSION: &str = "rdb-tmp";
pub const ATOMIC_TEMP_EXTENSION: &str = "tmp";
pub const PRIMARY_WAL_EXTENSION: &str = "redwal";
pub const PAGER_LEGACY_WAL_EXTENSION: &str = "wal";
pub const ENGINE_WAL_EXTENSION: &str = "rdb-wal";
pub const PAGER_HEADER_EXTENSION: &str = "rdb-hdr";
pub const PAGER_META_EXTENSION: &str = "rdb-meta";
pub const PAGER_DWB_EXTENSION: &str = "rdb-dwb";
pub const SHM_FILE_SUFFIX: &str = "shm";
pub const PHYSICAL_METADATA_JSON_SUFFIX: &str = "meta.json";
pub const PHYSICAL_METADATA_BINARY_EXTENSION: &str = "meta.rdbx";
pub const REBOOTSTRAP_STAGING_EXTENSION: &str = "rebootstrap.redbase";
pub const REBOOTSTRAP_PENDING_EXTENSION: &str = "rebootstrap.pending.rdb";
pub const REBOOTSTRAP_READY_EXTENSION: &str = "rebootstrap.ready";
pub const REBOOTSTRAP_INTENT_LOG_EXTENSION: &str = "rebootstrap.intent.jsonl";
pub const REBOOTSTRAP_PREVIOUS_EXTENSION: &str = "rebootstrap.previous.rdb";
pub const PRIMARY_REPLICA_ROOT_EXTENSION: &str = "primary-replica";
pub const LEGACY_LOGICAL_SLOTS_SUFFIX: &str = "logical.slots.json";
pub const LEGACY_LOGICAL_SLOTS_TEMP_EXTENSION: &str = "logical.slots.tmp";
pub const SERVERLESS_ROOT_EXTENSION: &str = "serverless";
pub const SERVERLESS_CACHE_DIR: &str = "cache";
pub const RESULT_CACHE_L2_EXTENSION: &str = "result-cache.l2";

/// Storage layout preset for tier-aware RedDB file placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum StorageLayout {
    /// Keep only required durability sidecars next to the data file.
    Minimal,
    /// Default balance: shared support directory for durable metadata.
    #[default]
    Standard,
    /// Put hot write/read artifacts into dedicated directories.
    Performance,
    /// Enable every known dedicated tier directory.
    Max,
}

/// Optional per-toggle override applied after preset expansion.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutOverrides {
    pub dedicated_wal_dir: Option<bool>,
    pub dedicated_index_dir: Option<bool>,
    pub dedicated_cache_dir: Option<bool>,
    pub dedicated_snapshot_dir: Option<bool>,
    pub dedicated_blob_dir: Option<bool>,
    pub dedicated_temp_dir: Option<bool>,
    pub dedicated_metrics_dir: Option<bool>,
    /// Per-log routing overrides. See [`LogRoutingOverrides`].
    #[serde(default)]
    pub logs: LogRoutingOverrides,
}

/// Where a log stream should be written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "path")]
pub enum LogDestination {
    Stderr,
    File(PathBuf),
    Syslog,
}

impl LogDestination {
    /// Human-readable destination tag for status and diagnostics.
    pub fn describe(&self) -> String {
        match self {
            Self::Stderr => "stderr".to_string(),
            Self::Syslog => "syslog".to_string(),
            Self::File(path) => format!("file:{}", path.display()),
        }
    }

    /// Returns the file path if this destination writes to a file.
    pub fn file_path(&self) -> Option<&Path> {
        match self {
            Self::File(path) => Some(path.as_path()),
            _ => None,
        }
    }
}

/// Per-log destination overrides. `None` keeps the tier default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogRoutingOverrides {
    pub audit_log: Option<LogDestination>,
    pub slow_log: Option<LogDestination>,
}

/// Fully expanded layout toggles after applying a preset and overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutToggles {
    pub dedicated_wal_dir: bool,
    pub dedicated_index_dir: bool,
    pub dedicated_cache_dir: bool,
    pub dedicated_snapshot_dir: bool,
    pub dedicated_blob_dir: bool,
    pub dedicated_temp_dir: bool,
    pub dedicated_metrics_dir: bool,
}

impl StorageLayout {
    /// Default audit-log destination for this tier, before any override.
    pub fn default_audit_log_in(self, support_dir: &Path) -> LogDestination {
        match self {
            Self::Performance | Self::Max => {
                LogDestination::File(support_dir.join("logs").join("audit.log"))
            }
            Self::Minimal | Self::Standard => LogDestination::Stderr,
        }
    }

    /// Default slow-query log destination for this tier, before any override.
    pub fn default_slow_log_in(self, support_dir: &Path) -> LogDestination {
        match self {
            Self::Performance | Self::Max => {
                LogDestination::File(support_dir.join("logs").join("slow.log"))
            }
            Self::Minimal | Self::Standard => LogDestination::Stderr,
        }
    }

    pub fn expand(self, overrides: &LayoutOverrides) -> LayoutToggles {
        let mut toggles = match self {
            Self::Minimal => LayoutToggles {
                dedicated_wal_dir: false,
                dedicated_index_dir: false,
                dedicated_cache_dir: false,
                dedicated_snapshot_dir: false,
                dedicated_blob_dir: false,
                dedicated_temp_dir: false,
                dedicated_metrics_dir: false,
            },
            Self::Standard => LayoutToggles {
                dedicated_wal_dir: false,
                dedicated_index_dir: true,
                dedicated_cache_dir: false,
                dedicated_snapshot_dir: true,
                dedicated_blob_dir: false,
                dedicated_temp_dir: false,
                dedicated_metrics_dir: false,
            },
            Self::Performance => LayoutToggles {
                dedicated_wal_dir: true,
                dedicated_index_dir: true,
                dedicated_cache_dir: true,
                dedicated_snapshot_dir: true,
                dedicated_blob_dir: true,
                dedicated_temp_dir: false,
                dedicated_metrics_dir: false,
            },
            Self::Max => LayoutToggles {
                dedicated_wal_dir: true,
                dedicated_index_dir: true,
                dedicated_cache_dir: true,
                dedicated_snapshot_dir: true,
                dedicated_blob_dir: true,
                dedicated_temp_dir: true,
                dedicated_metrics_dir: true,
            },
        };

        if let Some(value) = overrides.dedicated_wal_dir {
            toggles.dedicated_wal_dir = value;
        }
        if let Some(value) = overrides.dedicated_index_dir {
            toggles.dedicated_index_dir = value;
        }
        if let Some(value) = overrides.dedicated_cache_dir {
            toggles.dedicated_cache_dir = value;
        }
        if let Some(value) = overrides.dedicated_snapshot_dir {
            toggles.dedicated_snapshot_dir = value;
        }
        if let Some(value) = overrides.dedicated_blob_dir {
            toggles.dedicated_blob_dir = value;
        }
        if let Some(value) = overrides.dedicated_temp_dir {
            toggles.dedicated_temp_dir = value;
        }
        if let Some(value) = overrides.dedicated_metrics_dir {
            toggles.dedicated_metrics_dir = value;
        }

        toggles
    }
}

/// Deterministic paths derived from a data file and expanded layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TieredLayoutPaths {
    pub data_file: PathBuf,
    pub support_dir: PathBuf,
    pub wal_file: PathBuf,
    pub logical_wal_file: PathBuf,
    pub temp_file: PathBuf,
    pub snapshot_dir: Option<PathBuf>,
    pub index_dir: Option<PathBuf>,
    pub cache_dir: Option<PathBuf>,
    pub blob_dir: Option<PathBuf>,
    pub metrics_dir: Option<PathBuf>,
    pub logs_dir: Option<PathBuf>,
    pub audit_log_destination: LogDestination,
    pub slow_log_destination: LogDestination,
    pub toggles: LayoutToggles,
}

impl TieredLayoutPaths {
    pub fn new(
        data_path: &Path,
        layout: StorageLayout,
        overrides: LayoutOverrides,
    ) -> TieredLayoutPaths {
        let toggles = layout.expand(&overrides);
        let data_file = data_path.to_path_buf();
        let support_dir = support_dir_for(data_path);

        let wal_file = if toggles.dedicated_wal_dir {
            unified_wal_path_in(&support_dir, data_path)
        } else {
            unified_wal_path(data_path)
        };
        let logical_wal_file = if toggles.dedicated_wal_dir {
            logical_wal_path_in(&support_dir, data_path)
        } else {
            logical_wal_path(data_path)
        };
        let temp_file = if toggles.dedicated_temp_dir {
            temp_path_in(&support_dir, data_path)
        } else {
            temp_path(data_path)
        };

        let audit_log_destination = overrides
            .logs
            .audit_log
            .clone()
            .unwrap_or_else(|| layout.default_audit_log_in(&support_dir));
        let slow_log_destination = overrides
            .logs
            .slow_log
            .clone()
            .unwrap_or_else(|| layout.default_slow_log_in(&support_dir));
        let logs_dir = match (
            audit_log_destination.file_path(),
            slow_log_destination.file_path(),
        ) {
            (None, None) => None,
            _ => Some(support_dir.join("logs")),
        };

        TieredLayoutPaths {
            data_file,
            support_dir: support_dir.clone(),
            wal_file,
            logical_wal_file,
            temp_file,
            snapshot_dir: toggles
                .dedicated_snapshot_dir
                .then(|| support_dir.join("snapshots")),
            index_dir: toggles
                .dedicated_index_dir
                .then(|| support_dir.join("indexes")),
            cache_dir: toggles
                .dedicated_cache_dir
                .then(|| support_dir.join("cache")),
            blob_dir: toggles
                .dedicated_blob_dir
                .then(|| support_dir.join("blobs")),
            metrics_dir: toggles
                .dedicated_metrics_dir
                .then(|| support_dir.join("metrics")),
            logs_dir,
            audit_log_destination,
            slow_log_destination,
            toggles,
        }
    }

    pub fn dirs_to_create(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        push_parent(&mut dirs, &self.data_file);
        push_parent(&mut dirs, &self.wal_file);
        push_parent(&mut dirs, &self.logical_wal_file);
        push_parent(&mut dirs, &self.temp_file);
        push_optional(&mut dirs, self.snapshot_dir.as_ref());
        push_optional(&mut dirs, self.index_dir.as_ref());
        push_optional(&mut dirs, self.cache_dir.as_ref());
        push_optional(&mut dirs, self.blob_dir.as_ref());
        push_optional(&mut dirs, self.metrics_dir.as_ref());
        push_optional(&mut dirs, self.logs_dir.as_ref());
        if let Some(path) = self.audit_log_destination.file_path() {
            push_parent(&mut dirs, path);
        }
        if let Some(path) = self.slow_log_destination.file_path() {
            push_parent(&mut dirs, path);
        }
        dirs.sort();
        dirs.dedup();
        dirs
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        for dir in self.dirs_to_create() {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    /// Path for a `vector.turbo` collection's `.tv` snapshot.
    pub fn turbo_snapshot_path(&self, collection: &str) -> Option<PathBuf> {
        if !self.toggles.dedicated_snapshot_dir {
            return None;
        }
        if let Some(dir) = &self.snapshot_dir {
            return Some(dir.join(format!("{collection}.tv")));
        }
        let stem = data_file_name(&self.data_file);
        Some(sibling_path(
            &self.data_file,
            &format!("{stem}.{collection}.tv"),
        ))
    }
}

pub fn data_file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("data.rdb")
        .to_string()
}

pub fn sibling_path(path: &Path, file_name: &str) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(file_name),
        _ => PathBuf::from(file_name),
    }
}

pub fn sidecar_file_name(path: &Path, extension: &str) -> String {
    path.with_extension(extension)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("data.rdb")
        .to_string()
}

pub fn support_dir_for(data_path: &Path) -> PathBuf {
    let file_name = data_file_name(data_path);
    sibling_path(data_path, &format!("{file_name}.{SUPPORT_DIR_SUFFIX}"))
}

pub fn unified_wal_path(data_path: &Path) -> PathBuf {
    data_path.with_extension(UNIFIED_WAL_EXTENSION)
}

pub fn unified_wal_path_in(support_dir: &Path, data_path: &Path) -> PathBuf {
    support_dir
        .join("wal")
        .join(sidecar_file_name(data_path, UNIFIED_WAL_EXTENSION))
}

pub fn store_commit_coord_temp_wal_path(
    temp_dir: &Path,
    name: &str,
    process_id: u32,
    nanos: u128,
) -> PathBuf {
    temp_dir.join(store_commit_coord_temp_wal_file_name(
        name, process_id, nanos,
    ))
}

pub fn store_commit_coord_temp_wal_file_name(name: &str, process_id: u32, nanos: u128) -> String {
    format!("rb_commit_coord_{name}_{process_id}_{nanos}.wal")
}

pub fn wal_component_temp_path(
    temp_dir: &Path,
    component: &str,
    name: &str,
    process_id: u32,
) -> PathBuf {
    temp_dir.join(wal_component_temp_file_name(component, name, process_id))
}

pub fn wal_component_temp_file_name(component: &str, name: &str, process_id: u32) -> String {
    format!("rb_wal_{component}_{name}_{process_id}.wal")
}

pub fn backup_temp_json_path(
    temp_dir: &Path,
    prefix: &str,
    process_id: u32,
    nanos: u128,
    start_lsn: Option<u64>,
    end_lsn: Option<u64>,
) -> PathBuf {
    temp_dir.join(backup_temp_json_file_name(
        prefix, process_id, nanos, start_lsn, end_lsn,
    ))
}

pub fn backup_temp_json_file_name(
    prefix: &str,
    process_id: u32,
    nanos: u128,
    start_lsn: Option<u64>,
    end_lsn: Option<u64>,
) -> String {
    match (start_lsn, end_lsn) {
        (Some(start_lsn), Some(end_lsn)) => {
            format!("{prefix}-{process_id}-{start_lsn}-{end_lsn}-{nanos}.json")
        }
        _ => format!("{prefix}-{process_id}-{nanos}.json"),
    }
}

pub fn logical_wal_path(data_path: &Path) -> PathBuf {
    sibling_path(
        data_path,
        &format!("{}.{}", data_file_name(data_path), LOGICAL_WAL_SUFFIX),
    )
}

pub fn logical_wal_temp_path(logical_wal_path: &Path) -> PathBuf {
    logical_wal_path.with_extension("logical.wal.tmp")
}

pub fn logical_wal_path_in(support_dir: &Path, data_path: &Path) -> PathBuf {
    support_dir.join("wal").join(format!(
        "{}.{}",
        data_file_name(data_path),
        LOGICAL_WAL_SUFFIX
    ))
}

pub fn temp_path(data_path: &Path) -> PathBuf {
    data_path.with_extension(TEMP_EXTENSION)
}

pub fn atomic_temp_path(path: &Path) -> PathBuf {
    path.with_extension(ATOMIC_TEMP_EXTENSION)
}

pub fn result_cache_l2_path(data_path: &Path) -> PathBuf {
    data_path.with_extension(RESULT_CACHE_L2_EXTENSION)
}

pub fn temp_path_in(support_dir: &Path, data_path: &Path) -> PathBuf {
    support_dir
        .join("tmp")
        .join(sidecar_file_name(data_path, TEMP_EXTENSION))
}

pub fn primary_wal_segment_file_name(segment_index: u64) -> String {
    format!("{segment_index:020}.{PRIMARY_WAL_EXTENSION}")
}

pub fn relay_segment_relative_path(start_lsn: u64, end_lsn: u64) -> PathBuf {
    PathBuf::from(format!(
        "relay-{start_lsn:020}-{end_lsn:020}.{PRIMARY_WAL_EXTENSION}"
    ))
}

pub fn pager_legacy_wal_path(data_path: &Path) -> PathBuf {
    data_path.with_extension(PAGER_LEGACY_WAL_EXTENSION)
}

pub fn engine_wal_path(data_path: &Path) -> PathBuf {
    data_path.with_extension(ENGINE_WAL_EXTENSION)
}

pub fn pager_header_path(data_path: &Path) -> PathBuf {
    data_path.with_extension(PAGER_HEADER_EXTENSION)
}

pub fn pager_meta_path(data_path: &Path) -> PathBuf {
    data_path.with_extension(PAGER_META_EXTENSION)
}

pub fn pager_dwb_path(data_path: &Path) -> PathBuf {
    data_path.with_extension(PAGER_DWB_EXTENSION)
}

pub fn shm_path(data_path: &Path) -> PathBuf {
    sibling_path(
        data_path,
        &format!("{}-{SHM_FILE_SUFFIX}", data_file_name(data_path)),
    )
}

pub fn physical_metadata_json_path(data_path: &Path) -> PathBuf {
    sibling_path(
        data_path,
        &format!(
            "{}.{PHYSICAL_METADATA_JSON_SUFFIX}",
            data_file_name(data_path)
        ),
    )
}

pub fn physical_metadata_binary_path(data_path: &Path) -> PathBuf {
    sibling_path(
        data_path,
        &format!(
            "{}.{PHYSICAL_METADATA_BINARY_EXTENSION}",
            data_file_name(data_path)
        ),
    )
}

pub fn physical_metadata_journal_path(data_path: &Path, sequence: u64) -> PathBuf {
    sibling_path(
        data_path,
        &format!(
            "{}.{PHYSICAL_METADATA_BINARY_EXTENSION}.seq-{sequence:020}",
            data_file_name(data_path)
        ),
    )
}

pub fn physical_metadata_journal_prefix(data_path: &Path) -> String {
    format!(
        "{}.{PHYSICAL_METADATA_BINARY_EXTENSION}.seq-",
        data_file_name(data_path)
    )
}

pub fn physical_export_data_path(data_path: &Path, name: &str) -> PathBuf {
    let file_name = data_file_name(data_path);
    let stem = file_name.strip_suffix(".rdb").unwrap_or(&file_name);
    sibling_path(
        data_path,
        &format!("{stem}.export.{}.rdb", sanitize_export_name(name)),
    )
}

pub fn rebootstrap_staging_root(data_path: &Path) -> PathBuf {
    data_path.with_extension(REBOOTSTRAP_STAGING_EXTENSION)
}

pub fn rebootstrap_pending_path(data_path: &Path) -> PathBuf {
    data_path.with_extension(REBOOTSTRAP_PENDING_EXTENSION)
}

pub fn rebootstrap_ready_marker_path(data_path: &Path) -> PathBuf {
    data_path.with_extension(REBOOTSTRAP_READY_EXTENSION)
}

pub fn rebootstrap_intent_log_path(data_path: &Path) -> PathBuf {
    data_path.with_extension(REBOOTSTRAP_INTENT_LOG_EXTENSION)
}

pub fn rebootstrap_previous_path(data_path: &Path) -> PathBuf {
    data_path.with_extension(REBOOTSTRAP_PREVIOUS_EXTENSION)
}

pub fn primary_replica_root(data_path: &Path) -> PathBuf {
    data_path.with_extension(PRIMARY_REPLICA_ROOT_EXTENSION)
}

pub fn legacy_logical_slots_path(data_path: &Path) -> PathBuf {
    let file_name = data_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("reddb.rdb");
    sibling_path(
        data_path,
        &format!("{file_name}.{LEGACY_LOGICAL_SLOTS_SUFFIX}"),
    )
}

pub fn legacy_logical_slots_temp_path(path: &Path) -> PathBuf {
    path.with_extension(LEGACY_LOGICAL_SLOTS_TEMP_EXTENSION)
}

pub fn serverless_root(data_path: &Path) -> PathBuf {
    data_path.with_extension(SERVERLESS_ROOT_EXTENSION)
}

pub fn serverless_namespace(data_path: &Path) -> String {
    data_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("default")
        .to_string()
}

pub fn serverless_cache_root(root: &Path, namespace: &str) -> PathBuf {
    root.join(namespace).join(SERVERLESS_CACHE_DIR)
}

fn push_parent(dirs: &mut Vec<PathBuf>, path: &Path) {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            dirs.push(parent.to_path_buf());
        }
    }
}

fn push_optional(dirs: &mut Vec<PathBuf>, path: Option<&PathBuf>) {
    if let Some(path) = path {
        dirs.push(path.clone());
    }
}

fn sanitize_export_name(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "export".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_standard_sidecars_next_to_database() {
        let path = Path::new("/var/lib/reddb/main.rdb");

        assert_eq!(
            support_dir_for(path),
            PathBuf::from("/var/lib/reddb/main.rdb.red")
        );
        assert_eq!(
            unified_wal_path(path),
            PathBuf::from("/var/lib/reddb/main.rdb-uwal")
        );
        assert_eq!(
            store_commit_coord_temp_wal_path(Path::new("/tmp"), "burst", 7, 99),
            PathBuf::from("/tmp/rb_commit_coord_burst_7_99.wal")
        );
        assert_eq!(
            wal_component_temp_path(Path::new("/tmp"), "writer", "create", 7),
            PathBuf::from("/tmp/rb_wal_writer_create_7.wal")
        );
        assert_eq!(
            wal_component_temp_path(Path::new("/tmp"), "reader", "empty", 7),
            PathBuf::from("/tmp/rb_wal_reader_empty_7.wal")
        );
        assert_eq!(
            backup_temp_json_path(
                Path::new("/tmp"),
                "reddb-archived-change-records",
                7,
                99,
                Some(10),
                Some(20)
            ),
            PathBuf::from("/tmp/reddb-archived-change-records-7-10-20-99.json")
        );
        assert_eq!(
            backup_temp_json_path(Path::new("/tmp"), "reddb-json-object", 7, 99, None, None),
            PathBuf::from("/tmp/reddb-json-object-7-99.json")
        );
        assert_eq!(
            logical_wal_path(path),
            PathBuf::from("/var/lib/reddb/main.rdb.logical.wal")
        );
        assert_eq!(
            logical_wal_temp_path(&logical_wal_path(path)),
            PathBuf::from("/var/lib/reddb/main.rdb.logical.logical.wal.tmp")
        );
        assert_eq!(
            temp_path(path),
            PathBuf::from("/var/lib/reddb/main.rdb-tmp")
        );
        assert_eq!(
            atomic_temp_path(&pager_meta_path(path)),
            PathBuf::from("/var/lib/reddb/main.tmp")
        );
        assert_eq!(
            result_cache_l2_path(path),
            PathBuf::from("/var/lib/reddb/main.result-cache.l2")
        );
        assert_eq!(
            engine_wal_path(path),
            PathBuf::from("/var/lib/reddb/main.rdb-wal")
        );
        assert_eq!(
            pager_legacy_wal_path(path),
            PathBuf::from("/var/lib/reddb/main.wal")
        );
        assert_eq!(shm_path(path), PathBuf::from("/var/lib/reddb/main.rdb-shm"));
        assert_eq!(
            physical_metadata_json_path(path),
            PathBuf::from("/var/lib/reddb/main.rdb.meta.json")
        );
        assert_eq!(
            physical_metadata_binary_path(path),
            PathBuf::from("/var/lib/reddb/main.rdb.meta.rdbx")
        );
        assert_eq!(
            physical_metadata_journal_path(path, 7),
            PathBuf::from("/var/lib/reddb/main.rdb.meta.rdbx.seq-00000000000000000007")
        );
        assert_eq!(
            physical_metadata_journal_prefix(path),
            "main.rdb.meta.rdbx.seq-"
        );
        assert_eq!(
            physical_export_data_path(path, "nightly backup"),
            PathBuf::from("/var/lib/reddb/main.export.nightly_backup.rdb")
        );
        assert_eq!(
            rebootstrap_staging_root(path),
            PathBuf::from("/var/lib/reddb/main.rebootstrap.redbase")
        );
        assert_eq!(
            rebootstrap_pending_path(path),
            PathBuf::from("/var/lib/reddb/main.rebootstrap.pending.rdb")
        );
        assert_eq!(
            rebootstrap_ready_marker_path(path),
            PathBuf::from("/var/lib/reddb/main.rebootstrap.ready")
        );
        assert_eq!(
            rebootstrap_intent_log_path(path),
            PathBuf::from("/var/lib/reddb/main.rebootstrap.intent.jsonl")
        );
        assert_eq!(
            rebootstrap_previous_path(path),
            PathBuf::from("/var/lib/reddb/main.rebootstrap.previous.rdb")
        );
        assert_eq!(
            primary_replica_root(path),
            PathBuf::from("/var/lib/reddb/main.primary-replica")
        );
        assert_eq!(
            legacy_logical_slots_path(path),
            PathBuf::from("/var/lib/reddb/main.rdb.logical.slots.json")
        );
        assert_eq!(
            legacy_logical_slots_temp_path(&legacy_logical_slots_path(path)),
            PathBuf::from("/var/lib/reddb/main.rdb.logical.slots.logical.slots.tmp")
        );
        assert_eq!(
            serverless_root(path),
            PathBuf::from("/var/lib/reddb/main.serverless")
        );
        assert_eq!(serverless_namespace(path), "main");
        assert_eq!(
            serverless_cache_root(&serverless_root(path), &serverless_namespace(path)),
            PathBuf::from("/var/lib/reddb/main.serverless/main/cache")
        );
    }

    #[test]
    fn derives_dedicated_support_sidecars() {
        let path = Path::new("/var/lib/reddb/main.rdb");
        let support = support_dir_for(path);

        assert_eq!(
            unified_wal_path_in(&support, path),
            PathBuf::from("/var/lib/reddb/main.rdb.red/wal/main.rdb-uwal")
        );
        assert_eq!(
            logical_wal_path_in(&support, path),
            PathBuf::from("/var/lib/reddb/main.rdb.red/wal/main.rdb.logical.wal")
        );
        assert_eq!(
            temp_path_in(&support, path),
            PathBuf::from("/var/lib/reddb/main.rdb.red/tmp/main.rdb-tmp")
        );
    }

    #[test]
    fn derives_primary_replica_segment_names() {
        assert_eq!(
            primary_wal_segment_file_name(2),
            "00000000000000000002.redwal"
        );
        assert_eq!(
            relay_segment_relative_path(10, 20),
            PathBuf::from("relay-00000000000000000010-00000000000000000020.redwal")
        );
    }
}
