//! Byte I/O shared by the native pager and exclusively owned browser handles.
//!
//! The owner supplies locking, read-only policy, WAL-first ordering, allocation
//! fences and encryption. It must stop using a handle after failed I/O and
//! reopen/recover before resuming. These functions do not form a database API.

#![deny(
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::cast_possible_truncation
)]

use super::page::{Page, PageError, PageType, PAGE_SIZE};
use reddb_file::{VfsFile, PAGED_SUPERBLOCK_SLOT_SIZE, PAGED_SUPERBLOCK_ZONE_SIZE};
use std::io::{self, SeekFrom};

pub const DWB_ZONE_START_PAGE: u32 = 3;
pub const DWB_ZONE_PAGES: u32 = 64;
pub const DWB_ZONE_BYTES: usize = DWB_ZONE_PAGES as usize * PAGE_SIZE;
pub const FIRST_ALLOCATABLE_PAGE: u32 = DWB_ZONE_START_PAGE + DWB_ZONE_PAGES;
pub const DWB_MAX_PAGES_PER_FRAME: usize =
    (DWB_ZONE_BYTES - reddb_file::PAGED_DWB_HEADER_SIZE) / reddb_file::PAGED_DWB_ENTRY_SIZE;

#[derive(Debug)]
pub enum PagerIoError {
    Io(io::Error),
    Page(PageError),
    InvalidDatabase(String),
}

impl std::fmt::Display for PagerIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Page(error) => write!(f, "Page error: {error}"),
            Self::InvalidDatabase(message) => write!(f, "Invalid database: {message}"),
        }
    }
}

impl std::error::Error for PagerIoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Page(error) => Some(error),
            Self::InvalidDatabase(_) => None,
        }
    }
}

impl From<io::Error> for PagerIoError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<PageError> for PagerIoError {
    fn from(error: PageError) -> Self {
        Self::Page(error)
    }
}

pub fn read_page<F: VfsFile>(
    file: &mut F,
    page_id: u32,
    verify_checksum: bool,
) -> Result<Page, PagerIoError> {
    file.seek(SeekFrom::Start(u64::from(page_id) * PAGE_SIZE as u64))?;
    let mut buffer = [0u8; PAGE_SIZE];
    file.read_exact(&mut buffer)?;
    let page = Page::from_bytes(buffer);
    // Page 0 contains independently sealed superblocks, not a page checksum.
    if verify_checksum && page_id != 0 {
        page.verify_checksum()?;
    }
    Ok(page)
}

/// Raw write, without a durability barrier. The owner controls publication.
pub fn write_page<F: VfsFile>(file: &mut F, page_id: u32, page: &Page) -> Result<(), PagerIoError> {
    file.seek(SeekFrom::Start(u64::from(page_id) * PAGE_SIZE as u64))?;
    file.write_all(page.as_bytes())?;
    Ok(())
}

pub fn read_superblock_zone<F: VfsFile>(
    file: &mut F,
) -> Result<[u8; PAGED_SUPERBLOCK_ZONE_SIZE], PagerIoError> {
    let len = file.file_len()?;
    let mut zone = [0u8; PAGED_SUPERBLOCK_ZONE_SIZE];
    let readable = usize::try_from(len.min(PAGED_SUPERBLOCK_ZONE_SIZE as u64))
        .expect("readable prefix is bounded by the superblock zone");
    if readable > 0 {
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut zone[..readable])?;
    }
    Ok(zone)
}

/// Seal and sync only the stale slot; the owner selects it and its generation.
pub fn write_superblock_slot<F: VfsFile>(
    file: &mut F,
    slot: &mut [u8; PAGED_SUPERBLOCK_SLOT_SIZE],
    copy_index: usize,
    generation: u64,
) -> Result<(), PagerIoError> {
    reddb_file::seal_paged_superblock_slot(slot, copy_index, generation)
        .map_err(|error| PagerIoError::InvalidDatabase(error.to_string()))?;
    file.seek(SeekFrom::Start(reddb_file::paged_superblock_slot_offset(
        copy_index,
    )))?;
    file.write_all(slot)?;
    file.sync_all()?;
    Ok(())
}

pub fn dwb_zone_offset() -> u64 {
    u64::from(DWB_ZONE_START_PAGE) * PAGE_SIZE as u64
}

/// Clear the in-file DWB zone with valid reserved pages and sync it.
pub fn clear_dwb_zone<F: VfsFile>(file: &mut F) -> Result<(), PagerIoError> {
    for page_id in DWB_ZONE_START_PAGE..FIRST_ALLOCATABLE_PAGE {
        let mut page = Page::new(PageType::Header, page_id);
        page.update_checksum();
        write_page(file, page_id, &page)?;
    }
    file.sync_all()?;
    Ok(())
}

pub fn dwb_zone_is_clear(buffer: &[u8]) -> bool {
    if buffer.len() != DWB_ZONE_BYTES {
        return false;
    }
    for (index, chunk) in buffer.chunks_exact(PAGE_SIZE).enumerate() {
        let Ok(page) = Page::from_slice(chunk) else {
            return false;
        };
        if page.verify_checksum().is_err()
            || page.page_id()
                != DWB_ZONE_START_PAGE
                    + u32::try_from(index).expect("DWB page index is bounded by its zone")
            || page.page_type().ok() != Some(PageType::Header)
        {
            return false;
        }
    }
    true
}

/// The caller must make the batch's WAL durable before entering this function.
/// With DWB enabled: stage+sync and home pages+sync for each bounded frame,
/// then clear+sync once. With DWB disabled, the caller's proven atomic-write
/// policy applies.
pub fn write_pages<F: VfsFile>(
    file: &mut F,
    pages: &[(u32, Page)],
    double_write: bool,
) -> Result<(), PagerIoError> {
    if !double_write {
        for (page_id, page) in pages {
            write_page(file, *page_id, page)?;
        }
        return Ok(());
    }

    for chunk in pages.chunks(DWB_MAX_PAGES_PER_FRAME.max(1)) {
        let buffer = reddb_file::encode_paged_dwb_frame(
            chunk
                .iter()
                .map(|(page_id, page)| (*page_id, page.as_bytes())),
        );
        assert!(buffer.len() <= DWB_ZONE_BYTES, "DWB frame fits its zone");
        file.seek(SeekFrom::Start(dwb_zone_offset()))?;
        file.write_all(&buffer)?;
        file.sync_all()?;
        for (page_id, page) in chunk {
            write_page(file, *page_id, page)?;
        }
        file.sync_all()?;
    }
    // Each frame's home pages are durable before the next frame replaces its
    // staging image. An interrupted replacement can replay the old frame or
    // discard an incomplete new one: neither can lose those durable homes.
    // Only the final frame needs clearing before returning to the owner.
    if !pages.is_empty() {
        clear_dwb_zone(file)?;
    }
    Ok(())
}

/// Replay a complete staged DWB frame before allowing page access. Incomplete
/// staging is discarded as on the native path: home writes start only after
/// the complete frame crossed its barrier. No database root is recreated.
pub fn recover_from_dwb<F: VfsFile>(file: &mut F) -> Result<(), PagerIoError> {
    if file.file_len()? < dwb_zone_offset() + DWB_ZONE_BYTES as u64 {
        return Ok(());
    }
    let mut buffer = vec![0u8; DWB_ZONE_BYTES];
    file.seek(SeekFrom::Start(dwb_zone_offset()))?;
    file.read_exact(&mut buffer)?;
    if buffer.get(..4) != Some(reddb_file::DWB_MAGIC.as_slice()) {
        if dwb_zone_is_clear(&buffer) {
            return Ok(());
        }
        return clear_dwb_zone(file);
    }
    let entries = match reddb_file::decode_paged_dwb_frame(&buffer) {
        Ok(entries) => entries,
        Err(_) => return clear_dwb_zone(file),
    };
    for entry in entries {
        write_page(file, entry.page_id, &Page::from_bytes(entry.page))?;
    }
    file.sync_all()?;
    clear_dwb_zone(file)
}
