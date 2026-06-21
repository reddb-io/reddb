use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{layout, RdbFileResult};

/// Process-global monotonic counter that makes every temp-file name unique
/// even when `now_nanos()` collides (coarse clock resolution) or when
/// concurrent archives stage the same LSN range. Without it, two threads in
/// the same process could write distinct payloads to the same staging path,
/// uploading one body under the other's manifest digest and failing the WAL
/// segment integrity check on restore.
static BACKUP_TEMP_JSON_COUNTER: AtomicU64 = AtomicU64::new(0);

pub const BACKUP_JSON_OBJECT_TEMP_PREFIX: &str = "reddb-json-object";
pub const BACKUP_JSON_OBJECT_READ_TEMP_PREFIX: &str = "reddb-json-object-read";
pub const ARCHIVED_CHANGE_RECORDS_TEMP_PREFIX: &str = "reddb-archived-change-records";
pub const ARCHIVED_CHANGE_RECORDS_READ_TEMP_PREFIX: &str = "reddb-archived-change-records-read";

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
            BACKUP_TEMP_JSON_COUNTER.fetch_add(1, Ordering::Relaxed),
            start_lsn,
            end_lsn,
        )
    }

    pub fn json_object() -> Self {
        Self::new(BACKUP_JSON_OBJECT_TEMP_PREFIX, None, None)
    }

    pub fn json_object_read() -> Self {
        Self::new(BACKUP_JSON_OBJECT_READ_TEMP_PREFIX, None, None)
    }

    pub fn archived_change_records(start_lsn: u64, end_lsn: u64) -> Self {
        Self::new(
            ARCHIVED_CHANGE_RECORDS_TEMP_PREFIX,
            Some(start_lsn),
            Some(end_lsn),
        )
    }

    pub fn archived_change_records_read() -> Self {
        Self::new(ARCHIVED_CHANGE_RECORDS_READ_TEMP_PREFIX, None, None)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_clock(
        temp_dir: &Path,
        prefix: &str,
        process_id: u32,
        nanos: u128,
        unique: u64,
        start_lsn: Option<u64>,
        end_lsn: Option<u64>,
    ) -> Self {
        Self {
            path: layout::backup_temp_json_path(
                temp_dir, prefix, process_id, nanos, unique, start_lsn, end_lsn,
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

        let temp = BackupTempJsonFile::with_clock(
            &root,
            BACKUP_JSON_OBJECT_TEMP_PREFIX,
            7,
            99,
            3,
            Some(10),
            Some(20),
        );

        assert_eq!(
            temp.path(),
            root.join("reddb-json-object-7-10-20-99-3.json").as_path()
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

        let temp = BackupTempJsonFile::with_clock(
            &root,
            BACKUP_JSON_OBJECT_TEMP_PREFIX,
            7,
            99,
            3,
            None,
            None,
        );

        temp.cleanup().expect("missing cleanup");
        assert!(!temp.path().exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn temp_json_constructors_use_distinct_prefixes_and_cleanup_on_drop() {
        let object = BackupTempJsonFile::json_object();
        let object_read = BackupTempJsonFile::json_object_read();
        let archived = BackupTempJsonFile::archived_change_records(10, 20);
        let archived_read = BackupTempJsonFile::archived_change_records_read();

        assert!(object
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(BACKUP_JSON_OBJECT_TEMP_PREFIX));
        assert!(object_read
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(BACKUP_JSON_OBJECT_READ_TEMP_PREFIX));
        assert!(archived
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("-10-20-"));
        assert!(archived_read
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(ARCHIVED_CHANGE_RECORDS_READ_TEMP_PREFIX));

        object.write_bytes(b"drop").unwrap();
        let path = object.path().to_path_buf();
        drop(object);
        assert!(!path.exists());
    }

    #[test]
    fn same_lsn_range_stagings_get_distinct_paths() {
        // Two stagings of the identical LSN range within one process must
        // never share a path, otherwise concurrent archives could overwrite
        // each other's payload and break WAL segment integrity on restore.
        let a = BackupTempJsonFile::archived_change_records(1, 5);
        let b = BackupTempJsonFile::archived_change_records(1, 5);
        assert_ne!(a.path(), b.path());
    }
}
