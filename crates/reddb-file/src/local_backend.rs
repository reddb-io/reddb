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
    if let Err(err) = fs::File::open(&temp).and_then(|file| file.sync_all()) {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }
    if let Err(err) = fs::rename(&temp, dest) {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }
    if let Some(parent) = dest.parent() {
        let _ = fs::File::open(parent).and_then(|dir| dir.sync_all());
    }
    Ok(())
}
