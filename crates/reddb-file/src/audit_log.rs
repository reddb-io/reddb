//! Audit-log file lifecycle helpers.
//!
//! The server owns audit schema, emission policy, and fallback logging. This
//! module owns the rotated audit-log artifact lifecycle.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditLogRotation {
    pub plain_path: PathBuf,
    pub compressed_path: Option<PathBuf>,
    pub compression_error: Option<String>,
}

pub fn rotate_audit_log(active: &Path, timestamp_nanos: u128) -> io::Result<AuditLogRotation> {
    let plain = crate::layout::audit_log_rotated_plain_path(active, timestamp_nanos);
    fs::rename(active, &plain)?;
    let raw = fs::read(&plain)?;
    let compressed = match zstd::bulk::compress(&raw, 3) {
        Ok(compressed) => compressed,
        Err(err) => {
            return Ok(AuditLogRotation {
                plain_path: plain,
                compressed_path: None,
                compression_error: Some(err.to_string()),
            });
        }
    };

    let zst = crate::layout::audit_log_rotated_compressed_path(active, timestamp_nanos);
    let mut out = fs::File::create(&zst)?;
    out.write_all(&compressed)?;
    out.sync_data()?;
    drop(out);
    let _ = fs::remove_file(&plain);
    Ok(AuditLogRotation {
        plain_path: plain,
        compressed_path: Some(zst),
        compression_error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotate_audit_log_compresses_and_removes_plaintext() {
        let dir = std::env::temp_dir();
        let active = dir.join(format!(
            "reddb-audit-rotate-{}-{}.log",
            std::process::id(),
            42
        ));
        let _ = fs::remove_file(&active);
        let _ = fs::remove_file(crate::layout::audit_log_rotated_plain_path(&active, 7));
        let _ = fs::remove_file(crate::layout::audit_log_rotated_compressed_path(&active, 7));

        fs::write(&active, b"{\"event\":1}\n").expect("write active");
        let rotated = rotate_audit_log(&active, 7).expect("rotate");
        assert!(!active.exists());
        assert!(!rotated.plain_path.exists());
        let compressed = rotated.compressed_path.expect("compressed path");
        let bytes = fs::read(&compressed).expect("read compressed");
        let plain = zstd::bulk::decompress(&bytes, 1024).expect("decompress");
        assert_eq!(plain, b"{\"event\":1}\n");

        let _ = fs::remove_file(compressed);
    }
}
