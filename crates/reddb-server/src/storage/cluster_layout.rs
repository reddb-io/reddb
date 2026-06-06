//! Cluster range-directory layout tracer.
//!
//! This is deliberately a small physical-layout tracer: it records the mapping
//! from logical collection/range identity to an on-disk range directory and
//! creates the per-range data/index/append-only segment files. The existing
//! pager remains the storage engine for this slice.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterRangeLayout {
    ranges_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeMetadata {
    pub collection: String,
    pub collection_id: u64,
    pub logical_range_id: String,
    pub physical_range_dir_id: String,
    pub physical_dir: PathBuf,
    pub data_file: PathBuf,
    pub index_file: PathBuf,
    pub append_segment_file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeSnapshot {
    pub metadata: RangeMetadata,
    pub checkpoint_dir: PathBuf,
    pub snapshot_lsn: u64,
    pub owner_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeQuarantine {
    pub range_id: String,
    pub original_dir: PathBuf,
    pub quarantine_dir: PathBuf,
    pub reason: String,
}

impl ClusterRangeLayout {
    pub fn new(support_dir: impl Into<PathBuf>) -> Self {
        Self {
            ranges_dir: support_dir.into().join("ranges"),
        }
    }

    pub fn ranges_dir(&self) -> &Path {
        &self.ranges_dir
    }

    pub fn metadata_for(
        &self,
        collection: &str,
        collection_id: u64,
        physical_file_id: &str,
    ) -> RangeMetadata {
        let logical_range_id = format!("range-{collection_id:016x}");
        let physical_range_dir_id = format!("{physical_file_id}-range");
        let physical_dir = self.ranges_dir.join(&physical_range_dir_id);
        RangeMetadata {
            collection: collection.to_string(),
            collection_id,
            logical_range_id,
            physical_range_dir_id,
            data_file: physical_dir.join("data.rdb"),
            index_file: physical_dir.join("index.rdb"),
            append_segment_file: physical_dir.join("segments.aof"),
            physical_dir,
        }
    }

    pub fn prepare_range(&self, metadata: &RangeMetadata) -> io::Result<()> {
        fs::create_dir_all(&metadata.physical_dir)?;
        touch(&metadata.data_file)?;
        touch(&metadata.index_file)?;
        touch(&metadata.append_segment_file)?;
        write_metadata_file(metadata)
    }

    pub fn export_range_snapshot(
        &self,
        metadata: &RangeMetadata,
        checkpoint_root: impl AsRef<Path>,
        snapshot_lsn: u64,
        owner_epoch: u64,
    ) -> io::Result<RangeSnapshot> {
        let checkpoint_dir = checkpoint_root
            .as_ref()
            .join(&metadata.physical_range_dir_id);
        replace_dir(&metadata.physical_dir, &checkpoint_dir)?;
        let snapshot_metadata = metadata.rebased_to(&checkpoint_dir);
        write_metadata_file(&snapshot_metadata)?;
        Ok(RangeSnapshot {
            metadata: snapshot_metadata,
            checkpoint_dir,
            snapshot_lsn,
            owner_epoch,
        })
    }

    pub fn install_range_snapshot(&self, snapshot: &RangeSnapshot) -> io::Result<RangeMetadata> {
        let target_dir = self
            .ranges_dir
            .join(&snapshot.metadata.physical_range_dir_id);
        replace_dir(&snapshot.checkpoint_dir, &target_dir)?;
        let installed = snapshot.metadata.rebased_to(&target_dir);
        write_metadata_file(&installed)?;
        Ok(installed)
    }

    pub fn quarantine_range(
        &self,
        metadata: &RangeMetadata,
        reason: impl AsRef<str>,
    ) -> io::Result<RangeQuarantine> {
        let reason = sanitize_quarantine_reason(reason.as_ref());
        let quarantine_root = self.ranges_dir.join("quarantine");
        fs::create_dir_all(&quarantine_root)?;
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let quarantine_dir = quarantine_root.join(format!(
            "{}.{}.{}",
            metadata.physical_range_dir_id, reason, suffix
        ));
        fs::rename(&metadata.physical_dir, &quarantine_dir)?;
        Ok(RangeQuarantine {
            range_id: metadata.logical_range_id.clone(),
            original_dir: metadata.physical_dir.clone(),
            quarantine_dir,
            reason,
        })
    }

    pub fn load_collection_range(&self, collection: &str) -> io::Result<Option<RangeMetadata>> {
        if !self.ranges_dir.exists() {
            return Ok(None);
        }
        for entry in fs::read_dir(&self.ranges_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let path = entry.path().join("range.meta");
            if !path.exists() {
                continue;
            }
            let raw = fs::read_to_string(&path)?;
            let Some(metadata) = parse_metadata(&raw) else {
                continue;
            };
            if metadata.collection == collection {
                return Ok(Some(metadata));
            }
        }
        Ok(None)
    }
}

impl RangeMetadata {
    fn rebased_to(&self, physical_dir: &Path) -> Self {
        Self {
            collection: self.collection.clone(),
            collection_id: self.collection_id,
            logical_range_id: self.logical_range_id.clone(),
            physical_range_dir_id: self.physical_range_dir_id.clone(),
            physical_dir: physical_dir.to_path_buf(),
            data_file: physical_dir.join("data.rdb"),
            index_file: physical_dir.join("index.rdb"),
            append_segment_file: physical_dir.join("segments.aof"),
        }
    }
}

fn write_metadata_file(metadata: &RangeMetadata) -> io::Result<()> {
    let path = metadata.physical_dir.join("range.meta");
    let body = format!(
        "collection={}\ncollection_id={}\nlogical_range_id={}\nphysical_range_dir_id={}\ndata_file={}\nindex_file={}\nappend_segment_file={}\n",
        metadata.collection,
        metadata.collection_id,
        metadata.logical_range_id,
        metadata.physical_range_dir_id,
        metadata.data_file.display(),
        metadata.index_file.display(),
        metadata.append_segment_file.display(),
    );
    fs::write(path, body)
}

fn replace_dir(source: &Path, target: &Path) -> io::Result<()> {
    if target.exists() {
        fs::remove_dir_all(target)?;
    }
    copy_dir_recursive(source, target)
}

fn copy_dir_recursive(source: &Path, target: &Path) -> io::Result<()> {
    let entries = fs::read_dir(source)?;
    fs::create_dir_all(target)?;
    for entry in entries {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&source_path, &target_path)?;
        } else {
            fs::copy(&source_path, &target_path)?;
        }
    }
    Ok(())
}

fn touch(path: &Path) -> io::Result<()> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map(|_| ())
}

fn sanitize_quarantine_reason(reason: &str) -> String {
    let sanitized: String = reason
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "repair".to_string()
    } else {
        sanitized
    }
}

fn parse_metadata(raw: &str) -> Option<RangeMetadata> {
    let mut collection = None;
    let mut collection_id = None;
    let mut logical_range_id = None;
    let mut physical_range_dir_id = None;
    let mut data_file = None;
    let mut index_file = None;
    let mut append_segment_file = None;
    for line in raw.lines() {
        let (key, value) = line.split_once('=')?;
        match key {
            "collection" => collection = Some(value.to_string()),
            "collection_id" => collection_id = value.parse::<u64>().ok(),
            "logical_range_id" => logical_range_id = Some(value.to_string()),
            "physical_range_dir_id" => physical_range_dir_id = Some(value.to_string()),
            "data_file" => data_file = Some(PathBuf::from(value)),
            "index_file" => index_file = Some(PathBuf::from(value)),
            "append_segment_file" => append_segment_file = Some(PathBuf::from(value)),
            _ => {}
        }
    }
    let data_file = data_file?;
    let physical_dir = data_file.parent()?.to_path_buf();
    Some(RangeMetadata {
        collection: collection?,
        collection_id: collection_id?,
        logical_range_id: logical_range_id?,
        physical_range_dir_id: physical_range_dir_id?,
        physical_dir,
        data_file,
        index_file: index_file?,
        append_segment_file: append_segment_file?,
    })
}
