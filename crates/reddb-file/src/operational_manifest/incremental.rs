//! Resumable reads of one immutable operational-manifest generation.
use std::collections::BTreeMap;
use std::io;

use super::{
    append_only::next_unverified_from_manifest, invalid_data, manifest_from_bytes,
    AppendOnlySegmentManifestEntry, Manifest, OperationalManifest, MANIFEST_FILE,
};
use crate::{OpenMode, Vfs, VfsFile};

const READ_CHUNK: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ManifestReadStep {
    pub bytes_read: usize,
    pub complete: bool,
}

/// A pinned file generation. The host must fence manifest writers while
/// checking identity and publishing changes through this object.
pub struct IncrementalManifest<V: Vfs> {
    owner: OperationalManifest<V>,
    file: V::File,
    source_len: u64,
    bytes: Vec<u8>,
    manifest: Option<Manifest>,
    invalidated: bool,
}

impl<V: Vfs> OperationalManifest<V> {
    pub fn begin_incremental_read(&self) -> io::Result<Option<IncrementalManifest<V>>> {
        let file = match self
            .vfs
            .open(&self.root.join(MANIFEST_FILE), OpenMode::read_only())
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let source_len = file.file_len()?;
        usize::try_from(source_len).map_err(invalid_data)?;
        Ok(Some(IncrementalManifest {
            owner: self.clone(),
            file,
            source_len,
            bytes: Vec::new(),
            manifest: None,
            invalidated: false,
        }))
    }
}

impl<V: Vfs> IncrementalManifest<V> {
    pub fn step(&mut self, byte_budget: usize) -> io::Result<ManifestReadStep> {
        if self.invalidated {
            return Err(invalid_data("manifest snapshot was invalidated"));
        }
        if self.manifest.is_some() {
            return Ok(ManifestReadStep {
                bytes_read: 0,
                complete: true,
            });
        }
        let mut read_total = 0;
        while read_total < byte_budget && (self.bytes.len() as u64) < self.source_len {
            let remaining =
                usize::try_from(self.source_len - self.bytes.len() as u64).map_err(invalid_data)?;
            let amount = remaining.min(byte_budget - read_total).min(READ_CHUNK);
            self.bytes
                .try_reserve(amount)
                .map_err(|error| io::Error::other(error.to_string()))?;
            let mut chunk = vec![0; amount];
            let read = loop {
                match self.file.read(&mut chunk) {
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    result => break result?,
                }
            };
            if read == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            self.bytes.extend_from_slice(&chunk[..read]);
            read_total += read;
        }
        if self.bytes.len() as u64 == self.source_len {
            if self.file.file_len()? != self.source_len {
                return Err(invalid_data("manifest changed during incremental read"));
            }
            self.manifest = Some(manifest_from_bytes(&self.bytes)?);
            self.bytes = Vec::new();
        }
        Ok(ManifestReadStep {
            bytes_read: read_total,
            complete: self.manifest.is_some(),
        })
    }

    /// The manifest writer atomically replaces, and never edits, a published
    /// generation in place. Open handles pin the old identity until checked.
    pub fn is_current(&self) -> io::Result<bool> {
        if self.invalidated {
            return Ok(false);
        }
        let current = match self
            .owner
            .vfs
            .open(&self.owner.root.join(MANIFEST_FILE), OpenMode::read_only())
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        Ok(self.file.file_len()? == self.source_len
            && current.file_len()? == self.source_len
            && self.file.same_file(&current)?)
    }

    fn snapshot(&self) -> io::Result<&Manifest> {
        self.manifest
            .as_ref()
            .filter(|_| !self.invalidated)
            .ok_or_else(|| invalid_data("manifest snapshot is not ready"))
    }

    pub fn next_unverified_append_only_segment(
        &self,
        columns: &BTreeMap<String, (i64, String)>,
        after: Option<(&str, u64)>,
    ) -> io::Result<Option<AppendOnlySegmentManifestEntry>> {
        Ok(next_unverified_from_manifest(
            self.snapshot()?,
            columns,
            after,
        ))
    }

    pub fn contains_append_only_segment(
        &self,
        expected: &AppendOnlySegmentManifestEntry,
    ) -> io::Result<bool> {
        Ok(self
            .snapshot()?
            .append_only_segments
            .iter()
            .any(|entry| entry == expected))
    }

    pub fn publish_revalidated_append_only_bounds(
        &mut self,
        expected: &AppendOnlySegmentManifestEntry,
        column: &str,
        bounds: (i64, i64),
    ) -> io::Result<bool> {
        if !self.is_current()? {
            self.invalidated = true;
            return Err(invalid_data("manifest snapshot is stale"));
        }
        let manifest = self
            .manifest
            .as_mut()
            .ok_or_else(|| invalid_data("manifest snapshot is not ready"))?;
        let result = self
            .owner
            .publish_revalidated_append_only_bounds_loaded(manifest, expected, column, bounds);
        if !matches!(&result, Ok(false)) {
            self.invalidated = true;
        }
        result
    }

    pub fn retire_append_only_retention_batch(
        &mut self,
        cutoffs: &BTreeMap<String, (i64, String)>,
        limit: usize,
    ) -> io::Result<Vec<(String, u64)>> {
        if !self.is_current()? {
            self.invalidated = true;
            return Err(invalid_data("manifest snapshot is stale"));
        }
        let manifest = self
            .manifest
            .as_mut()
            .ok_or_else(|| invalid_data("manifest snapshot is not ready"))?;
        let result = self
            .owner
            .retire_append_only_retention_loaded(manifest, cutoffs, limit);
        if !matches!(&result, Ok(entries) if entries.is_empty()) {
            self.invalidated = true;
        }
        result
    }
}
