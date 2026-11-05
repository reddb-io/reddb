use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{RdbFileError, RdbFileResult};

/// Publish a complete document, then persist the directory entry. The caller
/// serializes read-modify-write operations; unique siblings protect readers
/// from partial bytes even when independent publishers use this file API.
pub(super) fn write_physical_metadata_bytes_atomically(
    path: &Path,
    bytes: &[u8],
) -> RdbFileResult<()> {
    static NEXT_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let mut name = path
        .file_name()
        .ok_or_else(|| {
            RdbFileError::InvalidOperation(format!(
                "metadata path has no file name: {}",
                path.display()
            ))
        })?
        .to_os_string();
    name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        NEXT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let temp_path = path.with_file_name(name);
    // Only clean up a sibling this call successfully created. A stale sibling
    // (for example after PID reuse) belongs to an earlier publication.
    let file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp_path)?;
    let result = (|| -> std::io::Result<()> {
        {
            let mut file = file;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        fs::rename(&temp_path, path)?;
        #[cfg(unix)]
        {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result.map_err(Into::into)
}
