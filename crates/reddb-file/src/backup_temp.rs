use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{layout, RdbFileResult};

/// Local temporary JSON artifact used while publishing or reading backup metadata.
///
/// Remote backends still own upload/download transport. This type owns the local
/// file contract around backup JSON staging: canonical name, path, byte IO, and
/// best-effort cleanup.
#[derive(Debug)]
pub struct BackupTempJsonFile {
    path: PathBuf,
}

impl BackupTempJsonFile {
    pub fn new(prefix: &str, start_lsn: Option<u64>, end_lsn: Option<u64>) -> Self {
        Self::with_clock(
            &std::env::temp_dir(),
            prefix,
            std::process::id(),
            now_nanos(),
            start_lsn,
            end_lsn,
        )
    }

    pub fn with_clock(
        temp_dir: &Path,
        prefix: &str,
        process_id: u32,
        nanos: u128,
        start_lsn: Option<u64>,
        end_lsn: Option<u64>,
    ) -> Self {
        Self {
            path: layout::backup_temp_json_path(
                temp_dir, prefix, process_id, nanos, start_lsn, end_lsn,
            ),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write_bytes(&self, bytes: &[u8]) -> RdbFileResult<u64> {
        fs::write(&self.path, bytes)?;
        Ok(fs::metadata(&self.path)?.len())
    }

    pub fn read_bytes(&self) -> RdbFileResult<Vec<u8>> {
        Ok(fs::read(&self.path)?)
    }

    pub fn cleanup(&self) -> RdbFileResult<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}

impl Drop for BackupTempJsonFile {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_json_file_uses_canonical_layout_and_roundtrips_bytes() {
        let root =
            std::env::temp_dir().join(format!("reddb-file-backup-temp-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create temp root");

        let temp =
            BackupTempJsonFile::with_clock(&root, "reddb-json-object", 7, 99, Some(10), Some(20));

        assert_eq!(
            temp.path(),
            root.join("reddb-json-object-7-10-20-99.json").as_path()
        );
        assert_eq!(temp.write_bytes(b"{\"ok\":true}").expect("write"), 11);
        assert_eq!(temp.read_bytes().expect("read"), b"{\"ok\":true}");
        temp.cleanup().expect("cleanup");
        assert!(!temp.path().exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn temp_json_cleanup_tolerates_missing_file() {
        let root = std::env::temp_dir().join(format!(
            "reddb-file-backup-temp-missing-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create temp root");

        let temp = BackupTempJsonFile::with_clock(&root, "reddb-json-object", 7, 99, None, None);

        temp.cleanup().expect("missing cleanup");
        assert!(!temp.path().exists());

        let _ = fs::remove_dir_all(&root);
    }
}
