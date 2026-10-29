//! Shared pager orchestration over an exclusively owned VFS file.
//!
//! Native and browser use the same cache, allocation fences, freelist, DWB,
//! superblock publication and checkpoint algorithms. Opening/locking files,
//! scheduling, WAL implementation and encryption remain host policy.
//! The owner explicitly calls `sync` before teardown; this low-level pager
//! does not hide I/O failures in a destructor. It is not a transaction API.
use super::freelist::FreeList;
use super::page::{Page, PageError, PageType, PAGE_SIZE};
use super::page_cache::PageCache;
use super::pager_io::{self, FIRST_ALLOCATABLE_PAGE};
use reddb_file::VfsFile;
pub use reddb_file::{DatabaseHeader, PhysicalFileHeader};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

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
    /// Database is encrypted but no key was supplied.
    EncryptionRequired,
    /// Plain (unencrypted) database opened with an encryption key.
    PlainDatabaseRefusesKey,
    /// Encryption key validation failed for an encrypted database.
    InvalidKey,
    /// Both superblock copies failed validation (ADR 0074 §2).
    SuperblockZoneUnrecoverable(PathBuf),
    /// The internal manifest zone failed its checksum (ADR 0074 §2).
    ManifestZoneCorrupt(PathBuf),
    /// The in-file double-write zone failed validation (ADR 0074 §2).
    DwbZoneCorrupt(PathBuf),
    /// The store still carries a retired phase-1 pager sidecar (ADR 0038 §4).
    LegacySidecarStore { sidecar: PathBuf },
    /// The store predates the phase-3 in-file DWB layout marker (ADR 0038 §4).
    LegacyPagedStore { path: PathBuf },
}

/// A contiguous run of database pages reserved for vector-turbo payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtentId {
    pub start_page: u32,
    pub n_pages: u32,
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
            Self::EncryptionRequired => write!(
                f,
                "Database is encrypted but no key was supplied (set PagerConfig::encryption)"
            ),
            Self::PlainDatabaseRefusesKey => write!(
                f,
                "Plain (unencrypted) database opened with an encryption key — refusing"
            ),
            Self::InvalidKey => write!(f, "Encryption key validation failed for this database"),
            Self::SuperblockZoneUnrecoverable(path) => write!(
                f,
                "superblock zone of {} is unrecoverable: both ping-pong copies failed \
                 validation, so no generation can be trusted to root the store. The store \
                 will not be opened. Recover what survives with red salvage \
                 (ADR 0074 §4); it reads the damaged file without writing to it and \
                 reports what it could not verify.",
                path.display()
            ),
            Self::ManifestZoneCorrupt(path) => write!(
                f,
                "internal manifest zone of {} failed its checksum: the zone that names \
                 collections, indexes and the checkpoint boundary cannot be trusted, so no \
                 rows are returned rather than garbage ones. Run the `SCRUB` statement to classify the fault \
                 and `red salvage` to extract what survives (ADR 0074 §2/§4).",
                path.display()
            ),
            Self::DwbZoneCorrupt(path) => write!(
                f,
                "double-write zone of {} failed validation: page recovery cannot prove \
                 whether the protected in-place write completed. The store will not be opened \
                 silently; run the `SCRUB` statement to classify the fault and `red salvage` to extract trusted \
                 rows (ADR 0074 §2/§4).",
                path.display()
            ),
            Self::LegacySidecarStore { sidecar } => write!(
                f,
                "refusing to open a legacy sidecar-backed store: found {}. The superblock \
                 pair, internal manifest, and double-write recovery state now live inside the \
                 .rdb file (ADR 0038 §2/§4), \
                 and the engine never reads the retired sidecars silently. Convert the store \
                 first with the offline migration tool \
                 (`reddb_server::pager_zone_migration::migrate_to_zoned`); it is reversible.",
                sidecar.display()
            ),
            Self::LegacyPagedStore { path } => write!(
                f,
                "refusing to open a pre-phase-3 paged store: {} does not carry the layout marker \
                 for the in-file double-write zone. Convert the store first with the offline \
                 migration tool (`reddb_server::pager_zone_migration::migrate_to_zoned`); it is \
                 reversible.",
                path.display()
            ),
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

impl From<pager_io::PagerIoError> for PagerError {
    fn from(error: pager_io::PagerIoError) -> Self {
        match error {
            pager_io::PagerIoError::Io(error) => Self::Io(error),
            pager_io::PagerIoError::Page(error) => Self::Page(error),
            pager_io::PagerIoError::InvalidDatabase(message) => Self::InvalidDatabase(message),
        }
    }
}

/// Host-independent pager configuration. File creation and ownership are host policy.
#[derive(Debug, Clone)]
pub struct PagerConfig {
    pub cache_size: usize,
    pub read_only: bool,
    pub verify_checksums: bool,
    pub double_write: bool,
}

impl Default for PagerConfig {
    fn default() -> Self {
        Self {
            cache_size: 10_000,
            read_only: false,
            verify_checksums: true,
            double_write: true,
        }
    }
}

/// The host's WAL durability barrier, called before publishing dirty page batches.
/// Implementations must make all records through `lsn` durable or return an error.
pub trait WalBarrier {
    fn flush_until(&self, lsn: u64) -> std::io::Result<()>;
}

/// A pager without a WAL may only publish pages whose LSN is zero.
pub struct NoWal;
impl WalBarrier for NoWal {
    fn flush_until(&self, _lsn: u64) -> std::io::Result<()> {
        Err(std::io::Error::other(
            "nonzero page LSN requires a WAL durability barrier",
        ))
    }
}
pub struct Pager<F: VfsFile, W: WalBarrier> {
    /// Database file path
    path: PathBuf,
    /// File handle
    file: Mutex<F>,
    /// Whether mutable pages are protected by the in-file DWB zone.
    dwb_enabled: bool,
    /// A failed writeback may leave the only complete image in the DWB.
    /// Refuse further writes until reopen has recovered that image.
    writeback_failed: std::sync::atomic::AtomicBool,
    /// Page cache
    cache: PageCache,
    /// Free page list
    freelist: RwLock<FreeList>,
    /// Store-managed frees stay unavailable until their removal from the
    /// durable catalog has crossed a publication boundary.
    deferred_reclamation: Mutex<DeferredPageReclamation>,
    /// Reused page ownership must reach both superblock copies before a
    /// manifest can refer to those pages, including single-slot recovery.
    allocation_reused: std::sync::atomic::AtomicBool,
    /// A consumed trunk cannot be reused until both header fences succeed.
    allocation_fence_pending: std::sync::atomic::AtomicBool,
    /// Database header
    header: RwLock<DatabaseHeader>,
    /// Configuration
    config: PagerConfig,
    /// Dirty flag for header
    header_dirty: Mutex<bool>,
    wal: W,
}

#[derive(Default)]
struct DeferredPageReclamation {
    enabled: bool,
    pending: std::collections::BTreeSet<u32>,
    published: std::collections::BTreeSet<u32>,
}

/// The superblock zone occupies the head of page 0 (ADR 0038 §2 phase 1).
const SUPERBLOCK_SLOT_SIZE: usize = reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE;
const SUPERBLOCK_ZONE_SIZE: usize = reddb_file::PAGED_SUPERBLOCK_ZONE_SIZE;
// The phase-3 layout was introduced in 1.0.1. Later file format versions
// must not make existing phase-3 stores look like legacy sidecar stores.
const PHASE3_DWB_ZONE_FILE_VERSION: u32 = 0x0001_0001;

/// The newest valid superblock copy together with its slot image.
pub type NewestSuperblock = (
    reddb_file::PagedSuperblockSelection,
    [u8; SUPERBLOCK_SLOT_SIZE],
);

/// Take the first slot's worth of a freshly-built page-0 image. The database
/// header and the encryption header both live well inside this prefix.
fn superblock_slot_image(page: &[u8; PAGE_SIZE]) -> [u8; SUPERBLOCK_SLOT_SIZE] {
    let mut image = [0u8; SUPERBLOCK_SLOT_SIZE];
    image.copy_from_slice(&page[..SUPERBLOCK_SLOT_SIZE]);
    image
}

impl<F: VfsFile, W: WalBarrier> Pager<F, W> {
    /// Use an already-owned file. The host retains exclusive ownership until
    /// this pager is dropped, and supplies the native WAL/encryption policy.
    /// `create` is explicit: a corrupt or empty existing file is never recreated.
    pub fn open_owned(
        file: F,
        path: PathBuf,
        config: PagerConfig,
        wal: W,
        create: bool,
    ) -> Result<Self, PagerError> {
        if create && file.file_len()? != 0 {
            return Err(PagerError::InvalidDatabase(
                "new pager file is not empty".into(),
            ));
        }
        let pager = Self {
            path,
            file: Mutex::new(file),
            dwb_enabled: config.double_write && !config.read_only,
            writeback_failed: std::sync::atomic::AtomicBool::new(false),
            cache: PageCache::new(config.cache_size),
            freelist: RwLock::new(FreeList::new()),
            deferred_reclamation: Mutex::new(DeferredPageReclamation::default()),
            allocation_reused: std::sync::atomic::AtomicBool::new(false),
            allocation_fence_pending: std::sync::atomic::AtomicBool::new(false),
            header: RwLock::new(DatabaseHeader::default()),
            config,
            header_dirty: Mutex::new(false),
            wal,
        };
        if create {
            pager.initialize()?;
        } else {
            pager.ensure_phase3_dwb_zone_layout()?;
            pager.recover_from_dwb()?;
            pager.load_header()?;
        }
        Ok(pager)
    }

    pub fn wal_barrier(&self) -> &W {
        &self.wal
    }

    /// Run a host operation while holding the same lock as protected page I/O.
    /// This is used only for native prefetch and fault-injection tests.
    #[doc(hidden)]
    pub fn with_file<T>(&self, operation: impl FnOnce(&mut F) -> T) -> Result<T, PagerError> {
        Ok(operation(&mut *self.file_lock()?))
    }

    /// Initialize a new database
    fn initialize(&self) -> Result<(), PagerError> {
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }

        // Create header page. Page ids 1 and 2 are reserved for fixed
        // metadata/vault pages. ADR 0038 phase 3 reserves the following DWB
        // zone in-file so normal B-tree allocation starts after it.
        let initial_page_count = FIRST_ALLOCATABLE_PAGE;
        let header_page = Page::new_header_page(initial_page_count);
        self.header_write()?.page_count = initial_page_count;

        // Reserve page 0 (the superblock zone) plus the reserved pages so any
        // scan over 0..page_count can read every allocated page in a brand-new
        // database. The zone's own bytes are published below, one slot at a
        // time; this first whole-page write only sizes the file.
        self.write_page_raw(0, &header_page)?;
        let mut metadata_page = Page::new(PageType::Header, 1);
        metadata_page.update_checksum();
        self.write_page_raw(1, &metadata_page)?;
        let mut vault_page = Page::new(PageType::Vault, 2);
        vault_page.update_checksum();
        self.write_page_raw(2, &vault_page)?;
        self.clear_dwb_zone()?;

        // Publish both superblock copies so the ping-pong invariant — one
        // valid copy always survives a crash — holds from the very first
        // update onward, not just from the second.
        let mut slot = superblock_slot_image(header_page.as_bytes());
        self.write_superblock_slot(&mut slot, 0, 1)?;
        self.write_superblock_slot(&mut slot, 1, 2)?;

        // Sync to disk
        self.sync()?;

        Ok(())
    }

    /// Acquire a write lock on the header RwLock, mapping poison errors.
    fn header_write(&self) -> Result<std::sync::RwLockWriteGuard<'_, DatabaseHeader>, PagerError> {
        self.header.write().map_err(|_| PagerError::LockPoisoned)
    }

    /// Acquire a read lock on the header RwLock, mapping poison errors.
    fn header_read(&self) -> Result<std::sync::RwLockReadGuard<'_, DatabaseHeader>, PagerError> {
        self.header.read().map_err(|_| PagerError::LockPoisoned)
    }

    /// Acquire a write lock on the freelist RwLock, mapping poison errors.
    fn freelist_write(&self) -> Result<std::sync::RwLockWriteGuard<'_, FreeList>, PagerError> {
        self.freelist.write().map_err(|_| PagerError::LockPoisoned)
    }

    /// Acquire a lock on the file Mutex, mapping poison errors.
    fn file_lock(&self) -> Result<std::sync::MutexGuard<'_, F>, PagerError> {
        self.file.lock().map_err(|_| PagerError::LockPoisoned)
    }

    /// Acquire a lock on the header_dirty Mutex, mapping poison errors.
    fn header_dirty_lock(&self) -> Result<std::sync::MutexGuard<'_, bool>, PagerError> {
        self.header_dirty
            .lock()
            .map_err(|_| PagerError::LockPoisoned)
    }

    /// Read the whole superblock zone (both ping-pong copies) from the head of
    /// the file. A file shorter than the zone yields zero-filled tail bytes,
    /// which decode as "slot never written".
    fn read_superblock_zone(&self) -> Result<[u8; SUPERBLOCK_ZONE_SIZE], PagerError> {
        let mut file = self.file_lock()?;
        Ok(pager_io::read_superblock_zone(&mut *file)?)
    }

    /// The newest valid superblock copy, with its slot image.
    ///
    /// Both copies invalid is the one unrecoverable superblock state (ADR 0074
    /// §2): the store does not open, and the error names the zone and points
    /// at salvage rather than panicking or silently re-initialising.
    #[doc(hidden)]
    pub fn newest_superblock(&self) -> Result<NewestSuperblock, PagerError> {
        let zone = self.read_superblock_zone()?;
        let selection = reddb_file::select_paged_superblock(&zone)
            .ok_or_else(|| PagerError::SuperblockZoneUnrecoverable(self.path.clone()))?;

        let start = selection.copy_index * SUPERBLOCK_SLOT_SIZE;
        let mut image = [0u8; SUPERBLOCK_SLOT_SIZE];
        image.copy_from_slice(&zone[start..start + SUPERBLOCK_SLOT_SIZE]);
        Ok((selection, image))
    }

    fn ensure_phase3_dwb_zone_layout(&self) -> Result<(), PagerError> {
        let (_, image) = self.newest_superblock()?;
        let header = reddb_file::decode_database_header(&image)
            .map_err(|err| PagerError::InvalidDatabase(err.to_string()))?;
        if header.version < PHASE3_DWB_ZONE_FILE_VERSION {
            return Err(PagerError::LegacyPagedStore {
                path: self.path.clone(),
            });
        }
        Ok(())
    }

    /// Seal `slot` for `copy_index`/`generation` and write exactly that slot,
    /// then fsync. The caller guarantees `copy_index` is the stale copy.
    fn write_superblock_slot(
        &self,
        slot: &mut [u8; SUPERBLOCK_SLOT_SIZE],
        copy_index: usize,
        generation: u64,
    ) -> Result<(), PagerError> {
        self.ensure_writeback_available()?;
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }
        let mut file = self.file_lock()?;
        self.ensure_writeback_available()?;
        Ok(pager_io::write_superblock_slot(
            &mut *file, slot, copy_index, generation,
        )?)
    }

    /// Publish `mutate`'s edit of the newest slot image into the *stale* copy.
    ///
    /// Reading the newest image first means every byte the pager does not
    /// explicitly rewrite — most importantly the encryption marker and header —
    /// carries forward untouched.
    #[doc(hidden)]
    pub fn commit_superblock(
        &self,
        mutate: impl FnOnce(&mut [u8; SUPERBLOCK_SLOT_SIZE]) -> Result<(), PagerError>,
    ) -> Result<(), PagerError> {
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }
        let (newest, mut image) = self.newest_superblock()?;
        mutate(&mut image)?;
        let next_copy = reddb_file::paged_superblock_next_copy(Some(newest));
        self.write_superblock_slot(&mut image, next_copy, newest.generation.saturating_add(1))
    }

    /// Load the database header from the newest valid superblock copy.
    fn load_header(&self) -> Result<(), PagerError> {
        let (_, image) = self.newest_superblock()?;

        let mut decoded_header = reddb_file::decode_database_header(&image)
            .map_err(|err| PagerError::InvalidDatabase(err.to_string()))?;
        // Cache eviction (or DWB recovery) can persist pages before the next
        // superblock publication. After a crash the allocation counter may
        // therefore lag behind live pages already in the file. Reserve the
        // entire physical extent before allocating; never overwrite that tail.
        // A partial last page is reserved too, without treating it as verified.
        let physical_pages = self.file_lock()?.file_len()?.div_ceil(PAGE_SIZE as u64);
        let physical_pages = u32::try_from(physical_pages).map_err(|_| {
            PagerError::InvalidDatabase("data file exceeds the page address space".into())
        })?;
        if decoded_header.page_count < physical_pages {
            decoded_header.page_count = physical_pages;
            if !self.config.read_only {
                *self.header_dirty_lock()? = true;
            }
        }
        let freelist_head = decoded_header.freelist_head;

        {
            let mut header = self.header_write()?;
            *header = decoded_header;
        }

        // Initialize freelist
        {
            let mut freelist = self.freelist_write()?;
            *freelist = FreeList::from_header(freelist_head, 0);
        }

        Ok(())
    }

    /// Publish the in-memory database header as a new superblock generation.
    fn write_header(&self) -> Result<(), PagerError> {
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }

        let header = self.header_read()?.clone();
        self.commit_superblock(|image| {
            reddb_file::encode_database_header(image, &header)
                .map_err(|err| PagerError::InvalidDatabase(err.to_string()))
        })?;
        *self.header_dirty_lock()? = false;

        Ok(())
    }

    /// Read a page from disk (bypassing cache)
    fn read_page_raw(&self, page_id: u32) -> Result<Page, PagerError> {
        let mut file = self.file_lock()?;
        Ok(pager_io::read_page(
            &mut *file,
            page_id,
            self.config.verify_checksums,
        )?)
    }

    fn ensure_writeback_available(&self) -> Result<(), PagerError> {
        if self
            .writeback_failed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(PagerError::Io(std::io::Error::other(
                "page writeback failed; reopen the database to recover before writing",
            )));
        }
        Ok(())
    }

    /// Amortize protected eviction over one bounded DWB frame. Keeping the
    /// remaining cache dirty would otherwise pay three fsyncs per victim.
    fn write_evicted_dirty_page(&self, page: Page) -> Result<(), PagerError> {
        let mut ids = self
            .cache
            .dirty_page_ids(pager_io::DWB_MAX_PAGES_PER_FRAME.saturating_sub(1));
        ids.sort_unstable();
        let mut pages = self.cache.flush_dirty_pages(&ids);
        pages.push((page.page_id(), page));
        pages.sort_unstable_by_key(|(id, _)| *id);
        self.write_dirty_page_batch(&pages)
    }

    /// Write a page to disk (bypassing cache)
    fn write_page_raw(&self, page_id: u32, page: &Page) -> Result<(), PagerError> {
        self.ensure_writeback_available()?;
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }
        let mut file = self.file_lock()?;
        self.ensure_writeback_available()?;
        Ok(pager_io::write_page(&mut *file, page_id, page)?)
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
            self.write_evicted_dirty_page(dirty_page)?;
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

        // Cache miss - use the same byte I/O without a page CRC check.
        let mut file = self.file_lock()?;
        let page = pager_io::read_page(&mut *file, page_id, false)?;
        drop(file);

        // Add to cache (no checksum verification)
        if let Some(dirty_page) = self.cache.insert(page_id, page.clone()) {
            // Evicted page was dirty, need to write it back
            self.write_evicted_dirty_page(dirty_page)?;
        }

        Ok(page)
    }

    /// Write a page (cache-aware)
    pub fn write_page(&self, page_id: u32, mut page: Page) -> Result<(), PagerError> {
        self.ensure_writeback_available()?;
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }

        // Update checksum
        page.update_checksum();

        // Add to cache and mark dirty. If the cache had to evict a
        // dirty entry, write it through immediately — the evicted
        // page will never be flushed otherwise (same bug fixed in
        // `allocate_page`).
        if let Some(dirty_page) = self.cache.insert(page_id, page) {
            self.write_evicted_dirty_page(dirty_page)?;
        }
        self.cache.mark_dirty(page_id);

        Ok(())
    }

    /// Write a page without updating checksum (for encrypted pages)
    ///
    /// Use this when the page content has its own integrity protection
    /// (e.g., AES-GCM authentication tag for encrypted pages).
    pub fn write_page_no_checksum(&self, page_id: u32, page: Page) -> Result<(), PagerError> {
        self.ensure_writeback_available()?;
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }

        // Add to cache and mark dirty (no checksum update). Same
        // eviction-write-through guard as `write_page`.
        if let Some(dirty_page) = self.cache.insert(page_id, page) {
            self.write_evicted_dirty_page(dirty_page)?;
        }
        self.cache.mark_dirty(page_id);

        Ok(())
    }

    /// Allocate a new page
    pub fn allocate_page(&self, page_type: PageType) -> Result<Page, PagerError> {
        self.ensure_writeback_available()?;
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }

        // Try to get from freelist first
        let page_id = {
            let mut freelist = self.freelist_write()?;
            // A previous fence can fail after loading the trunk into memory.
            // Retry it before the fast path can consume any of those pages.
            self.complete_allocation_fence()?;
            if let Some(id) = freelist.allocate() {
                self.allocation_reused
                    .store(true, std::sync::atomic::Ordering::Release);
                id
            } else if freelist.trunk_head() != 0 {
                let trunk_id = freelist.trunk_head();

                let trunk = self.read_page(trunk_id).map_err(|e| match e {
                    PagerError::PageNotFound(_) => {
                        PagerError::InvalidDatabase("Freelist trunk missing".to_string())
                    }
                    other => other,
                })?;

                freelist
                    .load_from_trunk(&trunk)
                    .map_err(|e| PagerError::InvalidDatabase(format!("Freelist: {}", e)))?;
                let mut header = self.header_write()?;
                header.freelist_head = freelist.trunk_head();
                *self.header_dirty_lock()? = true;
                drop(header);
                // The consumed trunk itself becomes reusable. Forget its old
                // allocator role durably before cache eviction can overwrite it.
                // Keep the allocator locked so another allocator cannot race
                // ahead of this fence. A failed fence leaves allocation stopped.
                self.allocation_fence_pending
                    .store(true, std::sync::atomic::Ordering::Release);
                self.complete_allocation_fence()?;
                let id = freelist.allocate().ok_or_else(|| {
                    PagerError::InvalidDatabase("Freelist empty after trunk load".to_string())
                })?;
                self.allocation_reused
                    .store(true, std::sync::atomic::Ordering::Release);
                id
            } else {
                // No free pages, extend file
                let mut header = self.header_write()?;
                let id = header.page_count;
                header.page_count += 1;
                *self.header_dirty_lock()? = true;
                id
            }
        };

        let page = Page::new(page_type, page_id);

        // Write to cache. The evicted page (if any) is dirty by
        // definition — `cache.insert` only returns `Some` when it
        // had to evict a dirty entry to make room. The previous
        // version dropped that return value, which silently lost
        // writes whenever a freshly-allocated page caused an LRU
        // eviction. This shows up under heavy ingest as
        // "B-tree insert error: Pager error: I/O error: failed to
        // fill whole buffer" later, when something tries to read
        // back the never-flushed page. Mirror `read_page`'s
        // handling: write the evicted page through immediately so
        // the on-disk image stays consistent.
        if let Some(dirty_page) = self.cache.insert(page_id, page.clone()) {
            self.write_evicted_dirty_page(dirty_page)?;
        }
        self.cache.mark_dirty(page_id);

        Ok(page)
    }

    /// Reserve a contiguous extent of vector pages.
    pub fn reserve_contig_extent(&self, n_pages: u32) -> Result<ExtentId, PagerError> {
        self.ensure_writeback_available()?;
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }
        if n_pages == 0 {
            return Err(PagerError::InvalidDatabase(
                "contiguous extent must reserve at least one page".to_string(),
            ));
        }

        let start_page = {
            let mut header = self.header_write()?;
            let start = header.page_count;
            header.page_count = header.page_count.checked_add(n_pages).ok_or_else(|| {
                PagerError::InvalidDatabase("contiguous extent page count overflow".to_string())
            })?;
            *self.header_dirty_lock()? = true;
            start
        };

        for page_id in start_page..start_page + n_pages {
            let mut page = Page::new(PageType::Vector, page_id);
            page.update_checksum();
            if let Some(dirty_page) = self.cache.insert(page_id, page) {
                self.write_evicted_dirty_page(dirty_page)?;
            }
            self.cache.mark_dirty(page_id);
        }

        Ok(ExtentId {
            start_page,
            n_pages,
        })
    }

    /// Free a page (return to freelist)
    pub fn free_page(&self, page_id: u32) -> Result<(), PagerError> {
        self.ensure_writeback_available()?;
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }

        let mut deferred = self
            .deferred_reclamation
            .lock()
            .map_err(|_| PagerError::LockPoisoned)?;
        if deferred.enabled {
            if page_id < FIRST_ALLOCATABLE_PAGE || page_id >= self.page_count()? {
                return Err(PagerError::InvalidDatabase(
                    "invalid deferred free page".into(),
                ));
            }
            // Keep dirty forwarding/empty nodes until publication. An
            // incidental flush must leave the previous root traversable.
            deferred.pending.insert(page_id);
            return Ok(());
        }
        drop(deferred);

        // Remove from cache
        self.cache.remove(page_id);

        // Add to freelist
        let mut freelist = self.freelist_write()?;
        freelist.free(page_id);

        *self.header_dirty_lock()? = true;

        Ok(())
    }

    pub fn enable_deferred_reclamation(&self) -> Result<(), PagerError> {
        self.deferred_reclamation
            .lock()
            .map_err(|_| PagerError::LockPoisoned)?
            .enabled = true;
        Ok(())
    }

    pub fn deferred_reclamation_pages(&self) -> Result<(Vec<u32>, Vec<u32>), PagerError> {
        let deferred = self
            .deferred_reclamation
            .lock()
            .map_err(|_| PagerError::LockPoisoned)?;
        Ok((
            deferred.pending.iter().copied().collect(),
            deferred.published.iter().copied().collect(),
        ))
    }

    pub fn restore_deferred_reclamation(&self, pages: &[u32]) -> Result<(), PagerError> {
        let count = self.page_count()?;
        if pages
            .iter()
            .any(|id| *id < FIRST_ALLOCATABLE_PAGE || *id >= count)
        {
            return Err(PagerError::InvalidDatabase(
                "invalid persisted deferred free page".into(),
            ));
        }
        let mut deferred = self
            .deferred_reclamation
            .lock()
            .map_err(|_| PagerError::LockPoisoned)?;
        deferred.pending.extend(pages);
        deferred.published.extend(pages);
        Ok(())
    }

    /// The caller has durably published a catalog that retains `retained`
    /// and no longer names `released`, after waiting for old tree readers.
    pub fn complete_deferred_publication(
        &self,
        retained: &[u32],
        released: &[u32],
    ) -> Result<(), PagerError> {
        let mut deferred = self
            .deferred_reclamation
            .lock()
            .map_err(|_| PagerError::LockPoisoned)?;
        if !released.is_empty() {
            self.free_retired_pages(released)?;
        }
        for id in released {
            deferred.pending.remove(id);
        }
        deferred.published = retained.iter().copied().collect();
        Ok(())
    }

    /// Return a fully validated, unpublished tree generation in one allocator
    /// mutation. Call only after its removal from the manifest is durable.
    pub fn free_retired_pages(&self, pages: &[u32]) -> Result<(), PagerError> {
        self.ensure_writeback_available()?;
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }
        let page_count = self.page_count()?;
        if pages
            .iter()
            .any(|id| *id < FIRST_ALLOCATABLE_PAGE || *id >= page_count)
        {
            return Err(PagerError::InvalidDatabase(
                "invalid retired page range".into(),
            ));
        }
        let mut freelist = self.freelist_write()?;
        for id in pages {
            self.cache.remove(*id);
        }
        freelist.free_batch(pages);
        *self.header_dirty_lock()? = true;
        Ok(())
    }

    /// Get database header
    /// Pages available to the allocator, including pages on unloaded trunks.
    pub fn free_page_count(&self) -> Result<u32, PagerError> {
        Ok(self.freelist_write()?.total_free())
    }

    pub fn header(&self) -> Result<DatabaseHeader, PagerError> {
        Ok(self.header_read()?.clone())
    }

    pub fn physical_header(&self) -> Result<PhysicalFileHeader, PagerError> {
        Ok(self.header_read()?.physical)
    }

    pub fn update_physical_header(&self, physical: PhysicalFileHeader) -> Result<(), PagerError> {
        self.ensure_writeback_available()?;
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }

        let mut header = self.header_write()?;
        header.physical = physical;
        *self.header_dirty_lock()? = true;
        Ok(())
    }

    /// Get page count
    pub fn page_count(&self) -> Result<u32, PagerError> {
        Ok(self.header_read()?.page_count)
    }

    /// Flush all dirty pages to disk
    pub fn flush(&self) -> Result<(), PagerError> {
        self.ensure_writeback_available()?;
        if self.config.read_only {
            return Ok(());
        }

        // Persist freelist to trunk pages when dirty
        let trunks = {
            let mut freelist = self.freelist_write()?;
            if freelist.is_dirty() {
                let mut header = self.header_write()?;
                let trunks = freelist.flush_to_trunks(0, || {
                    let id = header.page_count;
                    header.page_count += 1;
                    id
                });
                header.freelist_head = freelist.trunk_head();
                *self.header_dirty_lock()? = true;
                freelist.mark_clean();
                trunks
            } else {
                Vec::new()
            }
        };

        let mut dirty_pages = Vec::new();
        for trunk in trunks {
            let page_id = trunk.page_id();
            if let Some(evicted) = self.cache.insert(page_id, trunk) {
                // Trunk publication can evict dirty data. Keep it in the same
                // WAL-first, double-write-protected batch as resident pages.
                dirty_pages.push((evicted.page_id(), evicted));
            }
            self.cache.mark_dirty(page_id);
        }

        // IDs are compact; page images are not. At 16 KiB per page, staging
        // 256 images plus one DWB frame uses about 5 MiB, independently of
        // the dirty cache size. Publish the header only after every batch.
        const FLUSH_BATCH_PAGES: usize = 256;
        for batch in dirty_pages.chunks(FLUSH_BATCH_PAGES) {
            self.write_dirty_page_batch(batch)?;
        }
        drop(dirty_pages);
        // Batch dirty IDs, not every resident ID: otherwise a few edits in
        // a mostly clean cache pay a protected write per cache-sized slice.
        // Physical order also avoids arbitrary seeks among each batch's homes.
        let mut page_ids = self.cache.dirty_page_ids(usize::MAX);
        page_ids.sort_unstable();
        for ids in page_ids.chunks(FLUSH_BATCH_PAGES) {
            let pages = self.cache.flush_dirty_pages(ids);
            self.write_dirty_page_batch(&pages)?;
        }

        // Write header if dirty
        if *self.header_dirty_lock()? {
            self.write_header()?;
        }

        Ok(())
    }

    fn write_dirty_page_batch(&self, pages: &[(u32, Page)]) -> Result<(), PagerError> {
        if pages.is_empty() {
            return Ok(());
        }
        // Every image's WAL must be durable before its home write, including
        // dirty data evicted while publishing freelist trunks. LSN zero is
        // reserved for pages not produced by a WAL append.
        let result = (|| {
            let max_lsn = pages
                .iter()
                .filter_map(|(_, page)| page.header().ok().map(|header| header.lsn))
                .max()
                .unwrap_or(0);
            if max_lsn > 0 {
                self.wal.flush_until(max_lsn)?;
            }
            self.write_pages_through_dwb(pages)
        })();
        if result.is_err() {
            self.writeback_failed
                .store(true, std::sync::atomic::Ordering::Release);
            for (page_id, _) in pages {
                self.cache.mark_dirty(*page_id);
            }
        }
        result
    }

    /// Sync file to disk (fsync)
    pub fn sync(&self) -> Result<(), PagerError> {
        self.flush()?;

        if self
            .allocation_reused
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            // flush published the current allocator in one slot. Mirror that
            // ownership before callers publish roots that use reclaimed pages;
            // recovery through the sibling slot must never allocate them again.
            if let Err(error) = self.write_header_and_sync() {
                self.allocation_reused
                    .store(true, std::sync::atomic::Ordering::Release);
                return Err(error);
            }
        }

        let file = self.file_lock()?;
        file.sync_all()?;

        Ok(())
    }

    /// Get cache statistics
    pub fn cache_stats(&self) -> super::page_cache::CacheStats {
        self.cache.stats()
    }

    /// Pages currently resident in the page cache. Slots are fixed size, so
    /// this times the page size is the cache's memory footprint (ADR 0073 §2).
    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }

    /// Preallocated page-cache slots — the sizing this process derived from
    /// its page-cache budget share.
    pub fn cache_capacity(&self) -> usize {
        self.cache.capacity()
    }

    /// Count dirty pages currently in the page cache.
    pub fn dirty_page_count(&self) -> usize {
        self.cache.dirty_count()
    }

    /// Estimated fraction of the page cache holding dirty pages.
    /// Returned in `[0, 1]`. Used by the background writer to
    /// decide when to kick in aggressive flushing.
    pub fn dirty_fraction(&self) -> f64 {
        let capacity = self.cache.capacity().max(1) as f64;
        self.cache.dirty_count() as f64 / capacity
    }

    /// Flush up to `max` dirty pages from the cache. Returns the
    /// number actually written. Background-writer entry point —
    /// reuses the same persistence path as `flush()` but bounded.
    pub fn flush_some_dirty(&self, max: usize) -> Result<usize, PagerError> {
        self.ensure_writeback_available()?;
        if self.config.read_only || max == 0 {
            return Ok(0);
        }
        let dirty_pages = self.cache.flush_some_dirty(max);
        if dirty_pages.is_empty() {
            return Ok(0);
        }
        let count = dirty_pages.len();
        // WAL-first: every cached dirty page carries an LSN that the
        // WAL must have already persisted. The full `flush()` path
        // enforces this with `wal.flush(max_lsn)`; here we simply
        // write through the pager — safe because callers only reach
        // this path via the bgwriter, which runs asynchronously
        // alongside normal commits that already respect WAL-first.
        for (page_id, page) in dirty_pages {
            self.write_page(page_id, page)?;
        }
        Ok(count)
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
        let file = self.file_lock()?;
        Ok(file.file_len()?)
    }

    /// Clear the in-file DWB zone with valid reserved pages.
    fn clear_dwb_zone(&self) -> Result<(), PagerError> {
        let mut file = self.file_lock()?;
        Ok(pager_io::clear_dwb_zone(&mut *file)?)
    }

    /// Read the internal manifest page (page 1), the zone the superblock roots.
    ///
    /// ADR 0074 §2: manifest corruption fails the open didactically, naming the
    /// zone — it never falls back to a sidecar and never returns garbage.
    pub fn read_manifest_page(&self) -> Result<Page, PagerError> {
        self.read_page(1)
            .map_err(|_| PagerError::ManifestZoneCorrupt(self.path.clone()))
    }

    /// Write pages through the double-write buffer for torn page protection.
    ///
    /// 1. Write pages to the in-file DWB zone with a header (magic + count + checksum)
    /// 2. fsync the DWB zone
    /// 3. Write all pages to their final locations in the .rdb file
    /// 4. Clear the DWB zone (marks as consumed)
    fn write_pages_through_dwb(&self, pages: &[(u32, Page)]) -> Result<(), PagerError> {
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }
        // Keep the file locked through staging, home writes and clearing so
        // another page writer cannot interleave with a protected batch.
        let mut file = self.file_lock()?;
        self.ensure_writeback_available()?;
        let result = pager_io::write_pages(&mut *file, pages, self.dwb_enabled);
        if result.is_err() {
            // Set this while holding the file lock: a waiting writer must not
            // replace the recovery frame after an interrupted home write.
            self.writeback_failed
                .store(true, std::sync::atomic::Ordering::Release);
        }
        Ok(result?)
    }

    /// Recover from double-write buffer after a crash.
    ///
    /// If the DWB zone contains valid pages, they were written before the crash
    /// interrupted writing to their home locations. Re-apply them.
    fn recover_from_dwb(&self) -> Result<(), PagerError> {
        let mut file = self.file_lock()?;
        Ok(pager_io::recover_from_dwb(&mut *file)?)
    }

    /// Finish a consumed trunk's durable ownership change, retaining errors
    /// across allocation attempts until both copies have been synchronized.
    /// Called with the freelist locked when consuming or retrying a trunk.
    fn complete_allocation_fence(&self) -> Result<(), PagerError> {
        if self
            .allocation_fence_pending
            .load(std::sync::atomic::Ordering::Acquire)
        {
            self.write_header_and_sync()?;
            self.write_header_and_sync()?;
            self.allocation_fence_pending
                .store(false, std::sync::atomic::Ordering::Release);
        }
        Ok(())
    }

    /// Write header and sync to disk (public for checkpointer).
    pub fn write_header_and_sync(&self) -> Result<(), PagerError> {
        self.write_header()?;
        let file = self.file_lock()?;
        file.sync_all()?;
        Ok(())
    }

    /// Set the checkpoint_in_progress flag in the header.
    pub fn set_checkpoint_in_progress(
        &self,
        in_progress: bool,
        target_lsn: u64,
    ) -> Result<(), PagerError> {
        let mut header = self.header_write()?;
        header.checkpoint_in_progress = in_progress;
        header.checkpoint_target_lsn = target_lsn;
        *self.header_dirty_lock()? = true;
        drop(header);
        self.write_header_and_sync()
    }

    /// Update the checkpoint LSN and clear the in-progress flag.
    pub fn complete_checkpoint(&self, lsn: u64) -> Result<(), PagerError> {
        let mut header = self.header_write()?;
        header.checkpoint_lsn = lsn;
        header.checkpoint_in_progress = false;
        header.checkpoint_target_lsn = 0;
        *self.header_dirty_lock()? = true;
        drop(header);
        self.write_header_and_sync()
    }
}
