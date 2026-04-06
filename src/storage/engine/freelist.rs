//! Free Page List Management
//!
//! Tracks free pages available for allocation. Uses a linked list of
//! trunk pages, each containing a list of free page IDs.
//!
//! # Structure
//!
//! The freelist is stored as:
//! 1. Header page (page 0) contains pointer to first trunk page
//! 2. Trunk pages contain:
//!    - Next trunk page ID (0 if last)
//!    - Count of free page IDs in this trunk
//!    - Array of free page IDs
//!
//! # Trunk Page Layout (4096 bytes)
//!
//! ```text
//! ┌────────────────────────────────────────────────────────────┐
//! │ PageHeader (32 bytes)                                      │
//! ├────────────────────────────────────────────────────────────┤
//! │ next_trunk: u32 (4 bytes) - Next trunk page ID            │
//! │ count: u32 (4 bytes) - Number of free page IDs            │
//! ├────────────────────────────────────────────────────────────┤
//! │ page_ids: [u32; N] - Free page IDs                        │
//! │ (N = (4096 - 32 - 8) / 4 = 1014 entries per trunk)        │
//! └────────────────────────────────────────────────────────────┘
//! ```

use super::page::{Page, PageType, HEADER_SIZE, PAGE_SIZE};

/// Maximum number of free page IDs per trunk page
pub const FREE_IDS_PER_TRUNK: usize = (PAGE_SIZE - HEADER_SIZE - 8) / 4;

/// Offset of next_trunk field in trunk page content
const NEXT_TRUNK_OFFSET: usize = HEADER_SIZE;

/// Offset of count field in trunk page content
const COUNT_OFFSET: usize = HEADER_SIZE + 4;

/// Offset of page_ids array in trunk page content
const PAGE_IDS_OFFSET: usize = HEADER_SIZE + 8;

/// Free list error types
#[derive(Debug, Clone)]
pub enum FreeListError {
    /// Freelist is empty
    Empty,
    /// Freelist is corrupted
    Corrupted(String),
    /// Invalid trunk page
    InvalidTrunk(u32),
}

impl std::fmt::Display for FreeListError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "Freelist is empty"),
            Self::Corrupted(msg) => write!(f, "Freelist corrupted: {}", msg),
            Self::InvalidTrunk(id) => write!(f, "Invalid trunk page: {}", id),
        }
    }
}

impl std::error::Error for FreeListError {}

/// In-memory freelist tracking
///
/// Maintains a fast in-memory list of free pages, with persistence
/// through trunk pages.
#[derive(Debug)]
pub struct FreeList {
    /// First trunk page ID (0 = no trunk pages)
    trunk_head: u32,
    /// In-memory list of free page IDs (fast allocation)
    free_pages: Vec<u32>,
    /// Total count of free pages (including those in trunk pages)
    total_free: u32,
    /// Whether the freelist has been modified
    dirty: bool,
}

impl FreeList {
    /// Create an empty freelist
    pub fn new() -> Self {
        Self {
            trunk_head: 0,
            free_pages: Vec::new(),
            total_free: 0,
            dirty: false,
        }
    }

    /// Create freelist from header page info
    pub fn from_header(trunk_head: u32, total_free: u32) -> Self {
        Self {
            trunk_head,
            free_pages: Vec::new(),
            total_free,
            dirty: false,
        }
    }

    /// Get trunk head page ID
    pub fn trunk_head(&self) -> u32 {
        self.trunk_head
    }

    /// Get total count of free pages
    pub fn total_free(&self) -> u32 {
        self.total_free
    }

    /// Check if freelist is empty
    pub fn is_empty(&self) -> bool {
        self.free_pages.is_empty() && self.trunk_head == 0
    }

    /// Check if freelist has been modified
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Mark freelist as clean
    pub fn mark_clean(&mut self) {
        self.dirty = false;
    }

    /// Allocate a free page
    ///
    /// Returns None if no free pages available.
    pub fn allocate(&mut self) -> Option<u32> {
        // Try in-memory list first
        if let Some(page_id) = self.free_pages.pop() {
            self.total_free = self.total_free.saturating_sub(1);
            self.dirty = true;
            return Some(page_id);
        }

        // Would need to load from trunk page
        // This is handled by the pager which has access to disk
        None
    }

    /// Return a page to the freelist
    pub fn free(&mut self, page_id: u32) {
        self.free_pages.push(page_id);
        self.total_free += 1;
        self.dirty = true;
    }

    /// Add multiple pages to freelist
    pub fn free_batch(&mut self, page_ids: &[u32]) {
        self.free_pages.extend_from_slice(page_ids);
        self.total_free += page_ids.len() as u32;
        self.dirty = true;
    }

    /// Get count of pages in memory (not including trunk pages)
    pub fn in_memory_count(&self) -> usize {
        self.free_pages.len()
    }

    /// Load free pages from a trunk page
    pub fn load_from_trunk(&mut self, trunk: &Page) -> Result<Option<u32>, FreeListError> {
        // Verify page type
        if trunk
            .page_type()
            .map_err(|_| FreeListError::InvalidTrunk(trunk.page_id()))?
            != PageType::FreelistTrunk
        {
            return Err(FreeListError::InvalidTrunk(trunk.page_id()));
        }

        let data = trunk.as_bytes();

        // Read next trunk pointer
        let next_trunk = u32::from_le_bytes([
            data[NEXT_TRUNK_OFFSET],
            data[NEXT_TRUNK_OFFSET + 1],
            data[NEXT_TRUNK_OFFSET + 2],
            data[NEXT_TRUNK_OFFSET + 3],
        ]);

        // Read count
        let count = u32::from_le_bytes([
            data[COUNT_OFFSET],
            data[COUNT_OFFSET + 1],
            data[COUNT_OFFSET + 2],
            data[COUNT_OFFSET + 3],
        ]) as usize;

        if count > FREE_IDS_PER_TRUNK {
            return Err(FreeListError::Corrupted(format!(
                "Trunk has {} entries, max is {}",
                count, FREE_IDS_PER_TRUNK
            )));
        }

        self.free_pages.push(trunk.page_id());

        // Read page IDs
        for i in 0..count {
            let offset = PAGE_IDS_OFFSET + i * 4;
            let page_id = u32::from_le_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]);
            self.free_pages.push(page_id);
        }
        self.total_free = self.total_free.saturating_add(count as u32 + 1);
        self.dirty = true;

        // Update trunk head to next trunk
        self.trunk_head = next_trunk;

        Ok(if next_trunk != 0 {
            Some(next_trunk)
        } else {
            None
        })
    }

    /// Create a trunk page from current free pages
    ///
    /// Moves pages from in-memory list to a trunk page.
    /// Returns the trunk page and remaining pages.
    pub fn create_trunk(&mut self, trunk_page_id: u32, next_trunk: u32) -> Page {
        let mut trunk = Page::new(PageType::FreelistTrunk, trunk_page_id);
        let data = trunk.as_bytes_mut();

        // Write next trunk pointer
        data[NEXT_TRUNK_OFFSET..NEXT_TRUNK_OFFSET + 4].copy_from_slice(&next_trunk.to_le_bytes());

        // Calculate how many pages to store
        let count = self.free_pages.len().min(FREE_IDS_PER_TRUNK);

        // Write count
        data[COUNT_OFFSET..COUNT_OFFSET + 4].copy_from_slice(&(count as u32).to_le_bytes());

        // Write page IDs (take from end to minimize moving)
        for i in 0..count {
            let page_id = self.free_pages.pop().unwrap();
            let offset = PAGE_IDS_OFFSET + i * 4;
            data[offset..offset + 4].copy_from_slice(&page_id.to_le_bytes());
        }

        trunk.update_checksum();
        self.dirty = true;

        trunk
    }

    /// Flush excess free pages to trunk pages
    ///
    /// If we have more than `threshold` pages in memory, create trunk pages.
    /// Returns trunk pages that need to be written.
    pub fn flush_to_trunks(
        &mut self,
        threshold: usize,
        mut allocate_page: impl FnMut() -> u32,
    ) -> Vec<Page> {
        let mut trunks = Vec::new();

        while self.free_pages.len() > threshold {
            // Allocate a page for the trunk
            let trunk_page_id = allocate_page();

            // Create trunk with current head as next
            let trunk = self.create_trunk(trunk_page_id, self.trunk_head);
            self.trunk_head = trunk_page_id;

            trunks.push(trunk);
        }

        trunks
    }

    /// Merge all trunk pages into memory
    ///
    /// Used during compaction to reclaim trunk pages.
    pub fn merge_all_trunks(
        &mut self,
        load_page: impl Fn(u32) -> Option<Page>,
    ) -> Result<Vec<u32>, FreeListError> {
        let mut reclaimed_trunks = Vec::new();

        while self.trunk_head != 0 {
            let trunk_id = self.trunk_head;
            let trunk = load_page(trunk_id).ok_or(FreeListError::InvalidTrunk(trunk_id))?;

            self.load_from_trunk(&trunk)?;
            reclaimed_trunks.push(trunk_id);
        }

        Ok(reclaimed_trunks)
    }
}

impl Default for FreeList {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_freelist_basic() {
        let mut fl = FreeList::new();

        assert!(fl.is_empty());
        assert_eq!(fl.total_free(), 0);

        // Free some pages
        fl.free(10);
        fl.free(20);
        fl.free(30);

        assert!(!fl.is_empty());
        assert_eq!(fl.total_free(), 3);

        // Allocate
        assert_eq!(fl.allocate(), Some(30));
        assert_eq!(fl.allocate(), Some(20));
        assert_eq!(fl.allocate(), Some(10));
        assert_eq!(fl.allocate(), None);

        assert!(fl.is_empty());
    }

    #[test]
    fn test_freelist_batch() {
        let mut fl = FreeList::new();

        fl.free_batch(&[1, 2, 3, 4, 5]);
        assert_eq!(fl.total_free(), 5);
        assert_eq!(fl.in_memory_count(), 5);
    }

    #[test]
    fn test_freelist_dirty() {
        let mut fl = FreeList::new();

        assert!(!fl.is_dirty());

        fl.free(1);
        assert!(fl.is_dirty());

        fl.mark_clean();
        assert!(!fl.is_dirty());

        fl.allocate();
        assert!(fl.is_dirty());
    }

    #[test]
    fn test_trunk_page_creation() {
        let mut fl = FreeList::new();

        // Add many pages
        for i in 0..100 {
            fl.free(i);
        }

        // Create a trunk
        let trunk = fl.create_trunk(999, 0);

        assert_eq!(trunk.page_type().unwrap(), PageType::FreelistTrunk);
        assert_eq!(trunk.page_id(), 999);

        // Some pages should have been moved to trunk
        assert!(fl.in_memory_count() < 100);
    }

    #[test]
    fn test_trunk_page_load() {
        let mut fl = FreeList::new();

        // Add pages and create trunk
        for i in 0..50 {
            fl.free(i);
        }

        let trunk = fl.create_trunk(999, 0);
        let pages_in_trunk = 50 - fl.in_memory_count();

        // Clear in-memory list
        fl.free_pages.clear();

        // Load from trunk
        fl.load_from_trunk(&trunk).unwrap();

        assert_eq!(fl.in_memory_count(), pages_in_trunk + 1);
    }

    #[test]
    fn test_trunk_page_reuse() {
        let mut original = FreeList::new();

        for i in 0..8 {
            original.free(i);
        }

        let trunk = original.create_trunk(999, 0);

        let mut fl = FreeList::new();
        fl.load_from_trunk(&trunk).unwrap();

        let mut ids = Vec::new();
        while let Some(id) = fl.allocate() {
            ids.push(id);
        }

        assert!(ids.contains(&999));
    }

    #[test]
    fn test_trunk_chain() {
        let mut fl = FreeList::new();

        // Add lots of pages (more than one trunk can hold)
        for i in 0..2000 {
            fl.free(i);
        }

        // Create trunk chain
        let mut next_trunk_id = 10000u32;
        let trunks = fl.flush_to_trunks(100, || {
            let id = next_trunk_id;
            next_trunk_id += 1;
            id
        });

        assert!(!trunks.is_empty());
        assert!(fl.in_memory_count() <= 100);
    }

    #[test]
    fn test_free_ids_per_trunk() {
        // Verify the constant is reasonable
        assert!(FREE_IDS_PER_TRUNK > 1000);
        assert!(FREE_IDS_PER_TRUNK < 1100);

        // (4096 - 32 - 8) / 4 = 1014
        assert_eq!(FREE_IDS_PER_TRUNK, 1014);
    }
}
