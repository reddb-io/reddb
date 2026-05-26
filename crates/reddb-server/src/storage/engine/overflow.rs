//! Overflow chain storage
//!
//! Owns chains of dedicated overflow pages (PageType::Overflow), independent of
//! B-tree semantics and MVCC. Backs the large-value spill path from ADR 0023
//! (slice B of PRD #662).
//!
//! Each overflow page lays a small chain-local header at the start of its
//! content area:
//!
//! ```text
//! Offset  Size  Field
//! ------  ----  -----
//!   0      4    next_page_id (u32 LE, 0 = tail sentinel)
//!   4      4    payload_len  (u32 LE, payload bytes on this page)
//!   8     ..    payload bytes
//! ```
//!
//! `version` (per ADR 0025) is added in slice C (#700); this slice only owns
//! the chain mechanics — allocate, link, walk, free.
//!
//! Whole-value materialisation only — no streaming reads.

use super::page::{Page, PageType, CONTENT_SIZE, HEADER_SIZE, PAGE_SIZE};
use super::pager::{Pager, PagerError};

/// Bytes consumed by the per-page chain header (next + payload_len).
pub const OVERFLOW_PAGE_HEADER: usize = 8;

/// Payload bytes that fit on a single overflow page.
pub const OVERFLOW_PAYLOAD_PER_PAGE: usize = CONTENT_SIZE - OVERFLOW_PAGE_HEADER;

const _: () = assert!(OVERFLOW_PAYLOAD_PER_PAGE == PAGE_SIZE - HEADER_SIZE - OVERFLOW_PAGE_HEADER);

/// Errors returned by overflow-chain operations.
#[derive(Debug)]
pub enum OverflowError {
    /// Underlying pager call failed.
    Pager(PagerError),
    /// A page reached while walking the chain is not an Overflow page.
    NotOverflowPage { page_id: u32 },
    /// A page declared a payload longer than the per-page capacity.
    PayloadTooLarge { page_id: u32, len: u32 },
    /// The advertised `total_len` disagrees with what the chain actually holds.
    LengthMismatch { expected: u64, actual: u64 },
    /// Caller asked to free a non-overflow page as a chain head.
    InvalidHead { page_id: u32 },
}

impl std::fmt::Display for OverflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pager(e) => write!(f, "pager error: {}", e),
            Self::NotOverflowPage { page_id } => {
                write!(f, "page {} is not an overflow page", page_id)
            }
            Self::PayloadTooLarge { page_id, len } => {
                write!(
                    f,
                    "overflow page {} declares payload_len {} (max {})",
                    page_id, len, OVERFLOW_PAYLOAD_PER_PAGE
                )
            }
            Self::LengthMismatch { expected, actual } => write!(
                f,
                "overflow chain length mismatch: expected {} bytes, chain holds {}",
                expected, actual
            ),
            Self::InvalidHead { page_id } => {
                write!(f, "free called with non-overflow head page {}", page_id)
            }
        }
    }
}

impl std::error::Error for OverflowError {}

impl From<PagerError> for OverflowError {
    fn from(e: PagerError) -> Self {
        Self::Pager(e)
    }
}

/// Number of overflow pages required to hold `len` bytes.
pub fn pages_needed(len: usize) -> usize {
    if len == 0 {
        1
    } else {
        len.div_ceil(OVERFLOW_PAYLOAD_PER_PAGE)
    }
}

fn read_chain_header(page: &Page) -> Result<(u32, u32), OverflowError> {
    if page.page_type().map_err(PagerError::from)? != PageType::Overflow {
        return Err(OverflowError::NotOverflowPage {
            page_id: page.page_id(),
        });
    }
    let content = page.content();
    let next = u32::from_le_bytes([content[0], content[1], content[2], content[3]]);
    let len = u32::from_le_bytes([content[4], content[5], content[6], content[7]]);
    if len as usize > OVERFLOW_PAYLOAD_PER_PAGE {
        return Err(OverflowError::PayloadTooLarge {
            page_id: page.page_id(),
            len,
        });
    }
    Ok((next, len))
}

fn write_chain_header(page: &mut Page, next: u32, payload_len: u32) {
    let content = page.content_mut();
    content[0..4].copy_from_slice(&next.to_le_bytes());
    content[4..8].copy_from_slice(&payload_len.to_le_bytes());
}

/// Chain operations over a pager.
///
/// Held by value rather than as `impl Pager` methods to keep overflow concerns
/// out of `engine::pager`. Slice E will hold one of these from inside the
/// B-tree write path.
pub struct OverflowChain<'p> {
    pager: &'p Pager,
}

impl<'p> OverflowChain<'p> {
    pub fn new(pager: &'p Pager) -> Self {
        Self { pager }
    }

    /// Allocate enough overflow pages to hold `bytes`, link them, and return
    /// the head page id together with the total length.
    ///
    /// Empty input still produces a single zero-length head page so that the
    /// leaf-side pointer is always valid.
    pub fn store(&self, bytes: &[u8]) -> Result<(u32, u64), OverflowError> {
        let total_len = bytes.len() as u64;
        let n_pages = pages_needed(bytes.len());

        let mut page_ids = Vec::with_capacity(n_pages);
        for _ in 0..n_pages {
            let page = self.pager.allocate_page(PageType::Overflow)?;
            page_ids.push(page.page_id());
        }

        let mut offset = 0usize;
        for (i, &pid) in page_ids.iter().enumerate() {
            let next = if i + 1 < page_ids.len() {
                page_ids[i + 1]
            } else {
                0
            };
            let chunk_end = (offset + OVERFLOW_PAYLOAD_PER_PAGE).min(bytes.len());
            let chunk = &bytes[offset..chunk_end];
            offset = chunk_end;

            let mut page = Page::new(PageType::Overflow, pid);
            write_chain_header(&mut page, next, chunk.len() as u32);
            page.content_mut()[OVERFLOW_PAGE_HEADER..OVERFLOW_PAGE_HEADER + chunk.len()]
                .copy_from_slice(chunk);

            self.pager.write_page(pid, page)?;
        }

        Ok((page_ids[0], total_len))
    }

    /// Walk the chain and return the concatenated payload.
    ///
    /// `total_len` must match the actual bytes carried by the chain; if it
    /// does not, a `LengthMismatch` error is returned (no truncation, no
    /// silent extension).
    pub fn read(&self, head_page_id: u32, total_len: u64) -> Result<Vec<u8>, OverflowError> {
        let expected = total_len as usize;
        let mut out = Vec::with_capacity(expected);
        let mut current = head_page_id;
        let mut collected: u64 = 0;

        while current != 0 {
            let page = self.pager.read_page(current).map_err(OverflowError::from)?;
            let (next, len) = read_chain_header(&page)?;
            let len_usize = len as usize;
            collected += len as u64;

            if collected > total_len {
                return Err(OverflowError::LengthMismatch {
                    expected: total_len,
                    actual: collected,
                });
            }

            let payload = &page.content()[OVERFLOW_PAGE_HEADER..OVERFLOW_PAGE_HEADER + len_usize];
            out.extend_from_slice(payload);
            current = next;
        }

        if collected != total_len {
            return Err(OverflowError::LengthMismatch {
                expected: total_len,
                actual: collected,
            });
        }

        Ok(out)
    }

    /// Walk the chain starting at `head_page_id` and return every page to the
    /// free list.
    pub fn free(&self, head_page_id: u32) -> Result<(), OverflowError> {
        let mut current = head_page_id;
        let mut first = true;
        while current != 0 {
            let page = self.pager.read_page(current).map_err(OverflowError::from)?;
            if page.page_type().map_err(PagerError::from)? != PageType::Overflow {
                return Err(if first {
                    OverflowError::InvalidHead { page_id: current }
                } else {
                    OverflowError::NotOverflowPage { page_id: current }
                });
            }
            let (next, _) = read_chain_header(&page)?;
            self.pager.free_page(current)?;
            current = next;
            first = false;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::engine::pager::Pager;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_db_path() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "reddb_overflow_test_{}_{}.db",
            std::process::id(),
            id
        ));
        path
    }

    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        for suffix in ["-hdr", "-meta", "-dwb"] {
            let mut p = path.to_path_buf().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(&p);
        }
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| ((i * 31 + 7) & 0xff) as u8).collect()
    }

    #[test]
    fn pages_needed_boundaries() {
        assert_eq!(pages_needed(0), 1);
        assert_eq!(pages_needed(1), 1);
        assert_eq!(pages_needed(OVERFLOW_PAYLOAD_PER_PAGE), 1);
        assert_eq!(pages_needed(OVERFLOW_PAYLOAD_PER_PAGE + 1), 2);
        assert_eq!(pages_needed(OVERFLOW_PAYLOAD_PER_PAGE * 4), 4);
        assert_eq!(pages_needed(OVERFLOW_PAYLOAD_PER_PAGE * 4 + 1), 5);
    }

    fn roundtrip(pager: &Pager, len: usize) {
        let chain = OverflowChain::new(pager);
        let data = pattern(len);
        let (head, total) = chain.store(&data).unwrap();
        assert_eq!(total, len as u64);
        let read_back = chain.read(head, total).unwrap();
        assert_eq!(read_back.len(), len);
        assert_eq!(read_back, data);
        chain.free(head).unwrap();
    }

    #[test]
    fn store_read_roundtrips_across_sizes() {
        let path = temp_db_path();
        cleanup(&path);
        {
            let pager = Pager::open_default(&path).unwrap();
            // one page
            roundtrip(&pager, 1);
            roundtrip(&pager, 100);
            roundtrip(&pager, OVERFLOW_PAYLOAD_PER_PAGE - 1);
            // exact one-page boundary
            roundtrip(&pager, OVERFLOW_PAYLOAD_PER_PAGE);
            // two pages
            roundtrip(&pager, OVERFLOW_PAYLOAD_PER_PAGE + 1);
            roundtrip(&pager, OVERFLOW_PAYLOAD_PER_PAGE * 2);
            // several pages
            roundtrip(&pager, OVERFLOW_PAYLOAD_PER_PAGE * 5 + 123);
            // many pages
            roundtrip(&pager, OVERFLOW_PAYLOAD_PER_PAGE * 32);
            // exact multi-page boundary
            roundtrip(&pager, OVERFLOW_PAYLOAD_PER_PAGE * 7);
        }
        cleanup(&path);
    }

    #[test]
    fn store_empty_value_produces_single_page() {
        let path = temp_db_path();
        cleanup(&path);
        {
            let pager = Pager::open_default(&path).unwrap();
            let chain = OverflowChain::new(&pager);
            let (head, total) = chain.store(&[]).unwrap();
            assert_eq!(total, 0);
            let bytes = chain.read(head, total).unwrap();
            assert!(bytes.is_empty());

            // Confirm it's exactly one page in the chain.
            let page = pager.read_page(head).unwrap();
            let (next, len) = read_chain_header(&page).unwrap();
            assert_eq!(next, 0);
            assert_eq!(len, 0);

            chain.free(head).unwrap();
        }
        cleanup(&path);
    }

    #[test]
    fn read_with_wrong_total_len_errors() {
        let path = temp_db_path();
        cleanup(&path);
        {
            let pager = Pager::open_default(&path).unwrap();
            let chain = OverflowChain::new(&pager);
            let data = pattern(OVERFLOW_PAYLOAD_PER_PAGE * 3 + 50);
            let (head, total) = chain.store(&data).unwrap();

            // Too short: chain reports more bytes than caller expects.
            let err = chain.read(head, total - 10).unwrap_err();
            assert!(matches!(err, OverflowError::LengthMismatch { .. }));

            // Too long: chain ends before caller's expected length.
            let err = chain.read(head, total + 10).unwrap_err();
            assert!(matches!(err, OverflowError::LengthMismatch { .. }));

            chain.free(head).unwrap();
        }
        cleanup(&path);
    }

    #[test]
    fn free_returns_pages_to_freelist_observably() {
        let path = temp_db_path();
        cleanup(&path);
        {
            let pager = Pager::open_default(&path).unwrap();
            let chain = OverflowChain::new(&pager);

            let len = OVERFLOW_PAYLOAD_PER_PAGE * 6 + 17;
            let n = pages_needed(len);
            let data = pattern(len);

            let before_alloc = pager.page_count().unwrap();
            let (head, _) = chain.store(&data).unwrap();
            let after_alloc = pager.page_count().unwrap();
            assert_eq!((after_alloc - before_alloc) as usize, n);

            chain.free(head).unwrap();

            // A second store of the same size must reuse the freed pages
            // rather than extending the file.
            let after_free = pager.page_count().unwrap();
            let (head2, _) = chain.store(&data).unwrap();
            let after_realloc = pager.page_count().unwrap();
            assert_eq!(after_realloc, after_free, "second store should reuse freed pages");

            chain.free(head2).unwrap();
        }
        cleanup(&path);
    }

    #[test]
    fn free_then_store_reuses_pages_exact_count() {
        let path = temp_db_path();
        cleanup(&path);
        {
            let pager = Pager::open_default(&path).unwrap();
            let chain = OverflowChain::new(&pager);

            let len = OVERFLOW_PAYLOAD_PER_PAGE * 4;
            let (head, _) = chain.store(&pattern(len)).unwrap();
            let baseline = pager.page_count().unwrap();
            chain.free(head).unwrap();
            // free does not shrink the file
            assert_eq!(pager.page_count().unwrap(), baseline);

            let (head2, _) = chain.store(&pattern(len)).unwrap();
            assert_eq!(pager.page_count().unwrap(), baseline);
            chain.free(head2).unwrap();
        }
        cleanup(&path);
    }
}
