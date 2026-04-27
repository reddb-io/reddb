use super::*;

/// DWB file magic: "RDDW"
const DWB_MAGIC: [u8; 4] = [0x52, 0x44, 0x44, 0x57];

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

        // Open double-write buffer file
        let dwb_file = if config.double_write && !config.read_only {
            let f = Self::open_dwb_file(&path)?;
            Some(Mutex::new(f))
        } else {
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
        const ENCRYPTION_MARKER_OFFSET: usize = HEADER_SIZE + 32;
        const ENCRYPTION_MARKER: &[u8; 4] = b"RDBE";

        if self.page_count().unwrap_or(0) == 0 {
            return self.bind_encryption_for_new();
        }
        let header_page = self.read_page_no_checksum(0)?;
        let data = header_page.as_bytes();
        let has_marker = data.len() > ENCRYPTION_MARKER_OFFSET + 4
            && &data[ENCRYPTION_MARKER_OFFSET..ENCRYPTION_MARKER_OFFSET + 4] == ENCRYPTION_MARKER;

        let key = self.config.encryption.clone();
        match (has_marker, key) {
            (true, Some(key)) => {
                let header_start = ENCRYPTION_MARKER_OFFSET + 4;
                let header =
                    crate::storage::encryption::EncryptionHeader::from_bytes(&data[header_start..])
                        .map_err(|e| {
                            PagerError::InvalidDatabase(format!(
                                "encryption header parse failed: {e}"
                            ))
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
        const ENCRYPTION_MARKER_OFFSET: usize = HEADER_SIZE + 32;
        const ENCRYPTION_MARKER: &[u8; 4] = b"RDBE";

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
            data[ENCRYPTION_MARKER_OFFSET..ENCRYPTION_MARKER_OFFSET + 4]
                .copy_from_slice(ENCRYPTION_MARKER);
            let header_bytes = header.to_bytes();
            let header_start = ENCRYPTION_MARKER_OFFSET + 4;
            data[header_start..header_start + header_bytes.len()].copy_from_slice(&header_bytes);
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

        // Create header page
        let header_page = Page::new_header_page(1);

        // Write header page
        self.write_page_raw(0, &header_page)?;

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
                let magic = &page.as_bytes()[HEADER_SIZE..HEADER_SIZE + 4];
                if magic == MAGIC_BYTES {
                    page
                } else {
                    // Page 0 corrupted — try shadow
                    self.recover_header_from_shadow()?
                }
            }
            Err(_) => self.recover_header_from_shadow()?,
        };

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
        if version > DB_VERSION {
            return Err(PagerError::InvalidDatabase(format!(
                "Unsupported database version: file version {version} is newer than supported {DB_VERSION}"
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
        let physical_format_version = u32::from_le_bytes([
            data[HEADER_SIZE + 32],
            data[HEADER_SIZE + 33],
            data[HEADER_SIZE + 34],
            data[HEADER_SIZE + 35],
        ]);
        let physical_sequence = u64::from_le_bytes([
            data[HEADER_SIZE + 36],
            data[HEADER_SIZE + 37],
            data[HEADER_SIZE + 38],
            data[HEADER_SIZE + 39],
            data[HEADER_SIZE + 40],
            data[HEADER_SIZE + 41],
            data[HEADER_SIZE + 42],
            data[HEADER_SIZE + 43],
        ]);
        let manifest_root = u64::from_le_bytes([
            data[HEADER_SIZE + 44],
            data[HEADER_SIZE + 45],
            data[HEADER_SIZE + 46],
            data[HEADER_SIZE + 47],
            data[HEADER_SIZE + 48],
            data[HEADER_SIZE + 49],
            data[HEADER_SIZE + 50],
            data[HEADER_SIZE + 51],
        ]);
        let manifest_oldest_root = u64::from_le_bytes([
            data[HEADER_SIZE + 52],
            data[HEADER_SIZE + 53],
            data[HEADER_SIZE + 54],
            data[HEADER_SIZE + 55],
            data[HEADER_SIZE + 56],
            data[HEADER_SIZE + 57],
            data[HEADER_SIZE + 58],
            data[HEADER_SIZE + 59],
        ]);
        let free_set_root = u64::from_le_bytes([
            data[HEADER_SIZE + 60],
            data[HEADER_SIZE + 61],
            data[HEADER_SIZE + 62],
            data[HEADER_SIZE + 63],
            data[HEADER_SIZE + 64],
            data[HEADER_SIZE + 65],
            data[HEADER_SIZE + 66],
            data[HEADER_SIZE + 67],
        ]);
        let manifest_page = u32::from_le_bytes([
            data[HEADER_SIZE + 68],
            data[HEADER_SIZE + 69],
            data[HEADER_SIZE + 70],
            data[HEADER_SIZE + 71],
        ]);
        let manifest_checksum = u64::from_le_bytes([
            data[HEADER_SIZE + 72],
            data[HEADER_SIZE + 73],
            data[HEADER_SIZE + 74],
            data[HEADER_SIZE + 75],
            data[HEADER_SIZE + 76],
            data[HEADER_SIZE + 77],
            data[HEADER_SIZE + 78],
            data[HEADER_SIZE + 79],
        ]);
        let collection_roots_page = u32::from_le_bytes([
            data[HEADER_SIZE + 80],
            data[HEADER_SIZE + 81],
            data[HEADER_SIZE + 82],
            data[HEADER_SIZE + 83],
        ]);
        let collection_roots_checksum = u64::from_le_bytes([
            data[HEADER_SIZE + 84],
            data[HEADER_SIZE + 85],
            data[HEADER_SIZE + 86],
            data[HEADER_SIZE + 87],
            data[HEADER_SIZE + 88],
            data[HEADER_SIZE + 89],
            data[HEADER_SIZE + 90],
            data[HEADER_SIZE + 91],
        ]);
        let collection_root_count = u32::from_le_bytes([
            data[HEADER_SIZE + 92],
            data[HEADER_SIZE + 93],
            data[HEADER_SIZE + 94],
            data[HEADER_SIZE + 95],
        ]);
        let snapshot_count = u32::from_le_bytes([
            data[HEADER_SIZE + 96],
            data[HEADER_SIZE + 97],
            data[HEADER_SIZE + 98],
            data[HEADER_SIZE + 99],
        ]);
        let index_count = u32::from_le_bytes([
            data[HEADER_SIZE + 100],
            data[HEADER_SIZE + 101],
            data[HEADER_SIZE + 102],
            data[HEADER_SIZE + 103],
        ]);
        let catalog_collection_count = u32::from_le_bytes([
            data[HEADER_SIZE + 104],
            data[HEADER_SIZE + 105],
            data[HEADER_SIZE + 106],
            data[HEADER_SIZE + 107],
        ]);
        let catalog_total_entities = u64::from_le_bytes([
            data[HEADER_SIZE + 108],
            data[HEADER_SIZE + 109],
            data[HEADER_SIZE + 110],
            data[HEADER_SIZE + 111],
            data[HEADER_SIZE + 112],
            data[HEADER_SIZE + 113],
            data[HEADER_SIZE + 114],
            data[HEADER_SIZE + 115],
        ]);
        let export_count = u32::from_le_bytes([
            data[HEADER_SIZE + 116],
            data[HEADER_SIZE + 117],
            data[HEADER_SIZE + 118],
            data[HEADER_SIZE + 119],
        ]);
        let graph_projection_count = u32::from_le_bytes([
            data[HEADER_SIZE + 120],
            data[HEADER_SIZE + 121],
            data[HEADER_SIZE + 122],
            data[HEADER_SIZE + 123],
        ]);
        let analytics_job_count = u32::from_le_bytes([
            data[HEADER_SIZE + 124],
            data[HEADER_SIZE + 125],
            data[HEADER_SIZE + 126],
            data[HEADER_SIZE + 127],
        ]);
        let manifest_event_count = u32::from_le_bytes([
            data[HEADER_SIZE + 128],
            data[HEADER_SIZE + 129],
            data[HEADER_SIZE + 130],
            data[HEADER_SIZE + 131],
        ]);
        let registry_page = u32::from_le_bytes([
            data[HEADER_SIZE + 132],
            data[HEADER_SIZE + 133],
            data[HEADER_SIZE + 134],
            data[HEADER_SIZE + 135],
        ]);
        let registry_checksum = u64::from_le_bytes([
            data[HEADER_SIZE + 136],
            data[HEADER_SIZE + 137],
            data[HEADER_SIZE + 138],
            data[HEADER_SIZE + 139],
            data[HEADER_SIZE + 140],
            data[HEADER_SIZE + 141],
            data[HEADER_SIZE + 142],
            data[HEADER_SIZE + 143],
        ]);
        let recovery_page = u32::from_le_bytes([
            data[HEADER_SIZE + 144],
            data[HEADER_SIZE + 145],
            data[HEADER_SIZE + 146],
            data[HEADER_SIZE + 147],
        ]);
        let recovery_checksum = u64::from_le_bytes([
            data[HEADER_SIZE + 148],
            data[HEADER_SIZE + 149],
            data[HEADER_SIZE + 150],
            data[HEADER_SIZE + 151],
            data[HEADER_SIZE + 152],
            data[HEADER_SIZE + 153],
            data[HEADER_SIZE + 154],
            data[HEADER_SIZE + 155],
        ]);
        let catalog_page = u32::from_le_bytes([
            data[HEADER_SIZE + 156],
            data[HEADER_SIZE + 157],
            data[HEADER_SIZE + 158],
            data[HEADER_SIZE + 159],
        ]);
        let catalog_checksum = u64::from_le_bytes([
            data[HEADER_SIZE + 160],
            data[HEADER_SIZE + 161],
            data[HEADER_SIZE + 162],
            data[HEADER_SIZE + 163],
            data[HEADER_SIZE + 164],
            data[HEADER_SIZE + 165],
            data[HEADER_SIZE + 166],
            data[HEADER_SIZE + 167],
        ]);
        let metadata_state_page = u32::from_le_bytes([
            data[HEADER_SIZE + 168],
            data[HEADER_SIZE + 169],
            data[HEADER_SIZE + 170],
            data[HEADER_SIZE + 171],
        ]);
        let metadata_state_checksum = u64::from_le_bytes([
            data[HEADER_SIZE + 172],
            data[HEADER_SIZE + 173],
            data[HEADER_SIZE + 174],
            data[HEADER_SIZE + 175],
            data[HEADER_SIZE + 176],
            data[HEADER_SIZE + 177],
            data[HEADER_SIZE + 178],
            data[HEADER_SIZE + 179],
        ]);
        let vector_artifact_page = u32::from_le_bytes([
            data[HEADER_SIZE + 180],
            data[HEADER_SIZE + 181],
            data[HEADER_SIZE + 182],
            data[HEADER_SIZE + 183],
        ]);
        let vector_artifact_checksum = u64::from_le_bytes([
            data[HEADER_SIZE + 184],
            data[HEADER_SIZE + 185],
            data[HEADER_SIZE + 186],
            data[HEADER_SIZE + 187],
            data[HEADER_SIZE + 188],
            data[HEADER_SIZE + 189],
            data[HEADER_SIZE + 190],
            data[HEADER_SIZE + 191],
        ]);

        // Two-phase checkpoint fields (offset 192-200)
        let checkpoint_in_progress = data[HEADER_SIZE + 192] != 0;
        let checkpoint_target_lsn = u64::from_le_bytes([
            data[HEADER_SIZE + 193],
            data[HEADER_SIZE + 194],
            data[HEADER_SIZE + 195],
            data[HEADER_SIZE + 196],
            data[HEADER_SIZE + 197],
            data[HEADER_SIZE + 198],
            data[HEADER_SIZE + 199],
            data[HEADER_SIZE + 200],
        ]);

        // Update header
        {
            let mut header = self.header_write()?;
            header.version = version;
            header.page_size = page_size;
            header.page_count = page_count;
            header.freelist_head = freelist_head;
            header.schema_version = schema_version;
            header.checkpoint_lsn = checkpoint_lsn;
            header.checkpoint_in_progress = checkpoint_in_progress;
            header.checkpoint_target_lsn = checkpoint_target_lsn;
            header.physical = PhysicalFileHeader {
                format_version: physical_format_version,
                sequence: physical_sequence,
                manifest_oldest_root,
                manifest_root,
                free_set_root,
                manifest_page,
                manifest_checksum,
                collection_roots_page,
                collection_roots_checksum,
                collection_root_count,
                snapshot_count,
                index_count,
                catalog_collection_count,
                catalog_total_entities,
                export_count,
                graph_projection_count,
                analytics_job_count,
                manifest_event_count,
                registry_page,
                registry_checksum,
                recovery_page,
                recovery_checksum,
                catalog_page,
                catalog_checksum,
                metadata_state_page,
                metadata_state_checksum,
                vector_artifact_page,
                vector_artifact_checksum,
            };
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
        data[HEADER_SIZE + 32..HEADER_SIZE + 36]
            .copy_from_slice(&header.physical.format_version.to_le_bytes());
        data[HEADER_SIZE + 36..HEADER_SIZE + 44]
            .copy_from_slice(&header.physical.sequence.to_le_bytes());
        data[HEADER_SIZE + 44..HEADER_SIZE + 52]
            .copy_from_slice(&header.physical.manifest_root.to_le_bytes());
        data[HEADER_SIZE + 52..HEADER_SIZE + 60]
            .copy_from_slice(&header.physical.manifest_oldest_root.to_le_bytes());
        data[HEADER_SIZE + 60..HEADER_SIZE + 68]
            .copy_from_slice(&header.physical.free_set_root.to_le_bytes());
        data[HEADER_SIZE + 68..HEADER_SIZE + 72]
            .copy_from_slice(&header.physical.manifest_page.to_le_bytes());
        data[HEADER_SIZE + 72..HEADER_SIZE + 80]
            .copy_from_slice(&header.physical.manifest_checksum.to_le_bytes());
        data[HEADER_SIZE + 80..HEADER_SIZE + 84]
            .copy_from_slice(&header.physical.collection_roots_page.to_le_bytes());
        data[HEADER_SIZE + 84..HEADER_SIZE + 92]
            .copy_from_slice(&header.physical.collection_roots_checksum.to_le_bytes());
        data[HEADER_SIZE + 92..HEADER_SIZE + 96]
            .copy_from_slice(&header.physical.collection_root_count.to_le_bytes());
        data[HEADER_SIZE + 96..HEADER_SIZE + 100]
            .copy_from_slice(&header.physical.snapshot_count.to_le_bytes());
        data[HEADER_SIZE + 100..HEADER_SIZE + 104]
            .copy_from_slice(&header.physical.index_count.to_le_bytes());
        data[HEADER_SIZE + 104..HEADER_SIZE + 108]
            .copy_from_slice(&header.physical.catalog_collection_count.to_le_bytes());
        data[HEADER_SIZE + 108..HEADER_SIZE + 116]
            .copy_from_slice(&header.physical.catalog_total_entities.to_le_bytes());
        data[HEADER_SIZE + 116..HEADER_SIZE + 120]
            .copy_from_slice(&header.physical.export_count.to_le_bytes());
        data[HEADER_SIZE + 120..HEADER_SIZE + 124]
            .copy_from_slice(&header.physical.graph_projection_count.to_le_bytes());
        data[HEADER_SIZE + 124..HEADER_SIZE + 128]
            .copy_from_slice(&header.physical.analytics_job_count.to_le_bytes());
        data[HEADER_SIZE + 128..HEADER_SIZE + 132]
            .copy_from_slice(&header.physical.manifest_event_count.to_le_bytes());
        data[HEADER_SIZE + 132..HEADER_SIZE + 136]
            .copy_from_slice(&header.physical.registry_page.to_le_bytes());
        data[HEADER_SIZE + 136..HEADER_SIZE + 144]
            .copy_from_slice(&header.physical.registry_checksum.to_le_bytes());
        data[HEADER_SIZE + 144..HEADER_SIZE + 148]
            .copy_from_slice(&header.physical.recovery_page.to_le_bytes());
        data[HEADER_SIZE + 148..HEADER_SIZE + 156]
            .copy_from_slice(&header.physical.recovery_checksum.to_le_bytes());
        data[HEADER_SIZE + 156..HEADER_SIZE + 160]
            .copy_from_slice(&header.physical.catalog_page.to_le_bytes());
        data[HEADER_SIZE + 160..HEADER_SIZE + 168]
            .copy_from_slice(&header.physical.catalog_checksum.to_le_bytes());
        data[HEADER_SIZE + 168..HEADER_SIZE + 172]
            .copy_from_slice(&header.physical.metadata_state_page.to_le_bytes());
        data[HEADER_SIZE + 172..HEADER_SIZE + 180]
            .copy_from_slice(&header.physical.metadata_state_checksum.to_le_bytes());
        data[HEADER_SIZE + 180..HEADER_SIZE + 184]
            .copy_from_slice(&header.physical.vector_artifact_page.to_le_bytes());
        data[HEADER_SIZE + 184..HEADER_SIZE + 192]
            .copy_from_slice(&header.physical.vector_artifact_checksum.to_le_bytes());

        // Two-phase checkpoint fields (offset 192-200)
        data[HEADER_SIZE + 192] = if header.checkpoint_in_progress { 1 } else { 0 };
        data[HEADER_SIZE + 193..HEADER_SIZE + 201]
            .copy_from_slice(&header.checkpoint_target_lsn.to_le_bytes());

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
    /// `read_page_no_checksum` directly.
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
                &*file,
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

    /// Write a shadow copy of the header page to .rdb-hdr
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
        let magic = &page.as_bytes()[HEADER_SIZE..HEADER_SIZE + 4];
        if magic != MAGIC_BYTES {
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

    /// Write a shadow copy of the metadata page to .rdb-meta
    pub fn write_meta_shadow(&self, page: &Page) -> Result<(), PagerError> {
        if self.config.read_only {
            return Ok(());
        }
        let shadow = Self::meta_shadow_path(&self.path);
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

            // Build DWB content: [magic:4][count:u32][checksum:u32][pages...]
            // Each page entry: [page_id:u32][page_data:4096]
            let entry_size = 4 + PAGE_SIZE; // page_id + data
            let header_len = 4 + 4 + 4; // magic + count + checksum
            let total = header_len + pages.len() * entry_size;
            let mut buf = Vec::with_capacity(total);

            // Header
            buf.extend_from_slice(&DWB_MAGIC);
            buf.extend_from_slice(&(pages.len() as u32).to_le_bytes());
            buf.extend_from_slice(&[0u8; 4]); // placeholder for checksum

            // Page entries
            for (page_id, page) in pages {
                buf.extend_from_slice(&page_id.to_le_bytes());
                buf.extend_from_slice(page.as_bytes());
            }

            // Compute and write checksum over all data after the header
            let checksum = super::super::crc32::crc32(&buf[header_len..]);
            buf[8..12].copy_from_slice(&checksum.to_le_bytes());

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
        if len < 12 {
            // Empty or incomplete header — keep the DWB file but clear stale bytes.
            return Self::clear_dwb_file(file);
        }

        let mut buf = vec![0u8; len as usize];
        file.read_exact(&mut buf)?;

        // Verify magic
        if buf[0..4] != DWB_MAGIC {
            // Not a valid DWB — clear it in place so the same file can be reused.
            return Self::clear_dwb_file(file);
        }

        let count = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
        let stored_checksum = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);

        let header_len = 12;
        let entry_size = 4 + PAGE_SIZE;
        let expected_len = header_len + count * entry_size;

        if buf.len() < expected_len {
            // Incomplete DWB write — discard it in place.
            return Self::clear_dwb_file(file);
        }

        // Verify checksum
        let computed = super::super::crc32::crc32(&buf[header_len..expected_len]);
        if computed != stored_checksum {
            // Corrupted DWB — discard it in place.
            return Self::clear_dwb_file(file);
        }

        // DWB is valid — re-apply pages to main file
        let mut offset = header_len;
        for _ in 0..count {
            let page_id = u32::from_le_bytes([
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            ]);
            offset += 4;

            let mut page_data = [0u8; PAGE_SIZE];
            page_data.copy_from_slice(&buf[offset..offset + PAGE_SIZE]);
            offset += PAGE_SIZE;

            let page = Page::from_bytes(page_data);
            self.write_page_raw(page_id, &page)?;
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
        future_header.as_bytes_mut()[HEADER_SIZE + 4..HEADER_SIZE + 8]
            .copy_from_slice(&(DB_VERSION + 1).to_le_bytes());
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
