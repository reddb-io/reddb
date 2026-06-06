//! Cluster range-directory layout tracer.
//!
//! This is deliberately a small physical-layout tracer: it records the mapping
//! from logical collection/range identity to an on-disk range directory and
//! creates the per-range data/index/append-only segment files. The existing
//! pager remains the storage engine for this slice.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

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
        self.write_metadata(metadata)
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

    fn write_metadata(&self, metadata: &RangeMetadata) -> io::Result<()> {
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
}

fn touch(path: &Path) -> io::Result<()> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map(|_| ())
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
