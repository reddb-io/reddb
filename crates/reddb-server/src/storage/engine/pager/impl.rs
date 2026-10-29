use super::*;

pub(super) const BTRFS_SUPER_MAGIC: i64 = 0x9123_683e;
pub(super) const ZFS_SUPER_MAGIC: i64 = 0x2fc1_2fc1;
pub(super) const FS_NOCOW_FL: u64 = 0x0080_0000;

#[cfg(test)]
const PHASE3_DWB_ZONE_FILE_VERSION: u32 = reddb_file::PAGE_FILE_VERSION;

#[cfg(test)]
pub(super) const SUPERBLOCK_SLOT_SIZE: usize = reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE;
#[cfg(test)]
pub(super) const SUPERBLOCK_ZONE_SIZE: usize = reddb_file::PAGED_SUPERBLOCK_ZONE_SIZE;

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
    // `statfs.f_type` is i32 on 32-bit glibc targets (armv7) and i64 on
    // 64-bit ones — widen explicitly so both compile.
    #[allow(clippy::unnecessary_cast)]
    Some(stat.f_type as i64)
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

        // ADR 0038 §4 phase 1 is a clean break: a store still carrying the
        // retired `rdb-hdr`/`rdb-meta` sidecars is never read by the engine.
        // The offline migration tool is the only path forward.
        if exists {
            if let Some(sidecar) = reddb_file::layout::retired::first_present_phase1_sidecar(&path)
            {
                return Err(PagerError::LegacySidecarStore { sidecar });
            }
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

        // ADR 0038 phase 3: the DWB sidecar is retired. When DWB is active,
        // the recovery frame is written to a reserved zone inside the data
        // file and fsynced before in-place page writes. A pre-existing sidecar
        // marks a legacy store that must go through the offline migration tool.
        //
        // gh-895: an explicit `double_write = false` request is honored only
        // when the already-open data file is proven to live on a filesystem
        // with atomic CoW page writes. Unknown and non-CoW filesystems fail
        // closed by keeping the in-file DWB zone enabled.
        let fold_dwb = crate::physical::fold_dwb_into_wal_enabled();
        if exists {
            if let Some(sidecar) =
                reddb_file::layout::retired::first_present_phase3_dwb_sidecar(&path)
            {
                return Err(PagerError::LegacySidecarStore { sidecar });
            }
        }
        if !config.double_write && !config.read_only && !fold_dwb {
            let skip_dwb_on_cow =
                Self::cow_filesystem_has_atomic_page_writes(&path, &file).is_some();
            if !skip_dwb_on_cow {
                tracing::warn!(
                    path = %path.display(),
                    "double_write=false requested, but the data file is not proven to be on \
                     ZFS or btrfs datacow; keeping the in-file double-write buffer enabled"
                );
                config.double_write = true;
            }
        }
        let dwb_enabled = config.double_write && !config.read_only && !fold_dwb;

        let core = SharedPager::open_owned(
            NativePagerFile(file),
            path,
            SharedPagerConfig {
                cache_size: config.cache_size,
                read_only: config.read_only,
                verify_checksums: config.verify_checksums,
                double_write: dwb_enabled,
            },
            NativeWalBarrier::default(),
            !exists,
        )?;
        let mut pager = Self {
            core,
            _lock_file: lock_file,
            config,
            encryption: None,
        };

        if exists {
            pager.bind_encryption_for_existing()?;
        } else {
            // Initialize new database
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
        let (_, image) = self.newest_superblock()?;
        let data = &image[..];
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

        // Stamp the marker + header into the superblock zone if `initialize()`
        // has already published it.
        if self.page_count().unwrap_or(0) > 0 {
            let header_bytes = header.to_bytes();
            self.commit_superblock(|image| {
                reddb_file::write_paged_encryption_marker_and_header(image, &header_bytes)
                    .map_err(|err| PagerError::InvalidDatabase(err.to_string()))
            })?;
        }
        self.encryption = Some((encryptor, header));
        Ok(())
    }

    /// Open with default configuration
    pub fn open_default<P: AsRef<Path>>(path: P) -> Result<Self, PagerError> {
        Self::open(path, PagerConfig::default())
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
        // Canonical per-page envelope overhead (nonce + GCM tag),
        // owned by reddb-io-crypto (#1053, ADR 0054).
        const OVERHEAD: usize = crate::storage::encryption::page_encryptor::OVERHEAD;
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
        let mut slot = self
            .core
            .wal_barrier()
            .0
            .write()
            .unwrap_or_else(|p| p.into_inner());
        *slot = Some(wal);
    }

    /// Detach the WAL writer (test / shutdown path).
    pub fn clear_wal_writer(&self) {
        let mut slot = self
            .core
            .wal_barrier()
            .0
            .write()
            .unwrap_or_else(|p| p.into_inner());
        *slot = None;
    }

    /// Has a WAL writer been attached?
    pub fn has_wal_writer(&self) -> bool {
        self.core
            .wal_barrier()
            .0
            .read()
            .map(|s| s.is_some())
            .unwrap_or(false)
    }

    /// Install an ownership recorder while all other mutators are fenced.
    #[allow(dead_code)] // Used when ownership recovery is activated.
    pub(crate) fn install_ownership_recorder(
        &self,
        recorder: Arc<dyn super::PageOwnershipRecorder>,
    ) -> std::io::Result<()> {
        let mut slot = self
            .core
            .wal_barrier()
            .1
            .write()
            .map_err(|_| std::io::Error::other("ownership recorder lock poisoned"))?;
        if slot.is_some() {
            return Err(std::io::Error::other(
                "ownership recorder already installed",
            ));
        }
        *slot = Some(recorder);
        Ok(())
    }

    /// Issue a best-effort OS read-ahead hint under the page-I/O lock.
    pub fn prefetch_hint(&self, page_id: u32) {
        let _ = self.core.with_file(|file| {
            super::super::prefetch::prefetch_page(&file.0, u64::from(page_id), PAGE_SIZE as u32)
        });
    }

    #[cfg(test)]
    fn dwb_path(path: &Path) -> PathBuf {
        reddb_file::layout::pager_dwb_shadow_path(path)
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

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(Pager::dwb_path(path));
    }

    fn read_zone(path: &Path) -> Vec<u8> {
        let mut zone = vec![0u8; SUPERBLOCK_ZONE_SIZE];
        let mut file = File::open(path).expect("open() should succeed");
        file.read_exact(&mut zone)
            .expect("read_exact() should succeed");
        zone
    }

    /// Overwrite one superblock slot in place, leaving the sibling untouched.
    fn poke_slot(path: &Path, copy_index: usize, bytes: &[u8]) {
        let mut file = OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open() should succeed");
        file.seek(SeekFrom::Start(reddb_file::paged_superblock_slot_offset(
            copy_index,
        )))
        .expect("operation should succeed");
        file.write_all(bytes).expect("write_all() should succeed");
        file.sync_all().expect("sync_all() should succeed");
    }

    fn stamp_pre_phase3_layout(path: &Path, page_count: u32) {
        let zone = read_zone(path);
        for copy_index in 0..reddb_file::PAGED_SUPERBLOCK_SLOT_COUNT {
            let start = copy_index * SUPERBLOCK_SLOT_SIZE;
            let mut slot = zone[start..start + SUPERBLOCK_SLOT_SIZE].to_vec();
            let generation = reddb_file::paged_superblock_slot_generation(&slot, copy_index)
                .expect("fresh store should publish both slots");
            reddb_file::set_database_header_version(&mut slot, PHASE3_DWB_ZONE_FILE_VERSION - 1)
                .expect("set_database_header_version() should succeed");
            reddb_file::set_database_header_page_count(&mut slot, page_count)
                .expect("set_database_header_page_count() should succeed");
            reddb_file::seal_paged_superblock_slot(&mut slot, copy_index, generation)
                .expect("seal_paged_superblock_slot() should succeed");
            poke_slot(path, copy_index, &slot);
        }
    }

    #[test]
    fn open_refuses_future_database_version() {
        let path = temp_db_path("future-version");
        let pager = Pager::open_default(&path).expect("open_default() should succeed");
        drop(pager);

        // A *valid* superblock slot — correct magic, copy index and CRC — whose
        // database header names a version we cannot read. The slot is intact;
        // the header is from the future.
        let selection = reddb_file::select_paged_superblock(&read_zone(&path))
            .expect("operation should succeed");
        let mut slot = vec![0u8; SUPERBLOCK_SLOT_SIZE];
        reddb_file::init_database_header_page(&mut slot, 1)
            .expect("init_database_header_page() should succeed");
        reddb_file::set_database_header_version(&mut slot, reddb_file::PAGE_FILE_VERSION + 1)
            .expect("operation should succeed");
        reddb_file::seal_paged_superblock_slot(
            &mut slot,
            selection.copy_index,
            selection.generation + 1,
        )
        .expect("operation should succeed");
        poke_slot(&path, selection.copy_index, &slot);

        let err = match Pager::open_default(&path) {
            Ok(_) => panic!("future database version should be rejected"),
            Err(err) => err,
        };
        match err {
            PagerError::InvalidDatabase(msg) => {
                assert!(msg.contains("newer than supported"), "{msg}");
            }
            other => panic!("expected InvalidDatabase, got {other:?}"),
        }

        cleanup(&path);
    }

    #[test]
    fn a_fresh_store_publishes_both_superblock_copies() {
        // The ping-pong invariant must hold from the very first update, which
        // means creation cannot leave one copy blank.
        let path = temp_db_path("fresh-pair");
        let pager = Pager::open_default(&path).expect("open_default() should succeed");
        drop(pager);

        let zone = read_zone(&path);
        for copy_index in 0..reddb_file::PAGED_SUPERBLOCK_SLOT_COUNT {
            let start = copy_index * SUPERBLOCK_SLOT_SIZE;
            assert!(
                reddb_file::paged_superblock_slot_generation(
                    &zone[start..start + SUPERBLOCK_SLOT_SIZE],
                    copy_index
                )
                .is_some(),
                "copy {copy_index} must be valid on a fresh store"
            );
        }
        cleanup(&path);
    }

    #[test]
    fn an_update_writes_the_stale_copy_and_leaves_the_newest_one_intact() {
        let path = temp_db_path("ping-pong");
        let pager = Pager::open_default(&path).expect("open_default() should succeed");
        let before = reddb_file::select_paged_superblock(&read_zone(&path))
            .expect("operation should succeed");

        pager
            .allocate_page(PageType::BTreeLeaf)
            .expect("allocate_page() should succeed");
        pager.sync().expect("sync() should succeed");

        let zone = read_zone(&path);
        let after = reddb_file::select_paged_superblock(&zone)
            .expect("select_paged_superblock() should succeed");
        assert_ne!(
            after.copy_index, before.copy_index,
            "an update must target the stale copy"
        );
        assert!(after.generation > before.generation);

        // The copy that was newest before the update is still readable — this
        // is the byte-level statement of "a crash mid-write always leaves one
        // valid copy".
        let start = before.copy_index * SUPERBLOCK_SLOT_SIZE;
        assert_eq!(
            reddb_file::paged_superblock_slot_generation(
                &zone[start..start + SUPERBLOCK_SLOT_SIZE],
                before.copy_index
            ),
            Some(before.generation)
        );

        drop(pager);
        cleanup(&path);
    }

    #[test]
    fn failed_trunk_fence_is_completed_before_retrying_allocation() {
        let path = temp_db_path("retry-trunk-fence");
        let pager = Pager::open_default(&path).expect("pager");
        let mut pages = Vec::new();
        for _ in 0..8 {
            pages.push(pager.allocate_page(PageType::BTreeLeaf).unwrap().page_id());
        }
        pager.sync().unwrap();
        for id in pages {
            pager.free_page(id).unwrap();
        }
        pager.sync().unwrap();
        let trunk = pager.header().unwrap().freelist_head;
        assert_ne!(trunk, 0);

        // A real write error after the trunk has been read must not turn a
        // subsequent allocation into an unfenced fast-path reuse.
        let writable = pager
            .core
            .with_file(|file| std::mem::replace(&mut file.0, File::open(&path).unwrap()))
            .unwrap();
        assert!(pager.allocate_page(PageType::BTreeLeaf).is_err());
        pager.core.with_file(|file| file.0 = writable).unwrap();
        pager.allocate_page(PageType::BTreeLeaf).unwrap();

        let zone = read_zone(&path);
        for copy in 0..reddb_file::PAGED_SUPERBLOCK_SLOT_COUNT {
            let start = copy * SUPERBLOCK_SLOT_SIZE;
            let header =
                reddb_file::decode_database_header(&zone[start..start + SUPERBLOCK_SLOT_SIZE])
                    .unwrap();
            assert_ne!(
                header.freelist_head, trunk,
                "copy {copy} still owns consumed trunk"
            );
        }
        drop(pager);
        cleanup(&path);
    }

    #[test]
    fn sibling_superblock_recovery_does_not_reallocate_live_reused_pages() {
        let path = temp_db_path("reused-allocator-pair");
        let pager = Pager::open_default(&path).expect("pager");
        let mut pages = Vec::new();
        for _ in 0..32 {
            pages.push(
                pager
                    .allocate_page(PageType::BTreeLeaf)
                    .expect("allocate")
                    .page_id(),
            );
        }
        pager.sync().expect("seed");
        for id in pages {
            pager.free_page(id).expect("free");
        }
        pager.sync().expect("publish free list");
        let mut live = std::collections::HashSet::new();
        for _ in 0..8 {
            let page = pager.allocate_page(PageType::BTreeLeaf).expect("reuse");
            live.insert(page.page_id());
            pager
                .write_page(page.page_id(), page)
                .expect("write valid page");
        }
        pager.sync().expect("publish reused ownership");
        drop(pager);
        let selection = reddb_file::select_paged_superblock(&read_zone(&path)).expect("superblock");
        poke_slot(&path, selection.copy_index, &[0; SUPERBLOCK_SLOT_SIZE]);
        let recovered = Pager::open_default(&path).expect("sibling recovery");
        for _ in 0..64 {
            let id = recovered
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate after recovery")
                .page_id();
            assert!(
                !live.contains(&id),
                "sibling allocator still considers live page {id} free"
            );
        }
        drop(recovered);
        cleanup(&path);
    }

    #[test]
    fn open_recovers_from_the_older_copy_when_the_newest_is_torn() {
        let path = temp_db_path("torn-newest");
        {
            let pager = Pager::open_default(&path).expect("open_default() should succeed");
            pager
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate_page() should succeed");
            pager.sync().expect("sync() should succeed");
        }

        let newest = reddb_file::select_paged_superblock(&read_zone(&path))
            .expect("operation should succeed");
        // Tear the newest copy: a torn write leaves its CRC broken.
        let mut torn = vec![0u8; SUPERBLOCK_SLOT_SIZE];
        torn.copy_from_slice(
            &read_zone(&path)[newest.copy_index * SUPERBLOCK_SLOT_SIZE
                ..newest.copy_index * SUPERBLOCK_SLOT_SIZE + SUPERBLOCK_SLOT_SIZE],
        );
        torn[64] ^= 0xFF;
        poke_slot(&path, newest.copy_index, &torn);

        let pager = Pager::open_default(&path).expect("older copy must root the store");
        assert!(pager.page_count().expect("page_count() should succeed") >= 3);
        drop(pager);
        cleanup(&path);
    }

    #[test]
    fn open_refuses_didactically_when_both_superblock_copies_are_invalid() {
        let path = temp_db_path("both-invalid");
        {
            let _ = Pager::open_default(&path).expect("open_default() should succeed");
        }

        let zone = read_zone(&path);
        for copy_index in 0..reddb_file::PAGED_SUPERBLOCK_SLOT_COUNT {
            let start = copy_index * SUPERBLOCK_SLOT_SIZE;
            let mut slot = zone[start..start + SUPERBLOCK_SLOT_SIZE].to_vec();
            slot[64] ^= 0xFF;
            poke_slot(&path, copy_index, &slot);
        }

        let err = match Pager::open_default(&path) {
            Ok(_) => panic!("a rootless store must not open"),
            Err(err) => err,
        };
        let rendered = err.to_string();
        assert!(matches!(err, PagerError::SuperblockZoneUnrecoverable(_)));
        assert!(rendered.contains("superblock zone"), "{rendered}");
        assert!(rendered.contains("salvage"), "{rendered}");

        cleanup(&path);
    }

    #[test]
    fn open_refuses_a_legacy_sidecar_store_and_names_the_migration_tool() {
        let path = temp_db_path("legacy-sidecar");
        {
            let _ = Pager::open_default(&path).expect("open_default() should succeed");
        }

        let sidecar = reddb_file::layout::retired::pager_header_path_v0(&path);
        std::fs::write(&sidecar, b"legacy header shadow").expect("write() should succeed");

        let err = match Pager::open_default(&path) {
            Ok(_) => panic!("legacy stores need migration first"),
            Err(err) => err,
        };
        let rendered = err.to_string();
        assert!(matches!(err, PagerError::LegacySidecarStore { .. }));
        assert!(rendered.contains("migration tool"), "{rendered}");

        let _ = std::fs::remove_file(&sidecar);
        cleanup(&path);
    }

    #[test]
    fn pre_phase3_large_store_without_sidecar_routes_to_migration() {
        let path = temp_db_path("pre-phase3-large");
        {
            let _ = Pager::open_default(&path).expect("open_default() should succeed");
        }
        stamp_pre_phase3_layout(&path, FIRST_ALLOCATABLE_PAGE);

        let err = match Pager::open_default(&path) {
            Ok(_) => panic!("pre-phase-3 stores need migration first"),
            Err(err) => err,
        };
        let rendered = err.to_string();
        assert!(matches!(err, PagerError::LegacyPagedStore { .. }));
        assert!(rendered.contains("migration tool"), "{rendered}");

        cleanup(&path);
    }

    #[test]
    fn pre_phase3_small_store_without_sidecar_routes_to_migration() {
        let path = temp_db_path("pre-phase3-small");
        {
            let _ = Pager::open_default(&path).expect("open_default() should succeed");
        }
        stamp_pre_phase3_layout(&path, 3);
        OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open() should succeed")
            .set_len(3 * PAGE_SIZE as u64)
            .expect("set_len() should succeed");

        let err = match Pager::open_default(&path) {
            Ok(_) => panic!("pre-phase-3 stores need migration first"),
            Err(err) => err,
        };
        assert!(matches!(err, PagerError::LegacyPagedStore { .. }));

        cleanup(&path);
    }

    #[test]
    fn no_phase_one_sidecar_survives_a_create_write_reopen_cycle() {
        let path = temp_db_path("census");
        {
            let pager = Pager::open_default(&path).expect("open_default() should succeed");
            let page = pager
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate_page() should succeed");
            pager
                .write_page(page.page_id(), page)
                .expect("operation should succeed");
            pager.sync().expect("sync() should succeed");
        }
        {
            let pager = Pager::open_default(&path).expect("open_default() should succeed");
            pager.sync().expect("sync() should succeed");
        }

        assert_eq!(
            reddb_file::layout::retired::first_present_phase1_sidecar(&path),
            None,
            "the pager must never create a retired phase-1 sidecar"
        );
        cleanup(&path);
    }
}
