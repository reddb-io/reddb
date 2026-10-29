//! Pager - Page I/O Manager
//!
//! The Pager is responsible for reading and writing pages to/from disk.
//! It integrates with the PageCache for efficient caching and the FreeList
//! for page allocation.
//!
//! # Responsibilities
//!
//! 1. **Page I/O**: Read/write 16 KiB pages from/to disk
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

#[cfg(test)]
use super::page::PageType;
use super::page::{Page, PAGE_SIZE};
use crate::storage::wal::writer::WalWriter;
use fs2::FileExt;
pub use reddb_engine::storage::pager::{DatabaseHeader, ExtentId, PagerError, PhysicalFileHeader};
use reddb_engine::storage::pager::{
    Pager as SharedPager, PagerConfig as SharedPagerConfig, WalBarrier,
};
#[cfg(test)]
use reddb_engine::storage::pager_io::{
    DWB_ZONE_BYTES, DWB_ZONE_START_PAGE, FIRST_ALLOCATABLE_PAGE,
};
use std::fs::{File, OpenOptions};
#[cfg(test)]
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, RwLock};
const DEFAULT_CACHE_SIZE: usize = 10_000;
#[cfg(test)]
static COW_ATOMIC_WRITE_TEST_OVERRIDE: AtomicU8 = AtomicU8::new(0);

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
    /// Optional encryption key. When set, `Pager::open` writes/reads
    /// pages through `PageEncryptor` and rejects any DB whose
    /// encryption-marker disagrees with the supplied key (or its
    /// absence). When `None`, the pager refuses to open a DB whose
    /// header carries the `RDBE` encryption marker.
    pub encryption: Option<crate::storage::encryption::SecureKey>,
}

impl Default for PagerConfig {
    fn default() -> Self {
        Self {
            cache_size: DEFAULT_CACHE_SIZE,
            read_only: false,
            create: true,
            verify_checksums: true,
            double_write: true,
            encryption: None,
        }
    }
}

/// Native host adapter for the shared pager.
pub struct Pager {
    core: SharedPager<NativePagerFile, NativeWalBarrier>,
    _lock_file: Option<File>,
    config: PagerConfig,
    pub(crate) encryption: Option<(
        crate::storage::encryption::PageEncryptor,
        crate::storage::encryption::EncryptionHeader,
    )>,
}

#[doc(hidden)]
pub struct NativePagerFile(File);

impl reddb_file::VfsFile for NativePagerFile {
    fn try_clone(&self) -> std::io::Result<Self> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
    fn file_len(&self) -> std::io::Result<u64> {
        Ok(self.0.metadata()?.len())
    }
    fn set_len(&self, len: u64) -> std::io::Result<()> {
        self.0.set_len(len)
    }
    fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        std::io::Write::write_all(&mut self.0, bytes)
    }
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(&mut self.0, bytes)
    }
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        std::io::Seek::seek(&mut self.0, pos)
    }
    fn sync_all(&self) -> std::io::Result<()> {
        self.0.sync_all()
    }
}

#[doc(hidden)]
#[derive(Default)]
pub struct NativeWalBarrier(RwLock<Option<Arc<Mutex<WalWriter>>>>);

impl WalBarrier for NativeWalBarrier {
    fn flush_until(&self, lsn: u64) -> std::io::Result<()> {
        let wal = self
            .0
            .read()
            .map_err(|_| std::io::Error::other("WAL slot lock poisoned"))?
            .clone();
        if let Some(wal) = wal {
            let mut writer = wal.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            writer.flush_until(lsn)?;
        } else if lsn != 0 {
            return Err(std::io::Error::other(
                "nonzero page LSN requires a WAL durability barrier",
            ));
        }
        Ok(())
    }
}

impl std::ops::Deref for Pager {
    type Target = SharedPager<NativePagerFile, NativeWalBarrier>;
    fn deref(&self) -> &Self::Target {
        &self.core
    }
}

impl reddb_engine::storage::page_store::PageStore for Pager {
    fn read_page(&self, id: u32) -> Result<Page, PagerError> {
        self.core.read_page(id)
    }
    fn write_page(&self, id: u32, page: Page) -> Result<(), PagerError> {
        self.core.write_page(id, page)
    }
    fn allocate_page(&self, kind: super::page::PageType) -> Result<Page, PagerError> {
        self.core.allocate_page(kind)
    }
    fn free_page(&self, id: u32) -> Result<(), PagerError> {
        self.core.free_page(id)
    }
    fn page_count(&self) -> Result<u32, PagerError> {
        self.core.page_count()
    }
    fn prefetch_hint(&self, id: u32) {
        self.prefetch_hint(id)
    }
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
    #[cfg(target_os = "linux")]
    use pager_impl::parse_mountinfo_options_for_path;
    use pager_impl::{
        classify_cow_filesystem, CowFilesystemKind, BTRFS_SUPER_MAGIC, FS_NOCOW_FL, ZFS_SUPER_MAGIC,
    };
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
        // Remove the data file AND every pager shadow sidecar via reddb-file's
        // authoritative path helpers (the pager writes only these three shadows
        // next to the data file). Deriving the paths through reddb-file keeps the
        // file-suffix contract out of the server source (enforced by reddb-file's
        // `server_source_does_not_embed_owned_file_suffixes`). Tests drop the
        // pager BEFORE calling this (every call site uses an inner block) so the
        // drop-time flush cannot re-create a sidecar after removal.
        let _ = fs::remove_file(path);
        for sidecar in reddb_file::layout::pager_shadow_sidecar_paths(path) {
            let _ = fs::remove_file(sidecar);
        }
    }

    fn dwb_path_for(path: &Path) -> PathBuf {
        reddb_file::layout::pager_dwb_shadow_path(path)
    }

    fn dwb_zone_offset_for_test() -> u64 {
        3 * PAGE_SIZE as u64
    }

    static COW_ATOMIC_WRITE_OVERRIDE_GUARD: Mutex<()> = Mutex::new(());

    struct CowAtomicWriteOverrideGuard {
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for CowAtomicWriteOverrideGuard {
        fn drop(&mut self) {
            COW_ATOMIC_WRITE_TEST_OVERRIDE.store(0, Ordering::Relaxed);
        }
    }

    fn cow_atomic_write_override(value: bool) -> CowAtomicWriteOverrideGuard {
        let guard = COW_ATOMIC_WRITE_OVERRIDE_GUARD
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        COW_ATOMIC_WRITE_TEST_OVERRIDE.store(if value { 1 } else { 2 }, Ordering::Relaxed);
        CowAtomicWriteOverrideGuard { _guard: guard }
    }

    fn write_dwb_fixture(path: &Path, pages: &[(u32, Page)]) {
        let pages: Vec<_> = pages
            .iter()
            .map(|(page_id, page)| {
                let mut page = page.clone();
                page.update_checksum();
                (*page_id, page)
            })
            .collect();
        let buf = reddb_file::encode_paged_dwb_frame(
            pages
                .iter()
                .map(|(page_id, page)| (*page_id, page.as_bytes())),
        );

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .expect("open() should succeed");
        file.seek(SeekFrom::Start(dwb_zone_offset_for_test()))
            .expect("seek() should succeed");
        file.write_all(&buf).expect("write_all() should succeed");
        file.sync_all().expect("sync_all() should succeed");
    }

    fn write_page_bytes(path: &Path, page_id: u32, page: &Page) {
        let mut file = OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open() should succeed");
        file.seek(SeekFrom::Start(page_id as u64 * PAGE_SIZE as u64))
            .expect("value is present");
        file.write_all(page.as_bytes())
            .expect("write_all() should succeed");
        file.sync_all().expect("sync_all() should succeed");
    }

    fn write_torn_page_bytes(path: &Path, page_id: u32, before: &Page, after: &Page) {
        let mut torn = *before.as_bytes();
        torn[..PAGE_SIZE / 2].copy_from_slice(&after.as_bytes()[..PAGE_SIZE / 2]);

        let mut file = OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open() should succeed");
        file.seek(SeekFrom::Start(page_id as u64 * PAGE_SIZE as u64))
            .expect("value is present");
        file.write_all(&torn).expect("write_all() should succeed");
        file.sync_all().expect("sync_all() should succeed");
    }

    #[test]
    fn test_pager_create_new() {
        let path = temp_db_path();
        cleanup(&path);

        {
            let pager = Pager::open_default(&path).expect("open_default() should succeed");
            assert_eq!(
                pager.page_count().expect("page_count() should succeed"),
                FIRST_ALLOCATABLE_PAGE
            );
            // Header + reserved metadata pages + DWB zone
        }

        cleanup(&path);
    }

    #[test]
    fn test_pager_reopen() {
        let path = temp_db_path();
        cleanup(&path);

        // Create and write
        {
            let pager = Pager::open_default(&path).expect("open_default() should succeed");

            // Allocate a page
            let page = pager
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate_page() should succeed");
            assert_eq!(page.page_id(), FIRST_ALLOCATABLE_PAGE);

            pager.sync().expect("sync() should succeed");
        }

        // Reopen and verify
        {
            let pager = Pager::open_default(&path).expect("open_default() should succeed");
            assert_eq!(
                pager.page_count().expect("page_count() should succeed"),
                FIRST_ALLOCATABLE_PAGE + 1
            );
            // Header + reserved metadata pages + DWB zone + 1 data page
        }

        cleanup(&path);
    }

    #[test]
    fn test_pager_read_write() {
        let path = temp_db_path();
        cleanup(&path);

        {
            let pager = Pager::open_default(&path).expect("open_default() should succeed");

            // Allocate and write
            let mut page = pager
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate_page() should succeed");
            let page_id = page.page_id();

            page.insert_cell(b"key", b"value")
                .expect("insert_cell() should succeed");
            pager
                .write_page(page_id, page)
                .expect("write_page() should succeed");

            // Read back
            let read_page = pager
                .read_page(page_id)
                .expect("read_page() should succeed");
            let (key, value) = read_page.read_cell(0).expect("read_cell() should succeed");
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
            let pager = Pager::open_default(&path).expect("open_default() should succeed");

            // Allocate a page
            let page = pager
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate_page() should succeed");
            let page_id = page.page_id();

            // First read - should be cached from allocate
            let _ = pager
                .read_page(page_id)
                .expect("read_page() should succeed");

            // Second read - should hit cache
            let _ = pager
                .read_page(page_id)
                .expect("read_page() should succeed");

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
            let pager = Pager::open_default(&path).expect("open_default() should succeed");

            // Allocate pages
            let page1 = pager
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate_page() should succeed");
            let page2 = pager
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate_page() should succeed");

            let id1 = page1.page_id();
            let id2 = page2.page_id();

            // Free page 1
            pager.free_page(id1).expect("free_page() should succeed");

            // Next allocation should reuse page 1
            let page3 = pager
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate_page() should succeed");
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
            let pager = Pager::open_default(&path).expect("open_default() should succeed");
            let page1 = pager
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate_page() should succeed");
            let _page2 = pager
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate_page() should succeed");
            freed_id = page1.page_id();

            pager
                .free_page(freed_id)
                .expect("free_page() should succeed");
            pager.sync().expect("sync() should succeed");
        }

        {
            let pager = Pager::open_default(&path).expect("open_default() should succeed");
            let page = pager
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate_page() should succeed");
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
            let pager = Pager::open_default(&path).expect("open_default() should succeed");
            pager.sync().expect("sync() should succeed");
        }

        // Open read-only
        {
            let config = PagerConfig {
                read_only: true,
                ..Default::default()
            };

            let pager = Pager::open(&path, config).expect("open() should succeed");
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
            let pager = Pager::open(&path, config.clone()).expect("open() should succeed");
            let page = pager
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate_page() should succeed");
            page_id = page.page_id();
            pager.sync().expect("sync() should succeed");
        }

        let mut recovered_page = Page::new(PageType::BTreeLeaf, page_id);
        recovered_page
            .insert_cell(b"key", b"value")
            .expect("insert_cell() should succeed");
        write_dwb_fixture(&path, &[(page_id, recovered_page.clone())]);

        assert!(!dwb_path_for(&path).exists());

        {
            let pager = Pager::open(&path, config).expect("open() should succeed");

            let read_page = pager
                .read_page(page_id)
                .expect("read_page() should succeed");
            let (key, value) = read_page.read_cell(0).expect("read_cell() should succeed");
            assert_eq!(key, b"key");
            assert_eq!(value, b"value");

            assert!(!dwb_path_for(&path).exists());

            let mut updated_page = recovered_page.clone();
            updated_page
                .insert_cell(b"key2", b"value2")
                .expect("insert_cell() should succeed");
            pager
                .write_page(page_id, updated_page)
                .expect("write_page() should succeed");
            pager.flush().expect("flush() should succeed");

            assert!(!dwb_path_for(&path).exists());
        }

        cleanup(&path);
    }

    #[test]
    fn retired_dwb_sidecar_routes_through_offline_migration() {
        let path = temp_db_path();
        cleanup(&path);

        {
            let pager = Pager::open_default(&path).expect("open_default() should succeed");
            pager.sync().expect("sync() should succeed");
        }

        let dwb = dwb_path_for(&path);
        fs::write(&dwb, b"legacy dwb sidecar").expect("write legacy sidecar");

        let err = match Pager::open_default(&path) {
            Ok(_) => panic!("retired DWB sidecar must refuse open"),
            Err(err) => err,
        };
        assert!(matches!(err, PagerError::LegacySidecarStore { .. }));

        cleanup(&path);
    }

    #[test]
    fn interrupted_dwb_staging_write_is_discarded_on_open() {
        let path = temp_db_path();
        cleanup(&path);

        {
            let pager = Pager::open_default(&path).expect("open_default() should succeed");
            pager.sync().expect("sync() should succeed");
        }

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open() should succeed");
        file.seek(SeekFrom::Start(dwb_zone_offset_for_test()))
            .expect("seek() should succeed");
        file.write_all(b"RDDW")
            .expect("write corrupt DWB magic should succeed");
        file.sync_all().expect("sync_all() should succeed");

        {
            let pager = Pager::open_default(&path).expect("torn staging frame should be discarded");
            pager.sync().expect("sync() should succeed");
        }

        let mut cleared = [0u8; 4];
        let mut file = OpenOptions::new()
            .read(true)
            .open(&path)
            .expect("open() should succeed");
        file.seek(SeekFrom::Start(dwb_zone_offset_for_test()))
            .expect("seek() should succeed");
        file.read_exact(&mut cleared)
            .expect("read_exact() should succeed");
        assert_ne!(cleared, reddb_file::DWB_MAGIC);

        cleanup(&path);
    }

    #[test]
    fn cow_probe_classification_fails_closed_for_btrfs_nodatacow() {
        assert_eq!(
            classify_cow_filesystem(ZFS_SUPER_MAGIC, None, None),
            Some(CowFilesystemKind::Zfs),
            "ZFS is always CoW"
        );
        assert_eq!(
            classify_cow_filesystem(BTRFS_SUPER_MAGIC, Some("rw,relatime"), Some(0)),
            Some(CowFilesystemKind::BtrfsDataCow),
            "btrfs qualifies only when datacow remains enabled"
        );
        assert_eq!(
            classify_cow_filesystem(BTRFS_SUPER_MAGIC, Some("rw,nodatacow"), Some(0)),
            None,
            "btrfs nodatacow mount option must reject DWB skip"
        );
        assert_eq!(
            classify_cow_filesystem(BTRFS_SUPER_MAGIC, Some("rw"), Some(FS_NOCOW_FL)),
            None,
            "btrfs chattr +C / NOCOW inode flag must reject DWB skip"
        );
        assert_eq!(
            classify_cow_filesystem(BTRFS_SUPER_MAGIC, Some("rw"), None),
            None,
            "missing btrfs inode flags are uncertain and must fail closed"
        );
        assert_eq!(
            classify_cow_filesystem(BTRFS_SUPER_MAGIC, None, Some(0)),
            None,
            "missing btrfs mount options are uncertain and must fail closed"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mountinfo_parser_uses_longest_cow_mount_and_rejects_nodatacow() {
        let mountinfo = "\
24 18 0:21 / / rw,relatime - ext4 /dev/root rw\n\
35 24 0:42 /subvol /mnt/reddb rw,relatime - btrfs /dev/sdb rw,space_cache=v2\n\
36 35 0:43 /nocow /mnt/reddb/nocow rw,relatime - btrfs /dev/sdb rw,nodatacow\n\
";

        assert_eq!(
            parse_mountinfo_options_for_path(mountinfo, Path::new("/mnt/reddb/data.rdb"))
                .as_deref(),
            Some("rw,relatime,rw,space_cache=v2")
        );
        assert_eq!(
            parse_mountinfo_options_for_path(mountinfo, Path::new("/mnt/reddb/nocow/data.rdb"))
                .as_deref(),
            Some("rw,relatime,rw,nodatacow")
        );
    }

    #[test]
    fn double_write_false_keeps_dwb_when_cow_probe_denies() {
        let _override = cow_atomic_write_override(false);
        let path = temp_db_path();
        cleanup(&path);

        {
            let config = PagerConfig {
                double_write: false,
                ..Default::default()
            };
            let pager = Pager::open(&path, config).expect("open() should succeed");
            let page = pager
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate_page() should succeed");
            pager
                .write_page(page.page_id(), page)
                .expect("write_page() should succeed");
            pager.flush().expect("flush() should succeed");
        }

        assert!(
            !dwb_path_for(&path).exists(),
            "DWB must stay enabled in-file when double_write=false is not proven safe"
        );

        cleanup(&path);
    }

    #[test]
    fn double_write_false_skips_dwb_when_cow_probe_allows() {
        let _override = cow_atomic_write_override(true);
        let path = temp_db_path();
        cleanup(&path);

        {
            let config = PagerConfig {
                double_write: false,
                ..Default::default()
            };
            let pager = Pager::open(&path, config).expect("open() should succeed");
            let page = pager
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate_page() should succeed");
            pager
                .write_page(page.page_id(), page)
                .expect("write_page() should succeed");
            pager.flush().expect("flush() should succeed");
        }

        assert!(
            !dwb_path_for(&path).exists(),
            "DWB may be skipped only after the CoW probe allows it"
        );

        cleanup(&path);
    }

    #[test]
    fn double_write_false_on_cow_recovers_from_in_file_dwb_zone() {
        let _override = cow_atomic_write_override(true);
        let path = temp_db_path();
        cleanup(&path);

        let page_id;
        {
            let pager = Pager::open(&path, PagerConfig::default()).expect("open() should succeed");
            let page = pager
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate_page() should succeed");
            page_id = page.page_id();
            pager.sync().expect("sync() should succeed");
        }

        let mut recovered_page = Page::new(PageType::BTreeLeaf, page_id);
        recovered_page
            .insert_cell(b"key", b"value")
            .expect("insert_cell() should succeed");
        write_dwb_fixture(&path, &[(page_id, recovered_page)]);

        {
            let config = PagerConfig {
                double_write: false,
                ..Default::default()
            };
            let pager = Pager::open(&path, config).expect("open() should succeed");
            let read_page = pager
                .read_page(page_id)
                .expect("read_page() should succeed");
            let (key, value) = read_page.read_cell(0).expect("read_cell() should succeed");
            assert_eq!(key, b"key");
            assert_eq!(value, b"value");
        }

        assert!(!dwb_path_for(&path).exists());

        cleanup(&path);
    }

    #[test]
    fn simulated_cow_mid_write_leaves_a_whole_consistent_page_without_dwb() {
        let _override = cow_atomic_write_override(true);
        let path = temp_db_path();
        cleanup(&path);

        let config = PagerConfig {
            double_write: false,
            ..Default::default()
        };

        let page_id;
        let before;
        let after;
        {
            let pager = Pager::open(&path, config.clone()).expect("open() should succeed");
            let mut page = pager
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate_page() should succeed");
            page_id = page.page_id();
            page.insert_cell(b"phase", b"before")
                .expect("insert_cell() should succeed");
            pager
                .write_page(page_id, page)
                .expect("write_page() should succeed");
            pager.sync().expect("sync() should succeed");
            before = pager
                .read_page(page_id)
                .expect("read_page() should succeed");

            let mut page = before.clone();
            page.insert_cell(b"phase2", b"after")
                .expect("insert_cell() should succeed");
            pager
                .write_page(page_id, page)
                .expect("write_page() should succeed");
            pager.flush().expect("flush() should succeed");
            after = pager
                .read_page(page_id)
                .expect("read_page() should succeed");
        }

        // CoW crash model: the interrupted write leaves either the old full
        // page or the new full page, never a torn mix. Exercise both outcomes.
        for (whole_page, expected_cells) in [(&before, 1), (&after, 2)] {
            write_page_bytes(&path, page_id, whole_page);

            let pager = Pager::open(&path, config.clone()).expect("open() should succeed");
            let recovered = pager
                .read_page(page_id)
                .expect("read_page() should succeed");
            assert_eq!(recovered.cell_count(), expected_cells);
            let (key, value) = recovered.read_cell(0).expect("read_cell() should succeed");
            assert_eq!(key, b"phase");
            assert_eq!(value, b"before");
            if expected_cells == 2 {
                let (key, value) = recovered.read_cell(1).expect("read_cell() should succeed");
                assert_eq!(key, b"phase2");
                assert_eq!(value, b"after");
            }
            drop(pager);
        }

        cleanup(&path);
    }

    #[test]
    fn same_mid_write_without_cow_recovers_from_dwb() {
        let _override = cow_atomic_write_override(false);
        let path = temp_db_path();
        cleanup(&path);

        let config = PagerConfig {
            double_write: false,
            ..Default::default()
        };

        let page_id;
        let before;
        let after;
        {
            let pager = Pager::open(&path, config.clone()).expect("open() should succeed");
            let mut page = pager
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate_page() should succeed");
            page_id = page.page_id();
            page.insert_cell(b"phase", b"before")
                .expect("insert_cell() should succeed");
            pager
                .write_page(page_id, page)
                .expect("write_page() should succeed");
            pager.sync().expect("sync() should succeed");
            before = pager
                .read_page(page_id)
                .expect("read_page() should succeed");

            let mut page = before.clone();
            page.insert_cell(b"phase2", b"after")
                .expect("insert_cell() should succeed");
            pager
                .write_page(page_id, page)
                .expect("write_page() should succeed");
            pager.flush().expect("flush() should succeed");
            after = pager
                .read_page(page_id)
                .expect("read_page() should succeed");
        }

        write_dwb_fixture(&path, &[(page_id, after.clone())]);
        write_torn_page_bytes(&path, page_id, &before, &after);

        {
            let pager = Pager::open(&path, config).expect("open() should succeed");
            let recovered = pager
                .read_page(page_id)
                .expect("read_page() should succeed");
            assert_eq!(recovered.cell_count(), 2);

            let (key, value) = recovered.read_cell(0).expect("read_cell() should succeed");
            assert_eq!(key, b"phase");
            assert_eq!(value, b"before");

            let (key, value) = recovered.read_cell(1).expect("read_cell() should succeed");
            assert_eq!(key, b"phase2");
            assert_eq!(value, b"after");
        }

        assert!(!dwb_path_for(&path).exists());
        cleanup(&path);
    }

    // -----------------------------------------------------------------
    // Target 3: WAL-first flush ordering
    // -----------------------------------------------------------------

    #[test]
    fn pager_starts_without_wal_writer() {
        let path = temp_db_path();
        let pager = Pager::open(&path, PagerConfig::default()).expect("open() should succeed");
        assert!(!pager.has_wal_writer());
        drop(pager);
        cleanup(&path);
    }

    #[test]
    fn flush_refuses_nonzero_lsn_without_a_wal_writer() {
        let path = temp_db_path();
        let pager = Pager::open(&path, PagerConfig::default()).expect("open pager");
        let mut page = pager
            .allocate_page(PageType::BTreeLeaf)
            .expect("allocate page");
        page.set_lsn(42);
        pager.write_page(page.page_id(), page).expect("write page");

        assert!(
            pager.flush().is_err(),
            "a page with an LSN needs a WAL barrier"
        );
        assert!(
            pager.dirty_page_count() > 0,
            "failed flush must retain dirty pages"
        );
        assert!(
            pager.flush().is_err(),
            "failed writeback must remain fenced"
        );
        drop(pager);
        cleanup(&path);
    }

    #[test]
    fn set_wal_writer_attaches_handle() {
        use crate::storage::wal::writer::WalWriter;
        use std::sync::{Arc, Mutex};

        let db_path = temp_db_path();
        let wal_path = reddb_file::layout::pager_legacy_wal_path(&db_path);
        let _ = fs::remove_file(&wal_path);

        let pager = Pager::open(&db_path, PagerConfig::default()).expect("open() should succeed");
        let wal = Arc::new(Mutex::new(
            WalWriter::open(&wal_path).expect("open() should succeed"),
        ));
        pager.set_wal_writer(Arc::clone(&wal));
        assert!(pager.has_wal_writer());

        pager.clear_wal_writer();
        assert!(!pager.has_wal_writer());

        drop(pager);
        let _ = fs::remove_file(&wal_path);
        cleanup(&db_path);
    }

    #[test]
    fn flush_with_lsn_zero_pages_skips_wal_call() {
        // When every dirty page has lsn == 0 (the legacy auto-commit
        // path), flush() must NOT call wal.flush_until — there is no
        // WAL record to wait for. We verify this by attaching a WAL
        // whose durable_lsn starts at 8 and confirming flush() does
        // not advance it (no append, no flush).
        use crate::storage::wal::writer::WalWriter;
        use std::sync::{Arc, Mutex};

        let db_path = temp_db_path();
        let wal_path = reddb_file::layout::pager_legacy_wal_path(&db_path);
        let _ = fs::remove_file(&wal_path);

        let pager = Pager::open(&db_path, PagerConfig::default()).expect("open() should succeed");
        let wal = Arc::new(Mutex::new(
            WalWriter::open(&wal_path).expect("open() should succeed"),
        ));
        let initial_durable = {
            let g = wal.lock().expect("lock() should succeed");
            g.durable_lsn()
        };
        pager.set_wal_writer(Arc::clone(&wal));

        // Allocate and write a page with lsn = 0.
        let mut page = pager
            .allocate_page(PageType::BTreeLeaf)
            .expect("allocate_page() should succeed");
        page.insert_cell(b"k", b"v")
            .expect("insert_cell() should succeed");
        // header.lsn stays at 0 — caller did not stamp.
        pager
            .write_page(page.page_id(), page)
            .expect("write_page() should succeed");
        pager.flush().expect("flush() should succeed");

        // WAL durable_lsn must be unchanged because flush_until was
        // never called (max lsn over dirty pages was 0).
        let after_flush = {
            let g = wal.lock().expect("lock() should succeed");
            g.durable_lsn()
        };
        assert_eq!(after_flush, initial_durable);

        drop(pager);
        let _ = fs::remove_file(&wal_path);
        cleanup(&db_path);
    }

    #[test]
    fn flush_advances_wal_durable_when_pages_carry_lsn() {
        // The full WAL-first dance: append a record, capture the
        // returned LSN, stamp it on a page, flush — afterwards the
        // WAL must be durable up to at least that LSN.
        use crate::storage::wal::record::WalRecord;
        use crate::storage::wal::writer::WalWriter;
        use std::sync::{Arc, Mutex};

        let db_path = temp_db_path();
        let wal_path = reddb_file::layout::pager_legacy_wal_path(&db_path);
        let _ = fs::remove_file(&wal_path);

        let pager = Pager::open(&db_path, PagerConfig::default()).expect("open() should succeed");
        let wal = Arc::new(Mutex::new(
            WalWriter::open(&wal_path).expect("open() should succeed"),
        ));
        pager.set_wal_writer(Arc::clone(&wal));

        // Stamp two dirty pages with a real WAL LSN.
        let stamped_lsn = {
            let mut wal_guard = wal.lock().expect("lock() should succeed");
            wal_guard
                .append(&WalRecord::Begin { tx_id: 1 })
                .expect("append() should succeed");
            wal_guard
                .append(&WalRecord::Commit { tx_id: 1 })
                .expect("append() should succeed");
            wal_guard.current_lsn()
        };
        let mut page = pager
            .allocate_page(PageType::BTreeLeaf)
            .expect("allocate_page() should succeed");
        page.insert_cell(b"k", b"v")
            .expect("insert_cell() should succeed");
        // Use the public Page API to set the LSN.
        page.set_lsn(stamped_lsn);
        pager
            .write_page(page.page_id(), page)
            .expect("write_page() should succeed");
        pager.flush().expect("flush() should succeed");

        // After flush, the WAL is durable at least up to our stamp.
        let after_flush = {
            let g = wal.lock().expect("lock() should succeed");
            g.durable_lsn()
        };
        assert!(
            after_flush >= stamped_lsn,
            "after flush durable_lsn {} must be >= stamped {}",
            after_flush,
            stamped_lsn
        );

        drop(pager);
        let _ = fs::remove_file(&wal_path);
        cleanup(&db_path);
    }

    // -----------------------------------------------------------------
    // gh-892: filesystem block-size alignment diagnostic
    // -----------------------------------------------------------------

    #[test]
    fn block_size_warn_fires_for_mismatched_block_size() {
        // A block size that does not divide the 16 KiB page size means a
        // page write straddles FS blocks — the predicate must report a
        // misalignment so `open()` emits the warning.
        assert!(Pager::page_size_misaligned_with_block(PAGE_SIZE, 6000));
        // Block larger than the page (e.g. 1 MiB): 16384 % 1048576 != 0.
        assert!(Pager::page_size_misaligned_with_block(PAGE_SIZE, 1_048_576));
        // 6 KiB also fails to divide 16 KiB.
        assert!(Pager::page_size_misaligned_with_block(PAGE_SIZE, 6 * 1024));
    }

    #[test]
    fn block_size_silent_for_divisor() {
        // Block sizes that evenly divide the page size: no straddle, no warn.
        assert!(!Pager::page_size_misaligned_with_block(PAGE_SIZE, 4096));
        assert!(!Pager::page_size_misaligned_with_block(PAGE_SIZE, 16384));
        assert!(!Pager::page_size_misaligned_with_block(PAGE_SIZE, 512));
        assert!(!Pager::page_size_misaligned_with_block(PAGE_SIZE, 8192));
    }

    #[test]
    fn block_size_unavailable_is_silent() {
        // st_blksize == 0 means the probe is unavailable; never warn on it.
        assert!(!Pager::page_size_misaligned_with_block(PAGE_SIZE, 0));
    }

    #[test]
    fn page_size_is_unchanged_16kib() {
        // The diagnostic must never alter the compile-time page size.
        assert_eq!(PAGE_SIZE, 16 * 1024);
    }
}
