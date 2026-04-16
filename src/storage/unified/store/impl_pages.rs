use super::*;
use parking_lot::RwLock;

const ENTITY_RECORD_MAGIC: &[u8; 4] = b"RER1";

impl UnifiedStore {
    pub(crate) fn mark_paged_registry_dirty(&self) {
        self.paged_registry_dirty.store(true, Ordering::Release);
    }

    pub(crate) fn flush_paged_state(&self) -> Result<(), StoreError> {
        let Some(pager) = &self.pager else {
            return Ok(());
        };

        if self.paged_registry_dirty.load(Ordering::Acquire) {
            self.flush_paged_registry()?;
            self.paged_registry_dirty.store(false, Ordering::Release);
            return Ok(());
        }

        pager
            .flush()
            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))
    }

    pub(crate) fn flush_paged_registry(&self) -> Result<(), StoreError> {
        let Some(pager) = &self.pager else {
            return Ok(());
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

        let format_version = STORE_VERSION_V6;
        self.set_format_version(format_version);

        let collections = self.collections.read();
        let btree_indices = self.btree_indices.read();
        let mut collection_roots = Vec::with_capacity(collections.len());
        for (name, _) in collections.iter() {
            let root_page = btree_indices.get(name).map_or(0, BTree::root_page_id);
            collection_roots.push((name.clone(), root_page));
        }
        drop(btree_indices);
        drop(collections);

        let mut meta_data = Vec::with_capacity(4096);
        meta_data.extend_from_slice(METADATA_MAGIC);
        meta_data.extend_from_slice(&format_version.to_le_bytes());
        meta_data.extend_from_slice(&(collection_roots.len() as u32).to_le_bytes());
        for (name, root_page) in &collection_roots {
            meta_data.extend_from_slice(&(name.len() as u32).to_le_bytes());
            meta_data.extend_from_slice(name.as_bytes());
            meta_data.extend_from_slice(&root_page.to_le_bytes());
        }

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

        let mut meta_page =
            crate::storage::engine::Page::new(crate::storage::engine::PageType::Header, 1);
        let page_data = meta_page.as_bytes_mut();
        let content_start = crate::storage::engine::HEADER_SIZE;
        let copy_len = meta_data.len().min(page_data.len() - content_start);
        page_data[content_start..content_start + copy_len].copy_from_slice(&meta_data[..copy_len]);

        pager
            .write_meta_shadow(&meta_page)
            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
        pager
            .write_page(1, meta_page)
            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
        pager
            .flush()
            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;

        Ok(())
    }

    /// Get a reference to the underlying pager (if in paged mode).
    pub fn pager(&self) -> Option<&Arc<Pager>> {
        self.pager.as_ref()
    }

    pub fn with_config(config: UnifiedStoreConfig) -> Self {
        Self {
            config,
            format_version: AtomicU32::new(STORE_VERSION_V6),
            next_entity_id: AtomicU64::new(1),
            collections: RwLock::new(HashMap::new()),
            cross_refs: RwLock::new(HashMap::new()),
            reverse_refs: RwLock::new(HashMap::new()),
            pager: None,
            db_path: None,
            btree_indices: RwLock::new(HashMap::new()),
            context_index: ContextIndex::new(),
            entity_cache: RwLock::new(HashMap::new()),
            graph_label_index: RwLock::new(HashMap::new()),
            paged_registry_dirty: AtomicBool::new(false),
            commit: None,
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
        Self::open_with_config(path, UnifiedStoreConfig::default())
    }

    pub fn open_with_config(
        path: impl AsRef<Path>,
        config: UnifiedStoreConfig,
    ) -> Result<Self, StoreError> {
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

        let wal_path = Self::wal_path_for_db(path);
        let commit = if StoreCommitCoordinator::should_open(&wal_path, config.durability_mode) {
            Some(Arc::new(
                StoreCommitCoordinator::open(wal_path, config.durability_mode, config.group_commit)
                    .map_err(StoreError::Io)?,
            ))
        } else {
            None
        };

        let store = Self {
            config,
            format_version: AtomicU32::new(STORE_VERSION_V6),
            next_entity_id: AtomicU64::new(1),
            collections: RwLock::new(HashMap::new()),
            cross_refs: RwLock::new(HashMap::new()),
            reverse_refs: RwLock::new(HashMap::new()),
            pager: Some(Arc::new(pager)),
            db_path: Some(path.to_path_buf()),
            btree_indices: RwLock::new(HashMap::new()),
            context_index: ContextIndex::new(),
            entity_cache: RwLock::new(HashMap::new()),
            graph_label_index: RwLock::new(HashMap::new()),
            paged_registry_dirty: AtomicBool::new(false),
            commit,
        };

        // Load existing data from pages if database exists
        store.load_from_pages()?;
        if let Some(commit) = &store.commit {
            commit.replay_into(&store).map_err(StoreError::Io)?;
        }

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

                        // Hydrate the collection in memory only. Loading must
                        // not emit WAL entries or rewrite the on-disk registry
                        // before the existing B-tree roots are attached.
                        let _ = self.create_collection_in_memory(&name);

                        // Load B-tree with root page if it exists
                        if root_page > 0 {
                            let btree = BTree::with_root(Arc::clone(pager), root_page);

                            // Load all entities from B-tree into the collection
                            if let Ok(mut cursor) = btree.cursor_first() {
                                let manager = self.get_collection(&name);
                                while let Ok(Some((key, value))) = cursor.next() {
                                    // Deserialize entity from value bytes
                                    if let Ok((entity, metadata)) = Self::deserialize_entity_record(
                                        &value,
                                        self.format_version(),
                                    ) {
                                        if let Some(m) = &manager {
                                            let id = entity.id;
                                            if let EntityKind::TableRow { row_id, .. } =
                                                &entity.kind
                                            {
                                                m.register_row_id(*row_id);
                                            }
                                            self.context_index.index_entity(&name, &entity);
                                            let _ = m.insert(entity.clone());
                                            if let Some(metadata) = metadata {
                                                let _ = m.set_metadata(id, metadata);
                                            }
                                            self.register_entity_id(id);
                                            if self.config.auto_index_refs {
                                                self.index_cross_refs(&entity, &name)?;
                                            }
                                        }
                                    }
                                }
                            }

                            // Store the B-tree for future lookups
                            self.btree_indices.write().insert(name, btree);
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

                        self.cross_refs.write().entry(source_id).or_default().push((
                            target_id,
                            ref_type,
                            target_collection.clone(),
                        ));

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

        if self.format_version() < STORE_VERSION_V6 {
            self.set_format_version(STORE_VERSION_V6);
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

    pub(crate) fn serialize_entity_record(
        entity: &UnifiedEntity,
        metadata: Option<&Metadata>,
        format_version: u32,
    ) -> Vec<u8> {
        let entity_bytes = Self::serialize_entity(entity, format_version);
        let metadata_bytes = serialize_metadata(metadata);
        let mut buf = Vec::with_capacity(12 + entity_bytes.len() + metadata_bytes.len());
        buf.extend_from_slice(ENTITY_RECORD_MAGIC);
        buf.extend_from_slice(&(entity_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(&entity_bytes);
        buf.extend_from_slice(&(metadata_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(&metadata_bytes);
        buf
    }

    pub(crate) fn deserialize_entity_record(
        data: &[u8],
        format_version: u32,
    ) -> Result<(UnifiedEntity, Option<Metadata>), StoreError> {
        if data.len() < 8 || &data[..4] != ENTITY_RECORD_MAGIC {
            return Self::deserialize_entity(data, format_version).map(|entity| (entity, None));
        }

        let mut pos = 4usize;
        let entity_len = read_u32(data, &mut pos)? as usize;
        if pos + entity_len > data.len() {
            return Err(StoreError::Serialization(
                "truncated entity record payload".to_string(),
            ));
        }
        let entity = Self::deserialize_entity(&data[pos..pos + entity_len], format_version)?;
        pos += entity_len;

        let metadata_len = read_u32(data, &mut pos)? as usize;
        if pos + metadata_len > data.len() {
            return Err(StoreError::Serialization(
                "truncated entity record metadata".to_string(),
            ));
        }
        let metadata = if metadata_len == 0 {
            None
        } else {
            let metadata = deserialize_metadata(&data[pos..pos + metadata_len])?;
            if metadata.is_empty() {
                None
            } else {
                Some(metadata)
            }
        };

        Ok((entity, metadata))
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

        if let Some(commit) = &self.commit {
            commit.force_sync().map_err(StoreError::Io)?;
        }

        let collections = self.collections.read();
        let mut btree_indices = self.btree_indices.write();

        // Collect collection names and their B-tree root pages
        let mut collection_roots: Vec<(String, u32)> = Vec::new();

        // For each collection, rebuild the B-tree from the live manager state.
        // A checkpoint must preserve deletes too, not just upsert the current rows.
        for (name, manager) in collections.iter() {
            let btree = btree_indices
                .entry(name.clone())
                .or_insert_with(|| BTree::new(Arc::clone(pager)));

            let mut existing_keys = Vec::new();
            if !btree.is_empty() {
                let mut cursor = btree.cursor_first().map_err(|e| {
                    StoreError::Io(std::io::Error::other(format!(
                        "B-tree cursor error while rebuilding '{name}': {e}"
                    )))
                })?;
                while let Some((key, _)) = cursor.next().map_err(|e| {
                    StoreError::Io(std::io::Error::other(format!(
                        "B-tree scan error while rebuilding '{name}': {e}"
                    )))
                })? {
                    existing_keys.push(key);
                }
            }

            for key in existing_keys {
                btree.delete(&key).map_err(|e| {
                    StoreError::Io(std::io::Error::other(format!(
                        "B-tree delete error while rebuilding '{name}': {e}"
                    )))
                })?;
            }

            let mut records: Vec<(Vec<u8>, Vec<u8>)> = manager
                .query_all(|_| true)
                .into_iter()
                .map(|entity| {
                    let metadata = manager.get_metadata(entity.id);
                    (
                        entity.id.raw().to_be_bytes().to_vec(),
                        Self::serialize_entity_record(
                            &entity,
                            metadata.as_ref(),
                            self.format_version(),
                        ),
                    )
                })
                .collect();
            records.sort_by(|left, right| left.0.cmp(&right.0));

            if !records.is_empty() {
                btree.bulk_insert_sorted(&records).map_err(|e| {
                    StoreError::Io(std::io::Error::other(format!(
                        "B-tree bulk rebuild error for '{name}': {e}"
                    )))
                })?;
            }

            collection_roots.push((name.clone(), btree.root_page_id()));
        }

        // Write collection metadata to page 1
        let mut meta_data = Vec::with_capacity(4096);

        let format_version = STORE_VERSION_V6;
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

        if let Some(commit) = &self.commit {
            commit.truncate().map_err(StoreError::Io)?;
        }

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

fn serialize_metadata(metadata: Option<&Metadata>) -> Vec<u8> {
    let Some(metadata) = metadata else {
        return Vec::new();
    };
    if metadata.is_empty() {
        return Vec::new();
    }

    let mut entries: Vec<_> = metadata.iter().collect();
    entries.sort_by(|(a, _), (b, _)| a.cmp(b));

    let mut buf = Vec::new();
    buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (key, value) in entries {
        write_string(&mut buf, key);
        write_metadata_value(&mut buf, value);
    }
    buf
}

fn deserialize_metadata(data: &[u8]) -> Result<Metadata, StoreError> {
    let mut pos = 0usize;
    let count = read_u32(data, &mut pos)? as usize;
    let mut metadata = Metadata::new();
    for _ in 0..count {
        let key = read_string(data, &mut pos)?;
        let value = read_metadata_value(data, &mut pos)?;
        metadata.set(key, value);
    }
    Ok(metadata)
}

fn write_string(buf: &mut Vec<u8>, value: &str) {
    buf.extend_from_slice(&(value.len() as u32).to_le_bytes());
    buf.extend_from_slice(value.as_bytes());
}

fn write_bytes(buf: &mut Vec<u8>, value: &[u8]) {
    buf.extend_from_slice(&(value.len() as u32).to_le_bytes());
    buf.extend_from_slice(value);
}

fn write_ref_target(buf: &mut Vec<u8>, target: &crate::storage::unified::RefTarget) {
    use crate::storage::unified::RefTarget;

    match target {
        RefTarget::TableRow { table, row_id } => {
            buf.push(0);
            write_string(buf, table);
            buf.extend_from_slice(&row_id.to_le_bytes());
        }
        RefTarget::Node {
            collection,
            node_id,
        } => {
            buf.push(1);
            write_string(buf, collection);
            buf.extend_from_slice(&node_id.raw().to_le_bytes());
        }
        RefTarget::Edge {
            collection,
            edge_id,
        } => {
            buf.push(2);
            write_string(buf, collection);
            buf.extend_from_slice(&edge_id.raw().to_le_bytes());
        }
        RefTarget::Vector {
            collection,
            vector_id,
        } => {
            buf.push(3);
            write_string(buf, collection);
            buf.extend_from_slice(&vector_id.raw().to_le_bytes());
        }
        RefTarget::Entity {
            collection,
            entity_id,
        } => {
            buf.push(4);
            write_string(buf, collection);
            buf.extend_from_slice(&entity_id.raw().to_le_bytes());
        }
    }
}

fn write_metadata_value(buf: &mut Vec<u8>, value: &MetadataValue) {
    match value {
        MetadataValue::Null => buf.push(0),
        MetadataValue::Bool(v) => {
            buf.push(1);
            buf.push(u8::from(*v));
        }
        MetadataValue::Int(v) => {
            buf.push(2);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        MetadataValue::Float(v) => {
            buf.push(3);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        MetadataValue::String(v) => {
            buf.push(4);
            write_string(buf, v);
        }
        MetadataValue::Bytes(v) => {
            buf.push(5);
            write_bytes(buf, v);
        }
        MetadataValue::Array(values) => {
            buf.push(6);
            buf.extend_from_slice(&(values.len() as u32).to_le_bytes());
            for value in values {
                write_metadata_value(buf, value);
            }
        }
        MetadataValue::Object(values) => {
            buf.push(7);
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.cmp(b));
            buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
            for (key, value) in entries {
                write_string(buf, key);
                write_metadata_value(buf, value);
            }
        }
        MetadataValue::Timestamp(v) => {
            buf.push(8);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        MetadataValue::Geo { lat, lon } => {
            buf.push(9);
            buf.extend_from_slice(&lat.to_le_bytes());
            buf.extend_from_slice(&lon.to_le_bytes());
        }
        MetadataValue::Reference(target) => {
            buf.push(10);
            write_ref_target(buf, target);
        }
        MetadataValue::References(targets) => {
            buf.push(11);
            buf.extend_from_slice(&(targets.len() as u32).to_le_bytes());
            for target in targets {
                write_ref_target(buf, target);
            }
        }
    }
}

fn read_exact_slice<'a>(
    data: &'a [u8],
    pos: &mut usize,
    len: usize,
) -> Result<&'a [u8], StoreError> {
    if *pos + len > data.len() {
        return Err(StoreError::Serialization(
            "truncated metadata payload".to_string(),
        ));
    }
    let slice = &data[*pos..*pos + len];
    *pos += len;
    Ok(slice)
}

fn read_u32(data: &[u8], pos: &mut usize) -> Result<u32, StoreError> {
    let bytes = read_exact_slice(data, pos, 4)?;
    let mut raw = [0u8; 4];
    raw.copy_from_slice(bytes);
    Ok(u32::from_le_bytes(raw))
}

fn read_u64(data: &[u8], pos: &mut usize) -> Result<u64, StoreError> {
    let bytes = read_exact_slice(data, pos, 8)?;
    let mut raw = [0u8; 8];
    raw.copy_from_slice(bytes);
    Ok(u64::from_le_bytes(raw))
}

fn read_i64(data: &[u8], pos: &mut usize) -> Result<i64, StoreError> {
    let bytes = read_exact_slice(data, pos, 8)?;
    let mut raw = [0u8; 8];
    raw.copy_from_slice(bytes);
    Ok(i64::from_le_bytes(raw))
}

fn read_f64(data: &[u8], pos: &mut usize) -> Result<f64, StoreError> {
    let bytes = read_exact_slice(data, pos, 8)?;
    let mut raw = [0u8; 8];
    raw.copy_from_slice(bytes);
    Ok(f64::from_le_bytes(raw))
}

fn read_u8(data: &[u8], pos: &mut usize) -> Result<u8, StoreError> {
    let bytes = read_exact_slice(data, pos, 1)?;
    Ok(bytes[0])
}

fn read_string(data: &[u8], pos: &mut usize) -> Result<String, StoreError> {
    let len = read_u32(data, pos)? as usize;
    let bytes = read_exact_slice(data, pos, len)?;
    String::from_utf8(bytes.to_vec()).map_err(|err| StoreError::Serialization(err.to_string()))
}

fn read_bytes(data: &[u8], pos: &mut usize) -> Result<Vec<u8>, StoreError> {
    let len = read_u32(data, pos)? as usize;
    Ok(read_exact_slice(data, pos, len)?.to_vec())
}

fn read_ref_target(
    data: &[u8],
    pos: &mut usize,
) -> Result<crate::storage::unified::RefTarget, StoreError> {
    use crate::storage::unified::RefTarget;

    match read_u8(data, pos)? {
        0 => Ok(RefTarget::TableRow {
            table: read_string(data, pos)?,
            row_id: read_u64(data, pos)?,
        }),
        1 => Ok(RefTarget::Node {
            collection: read_string(data, pos)?,
            node_id: EntityId::new(read_u64(data, pos)?),
        }),
        2 => Ok(RefTarget::Edge {
            collection: read_string(data, pos)?,
            edge_id: EntityId::new(read_u64(data, pos)?),
        }),
        3 => Ok(RefTarget::Vector {
            collection: read_string(data, pos)?,
            vector_id: EntityId::new(read_u64(data, pos)?),
        }),
        4 => Ok(RefTarget::Entity {
            collection: read_string(data, pos)?,
            entity_id: EntityId::new(read_u64(data, pos)?),
        }),
        tag => Err(StoreError::Serialization(format!(
            "unknown metadata ref target tag {tag}"
        ))),
    }
}

fn read_metadata_value(data: &[u8], pos: &mut usize) -> Result<MetadataValue, StoreError> {
    match read_u8(data, pos)? {
        0 => Ok(MetadataValue::Null),
        1 => Ok(MetadataValue::Bool(read_u8(data, pos)? != 0)),
        2 => Ok(MetadataValue::Int(read_i64(data, pos)?)),
        3 => Ok(MetadataValue::Float(read_f64(data, pos)?)),
        4 => Ok(MetadataValue::String(read_string(data, pos)?)),
        5 => Ok(MetadataValue::Bytes(read_bytes(data, pos)?)),
        6 => {
            let count = read_u32(data, pos)? as usize;
            let mut values = Vec::with_capacity(count);
            for _ in 0..count {
                values.push(read_metadata_value(data, pos)?);
            }
            Ok(MetadataValue::Array(values))
        }
        7 => {
            let count = read_u32(data, pos)? as usize;
            let mut values = std::collections::HashMap::with_capacity(count);
            for _ in 0..count {
                let key = read_string(data, pos)?;
                let value = read_metadata_value(data, pos)?;
                values.insert(key, value);
            }
            Ok(MetadataValue::Object(values))
        }
        8 => Ok(MetadataValue::Timestamp(read_u64(data, pos)?)),
        9 => Ok(MetadataValue::Geo {
            lat: read_f64(data, pos)?,
            lon: read_f64(data, pos)?,
        }),
        10 => Ok(MetadataValue::Reference(read_ref_target(data, pos)?)),
        11 => {
            let count = read_u32(data, pos)? as usize;
            let mut targets = Vec::with_capacity(count);
            for _ in 0..count {
                targets.push(read_ref_target(data, pos)?);
            }
            Ok(MetadataValue::References(targets))
        }
        tag => Err(StoreError::Serialization(format!(
            "unknown metadata value tag {tag}"
        ))),
    }
}
