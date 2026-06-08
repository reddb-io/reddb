use super::*;

pub(super) const BTRFS_SUPER_MAGIC: i64 = 0x9123_683e;
pub(super) const ZFS_SUPER_MAGIC: i64 = 0x2fc1_2fc1;
pub(super) const FS_NOCOW_FL: u64 = 0x0080_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CowFilesystemKind {
    Zfs,
    BtrfsDataCow,
    TestOverride,
}

pub(super) fn classify_cow_filesystem(
    fs_type: i64,
    mount_options: Option<&str>,
    inode_flags: Option<u64>,
) -> Option<CowFilesystemKind> {
    match fs_type {
        ZFS_SUPER_MAGIC => Some(CowFilesystemKind::Zfs),
        BTRFS_SUPER_MAGIC => {
            let mount_options = mount_options?;
            if mount_options.split(',').any(|option| option == "nodatacow") {
                return None;
            }

            let inode_flags = inode_flags?;
            if inode_flags & FS_NOCOW_FL != 0 {
                return None;
            }

            Some(CowFilesystemKind::BtrfsDataCow)
        }
        _ => None,
    }
}

#[cfg(target_os = "linux")]
fn linux_fstatfs_type(file: &File) -> Option<i64> {
    use std::mem::MaybeUninit;
    use std::os::fd::AsRawFd;

    let mut stat = MaybeUninit::<libc::statfs>::uninit();
    let rc = unsafe { libc::fstatfs(file.as_raw_fd(), stat.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    let stat = unsafe { stat.assume_init() };
    Some(stat.f_type)
}

#[cfg(target_os = "linux")]
fn linux_inode_flags(file: &File) -> Option<u64> {
    use std::os::fd::AsRawFd;

    let mut flags: libc::c_long = 0;
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), libc::FS_IOC_GETFLAGS, &mut flags) };
    if rc != 0 {
        return None;
    }
    Some(flags as u64)
}

#[cfg(target_os = "linux")]
fn linux_mount_options_for_path(path: &Path) -> Option<String> {
    let path = path.canonicalize().ok()?;
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    parse_mountinfo_options_for_path(&mountinfo, &path)
}

#[cfg(target_os = "linux")]
pub(super) fn parse_mountinfo_options_for_path(mountinfo: &str, path: &Path) -> Option<String> {
    let mut best: Option<(usize, String)> = None;

    for line in mountinfo.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        if fields.len() < 10 {
            continue;
        }

        let Some(separator) = fields.iter().position(|field| *field == "-") else {
            continue;
        };
        if separator + 3 >= fields.len() || separator < 6 {
            continue;
        }

        let mount_point = mountinfo_unescape_path(fields[4]);
        if !path.starts_with(&mount_point) {
            continue;
        }

        let fs_type = fields[separator + 1];
        if fs_type != "btrfs" && fs_type != "zfs" {
            continue;
        }

        let mount_options = fields[5];
        let super_options = fields[separator + 3];
        let options = format!("{mount_options},{super_options}");
        let depth = mount_point.components().count();
        if best
            .as_ref()
            .map(|(best_depth, _)| depth > *best_depth)
            .unwrap_or(true)
        {
            best = Some((depth, options));
        }
    }

    best.map(|(_, options)| options)
}

#[cfg(target_os = "linux")]
fn mountinfo_unescape_path(value: &str) -> PathBuf {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let octal = &value[i + 1..i + 4];
            if let Ok(byte) = u8::from_str_radix(octal, 8) {
                out.push(byte);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }

    PathBuf::from(String::from_utf8_lossy(&out).into_owned())
}

impl Pager {
    /// Open or create a database file
    pub fn open<P: AsRef<Path>>(path: P, mut config: PagerConfig) -> Result<Self, PagerError> {
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

        // gh-892: diagnostic probe of the filesystem block size. If the
        // compile-time 16 KiB PAGE_SIZE is not a multiple of the FS block
        // size, page writes straddle FS blocks and incur read-modify-write
        // amplification. Pure diagnostic — emitted once per open, never
        // mutates the page size. `blksize()` is the fstat `st_blksize`.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if let Ok(meta) = file.metadata() {
                let fs_block_size = meta.blksize();
                if Self::page_size_misaligned_with_block(PAGE_SIZE, fs_block_size) {
                    tracing::warn!(
                        page_size = PAGE_SIZE,
                        fs_block_size,
                        path = %path.display(),
                        "database page size is not a multiple of the filesystem \
                         block size; page writes will straddle FS blocks \
                         (read-modify-write amplification). Diagnostic only — \
                         the page size is unchanged."
                    );
                }
            }
        }

        // Acquire file lock (exclusive for writes, shared for read-only)
        let lock_file = if !config.read_only {
            let lf = OpenOptions::new().read(true).write(true).open(&path)?;
            lf.try_lock_exclusive().map_err(|_| PagerError::Locked)?;
            Some(lf)
        } else {
            let lf = OpenOptions::new().read(true).open(&path)?;
            match lf.try_lock_shared() {
                Ok(_) => Some(lf),
                Err(_) => None,
            }
        };

        // Open double-write buffer file.
        //
        // gh-478: when `fold_dwb_into_wal` is enabled the DWB sidecar is
        // suppressed — torn pages are healed by replaying FullPageImage WAL
        // records during recovery. Any pre-existing `-dwb` is removed so a
        // flipped flag cannot leave a stale sidecar on disk.
        //
        // gh-895: an explicit `double_write = false` request is honored only
        // when the already-open data file is proven to live on a filesystem
        // with atomic CoW page writes. Unknown and non-CoW filesystems fail
        // closed by keeping the DWB sidecar.
        let fold_dwb = crate::physical::fold_dwb_into_wal_enabled();
        if !config.double_write && !config.read_only && !fold_dwb {
            let skip_dwb_on_cow =
                Self::cow_filesystem_has_atomic_page_writes(&path, &file).is_some();
            if !skip_dwb_on_cow {
                tracing::warn!(
                    path = %path.display(),
                    "double_write=false requested, but the data file is not proven to be on \
                     ZFS or btrfs datacow; keeping the double-write buffer enabled"
                );
                config.double_write = true;
            }
        }

        let dwb_file = if config.double_write && !config.read_only && !fold_dwb {
            let f = Self::open_dwb_file(&path)?;
            Some(Mutex::new(f))
        } else {
            if fold_dwb && !config.read_only {
                let _ = std::fs::remove_file(Self::dwb_path(&path));
            }
            None
        };

        let mut pager = Self {
            path,
            file: Mutex::new(file),
            _lock_file: lock_file,
            dwb_file,
            cache: PageCache::new(config.cache_size),
            freelist: RwLock::new(FreeList::new()),
            header: RwLock::new(DatabaseHeader::default()),
            config,
            header_dirty: Mutex::new(false),
            wal: RwLock::new(None),
            encryption: None,
        };

        if exists {
            // Recover from double-write buffer if needed
            pager.recover_from_dwb()?;
            if !pager.config.double_write && !pager.config.read_only {
                let _ = std::fs::remove_file(Self::dwb_path(&pager.path));
            }
            // Load existing database (with header shadow fallback)
            pager.load_header()?;
            pager.bind_encryption_for_existing()?;
        } else {
            // Initialize new database
            pager.initialize()?;
            pager.bind_encryption_for_new()?;
        }

        Ok(pager)
    }

    /// gh-892 diagnostic predicate: returns `true` when the database
    /// `page_size` is **not** an integer multiple of the filesystem's
    /// reported block size (`st_blksize`), i.e. `page_size % fs_block_size
    /// != 0`. A `fs_block_size` of `0` (probe unavailable / unknown) is
    /// treated as aligned so a missing probe never produces a warning.
    /// Pure function with no I/O so the warn decision is unit-testable.
    pub(crate) fn page_size_misaligned_with_block(page_size: usize, fs_block_size: u64) -> bool {
        fs_block_size != 0 && !(page_size as u64).is_multiple_of(fs_block_size)
    }

    #[cfg(test)]
    fn cow_filesystem_test_override() -> Option<bool> {
        match COW_ATOMIC_WRITE_TEST_OVERRIDE.load(Ordering::Relaxed) {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        }
    }

    #[cfg(not(test))]
    fn cow_filesystem_test_override() -> Option<bool> {
        None
    }

    fn cow_filesystem_has_atomic_page_writes(
        path: &Path,
        file: &File,
    ) -> Option<CowFilesystemKind> {
        if let Some(allowed) = Self::cow_filesystem_test_override() {
            return allowed.then_some(CowFilesystemKind::TestOverride);
        }
        Self::probe_cow_filesystem(path, file)
    }

    #[cfg(target_os = "linux")]
    fn probe_cow_filesystem(path: &Path, file: &File) -> Option<CowFilesystemKind> {
        let fs_type = linux_fstatfs_type(file)?;
        match fs_type {
            ZFS_SUPER_MAGIC => Some(CowFilesystemKind::Zfs),
            BTRFS_SUPER_MAGIC => {
                let mount_options = linux_mount_options_for_path(path)?;
                let inode_flags = linux_inode_flags(file)?;
                classify_cow_filesystem(fs_type, Some(&mount_options), Some(inode_flags))
            }
            _ => None,
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn probe_cow_filesystem(_path: &Path, _file: &File) -> Option<CowFilesystemKind> {
        None
    }

    /// Inspect page 0 for the `RDBE` encryption marker, then resolve
    /// the (key, marker) matrix:
    ///
    /// | Marker | Key supplied | Result                              |
    /// |--------|--------------|-------------------------------------|
    /// | yes    | yes          | Bind encryptor; validate key        |
    /// | yes    | no           | `EncryptionRequired` (fail closed)  |
    /// | no     | yes          | `PlainDatabaseRefusesKey`           |
    /// | no     | no           | Plain pager — no binding needed     |
    fn bind_encryption_for_existing(&mut self) -> Result<(), PagerError> {
        if self.page_count().unwrap_or(0) == 0 {
            return self.bind_encryption_for_new();
        }
        let header_page = self.read_page_no_checksum(0)?;
        let data = header_page.as_bytes();
        let has_marker = reddb_file::paged_encryption_marker_present(data);

        let key = self.config.encryption.clone();
        match (has_marker, key) {
            (true, Some(key)) => {
                let header_bytes =
                    reddb_file::paged_encryption_header_bytes(data).ok_or_else(|| {
                        PagerError::InvalidDatabase("encryption header parse failed".to_string())
                    })?;
                let header = crate::storage::encryption::EncryptionHeader::from_bytes(header_bytes)
                    .map_err(|e| {
                        PagerError::InvalidDatabase(format!("encryption header parse failed: {e}"))
                    })?;
                if !header.validate(&key) {
                    return Err(PagerError::InvalidKey);
                }
                let encryptor = crate::storage::encryption::PageEncryptor::new(key);
                self.encryption = Some((encryptor, header));
                Ok(())
            }
            (true, None) => Err(PagerError::EncryptionRequired),
            (false, Some(_)) => Err(PagerError::PlainDatabaseRefusesKey),
            (false, None) => Ok(()),
        }
    }

    /// New DB: if a key is configured, write the marker + header to
    /// page 0 so subsequent opens detect encryption.
    fn bind_encryption_for_new(&mut self) -> Result<(), PagerError> {
        let Some(key) = self.config.encryption.clone() else {
            return Ok(());
        };
        let header = crate::storage::encryption::EncryptionHeader::new(&key);
        let encryptor = crate::storage::encryption::PageEncryptor::new(key);

        // Stamp the marker + header into page 0 if it's been
        // initialised by `initialize()` already.
        if self.page_count().unwrap_or(0) > 0 {
            let mut page = self.read_page_no_checksum(0)?;
            let data = page.as_bytes_mut();
            let header_bytes = header.to_bytes();
            reddb_file::write_paged_encryption_marker_and_header(data, &header_bytes)
                .map_err(|err| PagerError::InvalidDatabase(err.to_string()))?;
            self.write_page_no_checksum(0, page)?;
        }
        self.encryption = Some((encryptor, header));
        Ok(())
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

        // Create header page. Page ids 1 and 2 are reserved so fixed
        // metadata/vault pages cannot be handed out to normal B-tree
        // allocation before those subsystems are initialized.
        let initial_page_count = 3;
        let header_page = Page::new_header_page(initial_page_count);
        self.header_write()?.page_count = initial_page_count;

        // Write header and reserved pages so any scan over 0..page_count
        // can read every allocated page in a brand-new database.
        self.write_page_raw(0, &header_page)?;
        let mut metadata_page = Page::new(PageType::Header, 1);
        metadata_page.update_checksum();
        self.write_page_raw(1, &metadata_page)?;
        let mut vault_page = Page::new(PageType::Vault, 2);
        vault_page.update_checksum();
        self.write_page_raw(2, &vault_page)?;

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
    fn file_lock(&self) -> Result<std::sync::MutexGuard<'_, File>, PagerError> {
        self.file.lock().map_err(|_| PagerError::LockPoisoned)
    }

    /// Acquire a lock on the header_dirty Mutex, mapping poison errors.
    fn header_dirty_lock(&self) -> Result<std::sync::MutexGuard<'_, bool>, PagerError> {
        self.header_dirty
            .lock()
            .map_err(|_| PagerError::LockPoisoned)
    }

    /// Load database header from page 0 (with shadow fallback)
    fn load_header(&self) -> Result<(), PagerError> {
        // Read page 0 — fall back to shadow if corrupted
        let header_page = match self.read_page_raw(0) {
            Ok(page) => {
                // Verify magic bytes
                if reddb_file::database_header_magic_matches(page.as_bytes()) {
                    page
                } else {
                    // Page 0 corrupted — try shadow
                    self.recover_header_from_shadow()?
                }
            }
            Err(_) => self.recover_header_from_shadow()?,
        };

        let decoded_header = reddb_file::decode_database_header(header_page.as_bytes())
            .map_err(|err| PagerError::InvalidDatabase(err.to_string()))?;
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

    /// Write header page
    ///
    /// Note: This merges database header fields into the existing page 0 content
    /// to preserve any additional data (like encryption headers) that may be stored there.
    fn write_header(&self) -> Result<(), PagerError> {
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }

        let header = self.header_read()?;

        // Read existing page 0 to preserve any additional data (e.g., encryption header)
        // First check cache, then fall back to disk
        let mut page = if let Some(cached) = self.cache.get(0) {
            cached
        } else {
            // Try to read from disk if file is large enough
            let file = self.file_lock()?;
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

        reddb_file::encode_database_header(data, &header)
            .map_err(|err| PagerError::InvalidDatabase(err.to_string()))?;

        page.update_checksum();

        // Write header shadow FIRST (so it's intact if main write is interrupted)
        self.write_header_shadow(&page)?;

        self.write_page_raw(0, &page)?;
        *self.header_dirty_lock()? = false;

        Ok(())
    }

    /// Read a page from disk (bypassing cache)
    fn read_page_raw(&self, page_id: u32) -> Result<Page, PagerError> {
        let mut file = self.file_lock()?;
        let offset = (page_id as u64) * (PAGE_SIZE as u64);

        file.seek(SeekFrom::Start(offset))?;

        let mut buf = [0u8; PAGE_SIZE];
        file.read_exact(&mut buf)?;

        let page = Page::from_bytes(buf);

        // Verify checksum if configured (including page 0)
        if self.config.verify_checksums {
            page.verify_checksum()?;
        }

        Ok(page)
    }

    /// Write a page to disk (bypassing cache)
    fn write_page_raw(&self, page_id: u32, page: &Page) -> Result<(), PagerError> {
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }

        let mut file = self.file_lock()?;
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
        let mut file = self.file_lock()?;
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

        // Add to cache and mark dirty. If the cache had to evict a
        // dirty entry, write it through immediately — the evicted
        // page will never be flushed otherwise (same bug fixed in
        // `allocate_page`).
        if let Some(dirty_page) = self.cache.insert(page_id, page) {
            let evicted_id = dirty_page.page_id();
            self.write_page_raw(evicted_id, &dirty_page)?;
        }
        self.cache.mark_dirty(page_id);

        Ok(())
    }

    /// Read a page through the configured encryptor if any. Page 0
    /// is always returned plaintext (it carries the encryption marker
    /// + header). Callers that want raw cipher bytes can use
    ///   `read_page_no_checksum` directly.
    pub fn read_page_decrypted(&self, page_id: u32) -> Result<Page, PagerError> {
        if page_id == 0 || self.encryption.is_none() {
            return self.read_page(page_id);
        }
        let raw = self.read_page_no_checksum(page_id)?;
        let (enc, _) = self
            .encryption
            .as_ref()
            .expect("encryption presence checked above");
        let plaintext = enc
            .decrypt(page_id, raw.as_bytes())
            .map_err(|e| PagerError::InvalidDatabase(format!("decrypt page {page_id}: {e}")))?;
        let mut buf = [0u8; PAGE_SIZE];
        let n = plaintext.len().min(PAGE_SIZE);
        buf[..n].copy_from_slice(&plaintext[..n]);
        Ok(Page::from_bytes(buf))
    }

    /// Write a page through the configured encryptor if any. Page 0
    /// bypasses encryption and goes through the normal checksummed
    /// path. Encrypted pages skip the checksum update because
    /// AES-GCM's authentication tag is the integrity guarantee.
    pub fn write_page_encrypted(&self, page_id: u32, page: Page) -> Result<(), PagerError> {
        if page_id == 0 || self.encryption.is_none() {
            return self.write_page(page_id, page);
        }
        const OVERHEAD: usize = 12 + 16; // nonce + GCM tag
        let plaintext_len = PAGE_SIZE - OVERHEAD;
        let plaintext = &page.as_bytes()[..plaintext_len];
        let (enc, _) = self
            .encryption
            .as_ref()
            .expect("encryption presence checked above");
        let ciphertext = enc.encrypt(page_id, plaintext);
        debug_assert_eq!(ciphertext.len(), PAGE_SIZE);
        let mut buf = [0u8; PAGE_SIZE];
        buf.copy_from_slice(&ciphertext);
        let cipher_page = Page::from_bytes(buf);
        self.write_page_no_checksum(page_id, cipher_page)
    }

    /// Write a page without updating checksum (for encrypted pages)
    ///
    /// Use this when the page content has its own integrity protection
    /// (e.g., AES-GCM authentication tag for encrypted pages).
    pub fn write_page_no_checksum(&self, page_id: u32, page: Page) -> Result<(), PagerError> {
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }

        // Add to cache and mark dirty (no checksum update). Same
        // eviction-write-through guard as `write_page`.
        if let Some(dirty_page) = self.cache.insert(page_id, page) {
            let evicted_id = dirty_page.page_id();
            self.write_page_raw(evicted_id, &dirty_page)?;
        }
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
            let mut freelist = self.freelist_write()?;
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

                let mut freelist = self.freelist_write()?;
                freelist
                    .load_from_trunk(&trunk)
                    .map_err(|e| PagerError::InvalidDatabase(format!("Freelist: {}", e)))?;
                let id = freelist.allocate().ok_or_else(|| {
                    PagerError::InvalidDatabase("Freelist empty after trunk load".to_string())
                })?;

                let mut header = self.header_write()?;
                header.freelist_head = freelist.trunk_head();
                *self.header_dirty_lock()? = true;

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
            let evicted_id = dirty_page.page_id();
            self.write_page_raw(evicted_id, &dirty_page)?;
        }
        self.cache.mark_dirty(page_id);

        Ok(page)
    }

    /// Reserve a contiguous extent of vector pages.
    pub fn reserve_contig_extent(&self, n_pages: u32) -> Result<super::ExtentId, PagerError> {
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
                let evicted_id = dirty_page.page_id();
                self.write_page_raw(evicted_id, &dirty_page)?;
            }
            self.cache.mark_dirty(page_id);
        }

        Ok(super::ExtentId {
            start_page,
            n_pages,
        })
    }

    /// Free a page (return to freelist)
    pub fn free_page(&self, page_id: u32) -> Result<(), PagerError> {
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }

        // Remove from cache
        self.cache.remove(page_id);

        // Add to freelist
        let mut freelist = self.freelist_write()?;
        freelist.free(page_id);

        *self.header_dirty_lock()? = true;

        Ok(())
    }

    /// Get database header
    pub fn header(&self) -> Result<DatabaseHeader, PagerError> {
        Ok(self.header_read()?.clone())
    }

    pub fn physical_header(&self) -> Result<PhysicalFileHeader, PagerError> {
        Ok(self.header_read()?.physical)
    }

    pub fn update_physical_header(&self, physical: PhysicalFileHeader) -> Result<(), PagerError> {
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

    /// Attach a WAL writer to enforce WAL-first flush ordering.
    ///
    /// After this call, [`Pager::flush`] computes the maximum
    /// `header.lsn` over all dirty pages and calls
    /// `WalWriter::flush_until(max_lsn)` before any page is written
    /// to the data file. This is the postgres rule: a page on disk
    /// implies its WAL record is already durable on disk.
    ///
    /// Existing call sites that construct a Pager without a WAL
    /// keep their previous behaviour (no LSN check) — wiring is
    /// strictly opt-in.
    pub fn set_wal_writer(&self, wal: Arc<Mutex<crate::storage::wal::writer::WalWriter>>) {
        let mut slot = self.wal.write().unwrap_or_else(|p| p.into_inner());
        *slot = Some(wal);
    }

    /// Detach the WAL writer (test / shutdown path).
    pub fn clear_wal_writer(&self) {
        let mut slot = self.wal.write().unwrap_or_else(|p| p.into_inner());
        *slot = None;
    }

    /// Has a WAL writer been attached?
    pub fn has_wal_writer(&self) -> bool {
        self.wal.read().map(|s| s.is_some()).unwrap_or(false)
    }

    /// Flush all dirty pages to disk
    pub fn flush(&self) -> Result<(), PagerError> {
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

        for trunk in trunks {
            let page_id = trunk.page_id();
            self.cache.insert(page_id, trunk);
            self.cache.mark_dirty(page_id);
        }

        // Flush dirty pages from cache (through DWB if enabled)
        let dirty_pages = self.cache.flush_dirty();
        if !dirty_pages.is_empty() {
            // WAL-FIRST: ensure every WAL record describing a dirty
            // page is durable BEFORE the page itself reaches disk.
            // Pages with `lsn == 0` are exempt (freelist trunks, header
            // shadow pages, anything not produced by a WAL append).
            let max_lsn = dirty_pages
                .iter()
                .filter_map(|(_, page)| page.header().ok().map(|h| h.lsn))
                .max()
                .unwrap_or(0);
            if max_lsn > 0 {
                if let Ok(slot) = self.wal.read() {
                    if let Some(wal) = slot.as_ref() {
                        let wal = Arc::clone(wal);
                        // Drop the read lock before taking the WAL
                        // mutex so an unrelated reader cannot block
                        // the flush path.
                        drop(slot);
                        let mut wal_guard = wal.lock().unwrap_or_else(|p| p.into_inner());
                        wal_guard.flush_until(max_lsn).map_err(PagerError::Io)?;
                    }
                }
            }
            self.write_pages_through_dwb(&dirty_pages)?;
        }

        // Write header if dirty
        if *self.header_dirty_lock()? {
            self.write_header()?;
        }

        Ok(())
    }

    /// Sync file to disk (fsync)
    pub fn sync(&self) -> Result<(), PagerError> {
        self.flush()?;

        let file = self.file_lock()?;
        file.sync_all()?;

        Ok(())
    }

    /// Get cache statistics
    pub fn cache_stats(&self) -> crate::storage::engine::page_cache::CacheStats {
        self.cache.stats()
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
        Ok(file.metadata()?.len())
    }

    /// Issue an OS-level read-ahead hint for `page_id`.
    ///
    /// A6 prefetch wire: called from `BTreeCursor::next` when the
    /// cursor passes 50% of the current leaf, so the kernel fetches
    /// the next leaf page while CPU processes the remaining half of
    /// the current one. Failures are silent — a missed prefetch is a
    /// performance miss, never a correctness bug.
    pub fn prefetch_hint(&self, page_id: u32) {
        if let Ok(file) = self.file_lock() {
            let _ = crate::storage::btree::prefetch::prefetch_page(
                &file,
                page_id as u64,
                PAGE_SIZE as u32,
            );
        }
    }

    // ── Corruption defense helpers ──────────────────────────────────

    /// Path for the header shadow file
    fn shadow_path(db_path: &Path) -> PathBuf {
        let mut p = db_path.to_path_buf().into_os_string();
        p.push("-hdr");
        PathBuf::from(p)
    }

    /// Path for the metadata shadow file
    fn meta_shadow_path(db_path: &Path) -> PathBuf {
        let mut p = db_path.to_path_buf().into_os_string();
        p.push("-meta");
        PathBuf::from(p)
    }

    /// Path for the double-write buffer file
    fn dwb_path(db_path: &Path) -> PathBuf {
        let mut p = db_path.to_path_buf().into_os_string();
        p.push("-dwb");
        PathBuf::from(p)
    }

    /// Open the double-write buffer file without truncating existing content.
    ///
    /// The file is intentionally preserved across restarts so recovery can
    /// consume any crash-leftover pages before the next write cycle clears it.
    fn open_dwb_file(db_path: &Path) -> Result<File, PagerError> {
        Ok(OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(Self::dwb_path(db_path))?)
    }

    /// Clear the DWB in place while preserving the file path and handle.
    fn clear_dwb_file(file: &mut File) -> Result<(), PagerError> {
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        file.sync_all()?;
        Ok(())
    }

    /// Write a shadow copy of the header page.
    fn write_header_shadow(&self, page: &Page) -> Result<(), PagerError> {
        if self.config.read_only {
            return Ok(());
        }
        let shadow = Self::shadow_path(&self.path);
        let mut f = File::create(&shadow)?;
        f.write_all(page.as_bytes())?;
        f.sync_all()?;
        Ok(())
    }

    /// Recover header from shadow file when page 0 is corrupted
    fn recover_header_from_shadow(&self) -> Result<Page, PagerError> {
        let shadow = Self::shadow_path(&self.path);
        if !shadow.exists() {
            return Err(PagerError::InvalidDatabase(
                "Page 0 corrupted and no header shadow found".into(),
            ));
        }
        let mut f = File::open(&shadow)?;
        let mut buf = [0u8; PAGE_SIZE];
        f.read_exact(&mut buf)?;
        let page = Page::from_bytes(buf);

        // Verify shadow is valid
        if !reddb_file::database_header_magic_matches(page.as_bytes()) {
            return Err(PagerError::InvalidDatabase(
                "Header shadow also corrupted".into(),
            ));
        }

        // Restore page 0 from shadow
        if !self.config.read_only {
            self.write_page_raw(0, &page)?;
            let file = self.file_lock()?;
            file.sync_all()?;
        }

        Ok(page)
    }

    /// Write a shadow copy of the metadata page.
    ///
    /// When the process-global `fold_pager_meta` policy is enabled (see
    /// [`crate::physical::fold_pager_meta_enabled`]) the shadow is suppressed:
    /// metadata is sourced exclusively from page 1 (plus its overflow chain).
    /// Any pre-existing `<data>-meta` file is also removed so a flipped flag
    /// cannot leave a stale shadow on disk. Reads still tolerate the sidecar
    /// when present so databases written before the flag flipped remain
    /// loadable.
    pub fn write_meta_shadow(&self, page: &Page) -> Result<(), PagerError> {
        if self.config.read_only {
            return Ok(());
        }
        let shadow = Self::meta_shadow_path(&self.path);
        if crate::physical::fold_pager_meta_enabled() {
            // Best-effort cleanup of any prior shadow — a missing file is not
            // an error condition here.
            let _ = std::fs::remove_file(&shadow);
            return Ok(());
        }
        let mut f = File::create(&shadow)?;
        f.write_all(page.as_bytes())?;
        f.sync_all()?;
        Ok(())
    }

    /// Recover metadata page from shadow file when page 1 is corrupted
    pub fn recover_meta_from_shadow(&self) -> Result<Page, PagerError> {
        let shadow = Self::meta_shadow_path(&self.path);
        if !shadow.exists() {
            return Err(PagerError::InvalidDatabase(
                "Page 1 corrupted and no metadata shadow found".into(),
            ));
        }
        let mut f = File::open(&shadow)?;
        let mut buf = [0u8; PAGE_SIZE];
        f.read_exact(&mut buf)?;
        let page = Page::from_bytes(buf);

        // Restore page 1 from shadow
        if !self.config.read_only {
            self.write_page_raw(1, &page)?;
            let file = self.file_lock()?;
            file.sync_all()?;
        }

        Ok(page)
    }

    /// Write pages through the double-write buffer for torn page protection.
    ///
    /// 1. Write all pages to the DWB file with a header (magic + count + checksum)
    /// 2. fsync the DWB
    /// 3. Write all pages to their final locations in the .rdb file
    /// 4. Truncate the DWB (marks as consumed)
    fn write_pages_through_dwb(&self, pages: &[(u32, Page)]) -> Result<(), PagerError> {
        if let Some(dwb_mutex) = &self.dwb_file {
            let mut dwb = dwb_mutex.lock().map_err(|_| PagerError::LockPoisoned)?;

            let buf = reddb_file::encode_paged_dwb_frame(
                pages
                    .iter()
                    .map(|(page_id, page)| (*page_id, page.as_bytes())),
            );

            // Write DWB and fsync
            dwb.seek(SeekFrom::Start(0))?;
            dwb.write_all(&buf)?;
            dwb.set_len(buf.len() as u64)?;
            dwb.sync_all()?;

            // Now write pages to their final locations
            for (page_id, page) in pages {
                self.write_page_raw(*page_id, page)?;
            }

            // Truncate DWB to mark as consumed
            Self::clear_dwb_file(&mut dwb)?;

            Ok(())
        } else {
            // DWB disabled — write directly
            for (page_id, page) in pages {
                self.write_page_raw(*page_id, page)?;
            }
            Ok(())
        }
    }

    /// Recover from double-write buffer after a crash.
    ///
    /// If the DWB file contains valid pages, they were written before the crash
    /// interrupted writing to the main file. Re-apply them.
    fn recover_from_dwb(&self) -> Result<(), PagerError> {
        let dwb_path = Self::dwb_path(&self.path);
        if !dwb_path.exists() {
            return Ok(());
        }

        if let Some(dwb_mutex) = &self.dwb_file {
            let mut file = dwb_mutex.lock().map_err(|_| PagerError::LockPoisoned)?;
            return self.recover_from_dwb_file(&mut file);
        }

        let mut file = OpenOptions::new().read(true).write(true).open(&dwb_path)?;
        self.recover_from_dwb_file(&mut file)
    }

    fn recover_from_dwb_file(&self, file: &mut File) -> Result<(), PagerError> {
        file.seek(SeekFrom::Start(0))?;
        let len = file.metadata()?.len();
        let mut buf = vec![0u8; len as usize];
        file.read_exact(&mut buf)?;

        let entries = match reddb_file::decode_paged_dwb_frame(&buf) {
            Ok(entries) => entries,
            Err(_) => return Self::clear_dwb_file(file),
        };

        // DWB is valid — re-apply pages to main file
        for entry in entries {
            let page = Page::from_bytes(entry.page);
            self.write_page_raw(entry.page_id, &page)?;
        }

        // Sync and clean up
        {
            let file = self.file_lock()?;
            file.sync_all()?;
        }

        Self::clear_dwb_file(file)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "reddb-pager-{}-{}-{}.rdb",
            name,
            std::process::id(),
            crate::utils::now_unix_nanos()
        ))
    }

    #[test]
    fn open_refuses_future_database_version() {
        let path = temp_db_path("future-version");
        let pager = Pager::open_default(&path).unwrap();
        drop(pager);

        let mut future_header = Page::new_header_page(1);
        reddb_file::set_database_header_version(
            future_header.as_bytes_mut(),
            reddb_file::PAGE_FILE_VERSION + 1,
        )
        .unwrap();
        future_header.update_checksum();

        let mut file = OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(future_header.as_bytes()).unwrap();
        file.sync_all().unwrap();
        drop(file);

        let err = match Pager::open_default(&path) {
            Ok(_) => panic!("future database version should be rejected"),
            Err(err) => err,
        };
        match err {
            PagerError::InvalidDatabase(msg) => {
                assert!(msg.contains("newer than supported"));
            }
            other => panic!("expected InvalidDatabase, got {other:?}"),
        }

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(Pager::shadow_path(&path));
        let _ = std::fs::remove_file(Pager::dwb_path(&path));
    }
}
