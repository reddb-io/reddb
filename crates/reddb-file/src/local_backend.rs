//! Local filesystem backend file operations.
//!
//! Runtime crates own remote-backend policy. This module owns the local
//! filesystem artifact choreography for downloads and atomic uploads.

use std::fs;
use std::io;
use std::path::Path;

pub fn local_backend_download(remote_key: &str, local_path: &Path) -> io::Result<bool> {
    let source = Path::new(remote_key);
    if !source.exists() {
        return Ok(false);
    }
    fs::copy(source, local_path)?;
    Ok(true)
}

pub fn local_backend_atomic_upload(
    local_path: &Path,
    remote_key: &str,
    pid: u32,
    unique: u64,
) -> io::Result<()> {
    let dest = Path::new(remote_key);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = crate::layout::local_upload_temp_path(dest, pid, unique);

    if let Err(err) = fs::copy(local_path, &temp) {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }
    // Re-open for *write* before syncing: `File::open` is read-only, and
    // while POSIX permits `fsync` on a read-only descriptor, Windows'
    // `FlushFileBuffers` requires write access and fails with
    // `ERROR_ACCESS_DENIED`. That made every local-backend upload fail on
    // Windows — the durability barrier is the whole point of this step, so it
    // cannot simply be ignored.
    if let Err(err) = fs::OpenOptions::new()
        .write(true)
        .open(&temp)
        .and_then(|file| file.sync_all())
    {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }
    if let Err(err) = fs::rename(&temp, dest) {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }
    if let Some(parent) = dest.parent() {
        // POSIX-only: see `Vfs::sync_dir`. Already best-effort, but on
        // Windows it can only ever fail, so do not pretend to try.
        #[cfg(not(windows))]
        let _ = fs::File::open(parent).and_then(|dir| dir.sync_all());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_backend_download_reports_missing_or_copies_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.bin");
        let dest = dir.path().join("dest.bin");

        assert!(!local_backend_download(missing.to_str().unwrap(), &dest).unwrap());
        assert!(!dest.exists());

        let source = dir.path().join("source.bin");
        fs::write(&source, b"payload").unwrap();
        assert!(local_backend_download(source.to_str().unwrap(), &dest).unwrap());
        assert_eq!(fs::read(&dest).unwrap(), b"payload");
    }

    #[test]
    fn local_backend_atomic_upload_creates_parent_and_replaces_destination() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("local.bin");
        let remote = dir.path().join("nested").join("remote.bin");
        fs::write(&local, b"new").unwrap();

        local_backend_atomic_upload(&local, remote.to_str().unwrap(), 123, 456).unwrap();

        assert_eq!(fs::read(&remote).unwrap(), b"new");
        let temp = crate::layout::local_upload_temp_path(&remote, 123, 456);
        assert!(!temp.exists());

        fs::write(&local, b"replacement").unwrap();
        local_backend_atomic_upload(&local, remote.to_str().unwrap(), 123, 457).unwrap();
        assert_eq!(fs::read(&remote).unwrap(), b"replacement");
    }

    #[test]
    fn local_backend_atomic_upload_cleans_temp_when_copy_fails() {
        let dir = tempfile::tempdir().unwrap();
        let missing_local = dir.path().join("missing.bin");
        let remote = dir.path().join("remote.bin");

        assert!(
            local_backend_atomic_upload(&missing_local, remote.to_str().unwrap(), 1, 2).is_err()
        );
        let temp = crate::layout::local_upload_temp_path(&remote, 1, 2);
        assert!(!temp.exists());
        assert!(!remote.exists());
    }
}
