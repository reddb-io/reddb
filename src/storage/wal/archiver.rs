//! WAL Archiver — copies WAL segments to remote backend before truncation.
//!
//! Enables Point-in-Time Recovery (PITR) by preserving WAL history.
//! Integrates with the checkpoint flow to archive segments before they are truncated.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::storage::backend::{BackendError, RemoteBackend};

/// Metadata about an archived WAL segment.
#[derive(Debug, Clone)]
pub struct WalSegmentMeta {
    /// Remote key (e.g., "wal/000000000008-000000050432.wal")
    pub key: String,
    /// Starting LSN of this segment
    pub lsn_start: u64,
    /// Ending LSN of this segment
    pub lsn_end: u64,
    /// When this segment was archived (unix ms)
    pub created_at: u64,
    /// Size in bytes
    pub size_bytes: u64,
}

/// WAL Archiver — copies WAL segments to a remote backend.
pub struct WalArchiver {
    backend: Arc<dyn RemoteBackend>,
    prefix: String,
}

impl WalArchiver {
    /// Create a new archiver with a remote backend and key prefix.
    pub fn new(backend: Arc<dyn RemoteBackend>, prefix: impl Into<String>) -> Self {
        Self {
            backend,
            prefix: prefix.into(),
        }
    }

    /// Archive a WAL file as a named segment.
    /// Call this BEFORE truncating the WAL.
    pub fn archive_segment(
        &self,
        wal_path: &Path,
        lsn_start: u64,
        lsn_end: u64,
    ) -> Result<WalSegmentMeta, BackendError> {
        let size_bytes = std::fs::metadata(wal_path).map(|m| m.len()).unwrap_or(0);

        let key = format!("{}{:012}-{:012}.wal", self.prefix, lsn_start, lsn_end);

        self.backend.upload(wal_path, &key)?;

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Ok(WalSegmentMeta {
            key,
            lsn_start,
            lsn_end,
            created_at,
            size_bytes,
        })
    }

    /// Download an archived WAL segment to a local path.
    pub fn download_segment(&self, segment_key: &str, dest: &Path) -> Result<bool, BackendError> {
        self.backend.download(segment_key, dest)
    }

    /// Delete archived segments older than the given LSN.
    /// Returns the number of segments deleted.
    pub fn cleanup_before(&self, lsn: u64) -> Result<usize, BackendError> {
        // For now, this is a no-op since RemoteBackend trait doesn't have list().
        // In practice, the caller tracks segment metadata and deletes by key.
        let _ = lsn;
        Ok(0)
    }

    /// Check if a segment exists in the remote backend.
    pub fn segment_exists(&self, segment_key: &str) -> Result<bool, BackendError> {
        self.backend.exists(segment_key)
    }

    /// Get the backend name for logging.
    pub fn backend_name(&self) -> &str {
        self.backend.name()
    }
}

/// Archive a snapshot file to a remote backend.
pub fn archive_snapshot(
    backend: &dyn RemoteBackend,
    snapshot_path: &Path,
    snapshot_id: u64,
    prefix: &str,
) -> Result<String, BackendError> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let key = format!("{}{:012}-{}.snapshot", prefix, snapshot_id, timestamp);

    backend.upload(snapshot_path, &key)?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::backend::local::LocalBackend;
    use std::io::Write;

    #[test]
    fn test_archive_and_download() {
        let temp_dir = std::env::temp_dir().join("reddb_archiver_test");
        let _ = std::fs::create_dir_all(&temp_dir);
        let backend_dir = temp_dir.join("backend");
        let _ = std::fs::create_dir_all(&backend_dir);

        let backend = Arc::new(LocalBackend);
        let archiver = WalArchiver::new(backend, "wal/");

        // Create a fake WAL file
        let wal_path = temp_dir.join("test.wal");
        {
            let mut f = std::fs::File::create(&wal_path).unwrap();
            f.write_all(b"fake wal data").unwrap();
        }

        // Archive it
        let meta = archiver.archive_segment(&wal_path, 8, 500).unwrap();
        assert_eq!(meta.lsn_start, 8);
        assert_eq!(meta.lsn_end, 500);
        assert!(meta.key.starts_with("wal/"));
        assert!(meta.key.ends_with(".wal"));

        // Download it
        let dest = temp_dir.join("downloaded.wal");
        let found = archiver.download_segment(&meta.key, &dest).unwrap();
        assert!(found);
        assert!(dest.exists());

        // Cleanup
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
