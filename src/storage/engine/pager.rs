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
use fs2::FileExt;
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
    /// A Mutex or RwLock was poisoned (another thread panicked while holding it)
    LockPoisoned,
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
            Self::LockPoisoned => write!(f, "Internal lock poisoned (concurrent thread panicked)"),
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
    /// Enable double-write buffer for torn page protection
    pub double_write: bool,
}

impl Default for PagerConfig {
    fn default() -> Self {
        Self {
            cache_size: DEFAULT_CACHE_SIZE,
            read_only: false,
            create: true,
            verify_checksums: true,
            double_write: true,
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
    /// Whether a checkpoint is currently in progress (two-phase)
    pub checkpoint_in_progress: bool,
    /// Target LSN for the in-progress checkpoint
    pub checkpoint_target_lsn: u64,
    /// Physical layout header mirrored into page 0
    pub physical: PhysicalFileHeader,
}

/// Minimal physical state published into page 0 for paged databases.
#[derive(Debug, Clone, Copy, Default)]
pub struct PhysicalFileHeader {
    pub format_version: u32,
    pub sequence: u64,
    pub manifest_oldest_root: u64,
    pub manifest_root: u64,
    pub free_set_root: u64,
    pub manifest_page: u32,
    pub manifest_checksum: u64,
    pub collection_roots_page: u32,
    pub collection_roots_checksum: u64,
    pub collection_root_count: u32,
    pub snapshot_count: u32,
    pub index_count: u32,
    pub catalog_collection_count: u32,
    pub catalog_total_entities: u64,
    pub export_count: u32,
    pub graph_projection_count: u32,
    pub analytics_job_count: u32,
    pub manifest_event_count: u32,
    pub registry_page: u32,
    pub registry_checksum: u64,
    pub recovery_page: u32,
    pub recovery_checksum: u64,
    pub catalog_page: u32,
    pub catalog_checksum: u64,
    pub metadata_state_page: u32,
    pub metadata_state_checksum: u64,
    pub vector_artifact_page: u32,
    pub vector_artifact_checksum: u64,
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
            checkpoint_in_progress: false,
            checkpoint_target_lsn: 0,
            physical: PhysicalFileHeader::default(),
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
    /// Exclusive file lock (held for lifetime, released on drop)
    _lock_file: Option<File>,
    /// Double-write buffer file (.rdb-dwb)
    dwb_file: Option<Mutex<File>>,
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

#[path = "pager/impl.rs"]
mod pager_impl;
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
    use std::io::Write;

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
        // Clean up companion files
        let mut hdr = path.to_path_buf().into_os_string();
        hdr.push("-hdr");
        let _ = fs::remove_file(&hdr);
        let mut meta = path.to_path_buf().into_os_string();
        meta.push("-meta");
        let _ = fs::remove_file(&meta);
        let mut dwb = path.to_path_buf().into_os_string();
        dwb.push("-dwb");
        let _ = fs::remove_file(&dwb);
    }

    fn dwb_path_for(path: &Path) -> PathBuf {
        let mut dwb = path.to_path_buf().into_os_string();
        dwb.push("-dwb");
        PathBuf::from(dwb)
    }

    fn write_dwb_fixture(path: &Path, pages: &[(u32, Page)]) {
        let entry_size = 4 + PAGE_SIZE;
        let header_len = 12;
        let total = header_len + pages.len() * entry_size;
        let mut buf = Vec::with_capacity(total);

        buf.extend_from_slice(&[0x52, 0x44, 0x44, 0x57]); // "RDDW"
        buf.extend_from_slice(&(pages.len() as u32).to_le_bytes());
        buf.extend_from_slice(&[0u8; 4]);

        for (page_id, page) in pages {
            let mut page = page.clone();
            page.update_checksum();
            buf.extend_from_slice(&page_id.to_le_bytes());
            buf.extend_from_slice(page.as_bytes());
        }

        let checksum = crate::storage::engine::crc32::crc32(&buf[header_len..]);
        buf[8..12].copy_from_slice(&checksum.to_le_bytes());

        let dwb_path = dwb_path_for(path);
        let mut file = fs::File::create(&dwb_path).unwrap();
        file.write_all(&buf).unwrap();
        file.sync_all().unwrap();
    }

    #[test]
    fn test_pager_create_new() {
        let path = temp_db_path();
        cleanup(&path);

        {
            let pager = Pager::open_default(&path).unwrap();
            assert_eq!(pager.page_count().unwrap(), 1); // Just header
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
            assert_eq!(pager.page_count().unwrap(), 2); // Header + 1 data page
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

    #[test]
    fn test_dwb_recovery_clears_in_place_and_keeps_file_reusable() {
        let path = temp_db_path();
        cleanup(&path);

        let config = PagerConfig {
            double_write: true,
            ..Default::default()
        };

        let page_id;
        {
            let pager = Pager::open(&path, config.clone()).unwrap();
            let page = pager.allocate_page(PageType::BTreeLeaf).unwrap();
            page_id = page.page_id();
            pager.sync().unwrap();
        }

        let mut recovered_page = Page::new(PageType::BTreeLeaf, page_id);
        recovered_page.insert_cell(b"key", b"value").unwrap();
        write_dwb_fixture(&path, &[(page_id, recovered_page.clone())]);

        let dwb_path = dwb_path_for(&path);
        assert!(dwb_path.exists());
        assert!(fs::metadata(&dwb_path).unwrap().len() > 0);

        {
            let pager = Pager::open(&path, config).unwrap();

            let read_page = pager.read_page(page_id).unwrap();
            let (key, value) = read_page.read_cell(0).unwrap();
            assert_eq!(key, b"key");
            assert_eq!(value, b"value");

            assert!(dwb_path.exists());
            assert_eq!(fs::metadata(&dwb_path).unwrap().len(), 0);

            let mut updated_page = recovered_page.clone();
            updated_page.insert_cell(b"key2", b"value2").unwrap();
            pager.write_page(page_id, updated_page).unwrap();
            pager.flush().unwrap();

            assert!(dwb_path.exists());
            assert_eq!(fs::metadata(&dwb_path).unwrap().len(), 0);
        }

        cleanup(&path);
    }
}
