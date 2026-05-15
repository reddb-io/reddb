//! Pure tiered storage layout derivation.
//!
//! This module maps a configured database path and layout preset to
//! deterministic sidecar paths. Constructors and accessors perform no I/O;
//! callers opt into directory creation through [`TieredLayoutPaths::ensure_dirs`].

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Storage layout preset for future tier-aware startup integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StorageLayout {
    /// Keep only required durability sidecars next to the data file.
    Minimal,
    /// Default balance: shared support directory for durable metadata.
    Standard,
    /// Put hot write/read artifacts into dedicated directories.
    Performance,
    /// Enable every known dedicated tier directory.
    Max,
}

impl Default for StorageLayout {
    fn default() -> Self {
        Self::Standard
    }
}

/// Optional per-toggle override applied after preset expansion.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutOverrides {
    pub dedicated_wal_dir: Option<bool>,
    pub dedicated_index_dir: Option<bool>,
    pub dedicated_cache_dir: Option<bool>,
    pub dedicated_snapshot_dir: Option<bool>,
    pub dedicated_blob_dir: Option<bool>,
    pub dedicated_temp_dir: Option<bool>,
    pub dedicated_metrics_dir: Option<bool>,
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
    pub fn expand(self, overrides: LayoutOverrides) -> LayoutToggles {
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
    pub toggles: LayoutToggles,
}

impl TieredLayoutPaths {
    pub fn new(
        data_path: &Path,
        layout: StorageLayout,
        overrides: LayoutOverrides,
    ) -> TieredLayoutPaths {
        let toggles = layout.expand(overrides);
        let data_file = data_path.to_path_buf();
        let support_dir = sibling_path(data_path, &format!("{}.red", file_name(data_path)));

        let wal_file = if toggles.dedicated_wal_dir {
            support_dir
                .join("wal")
                .join(sidecar_file_name(data_path, "rdb-uwal"))
        } else {
            data_path.with_extension("rdb-uwal")
        };
        let logical_wal_file = if toggles.dedicated_wal_dir {
            support_dir
                .join("wal")
                .join(format!("{}.logical.wal", file_name(data_path)))
        } else {
            sibling_path(data_path, &format!("{}.logical.wal", file_name(data_path)))
        };
        let temp_file = if toggles.dedicated_temp_dir {
            support_dir
                .join("tmp")
                .join(sidecar_file_name(data_path, "rdb-tmp"))
        } else {
            data_path.with_extension("rdb-tmp")
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
        dirs.sort();
        dirs.dedup();
        dirs
    }

    pub fn ensure_dirs(&self) -> io::Result<()> {
        for dir in self.dirs_to_create() {
            fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("data.rdb")
        .to_string()
}

fn sibling_path(path: &Path, file_name: &str) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(file_name),
        _ => PathBuf::from(file_name),
    }
}

fn sidecar_file_name(path: &Path, extension: &str) -> String {
    path.with_extension(extension)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("data.rdb")
        .to_string()
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
