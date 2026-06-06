//! Read-only storage integrity validation.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::storage::embedded::{
    EmbeddedRdbArtifact, EMBEDDED_RDB_SUPERBLOCK_0_OFFSET, EMBEDDED_RDB_SUPERBLOCK_1_OFFSET,
};
use crate::storage::engine::{Page, PAGE_SIZE};
use crate::storage::operational_manifest::OperationalManifest;
use crate::storage::unified::{AppendOnlySegment, AppendOnlySegmentMetadata};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageValidationReport {
    pub path: PathBuf,
    pub embedded_superblock: ValidationCheck,
    pub embedded_manifest: ValidationCheck,
    pub operational_manifest: ValidationCheck,
    pub mutable_pages: ValidationCheck,
    pub append_only_segments: ValidationCheck,
}

impl StorageValidationReport {
    pub fn is_clean(&self) -> bool {
        self.embedded_superblock.is_clean()
            && self.embedded_manifest.is_clean()
            && self.operational_manifest.is_clean()
            && self.mutable_pages.is_clean()
            && self.append_only_segments.is_clean()
    }

    pub fn failures(&self) -> Vec<&str> {
        [
            &self.embedded_superblock,
            &self.embedded_manifest,
            &self.operational_manifest,
            &self.mutable_pages,
            &self.append_only_segments,
        ]
        .into_iter()
        .filter_map(|check| match check {
            ValidationCheck::Failed(message) => Some(message.as_str()),
            _ => None,
        })
        .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationCheck {
    Clean(String),
    NotPresent(String),
    Failed(String),
}

impl ValidationCheck {
    pub fn is_clean(&self) -> bool {
        matches!(self, Self::Clean(_) | Self::NotPresent(_))
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Clean(message) | Self::NotPresent(message) | Self::Failed(message) => message,
        }
    }
}

pub fn validate_storage(path: impl AsRef<Path>) -> StorageValidationReport {
    let path = path.as_ref();
    let embedded = validate_embedded(path);
    let mutable_pages = if matches!(embedded.0, ValidationCheck::Clean(_)) {
        ValidationCheck::NotPresent(
            "mutable page checksums are not present in embedded artifact layout".to_string(),
        )
    } else {
        validate_mutable_pages(path)
    };
    StorageValidationReport {
        path: path.to_path_buf(),
        embedded_superblock: embedded.0,
        embedded_manifest: embedded.1,
        operational_manifest: validate_operational_manifest(path),
        mutable_pages,
        append_only_segments: validate_append_only_segment_files(path),
    }
}

pub fn validate_append_only_segment(
    segment: &AppendOnlySegment,
    metadata: &AppendOnlySegmentMetadata,
) -> ValidationCheck {
    match segment.validate_checksums(metadata) {
        Ok(()) => ValidationCheck::Clean(format!(
            "append-only segment {} for {} validated {} chunk checksums",
            metadata.segment_id,
            metadata.collection,
            metadata.chunk_checksums.len()
        )),
        Err(err) => ValidationCheck::Failed(format!("append-only segment checksum failed: {err}")),
    }
}

fn validate_embedded(path: &Path) -> (ValidationCheck, ValidationCheck) {
    if !path.is_file() {
        return (
            ValidationCheck::NotPresent("embedded artifact is not present".to_string()),
            ValidationCheck::NotPresent("embedded manifest is not present".to_string()),
        );
    }
    if !has_embedded_superblock_magic(path) {
        return (
            ValidationCheck::NotPresent("embedded superblock is not present".to_string()),
            ValidationCheck::NotPresent("embedded manifest is not present".to_string()),
        );
    }
    match EmbeddedRdbArtifact::open(path) {
        Ok(open) => (
            ValidationCheck::Clean(format!(
                "embedded superblock copy {} generation {} validated",
                open.selected_superblock.copy_index, open.selected_superblock.generation
            )),
            ValidationCheck::Clean(format!(
                "embedded manifest checksum {:#010x} validated",
                open.manifest.checksum
            )),
        ),
        Err(err) => {
            let message = err.to_string();
            let superblock_failed =
                message.contains("superblock") || message.contains("no valid embedded");
            if superblock_failed {
                (
                    ValidationCheck::Failed(format!("embedded superblock validation failed: {err}")),
                    ValidationCheck::NotPresent(
                        "embedded manifest was not checked because no valid superblock was selected"
                            .to_string(),
                    ),
                )
            } else {
                (
                    ValidationCheck::Clean(
                        "embedded superblock selected before manifest validation failed"
                            .to_string(),
                    ),
                    ValidationCheck::Failed(format!("embedded manifest validation failed: {err}")),
                )
            }
        }
    }
}

fn has_embedded_superblock_magic(path: &Path) -> bool {
    const MAGIC: &[u8; 8] = b"RDBSBLK1";
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 8];
    for offset in [
        EMBEDDED_RDB_SUPERBLOCK_0_OFFSET,
        EMBEDDED_RDB_SUPERBLOCK_1_OFFSET,
    ] {
        if std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(offset)).is_ok()
            && file.read_exact(&mut buf).is_ok()
            && &buf == MAGIC
        {
            return true;
        }
    }
    false
}

fn validate_operational_manifest(path: &Path) -> ValidationCheck {
    match OperationalManifest::validate_read_only(path) {
        Ok(Some(generation)) => ValidationCheck::Clean(format!(
            "operational manifest generation {generation} checksum validated"
        )),
        Ok(None) => ValidationCheck::NotPresent("operational manifest is not present".to_string()),
        Err(err) => {
            ValidationCheck::Failed(format!("operational manifest validation failed: {err}"))
        }
    }
}

fn validate_mutable_pages(path: &Path) -> ValidationCheck {
    if !path.is_file() {
        return ValidationCheck::NotPresent("mutable page file is not present".to_string());
    }
    match validate_page_file(path) {
        Ok(count) => ValidationCheck::Clean(format!("validated {count} mutable page checksums")),
        Err(err) => {
            ValidationCheck::Failed(format!("mutable page checksum validation failed: {err}"))
        }
    }
}

fn validate_page_file(path: &Path) -> io::Result<u64> {
    let len = fs::metadata(path)?.len();
    if len == 0 {
        return Ok(0);
    }
    if len % PAGE_SIZE as u64 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("file size {len} is not a multiple of page size {PAGE_SIZE}"),
        ));
    }

    let mut file = File::open(path)?;
    let mut buf = [0u8; PAGE_SIZE];
    let mut page_id = 0u64;
    loop {
        match file.read_exact(&mut buf) {
            Ok(()) => {
                let page = Page::from_bytes(buf);
                page.verify_checksum().map_err(|err| {
                    io::Error::new(io::ErrorKind::InvalidData, format!("page {page_id}: {err}"))
                })?;
                page_id += 1;
            }
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(page_id),
            Err(err) => return Err(err),
        }
    }
}

fn validate_append_only_segment_files(path: &Path) -> ValidationCheck {
    let ops_root = {
        let mut root = path.as_os_str().to_os_string();
        root.push(".ops");
        PathBuf::from(root)
    };
    let Ok(entries) = fs::read_dir(&ops_root) else {
        return ValidationCheck::NotPresent(
            "append-only segment metadata is not present".to_string(),
        );
    };
    for entry in entries.flatten() {
        if entry.file_name() == "segments.aof" {
            return ValidationCheck::Clean(
                "append-only segment file is present; persisted chunk metadata is not recorded in this layout"
                    .to_string(),
            );
        }
    }
    ValidationCheck::NotPresent("append-only segment metadata is not present".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::embedded::EMBEDDED_RDB_MANIFEST_OFFSET;
    use crate::storage::engine::PAGE_SIZE;
    use crate::storage::unified::UnifiedStore;
    use crate::storage::unified::{AppendOnlySegment, AppendOnlySegmentCodec};
    use std::io::{Seek, SeekFrom, Write};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "reddb_storage_validation_{name}_{}_{}",
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.join("data.rdb")
    }

    #[test]
    fn clean_unified_store_validates_pages_and_operational_manifest() {
        let path = temp_path("clean_unified");
        {
            let store = UnifiedStore::open(&path).unwrap();
            store.create_collection("users").unwrap();
        }

        let report = validate_storage(&path);
        assert!(report.is_clean(), "{report:?}");
        assert!(matches!(
            report.operational_manifest,
            ValidationCheck::Clean(_)
        ));
        assert!(matches!(report.mutable_pages, ValidationCheck::Clean(_)));
    }

    #[test]
    fn corrupt_operational_manifest_is_actionable_failure() {
        let path = temp_path("bad_ops_manifest");
        {
            let store = UnifiedStore::open(&path).unwrap();
            store.create_collection("users").unwrap();
        }
        let mut manifest_path = path.as_os_str().to_os_string();
        manifest_path.push(".ops/manifest.json");
        let manifest_path = PathBuf::from(manifest_path);
        let text = fs::read_to_string(&manifest_path).unwrap();
        fs::write(
            &manifest_path,
            text.replace("\"generation\": 1", "\"generation\": 7"),
        )
        .unwrap();

        let report = validate_storage(&path);
        assert!(!report.is_clean());
        assert!(
            report
                .operational_manifest
                .message()
                .contains("checksum mismatch"),
            "{report:?}"
        );
    }

    #[test]
    fn corrupt_mutable_page_checksum_is_actionable_failure() {
        let path = temp_path("bad_page");
        {
            let store = UnifiedStore::open(&path).unwrap();
            store.create_collection("users").unwrap();
        }
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.seek(SeekFrom::Start(100)).unwrap();
        file.write_all(&[0xAA]).unwrap();
        file.sync_all().unwrap();

        let report = validate_storage(&path);
        assert!(!report.is_clean());
        assert!(
            report.mutable_pages.message().contains("page 0")
                && report.mutable_pages.message().contains("Checksum mismatch"),
            "{report:?}"
        );
    }

    #[test]
    fn embedded_manifest_corruption_is_reported_without_mutation() {
        let path = temp_path("bad_embedded_manifest");
        EmbeddedRdbArtifact::create(&path).unwrap();
        let before = fs::metadata(&path).unwrap().modified().unwrap();
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.seek(SeekFrom::Start(EMBEDDED_RDB_MANIFEST_OFFSET + 12))
            .unwrap();
        file.write_all(&[0xFF]).unwrap();
        file.sync_all().unwrap();
        let after_corruption = fs::metadata(&path).unwrap().modified().unwrap();

        let report = validate_storage(&path);
        let after_validation = fs::metadata(&path).unwrap().modified().unwrap();
        assert!(!report.is_clean());
        assert!(report.embedded_manifest.message().contains("checksum"));
        assert!(after_corruption >= before);
        assert_eq!(after_corruption, after_validation);
    }

    #[test]
    fn append_only_segment_metadata_checksum_mismatch_is_reported() {
        let mut segment =
            AppendOnlySegment::with_codec(42, "events", AppendOnlySegmentCodec::none());
        segment.append("pk-1", b"{\"id\":1}", []).unwrap();
        let metadata = segment.close().unwrap().clone();
        segment.corrupt_chunk_for_test(0, 1, b'X');

        let check = validate_append_only_segment(&segment, &metadata);
        assert!(!check.is_clean());
        assert!(check.message().contains("checksum"));
    }
}
