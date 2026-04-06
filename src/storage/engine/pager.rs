//! Pager - Page I/O Manager
//!
//! The Pager is responsible for reading and writing pages to/from disk.
//! It integrates with the PageCache for efficient caching and the FreeList
//! for page allocation.
//!
//! # Responsibilities
//!
//! 1. **Page I/O**: Read/write 4KB pages from/to disk
//! 2. **Caching**: Integrate with SIEVE PageCache
//! 3. **Allocation**: Manage free page allocation via FreeList
//! 4. **Header Management**: Maintain database header (page 0)
//!
//! # File Layout
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │ Page 0: Database Header                                     │
//! │   - Magic bytes "RDDB"                                      │
//! │   - Version                                                 │
//! │   - Page count                                              │
//! │   - Freelist head                                           │
//! ├─────────────────────────────────────────────────────────────┤
//! │ Page 1: Root B-tree page (or first data page)              │
//! ├─────────────────────────────────────────────────────────────┤
//! │ Page 2..N: Data pages                                       │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # References
//!
//! - Turso `core/storage/pager.rs:54-134` - HeaderRef::from_pager()
//! - Turso `core/storage/pager.rs:120` - pager.add_dirty(&page)

use super::freelist::FreeList;
use super::page::{Page, PageError, PageType, DB_VERSION, HEADER_SIZE, MAGIC_BYTES, PAGE_SIZE};
use super::page_cache::PageCache;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

/// Default cache size (pages)
const DEFAULT_CACHE_SIZE: usize = 10_000;

/// Pager error types
#[derive(Debug)]
pub enum PagerError {
    /// I/O error
    Io(std::io::Error),
    /// Page error
    Page(PageError),
    /// Invalid database file
    InvalidDatabase(String),
    /// Database is read-only
    ReadOnly,
    /// Page not found
    PageNotFound(u32),
    /// Database is locked
    Locked,
}

impl std::fmt::Display for PagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {}", e),
            Self::Page(e) => write!(f, "Page error: {}", e),
            Self::InvalidDatabase(msg) => write!(f, "Invalid database: {}", msg),
            Self::ReadOnly => write!(f, "Database is read-only"),
            Self::PageNotFound(id) => write!(f, "Page {} not found", id),
            Self::Locked => write!(f, "Database is locked"),
        }
    }
}

impl std::error::Error for PagerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Page(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for PagerError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<PageError> for PagerError {
    fn from(e: PageError) -> Self {
        Self::Page(e)
    }
}

/// Pager configuration
#[derive(Debug, Clone)]
pub struct PagerConfig {
    /// Page cache capacity
    pub cache_size: usize,
    /// Whether to open read-only
    pub read_only: bool,
    /// Whether to create if not exists
    pub create: bool,
    /// Whether to verify checksums on read
    pub verify_checksums: bool,
}

impl Default for PagerConfig {
    fn default() -> Self {
        Self {
            cache_size: DEFAULT_CACHE_SIZE,
            read_only: false,
            create: true,
            verify_checksums: true,
        }
    }
}

/// Database file header information
#[derive(Debug, Clone)]
pub struct DatabaseHeader {
    /// Database version
    pub version: u32,
    /// Page size (always 4096)
    pub page_size: u32,
    /// Total number of pages
    pub page_count: u32,
    /// First freelist trunk page ID (0 = no free pages)
    pub freelist_head: u32,
    /// Schema version (for migrations)
    pub schema_version: u32,
    /// Last checkpoint LSN
    pub checkpoint_lsn: u64,
}

impl Default for DatabaseHeader {
    fn default() -> Self {
        Self {
            version: DB_VERSION,
            page_size: PAGE_SIZE as u32,
            page_count: 1, // Header page
            freelist_head: 0,
            schema_version: 0,
            checkpoint_lsn: 0,
        }
    }
}

/// Page I/O Manager
///
/// Handles reading/writing pages and manages the page cache.
pub struct Pager {
    /// Database file path
    path: PathBuf,
    /// File handle
    file: Mutex<File>,
    /// Page cache
    cache: PageCache,
    /// Free page list
    freelist: RwLock<FreeList>,
    /// Database header
    header: RwLock<DatabaseHeader>,
    /// Configuration
    config: PagerConfig,
    /// Dirty flag for header
    header_dirty: Mutex<bool>,
}

impl Pager {
    /// Open or create a database file
    pub fn open<P: AsRef<Path>>(path: P, config: PagerConfig) -> Result<Self, PagerError> {
        let path = path.as_ref().to_path_buf();
        let exists = path.exists();

        if !exists && !config.create {
            return Err(PagerError::InvalidDatabase(
                "Database does not exist".into(),
            ));
        }

        if !exists && config.read_only {
            return Err(PagerError::InvalidDatabase(
                "Cannot create read-only database".into(),
            ));
        }

        // Open file
        // Note: create requires write access, so disable it for read-only mode
        let file = OpenOptions::new()
            .read(true)
            .write(!config.read_only)
            .create(config.create && !config.read_only)
            .open(&path)?;

        let pager = Self {
            path,
            file: Mutex::new(file),
            cache: PageCache::new(config.cache_size),
            freelist: RwLock::new(FreeList::new()),
            header: RwLock::new(DatabaseHeader::default()),
            config,
            header_dirty: Mutex::new(false),
        };

        if exists {
            // Load existing database
            pager.load_header()?;
        } else {
            // Initialize new database
            pager.initialize()?;
        }

        Ok(pager)
    }

    /// Open with default configuration
    pub fn open_default<P: AsRef<Path>>(path: P) -> Result<Self, PagerError> {
        Self::open(path, PagerConfig::default())
    }

    /// Initialize a new database
    fn initialize(&self) -> Result<(), PagerError> {
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }

        // Create header page
        let header_page = Page::new_header_page(1);

        // Write header page
        self.write_page_raw(0, &header_page)?;

        // Sync to disk
        self.sync()?;

        Ok(())
    }

    /// Load database header from page 0
    fn load_header(&self) -> Result<(), PagerError> {
        // Read page 0
        let header_page = self.read_page_raw(0)?;

        // Verify magic bytes
        let magic = &header_page.as_bytes()[HEADER_SIZE..HEADER_SIZE + 4];
        if magic != MAGIC_BYTES {
            return Err(PagerError::InvalidDatabase(format!(
                "Invalid magic bytes: {:02X?}",
                magic
            )));
        }

        // Read header fields
        let data = header_page.as_bytes();
        let version = u32::from_le_bytes([
            data[HEADER_SIZE + 4],
            data[HEADER_SIZE + 5],
            data[HEADER_SIZE + 6],
            data[HEADER_SIZE + 7],
        ]);

        let page_size = u32::from_le_bytes([
            data[HEADER_SIZE + 8],
            data[HEADER_SIZE + 9],
            data[HEADER_SIZE + 10],
            data[HEADER_SIZE + 11],
        ]);

        if page_size != PAGE_SIZE as u32 {
            return Err(PagerError::InvalidDatabase(format!(
                "Unsupported page size: {}",
                page_size
            )));
        }

        let page_count = u32::from_le_bytes([
            data[HEADER_SIZE + 12],
            data[HEADER_SIZE + 13],
            data[HEADER_SIZE + 14],
            data[HEADER_SIZE + 15],
        ]);

        let freelist_head = u32::from_le_bytes([
            data[HEADER_SIZE + 16],
            data[HEADER_SIZE + 17],
            data[HEADER_SIZE + 18],
            data[HEADER_SIZE + 19],
        ]);

        let schema_version = u32::from_le_bytes([
            data[HEADER_SIZE + 20],
            data[HEADER_SIZE + 21],
            data[HEADER_SIZE + 22],
            data[HEADER_SIZE + 23],
        ]);

        let checkpoint_lsn = u64::from_le_bytes([
            data[HEADER_SIZE + 24],
            data[HEADER_SIZE + 25],
            data[HEADER_SIZE + 26],
            data[HEADER_SIZE + 27],
            data[HEADER_SIZE + 28],
            data[HEADER_SIZE + 29],
            data[HEADER_SIZE + 30],
            data[HEADER_SIZE + 31],
        ]);

        // Update header
        {
            let mut header = self.header.write().unwrap();
            header.version = version;
            header.page_size = page_size;
            header.page_count = page_count;
            header.freelist_head = freelist_head;
            header.schema_version = schema_version;
            header.checkpoint_lsn = checkpoint_lsn;
        }

        // Initialize freelist
        {
            let mut freelist = self.freelist.write().unwrap();
            *freelist = FreeList::from_header(freelist_head, 0);
        }

        Ok(())
    }

    /// Write header page
    ///
    /// Note: This merges database header fields into the existing page 0 content
    /// to preserve any additional data (like encryption headers) that may be stored there.
    fn write_header(&self) -> Result<(), PagerError> {
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }

        let header = self.header.read().unwrap();

        // Read existing page 0 to preserve any additional data (e.g., encryption header)
        // First check cache, then fall back to disk
        let mut page = if let Some(cached) = self.cache.get(0) {
            cached
        } else {
            // Try to read from disk if file is large enough
            let file = self.file.lock().unwrap();
            let len = file.metadata().map(|m| m.len()).unwrap_or(0);
            drop(file);

            if len >= PAGE_SIZE as u64 {
                self.read_page_raw(0)?
            } else {
                // File is new/empty, create fresh header page
                Page::new(PageType::Header, 0)
            }
        };

        let data = page.as_bytes_mut();

        // Write magic
        data[HEADER_SIZE..HEADER_SIZE + 4].copy_from_slice(&MAGIC_BYTES);

        // Write fields (at fixed offsets in the DB header area)
        data[HEADER_SIZE + 4..HEADER_SIZE + 8].copy_from_slice(&header.version.to_le_bytes());
        data[HEADER_SIZE + 8..HEADER_SIZE + 12].copy_from_slice(&header.page_size.to_le_bytes());
        data[HEADER_SIZE + 12..HEADER_SIZE + 16].copy_from_slice(&header.page_count.to_le_bytes());
        data[HEADER_SIZE + 16..HEADER_SIZE + 20]
            .copy_from_slice(&header.freelist_head.to_le_bytes());
        data[HEADER_SIZE + 20..HEADER_SIZE + 24]
            .copy_from_slice(&header.schema_version.to_le_bytes());
        data[HEADER_SIZE + 24..HEADER_SIZE + 32]
            .copy_from_slice(&header.checkpoint_lsn.to_le_bytes());

        page.update_checksum();

        self.write_page_raw(0, &page)?;
        *self.header_dirty.lock().unwrap() = false;

        Ok(())
    }

    /// Read a page from disk (bypassing cache)
    fn read_page_raw(&self, page_id: u32) -> Result<Page, PagerError> {
        let mut file = self.file.lock().unwrap();
        let offset = (page_id as u64) * (PAGE_SIZE as u64);

        file.seek(SeekFrom::Start(offset))?;

        let mut buf = [0u8; PAGE_SIZE];
        file.read_exact(&mut buf)?;

        let page = Page::from_bytes(buf);

        // Verify checksum if configured
        if self.config.verify_checksums && page_id != 0 {
            page.verify_checksum()?;
        }

        Ok(page)
    }

    /// Write a page to disk (bypassing cache)
    fn write_page_raw(&self, page_id: u32, page: &Page) -> Result<(), PagerError> {
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }

        let mut file = self.file.lock().unwrap();
        let offset = (page_id as u64) * (PAGE_SIZE as u64);

        file.seek(SeekFrom::Start(offset))?;
        file.write_all(page.as_bytes())?;

        Ok(())
    }

    /// Read a page (cache-aware)
    pub fn read_page(&self, page_id: u32) -> Result<Page, PagerError> {
        // Check cache first
        if let Some(page) = self.cache.get(page_id) {
            return Ok(page);
        }

        // Cache miss - read from disk
        let page = self.read_page_raw(page_id)?;

        // Add to cache
        if let Some(dirty_page) = self.cache.insert(page_id, page.clone()) {
            // Evicted page was dirty, need to write it back
            let evicted_id = dirty_page.page_id();
            self.write_page_raw(evicted_id, &dirty_page)?;
        }

        Ok(page)
    }

    /// Read a page without verifying checksum (for encrypted pages)
    ///
    /// Use this when the page content has its own integrity protection
    /// (e.g., AES-GCM authentication tag for encrypted pages).
    pub fn read_page_no_checksum(&self, page_id: u32) -> Result<Page, PagerError> {
        // Check cache first
        if let Some(page) = self.cache.get(page_id) {
            return Ok(page);
        }

        // Cache miss - read from disk (skip checksum verification)
        let mut file = self.file.lock().unwrap();
        let offset = (page_id as u64) * (PAGE_SIZE as u64);

        file.seek(SeekFrom::Start(offset))?;

        let mut buf = [0u8; PAGE_SIZE];
        file.read_exact(&mut buf)?;
        drop(file);

        let page = Page::from_bytes(buf);

        // Add to cache (no checksum verification)
        if let Some(dirty_page) = self.cache.insert(page_id, page.clone()) {
            // Evicted page was dirty, need to write it back
            let evicted_id = dirty_page.page_id();
            self.write_page_raw(evicted_id, &dirty_page)?;
        }

        Ok(page)
    }

    /// Write a page (cache-aware)
    pub fn write_page(&self, page_id: u32, mut page: Page) -> Result<(), PagerError> {
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }

        // Update checksum
        page.update_checksum();

        // Add to cache and mark dirty
        self.cache.insert(page_id, page);
        self.cache.mark_dirty(page_id);

        Ok(())
    }

    /// Write a page without updating checksum (for encrypted pages)
    ///
    /// Use this when the page content has its own integrity protection
    /// (e.g., AES-GCM authentication tag for encrypted pages).
    pub fn write_page_no_checksum(&self, page_id: u32, page: Page) -> Result<(), PagerError> {
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }

        // Add to cache and mark dirty (no checksum update)
        self.cache.insert(page_id, page);
        self.cache.mark_dirty(page_id);

        Ok(())
    }

    /// Allocate a new page
    pub fn allocate_page(&self, page_type: PageType) -> Result<Page, PagerError> {
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }

        // Try to get from freelist first
        let page_id = {
            let mut freelist = self.freelist.write().unwrap();
            if let Some(id) = freelist.allocate() {
                id
            } else if freelist.trunk_head() != 0 {
                let trunk_id = freelist.trunk_head();
                drop(freelist);

                let trunk = self.read_page(trunk_id).map_err(|e| match e {
                    PagerError::PageNotFound(_) => {
                        PagerError::InvalidDatabase("Freelist trunk missing".to_string())
                    }
                    other => other,
                })?;

                let mut freelist = self.freelist.write().unwrap();
                freelist
                    .load_from_trunk(&trunk)
                    .map_err(|e| PagerError::InvalidDatabase(format!("Freelist: {}", e)))?;
                let id = freelist.allocate().ok_or_else(|| {
                    PagerError::InvalidDatabase("Freelist empty after trunk load".to_string())
                })?;

                let mut header = self.header.write().unwrap();
                header.freelist_head = freelist.trunk_head();
                *self.header_dirty.lock().unwrap() = true;

                id
            } else {
                // No free pages, extend file
                let mut header = self.header.write().unwrap();
                let id = header.page_count;
                header.page_count += 1;
                *self.header_dirty.lock().unwrap() = true;
                id
            }
        };

        let page = Page::new(page_type, page_id);

        // Write to cache
        self.cache.insert(page_id, page.clone());
        self.cache.mark_dirty(page_id);

        Ok(page)
    }

    /// Free a page (return to freelist)
    pub fn free_page(&self, page_id: u32) -> Result<(), PagerError> {
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }

        // Remove from cache
        self.cache.remove(page_id);

        // Add to freelist
        let mut freelist = self.freelist.write().unwrap();
        freelist.free(page_id);

        *self.header_dirty.lock().unwrap() = true;

        Ok(())
    }

    /// Get database header
    pub fn header(&self) -> DatabaseHeader {
        self.header.read().unwrap().clone()
    }

    /// Get page count
    pub fn page_count(&self) -> u32 {
        self.header.read().unwrap().page_count
    }

    /// Flush all dirty pages to disk
    pub fn flush(&self) -> Result<(), PagerError> {
        if self.config.read_only {
            return Ok(());
        }

        // Persist freelist to trunk pages when dirty
        let trunks = {
            let mut freelist = self.freelist.write().unwrap();
            if freelist.is_dirty() {
                let mut header = self.header.write().unwrap();
                let trunks = freelist.flush_to_trunks(0, || {
                    let id = header.page_count;
                    header.page_count += 1;
                    id
                });
                header.freelist_head = freelist.trunk_head();
                *self.header_dirty.lock().unwrap() = true;
                freelist.mark_clean();
                trunks
            } else {
                Vec::new()
            }
        };

        for trunk in trunks {
            let page_id = trunk.page_id();
            self.cache.insert(page_id, trunk);
            self.cache.mark_dirty(page_id);
        }

        // Flush dirty pages from cache
        let dirty_pages = self.cache.flush_dirty();
        for (page_id, page) in dirty_pages {
            self.write_page_raw(page_id, &page)?;
        }

        // Write header if dirty
        if *self.header_dirty.lock().unwrap() {
            self.write_header()?;
        }

        Ok(())
    }

    /// Sync file to disk (fsync)
    pub fn sync(&self) -> Result<(), PagerError> {
        self.flush()?;

        let file = self.file.lock().unwrap();
        file.sync_all()?;

        Ok(())
    }

    /// Get cache statistics
    pub fn cache_stats(&self) -> super::page_cache::CacheStats {
        self.cache.stats()
    }

    /// Get database file path
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Check if database is read-only
    pub fn is_read_only(&self) -> bool {
        self.config.read_only
    }

    /// Get file size in bytes
    pub fn file_size(&self) -> Result<u64, PagerError> {
        let file = self.file.lock().unwrap();
        Ok(file.metadata()?.len())
    }
}

impl Drop for Pager {
    fn drop(&mut self) {
        // Try to flush on drop
        let _ = self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_db_path() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!("reddb_test_{}_{}.db", std::process::id(), id));
        path
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_pager_create_new() {
        let path = temp_db_path();
        cleanup(&path);

        {
            let pager = Pager::open_default(&path).unwrap();
            assert_eq!(pager.page_count(), 1); // Just header
        }

        cleanup(&path);
    }

    #[test]
    fn test_pager_reopen() {
        let path = temp_db_path();
        cleanup(&path);

        // Create and write
        {
            let pager = Pager::open_default(&path).unwrap();

            // Allocate a page
            let page = pager.allocate_page(PageType::BTreeLeaf).unwrap();
            assert_eq!(page.page_id(), 1);

            pager.sync().unwrap();
        }

        // Reopen and verify
        {
            let pager = Pager::open_default(&path).unwrap();
            assert_eq!(pager.page_count(), 2); // Header + 1 data page
        }

        cleanup(&path);
    }

    #[test]
    fn test_pager_read_write() {
        let path = temp_db_path();
        cleanup(&path);

        {
            let pager = Pager::open_default(&path).unwrap();

            // Allocate and write
            let mut page = pager.allocate_page(PageType::BTreeLeaf).unwrap();
            let page_id = page.page_id();

            page.insert_cell(b"key", b"value").unwrap();
            pager.write_page(page_id, page).unwrap();

            // Read back
            let read_page = pager.read_page(page_id).unwrap();
            let (key, value) = read_page.read_cell(0).unwrap();
            assert_eq!(key, b"key");
            assert_eq!(value, b"value");
        }

        cleanup(&path);
    }

    #[test]
    fn test_pager_cache() {
        let path = temp_db_path();
        cleanup(&path);

        {
            let pager = Pager::open_default(&path).unwrap();

            // Allocate a page
            let page = pager.allocate_page(PageType::BTreeLeaf).unwrap();
            let page_id = page.page_id();

            // First read - should be cached from allocate
            let _ = pager.read_page(page_id).unwrap();

            // Second read - should hit cache
            let _ = pager.read_page(page_id).unwrap();

            let stats = pager.cache_stats();
            assert!(stats.hits >= 1);
        }

        cleanup(&path);
    }

    #[test]
    fn test_pager_free_page() {
        let path = temp_db_path();
        cleanup(&path);

        {
            let pager = Pager::open_default(&path).unwrap();

            // Allocate pages
            let page1 = pager.allocate_page(PageType::BTreeLeaf).unwrap();
            let page2 = pager.allocate_page(PageType::BTreeLeaf).unwrap();

            let id1 = page1.page_id();
            let id2 = page2.page_id();

            // Free page 1
            pager.free_page(id1).unwrap();

            // Next allocation should reuse page 1
            let page3 = pager.allocate_page(PageType::BTreeLeaf).unwrap();
            assert_eq!(page3.page_id(), id1);
        }

        cleanup(&path);
    }

    #[test]
    fn test_freelist_persistence() {
        let path = temp_db_path();
        cleanup(&path);

        let freed_id;
        {
            let pager = Pager::open_default(&path).unwrap();
            let page1 = pager.allocate_page(PageType::BTreeLeaf).unwrap();
            let _page2 = pager.allocate_page(PageType::BTreeLeaf).unwrap();
            freed_id = page1.page_id();

            pager.free_page(freed_id).unwrap();
            pager.sync().unwrap();
        }

        {
            let pager = Pager::open_default(&path).unwrap();
            let page = pager.allocate_page(PageType::BTreeLeaf).unwrap();
            assert_eq!(page.page_id(), freed_id);
        }

        cleanup(&path);
    }

    #[test]
    fn test_pager_read_only() {
        let path = temp_db_path();
        cleanup(&path);

        // Create database
        {
            let pager = Pager::open_default(&path).unwrap();
            pager.sync().unwrap();
        }

        // Open read-only
        {
            let config = PagerConfig {
                read_only: true,
                ..Default::default()
            };

            let pager = Pager::open(&path, config).unwrap();
            assert!(pager.is_read_only());

            // Should fail to allocate
            assert!(pager.allocate_page(PageType::BTreeLeaf).is_err());
        }

        cleanup(&path);
    }
}
