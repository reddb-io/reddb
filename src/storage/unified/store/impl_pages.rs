use super::*;
use parking_lot::RwLock;

impl UnifiedStore {
    /// Get a reference to the underlying pager (if in paged mode).
    pub fn pager(&self) -> Option<&Arc<Pager>> {
        self.pager.as_ref()
    }

    pub fn with_config(config: UnifiedStoreConfig) -> Self {
        Self {
            config,
            format_version: AtomicU32::new(STORE_VERSION_V2),
            next_entity_id: AtomicU64::new(1),
            collections: RwLock::new(HashMap::new()),
            cross_refs: RwLock::new(HashMap::new()),
            reverse_refs: RwLock::new(HashMap::new()),
            pager: None,
            db_path: None,
            btree_indices: RwLock::new(HashMap::new()),
            context_index: ContextIndex::new(),
            entity_cache: RwLock::new(HashMap::new()),
        }
    }

    /// Open or create a page-based database
    ///
    /// This uses the page engine for ACID durability with B-tree indices.
    /// The database file uses 4KB pages with checksums and efficient caching.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the database file (e.g., "data.rdb")
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let store = UnifiedStore::open("security.rdb")?;
    /// store.create_collection("hosts")?;
    /// // ... operations ...
    /// store.persist()?; // Flush to disk
    /// ```
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        let mut pager_config = PagerConfig::default();
        // Tunables via env — experimental, used by the benchmark harness
        // to compare durability profiles head-to-head with Postgres.
        // REDDB_DOUBLE_WRITE=0 disables the double-write buffer, which
        // otherwise adds two fsyncs per pager flush (one on DWB, one
        // on the main file). With DWB off the pager behaves more like
        // Postgres + full_page_writes=off — we trade torn-page
        // protection for ingest throughput.
        if matches!(
            std::env::var("REDDB_DOUBLE_WRITE").ok().as_deref(),
            Some("0") | Some("false") | Some("off")
        ) {
            pager_config.double_write = false;
        }
        let pager = Pager::open(path, pager_config)
            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;

        let store = Self {
            config: UnifiedStoreConfig::default(),
            format_version: AtomicU32::new(STORE_VERSION_V2),
            next_entity_id: AtomicU64::new(1),
            collections: RwLock::new(HashMap::new()),
            cross_refs: RwLock::new(HashMap::new()),
            reverse_refs: RwLock::new(HashMap::new()),
            pager: Some(Arc::new(pager)),
            db_path: Some(path.to_path_buf()),
            btree_indices: RwLock::new(HashMap::new()),
            context_index: ContextIndex::new(),
            entity_cache: RwLock::new(HashMap::new()),
        };

        // Load existing data from pages if database exists
        store.load_from_pages()?;

        Ok(store)
    }

    /// Load data from page-based storage
    ///
    /// Reads the B-tree indices and reconstructs collections from pages.
    fn load_from_pages(&self) -> Result<(), StoreError> {
        let pager = match &self.pager {
            Some(p) => p,
            None => return Ok(()), // No pager, nothing to load
        };

        // Get page count
        let page_count = pager.page_count().map_err(|e| {
            StoreError::Io(std::io::Error::other(format!(
                "failed to read page count: {}",
                e
            )))
        })?;
        if page_count <= 1 {
            // Empty database (only header page)
            return Ok(());
        }

        // Read metadata from page 1 (collections registry)
        // Falls back to metadata shadow if page 1 is corrupted
        let meta_page_result = pager
            .read_page(1)
            .or_else(|_| pager.recover_meta_from_shadow());
        if let Ok(meta_page) = meta_page_result {
            let data = meta_page.as_bytes();
            // Skip header (32 bytes), read content area
            let content = &data[crate::storage::engine::HEADER_SIZE..];
            if content.len() >= 4 {
                let mut pos = 0;
                let mut format_version = STORE_VERSION_V1;

                if content.len() >= 8 && &content[0..4] == METADATA_MAGIC {
                    format_version =
                        u32::from_le_bytes([content[4], content[5], content[6], content[7]]);
                    pos += 8;
                }

                self.set_format_version(format_version);

                // Collection count
                let collection_count = u32::from_le_bytes([
                    content[pos],
                    content[pos + 1],
                    content[pos + 2],
                    content[pos + 3],
                ]) as usize;
                pos += 4;

                // Read collection names and their B-tree root pages
                for _ in 0..collection_count {
                    if pos + 4 > content.len() {
                        break;
                    }

                    let name_len = u32::from_le_bytes([
                        content[pos],
                        content[pos + 1],
                        content[pos + 2],
                        content[pos + 3],
                    ]) as usize;
                    pos += 4;

                    if pos + name_len + 4 > content.len() {
                        break;
                    }

                    if let Ok(name) = String::from_utf8(content[pos..pos + name_len].to_vec()) {
                        pos += name_len;

                        // Root page ID for this collection's B-tree
                        let root_page = u32::from_le_bytes([
                            content[pos],
                            content[pos + 1],
                            content[pos + 2],
                            content[pos + 3],
                        ]);
                        pos += 4;

                        // Create the collection
                        let _ = self.create_collection(&name);

                        // Load B-tree with root page if it exists
                        if root_page > 0 {
                            let btree = BTree::with_root(Arc::clone(pager), root_page);

                            // Load all entities from B-tree into the collection
                            if let Ok(mut cursor) = btree.cursor_first() {
                                let manager = self.get_collection(&name);
                                while let Ok(Some((key, value))) = cursor.next() {
                                    // Deserialize entity from value bytes
                                    if let Ok(entity) =
                                        Self::deserialize_entity(&value, self.format_version())
                                    {
                                        if let Some(m) = &manager {
                                            let id = entity.id;
                                            if let EntityKind::TableRow { row_id, .. } =
                                                &entity.kind
                                            {
                                                m.register_row_id(*row_id);
                                            }
                                            self.context_index.index_entity(&name, &entity);
                                            let _ = m.insert(entity.clone());
                                            self.register_entity_id(id);
                                            if self.config.auto_index_refs {
                                                self.index_cross_refs(&entity, &name)?;
                                            }
                                        }
                                    }
                                }
                            }

                            // Store the B-tree for future lookups
                            self.btree_indices
                                .write()
                                .insert(name, btree);
                        }
                    } else {
                        pos += name_len + 4;
                    }
                }

                if format_version >= STORE_VERSION_V2 && pos + 4 <= content.len() {
                    let cross_ref_count = u32::from_le_bytes([
                        content[pos],
                        content[pos + 1],
                        content[pos + 2],
                        content[pos + 3],
                    ]) as usize;
                    pos += 4;

                    for _ in 0..cross_ref_count {
                        if pos + 17 > content.len() {
                            break;
                        }
                        let source_id = u64::from_le_bytes([
                            content[pos],
                            content[pos + 1],
                            content[pos + 2],
                            content[pos + 3],
                            content[pos + 4],
                            content[pos + 5],
                            content[pos + 6],
                            content[pos + 7],
                        ]);
                        pos += 8;
                        let target_id = u64::from_le_bytes([
                            content[pos],
                            content[pos + 1],
                            content[pos + 2],
                            content[pos + 3],
                            content[pos + 4],
                            content[pos + 5],
                            content[pos + 6],
                            content[pos + 7],
                        ]);
                        pos += 8;
                        let ref_type = RefType::from_byte(content[pos]);
                        pos += 1;

                        if pos + 4 > content.len() {
                            break;
                        }
                        let name_len = u32::from_le_bytes([
                            content[pos],
                            content[pos + 1],
                            content[pos + 2],
                            content[pos + 3],
                        ]) as usize;
                        pos += 4;
                        if pos + name_len > content.len() {
                            break;
                        }
                        let target_collection =
                            String::from_utf8_lossy(&content[pos..pos + name_len]).to_string();
                        pos += name_len;

                        let source_id = EntityId::new(source_id);
                        let target_id = EntityId::new(target_id);

                        self.cross_refs
                            .write()
                            .entry(source_id)
                            .or_default()
                            .push((target_id, ref_type, target_collection.clone()));

                        if let Some((collection, mut entity)) = self.get_any(source_id) {
                            let exists = entity.cross_refs().iter().any(|xref| {
                                xref.target == target_id
                                    && xref.ref_type == ref_type
                                    && xref.target_collection == target_collection
                            });
                            if !exists {
                                entity.cross_refs_mut().push(CrossRef::new(
                                    source_id,
                                    target_id,
                                    target_collection.clone(),
                                    ref_type,
                                ));
                                if let Some(manager) = self.get_collection(&collection) {
                                    let _ = manager.update(entity);
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Deserialize an entity from binary bytes
    pub(crate) fn deserialize_entity(
        data: &[u8],
        format_version: u32,
    ) -> Result<UnifiedEntity, StoreError> {
        let mut pos = 0;
        Self::read_entity_binary(data, &mut pos, format_version)
            .map_err(|e| StoreError::Serialization(e.to_string()))
    }

    /// Serialize an entity to binary bytes
    pub(crate) fn serialize_entity(entity: &UnifiedEntity, format_version: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        Self::write_entity_binary(&mut buf, entity, format_version);
        buf
    }

    /// Persist all data to page-based storage
    ///
    /// Writes all entities to B-tree pages and flushes to disk.
    /// This provides ACID durability guarantees.
    pub fn persist(&self) -> Result<(), StoreError> {
        let pager = match &self.pager {
            Some(p) => p,
            None => {
                // No pager attached - use binary file fallback if path available
                if let Some(path) = &self.db_path {
                    return self
                        .save_to_file(path)
                        .map_err(|e| StoreError::Serialization(e.to_string()));
                }
                return Err(StoreError::Io(std::io::Error::other(
                    "No pager or path configured for persistence",
                )));
            }
        };

        match pager.read_page(1) {
            Ok(_) => {}
            Err(PagerError::PageNotFound(_)) => {
                let meta_page = pager
                    .allocate_page(crate::storage::engine::PageType::Header)
                    .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
                pager
                    .write_page(meta_page.page_id(), meta_page)
                    .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
            }
            Err(e) => {
                return Err(StoreError::Io(std::io::Error::other(e.to_string())));
            }
        }

        let collections = self.collections.read();
        let mut btree_indices = self.btree_indices.write();

        // Collect collection names and their B-tree root pages
        let mut collection_roots: Vec<(String, u32)> = Vec::new();

        // For each collection, create/populate a B-tree and get its root page
        for (name, manager) in collections.iter() {
            // Get or create B-tree for this collection
            let btree = btree_indices
                .entry(name.clone())
                .or_insert_with(|| BTree::new(Arc::clone(pager)));

            // Insert all entities into the B-tree
            for entity in manager.query_all(|_| true) {
                let key = entity.id.raw().to_be_bytes();
                let value = Self::serialize_entity(&entity, self.format_version());

                // Ignore errors if key already exists (update scenario)
                match btree.insert(&key, &value) {
                    Ok(_) => {}
                    Err(BTreeError::DuplicateKey) => {
                        // Key exists - delete and re-insert for update
                        let _ = btree.delete(&key);
                        let _ = btree.insert(&key, &value);
                    }
                    Err(e) => {
                        return Err(StoreError::Io(std::io::Error::other(format!(
                            "B-tree insert error: {}",
                            e
                        ))));
                    }
                }
            }

            collection_roots.push((name.clone(), btree.root_page_id()));
        }

        // Write collection metadata to page 1
        let mut meta_data = Vec::with_capacity(4096);

        let format_version = STORE_VERSION_V2;
        self.set_format_version(format_version);

        // Metadata header: magic + version + collection count
        meta_data.extend_from_slice(METADATA_MAGIC);
        meta_data.extend_from_slice(&format_version.to_le_bytes());
        meta_data.extend_from_slice(&(collection_roots.len() as u32).to_le_bytes());

        // Write each collection's name and B-tree root page
        for (name, root_page) in &collection_roots {
            // Name length
            meta_data.extend_from_slice(&(name.len() as u32).to_le_bytes());
            // Name
            meta_data.extend_from_slice(name.as_bytes());
            // Root page ID from actual B-tree
            meta_data.extend_from_slice(&root_page.to_le_bytes());
        }

        // Write cross-reference metadata
        let cross_refs = self.cross_refs.read();
        let total_refs: usize = cross_refs.values().map(|v| v.len()).sum();
        meta_data.extend_from_slice(&(total_refs as u32).to_le_bytes());
        for (source_id, refs) in cross_refs.iter() {
            for (target_id, ref_type, collection) in refs {
                meta_data.extend_from_slice(&source_id.raw().to_le_bytes());
                meta_data.extend_from_slice(&target_id.raw().to_le_bytes());
                meta_data.push(ref_type.to_byte());
                meta_data.extend_from_slice(&(collection.len() as u32).to_le_bytes());
                meta_data.extend_from_slice(collection.as_bytes());
            }
        }

        // Create metadata page with Header type
        let mut meta_page = crate::storage::engine::Page::new(
            crate::storage::engine::PageType::Header,
            1, // page_id = 1
        );
        // Copy metadata into page content area (after header)
        let page_data = meta_page.as_bytes_mut();
        let content_start = crate::storage::engine::HEADER_SIZE;
        let copy_len = meta_data.len().min(page_data.len() - content_start);
        page_data[content_start..content_start + copy_len].copy_from_slice(&meta_data[..copy_len]);

        // Write metadata shadow FIRST (intact copy in case main write fails)
        pager
            .write_meta_shadow(&meta_page)
            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;

        // Write page
        pager
            .write_page(1, meta_page)
            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;

        // Flush and fsync all pages to disk
        pager
            .sync()
            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;

        Ok(())
    }

    /// Check if the store is using page-based persistence
    pub fn is_paged(&self) -> bool {
        self.pager.is_some()
    }

    /// Get the database file path (if using paged mode)
    pub fn db_path(&self) -> Option<&Path> {
        self.db_path.as_deref()
    }
}
