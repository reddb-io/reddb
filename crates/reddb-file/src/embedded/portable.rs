//! Single-owner embedded I/O, independent of paths, OS locks, and clocks.

use super::*;

/// An exclusively owned `.rdb` handle. The host must hold its writer fence for
/// the entire lifetime of this object (for OPFS, an exclusive sync access
/// handle). File formats and publication barriers are shared with native I/O.
///
/// An I/O failure makes publication ambiguous. This handle then refuses further
/// operations; the caller must reopen and recover before attempting more work.
pub struct EmbeddedRdbFile<F: VfsFile> {
    file: F,
    path: PathBuf,
    failed: bool,
}

impl<F: VfsFile> EmbeddedRdbFile<F> {
    /// Initialize an empty file. Never truncates an existing database.
    pub fn create(
        mut file: F,
        name: impl Into<PathBuf>,
        snapshot: &[u8],
        created_at_unix_ms: u128,
    ) -> RdbFileResult<Self> {
        if file.file_len()? != 0 {
            return Err(RdbFileError::InvalidOperation(
                "creating an embedded file requires an empty handle".into(),
            ));
        }
        let path = name.into();
        create_file(&mut file, &path, snapshot, created_at_unix_ms)?;
        Ok(Self {
            file,
            path,
            failed: false,
        })
    }

    /// Recover an existing file through its newest valid published root.
    pub fn open(mut file: F, name: impl Into<PathBuf>) -> RdbFileResult<Self> {
        let path = name.into();
        let open = EmbeddedRdbArtifact::open_inner_with_file(&mut file, &path, true)?;
        let scan = scan_wal_with_file(&mut file, &open, open.manifest.wal_region_offset)?;
        validate_wal(&open, &scan)?;
        Ok(Self {
            file,
            path,
            failed: false,
        })
    }

    pub fn state(&mut self) -> RdbFileResult<EmbeddedRdbOpen> {
        self.check_healthy()?;
        let result = EmbeddedRdbArtifact::open_inner_with_file(&mut self.file, &self.path, true);
        self.finish(result)
    }

    pub fn read_snapshot(&mut self) -> RdbFileResult<Option<Vec<u8>>> {
        let open = self.state()?;
        let result = read_snapshot_file(&mut self.file, &open);
        self.finish(result)
    }

    pub fn read_wal_payloads(&mut self) -> RdbFileResult<Vec<Vec<u8>>> {
        let open = self.state()?;
        let result = (|| {
            let scan = scan_wal_with_file(&mut self.file, &open, open.manifest.wal_region_offset)?;
            validate_wal(&open, &scan)?;
            Ok(scan.payloads)
        })();
        self.finish(result)
    }

    /// Publish a batch only after both WAL and root barriers succeed.
    pub fn append_wal_payloads(&mut self, payloads: &[Vec<u8>]) -> RdbFileResult<EmbeddedRdbOpen> {
        let open = self.state()?;
        if payloads.is_empty() {
            return Ok(open);
        }
        let result = (|| {
            let scan = scan_wal_with_file(&mut self.file, &open, open.manifest.wal_region_offset)?;
            append_file(&mut self.file, &self.path, payloads, open, scan, true)
                .map(|published| published.open)
        })();
        self.finish(result)
    }

    /// Checkpoint a complete native store snapshot, optionally growing the WAL.
    /// The caller is responsible for folding all committed WAL into the image.
    pub fn write_snapshot(
        &mut self,
        snapshot: &[u8],
        min_wal_bytes: u64,
    ) -> RdbFileResult<EmbeddedRdbOpen> {
        self.check_healthy()?;
        let result = checkpoint_file(&mut self.file, &self.path, snapshot, min_wal_bytes);
        self.finish(result)
    }

    pub fn into_inner(self) -> F {
        self.file
    }

    fn check_healthy(&self) -> RdbFileResult<()> {
        if self.failed {
            return Err(RdbFileError::InvalidOperation(
                "embedded file must be reopened after a failed operation".into(),
            ));
        }
        Ok(())
    }

    fn finish<T>(&mut self, result: RdbFileResult<T>) -> RdbFileResult<T> {
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}

fn validate_wal(open: &EmbeddedRdbOpen, scan: &WalScan) -> RdbFileResult<()> {
    if scan.valid_bytes != open.manifest.wal_live_bytes {
        return Err(RdbFileError::ZoneUnrecoverable {
            zone: "wal",
            path: open.path.clone(),
        });
    }
    Ok(())
}
