//! Contiguous page extent storage for TurboQuant payloads.
//!
//! MIT notice: clean-room RedDB implementation for the turbovec-compatible
//! TurboQuant surface; no upstream turbovec source is copied.

use crate::storage::engine::pager::{ExtentId, PagerError};
use crate::storage::engine::{Page, PageType, Pager, HEADER_SIZE, PAGE_SIZE};
use std::sync::Arc;

const PAYLOAD_BYTES_PER_PAGE: usize = PAGE_SIZE - HEADER_SIZE;

pub struct TurboExtent {
    pager: Arc<Pager>,
    extents: Vec<ExtentId>,
    write_offset: usize,
}

impl TurboExtent {
    pub fn new(pager: Arc<Pager>) -> Result<Self, PagerError> {
        let first = pager.reserve_contig_extent(64)?;
        Ok(Self {
            pager,
            extents: vec![first],
            write_offset: 0,
        })
    }

    pub fn append(&mut self, bytes: &[u8]) -> Result<usize, PagerError> {
        let offset = self.write_offset;
        self.ensure_capacity(self.write_offset + bytes.len())?;
        let mut written = 0;
        while written < bytes.len() {
            let absolute = self.write_offset;
            let (page_id, page_offset) = self.locate(absolute).ok_or_else(|| {
                PagerError::InvalidDatabase(
                    "turbo extent offset outside reserved pages".to_string(),
                )
            })?;
            let chunk_len = (PAYLOAD_BYTES_PER_PAGE - page_offset).min(bytes.len() - written);
            let mut page = self
                .pager
                .read_page(page_id)
                .unwrap_or_else(|_| Page::new(PageType::Vector, page_id));
            page.content_mut()[page_offset..page_offset + chunk_len]
                .copy_from_slice(&bytes[written..written + chunk_len]);
            self.pager.write_page(page_id, page)?;
            written += chunk_len;
            self.write_offset += chunk_len;
        }
        Ok(offset)
    }

    pub fn read(&self, offset: usize, len: usize) -> Result<Vec<u8>, PagerError> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let absolute = offset + out.len();
            let (page_id, page_offset) = self.locate(absolute).ok_or_else(|| {
                PagerError::InvalidDatabase("turbo extent read outside reserved pages".to_string())
            })?;
            let page = self.pager.read_page(page_id)?;
            let chunk_len = (PAYLOAD_BYTES_PER_PAGE - page_offset).min(len - out.len());
            out.extend_from_slice(&page.content()[page_offset..page_offset + chunk_len]);
        }
        Ok(out)
    }

    fn ensure_capacity(&mut self, required_bytes: usize) -> Result<(), PagerError> {
        while required_bytes > self.capacity_bytes() {
            let next_pages = self
                .extents
                .last()
                .map(|extent| extent.n_pages.saturating_mul(2))
                .unwrap_or(64)
                .max(1);
            self.extents
                .push(self.pager.reserve_contig_extent(next_pages)?);
        }
        Ok(())
    }

    fn capacity_bytes(&self) -> usize {
        self.extents
            .iter()
            .map(|extent| extent.n_pages as usize * PAYLOAD_BYTES_PER_PAGE)
            .sum()
    }

    fn locate(&self, mut offset: usize) -> Option<(u32, usize)> {
        for extent in &self.extents {
            let bytes = extent.n_pages as usize * PAYLOAD_BYTES_PER_PAGE;
            if offset < bytes {
                let page_delta = offset / PAYLOAD_BYTES_PER_PAGE;
                let page_offset = offset % PAYLOAD_BYTES_PER_PAGE;
                return Some((extent.start_page + page_delta as u32, page_offset));
            }
            offset -= bytes;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::engine::PagerConfig;

    /// RAII guard: removes the temp `.db` file and every pager sidecar
    /// (`-dwb`/`-hdr`/…) on drop by prefix-matching the file name, so nothing
    /// is left in TMPDIR for the leak guard (scripts/check-temp-residue.sh).
    /// Prefix-matching embeds no owned file-format suffix literals.
    struct TempExtentPath(std::path::PathBuf);
    impl Drop for TempExtentPath {
        fn drop(&mut self) {
            if let (Some(dir), Some(name)) = (
                self.0.parent(),
                self.0.file_name().and_then(|name| name.to_str()),
            ) {
                if let Ok(entries) = std::fs::read_dir(dir) {
                    for entry in entries.flatten() {
                        if entry
                            .file_name()
                            .to_str()
                            .is_some_and(|entry_name| entry_name.starts_with(name))
                        {
                            let _ = std::fs::remove_file(entry.path());
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn turbo_extent_reads_across_page_boundaries() {
        let path = TempExtentPath(
            std::env::temp_dir().join(format!("reddb-turbo-extent-{}.db", std::process::id())),
        );
        let _ = std::fs::remove_file(&path.0);
        let pager = Arc::new(Pager::open(&path.0, PagerConfig::default()).unwrap());
        let mut extent = TurboExtent::new(pager).unwrap();
        extent.write_offset = PAYLOAD_BYTES_PER_PAGE - 2;
        extent.ensure_capacity(PAYLOAD_BYTES_PER_PAGE + 2).unwrap();
        let offset = extent.append(&[1, 2, 3, 4]).unwrap();
        assert_eq!(extent.read(offset, 4).unwrap(), vec![1, 2, 3, 4]);
        // Drop the extent (and its pager) before the guard so the pager's
        // drop-time header flush happens before the prefix cleanup runs.
        drop(extent);
        drop(path);
    }
}
