use super::*;

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

        // Update header
        {
            let mut header = self.header.write().unwrap();
            header.version = version;
            header.page_size = page_size;
            header.page_count = page_count;
            header.freelist_head = freelist_head;
            header.schema_version = schema_version;
            header.checkpoint_lsn = checkpoint_lsn;
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

    pub fn physical_header(&self) -> PhysicalFileHeader {
        self.header.read().unwrap().physical
    }

    pub fn update_physical_header(
        &self,
        physical: PhysicalFileHeader,
    ) -> Result<(), PagerError> {
        if self.config.read_only {
            return Err(PagerError::ReadOnly);
        }

        let mut header = self.header.write().unwrap();
        header.physical = physical;
        *self.header_dirty.lock().unwrap() = true;
        Ok(())
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
