use super::*;
use crate::storage::unified::entity_cache::EntityCache;
use parking_lot::RwLock;

// ── Pager-meta overflow chain (gh-477) ──────────────────────────────────────
// When the serialized collection registry + cross-refs exceed a single page,
// page 1 carries a native metadata-overflow header pointing at an overflow chain of
// `PageType::Overflow` pages. Single-page metadata keeps the historical
// bit-identical layout (`METADATA_MAGIC = "RDM2"` written directly at the
// content offset).
//
// Page 1 (overflow form), starting at `HEADER_SIZE`:
//   [0..4]   native metadata-overflow magic
//   [4..8]   format_version (u32, mirrors inner payload version for debug)
//   [8..12]  total_payload_bytes (u32)
//   [12..16] next_overflow_page_id (u32, > 0)
//   [16..]   first payload chunk (up to META_V3_FIRST_PAYLOAD_CAP bytes)
//
// Overflow continuation page, starting at `HEADER_SIZE`:
//   [0..4]   next_overflow_page_id (u32, 0 if last)
//   [4..8]   chunk_bytes (u32)
//   [8..]    chunk payload (up to META_V3_OVERFLOW_PAYLOAD_CAP bytes)
const META_PAGE_CONTENT_CAP: usize =
    crate::storage::engine::PAGE_SIZE - crate::storage::engine::HEADER_SIZE;
const META_V3_PAGE1_HEADER: usize = reddb_file::METADATA_OVERFLOW_HEADER_BYTES;
const META_V3_OVERFLOW_HEADER: usize = reddb_file::METADATA_OVERFLOW_CONTINUATION_HEADER_BYTES;
const META_V3_FIRST_PAYLOAD_CAP: usize = META_PAGE_CONTENT_CAP - META_V3_PAGE1_HEADER;
const META_V3_OVERFLOW_PAYLOAD_CAP: usize = META_PAGE_CONTENT_CAP - META_V3_OVERFLOW_HEADER;

fn free_existing_overflow_chain(pager: &Pager) -> Result<(), PagerError> {
    let cs = crate::storage::engine::HEADER_SIZE;
    let page = match pager.read_page(1) {
        Ok(p) => p,
        Err(_) => return Ok(()),
    };
    let bytes = page.as_bytes();
    if bytes.len() < cs + META_V3_PAGE1_HEADER {
        return Ok(());
    }
    let Some(header) =
        reddb_file::decode_native_metadata_overflow_header(&bytes[cs..]).map_err(|err| {
            PagerError::InvalidDatabase(format!("invalid metadata overflow header: {err}"))
        })?
    else {
        return Ok(());
    };
    let mut next = header.next_overflow_page_id;
    while next != 0 {
        let ov = match pager.read_page(next) {
            Ok(p) => p,
            Err(_) => break,
        };
        let ob = ov.as_bytes();
        let nn = match reddb_file::decode_native_metadata_overflow_continuation_header(&ob[cs..]) {
            Ok(header) => header.next_overflow_page_id,
            Err(_) => 0,
        };
        let _ = pager.free_page(next);
        next = nn;
    }
    Ok(())
}

fn build_meta_page1_with_overflow(
    pager: &Pager,
    meta_data: &[u8],
) -> Result<crate::storage::engine::Page, PagerError> {
    use crate::storage::engine::{Page, PageType, HEADER_SIZE};
    free_existing_overflow_chain(pager)?;

    let mut page1 = Page::new(PageType::Header, 1);
    let cs = HEADER_SIZE;

    if meta_data.len() <= META_PAGE_CONTENT_CAP {
        // Single-page: bit-identical to the historical layout.
        let buf = page1.as_bytes_mut();
        buf[cs..cs + meta_data.len()].copy_from_slice(meta_data);
        return Ok(page1);
    }

    // Multi-page overflow form. Split the inner payload into the first chunk
    // (held on page 1) followed by zero-or-more continuation chunks chained
    // through `PageType::Overflow` pages.
    let first_chunk = &meta_data[..META_V3_FIRST_PAYLOAD_CAP];
    let mut tail = &meta_data[META_V3_FIRST_PAYLOAD_CAP..];
    let mut chunks: Vec<&[u8]> = Vec::new();
    while !tail.is_empty() {
        let take = tail.len().min(META_V3_OVERFLOW_PAYLOAD_CAP);
        chunks.push(&tail[..take]);
        tail = &tail[take..];
    }

    let mut overflow_pages: Vec<Page> = Vec::with_capacity(chunks.len());
    let mut overflow_ids: Vec<u32> = Vec::with_capacity(chunks.len());
    for _ in 0..chunks.len() {
        let pg = pager.allocate_page(PageType::Overflow)?;
        overflow_ids.push(pg.page_id());
        overflow_pages.push(pg);
    }

    for i in 0..chunks.len() {
        let next = if i + 1 < chunks.len() {
            overflow_ids[i + 1]
        } else {
            0u32
        };
        let len = chunks[i].len() as u32;
        let buf = overflow_pages[i].as_bytes_mut();
        reddb_file::encode_native_metadata_overflow_continuation_header(
            &mut buf[cs..cs + META_V3_OVERFLOW_HEADER],
            reddb_file::NativeMetadataOverflowContinuationHeader {
                next_overflow_page_id: next,
                chunk_bytes: len,
            },
        )
        .map_err(|err| PagerError::InvalidDatabase(err.to_string()))?;
        buf[cs + 8..cs + 8 + chunks[i].len()].copy_from_slice(chunks[i]);
    }
    for (idx, page) in overflow_pages.into_iter().enumerate() {
        let id = overflow_ids[idx];
        pager.write_page(id, page)?;
    }

    // Mirror the inner format_version for debug-friendly hex dumps.
    let format_version = reddb_file::decode_native_paged_metadata_header(meta_data)
        .ok()
        .flatten()
        .map_or(0, |header| header.format_version);

    let buf = page1.as_bytes_mut();
    reddb_file::encode_native_metadata_overflow_header(
        &mut buf[cs..cs + META_V3_PAGE1_HEADER],
        reddb_file::NativeMetadataOverflowHeader {
            format_version,
            total_payload_bytes: meta_data.len() as u32,
            next_overflow_page_id: overflow_ids[0],
        },
    )
    .map_err(|err| PagerError::InvalidDatabase(err.to_string()))?;
    buf[cs + META_V3_PAGE1_HEADER..cs + META_V3_PAGE1_HEADER + first_chunk.len()]
        .copy_from_slice(first_chunk);

    Ok(page1)
}

/// Assemble the full metadata payload from page 1 (plus its overflow chain
/// when the native overflow wrapper is present). Returns the bytes that the
/// metadata parser would see starting from the content offset of page 1.
/// Single-page metadata returns the raw page content (including trailing
/// zero-pad), so the legacy parser sees the same bytes it always saw.
fn read_meta_payload(pager: &Pager) -> Option<Vec<u8>> {
    let cs = crate::storage::engine::HEADER_SIZE;
    let meta_page = pager
        .read_page(1)
        .or_else(|_| pager.recover_meta_from_shadow())
        .ok()?;
    let bytes = meta_page.as_bytes();
    if bytes.len() < cs + 4 {
        return Some(bytes.get(cs..).unwrap_or(&[]).to_vec());
    }
    let header = match reddb_file::decode_native_metadata_overflow_header(&bytes[cs..]).ok()? {
        Some(header) => header,
        None => {
            return Some(bytes[cs..].to_vec());
        }
    };
    if bytes.len() < cs + META_V3_PAGE1_HEADER {
        return None;
    }
    let total = header.total_payload_bytes as usize;
    let mut next = header.next_overflow_page_id;
    let mut payload: Vec<u8> = Vec::with_capacity(total);
    let first_take = total.min(META_V3_FIRST_PAYLOAD_CAP);
    payload.extend_from_slice(
        &bytes[cs + META_V3_PAGE1_HEADER..cs + META_V3_PAGE1_HEADER + first_take],
    );
    while next != 0 && payload.len() < total {
        let ov = pager.read_page(next).ok()?;
        let ob = ov.as_bytes();
        if ob.len() < cs + META_V3_OVERFLOW_HEADER {
            return None;
        }
        let continuation =
            reddb_file::decode_native_metadata_overflow_continuation_header(&ob[cs..]).ok()?;
        let nn = continuation.next_overflow_page_id;
        let len = continuation.chunk_bytes as usize;
        let remaining = total - payload.len();
        let take = len.min(remaining).min(META_V3_OVERFLOW_PAYLOAD_CAP);
        payload.extend_from_slice(
            &ob[cs + META_V3_OVERFLOW_HEADER..cs + META_V3_OVERFLOW_HEADER + take],
        );
        next = nn;
    }
    Some(payload)
}

impl UnifiedStore {
    pub(crate) fn mark_paged_registry_dirty(&self) {
        self.paged_registry_dirty.store(true, Ordering::Release);
    }

    /// Get (or lazily create) the per-collection B-tree under a *read*
    /// lock whenever possible. Returns a cloned `Arc<BTree>` so callers
    /// can mutate the tree without holding the outer map's RwLock —
    /// previously every insert serialised on `btree_indices.write()`,
    /// costing ~60% of the concurrent-insert throughput ceiling.
    pub(crate) fn get_or_create_btree(&self, collection: &str) -> Option<Arc<BTree>> {
        let pager = self.pager.as_ref()?;
        if let Some(btree) = self.btree_indices.read().get(collection).cloned() {
            return Some(btree);
        }
        let mut write = self.btree_indices.write();
        let btree = write
            .entry(collection.to_string())
            .or_insert_with(|| Arc::new(BTree::new(Arc::clone(pager))))
            .clone();
        Some(btree)
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

        let format_version = STORE_VERSION_V11;
        self.set_format_version(format_version);

        let collections = self.collections.read();
        let btree_indices = self.btree_indices.read();
        let mut collection_roots = Vec::with_capacity(collections.len());
        for (name, _) in collections.iter() {
            let root_page = btree_indices
                .get(name)
                .map_or(0, |btree| btree.root_page_id());
            collection_roots.push((name.clone(), root_page));
        }
        drop(btree_indices);
        drop(collections);

        let mut meta_data = Vec::with_capacity(4096);
        reddb_file::encode_native_paged_metadata_header(
            &mut meta_data,
            reddb_file::NativePagedMetadataHeader {
                format_version,
                collection_count: collection_roots.len() as u32,
            },
        );
        for (name, root_page) in &collection_roots {
            reddb_file::encode_native_paged_collection_root(&mut meta_data, name, *root_page);
        }

        let cross_refs = self.cross_refs.read();
        let total_refs: usize = cross_refs.values().map(|v| v.len()).sum();
        meta_data.extend_from_slice(&(total_refs as u32).to_le_bytes());
        for (source_id, refs) in cross_refs.iter() {
            for (target_id, ref_type, collection) in refs {
                reddb_file::encode_native_paged_cross_ref(
                    &mut meta_data,
                    source_id.raw(),
                    target_id.raw(),
                    ref_type.to_byte(),
                    collection,
                );
            }
        }

        let meta_page = build_meta_page1_with_overflow(pager, &meta_data)
            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;

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

    /// Bytes of page-cache memory currently occupied by resident pages
    /// (ADR 0073 §2). Slots are fixed size, so occupancy times the page size
    /// *is* the footprint; a store without a pager occupies none.
    pub fn page_cache_bytes_in_use(&self) -> u64 {
        self.pager.as_ref().map_or(0, |pager| {
            pager.cache_len() as u64 * crate::storage::memory_pools::PAGE_CACHE_PAGE_SIZE_BYTES
        })
    }

    /// Bytes of WAL buffer memory held by this store's group-commit
    /// coordinator: queued records plus the writer's fixed append buffer.
    pub fn wal_buffer_bytes_in_use(&self) -> u64 {
        self.commit
            .as_ref()
            .map_or(0, |commit| commit.buffered_bytes())
    }

    /// Approximate resident bytes across every collection's segment arena
    /// (ADR 0073 §2). Same number `stats().total_memory_bytes` reports, without
    /// building the per-collection stats map a sampler would throw away.
    pub fn segment_memory_bytes(&self) -> u64 {
        let managers: Vec<Arc<SegmentManager>> = self
            .collections
            .read()
            .values()
            .map(Arc::clone)
            .collect();

        managers
            .iter()
            .map(|manager| manager.memory_bytes())
            .fold(0, u64::saturating_add)
    }

    /// Borrow the immutable store configuration. Runtime hooks (e.g. the
    /// `auto_index_id` first-insert hook in `MutationEngine`) read knobs
    /// off this struct without going through the legacy global config tree.
    pub fn config(&self) -> &UnifiedStoreConfig {
        &self.config
    }

    pub fn with_config(config: UnifiedStoreConfig) -> Self {
        Self {
            config,
            format_version: AtomicU32::new(STORE_VERSION_V11),
            next_entity_id: AtomicU64::new(1),
            collections: RwLock::new(HashMap::new()),
            cross_refs: RwLock::new(HashMap::new()),
            reverse_refs: RwLock::new(HashMap::new()),
            pager: None,
            db_path: None,
            btree_indices: RwLock::new(HashMap::new()),
            context_index: ContextIndex::new(),
            entity_cache: EntityCache::new(),
            graph_label_index: RwLock::new(HashMap::new()),
            paged_registry_dirty: AtomicBool::new(false),
            commit: None,
            unindex_cross_refs_fast_path: AtomicU64::new(0),
            replayed_turbo_inserts: parking_lot::Mutex::new(HashMap::new()),
            replayed_probabilistic_deltas: parking_lot::Mutex::new(Vec::new()),
            aux_metadata: RwLock::new(Vec::new()),
        }
    }

    /// Open or create a page-based database
    ///
    /// This uses the page engine for ACID durability with B-tree indices.
    /// The database file uses 16 KiB pages with checksums and efficient caching.
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
        // ADR 0073 §2 — the page cache is pre-sized from its budget share.
        // The pager's own `cache_size` default survives only for direct
        // library callers who never resolved a budget.
        if let Some(slots) = config.page_cache_slots {
            pager_config.cache_size = slots;
        }
        // Tunables via env — experimental, used by the benchmark harness
        // to compare durability profiles head-to-head with Postgres.
        // REDDB_DOUBLE_WRITE=0 requests skipping the double-write buffer,
        // which otherwise adds two fsyncs per pager flush (one on DWB, one
        // on the main file). The pager honors this only when the actual
        // data file is proven to live on a CoW filesystem with atomic page
        // writes; otherwise it fails closed and keeps DWB enabled.
        if matches!(
            std::env::var("REDDB_DOUBLE_WRITE").ok().as_deref(),
            Some("0") | Some("false") | Some("off")
        ) {
            pager_config.double_write = false;
        }
        let pager = Pager::open(path, pager_config)
            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;

        let wal_path = reddb_file::layout::unified_wal_path(path);
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
            format_version: AtomicU32::new(STORE_VERSION_V11),
            next_entity_id: AtomicU64::new(1),
            collections: RwLock::new(HashMap::new()),
            cross_refs: RwLock::new(HashMap::new()),
            reverse_refs: RwLock::new(HashMap::new()),
            pager: Some(Arc::new(pager)),
            db_path: Some(path.to_path_buf()),
            btree_indices: RwLock::new(HashMap::new()),
            context_index: ContextIndex::new(),
            entity_cache: EntityCache::new(),
            graph_label_index: RwLock::new(HashMap::new()),
            paged_registry_dirty: AtomicBool::new(false),
            commit,
            unindex_cross_refs_fast_path: AtomicU64::new(0),
            replayed_turbo_inserts: parking_lot::Mutex::new(HashMap::new()),
            replayed_probabilistic_deltas: parking_lot::Mutex::new(Vec::new()),
            aux_metadata: RwLock::new(Vec::new()),
        };

        // Load existing data from pages if database exists
        store.load_from_pages()?;
        if let Some(commit) = &store.commit {
            commit.replay_into(&store).map_err(StoreError::Io)?;
        }
        store.recover_operational_manifest()?;

        Ok(store)
    }

    pub(crate) fn recover_operational_manifest(&self) -> Result<(), StoreError> {
        let Some(path) = &self.db_path else {
            return Ok(());
        };
        let mut collections = self.list_collections();
        collections.sort();
        let pending_drops =
            crate::storage::operational_manifest::OperationalManifest::for_db_path(path)
                .recover_or_bootstrap(&collections)
                .map_err(StoreError::Io)?;
        for name in pending_drops {
            if self.get_collection(&name).is_some() {
                self.drop_collection(&name)?;
            }
        }
        Ok(())
    }

    pub(crate) fn publish_operational_collection_create(
        &self,
        name: &str,
    ) -> Result<(), StoreError> {
        let Some(path) = &self.db_path else {
            return Ok(());
        };
        crate::storage::operational_manifest::OperationalManifest::for_db_path(path)
            .create_collection(name)
            .map_err(StoreError::Io)
    }

    pub(crate) fn publish_operational_collection_pending_drop(
        &self,
        name: &str,
    ) -> Result<(), StoreError> {
        let Some(path) = &self.db_path else {
            return Ok(());
        };
        crate::storage::operational_manifest::OperationalManifest::for_db_path(path)
            .begin_drop_collection(name)
            .map_err(StoreError::Io)
    }

    pub(crate) fn publish_operational_collection_drop_finished(
        &self,
        name: &str,
    ) -> Result<(), StoreError> {
        let Some(path) = &self.db_path else {
            return Ok(());
        };
        crate::storage::operational_manifest::OperationalManifest::for_db_path(path)
            .finish_drop_collection(name)
            .map_err(StoreError::Io)
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

        // Read metadata starting from page 1 (collections registry). The
        // helper transparently follows the `RDM3` overflow chain when the
        // metadata blob spans multiple pages and falls back to the legacy
        // `<data>-meta` shadow when page 1 itself is corrupted.
        if let Some(content_vec) = read_meta_payload(pager) {
            let content: &[u8] = &content_vec;
            if content.len() >= 4 {
                let mut pos = 0;
                let mut format_version = STORE_VERSION_V1;

                let collection_count = if let Some(header) =
                    reddb_file::decode_native_paged_metadata_header(content)
                        .map_err(|err| StoreError::Serialization(err.to_string()))?
                {
                    format_version = header.format_version;
                    pos += reddb_file::METADATA_HEADER_BYTES;
                    header.collection_count as usize
                } else {
                    let count = u32::from_le_bytes([
                        content[pos],
                        content[pos + 1],
                        content[pos + 2],
                        content[pos + 3],
                    ]) as usize;
                    pos += 4;
                    count
                };

                self.set_format_version(format_version);

                if pos > content.len() {
                    return Ok(());
                }

                // Read collection names and their B-tree root pages
                for _ in 0..collection_count {
                    if let Ok(root) =
                        reddb_file::decode_native_paged_collection_root(content, &mut pos)
                    {
                        // Root page ID for this collection's B-tree
                        let root_page = root.root_page;
                        let name = root.collection;

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
                            self.btree_indices.write().insert(name, Arc::new(btree));
                        }
                    } else {
                        break;
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
                        let Ok(cross_ref) =
                            reddb_file::decode_native_paged_cross_ref(content, &mut pos)
                        else {
                            break;
                        };
                        let source_id = EntityId::new(cross_ref.source_id);
                        let target_id = EntityId::new(cross_ref.target_id);
                        let ref_type = RefType::from_byte(cross_ref.ref_type);
                        let target_collection = cross_ref.target_collection;

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

        if self.format_version() < STORE_VERSION_V11 {
            self.set_format_version(STORE_VERSION_V11);
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
        // Pre-allocate ~256 bytes to cover the typical 15-column
        // typed row without any Vec growth. Bulk insert calls this
        // millions of times per bench run; saving 2-3 reallocs per
        // entity amortises.
        let mut buf = Vec::with_capacity(256);
        Self::write_entity_binary(&mut buf, entity, format_version);
        buf
    }

    pub(crate) fn serialize_entity_record(
        entity: &UnifiedEntity,
        metadata: Option<&Metadata>,
        format_version: u32,
    ) -> Vec<u8> {
        let entity_bytes = Self::serialize_entity(entity, format_version);
        // Skip the intermediate metadata Vec when there's no metadata
        // (common OLTP bulk-insert case): write a zero-length prefix
        // directly into the record buffer. Only fall back to the old
        // serialize_metadata() allocation when the caller actually
        // has fields to persist.
        let has_meta = matches!(metadata, Some(m) if !m.fields.is_empty());
        if has_meta {
            let metadata_bytes = serialize_metadata(metadata);
            reddb_file::encode_native_entity_record_frame(&entity_bytes, Some(&metadata_bytes))
        } else {
            reddb_file::encode_native_entity_record_frame(&entity_bytes, None)
        }
    }

    pub(crate) fn deserialize_entity_record(
        data: &[u8],
        format_version: u32,
    ) -> Result<(UnifiedEntity, Option<Metadata>), StoreError> {
        let Some(frame) = reddb_file::decode_native_entity_record_frame(data)
            .map_err(|err| StoreError::Serialization(err.to_string()))?
        else {
            return Self::deserialize_entity(data, format_version).map(|entity| (entity, None));
        };

        let entity = Self::deserialize_entity(frame.entity, format_version)?;
        let metadata = if frame.metadata.is_empty() {
            None
        } else {
            let metadata = deserialize_metadata(frame.metadata)?;
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
                .or_insert_with(|| Arc::new(BTree::new(Arc::clone(pager))));

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

            // Slice G (#704): no per-row skip. Oversized values are
            // spilled through the slice-E write ladder inside
            // `bulk_insert_sorted` (inline → compressed inline →
            // overflow chain). The only rejection is the hard
            // `MAX_VALUE_SIZE` (256 MiB) ceiling, which surfaces as
            // `ValueTooLarge` from the bulk path after the rest of
            // the batch has landed.
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

        let format_version = STORE_VERSION_V11;
        self.set_format_version(format_version);

        reddb_file::encode_native_paged_metadata_header(
            &mut meta_data,
            reddb_file::NativePagedMetadataHeader {
                format_version,
                collection_count: collection_roots.len() as u32,
            },
        );

        // Write each collection's name and B-tree root page
        for (name, root_page) in &collection_roots {
            reddb_file::encode_native_paged_collection_root(&mut meta_data, name, *root_page);
        }

        // Write cross-reference metadata
        let cross_refs = self.cross_refs.read();
        let total_refs: usize = cross_refs.values().map(|v| v.len()).sum();
        meta_data.extend_from_slice(&(total_refs as u32).to_le_bytes());
        for (source_id, refs) in cross_refs.iter() {
            for (target_id, ref_type, collection) in refs {
                reddb_file::encode_native_paged_cross_ref(
                    &mut meta_data,
                    source_id.raw(),
                    target_id.raw(),
                    ref_type.to_byte(),
                    collection,
                );
            }
        }

        // Build page 1 (+ overflow chain when needed) for the metadata blob.
        let meta_page = build_meta_page1_with_overflow(pager, &meta_data)
            .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;

        // Write metadata shadow FIRST (intact copy in case main write fails).
        // The shadow is a no-op when `fold_pager_meta` is enabled.
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

    /// Current root page for a collection's primary B-tree, if one has
    /// been materialized in this store.
    pub(crate) fn collection_root_page(&self, collection: &str) -> Option<u32> {
        self.btree_indices
            .read()
            .get(collection)
            .map(|btree| btree.root_page_id())
            .filter(|root| *root != 0)
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
    entries.sort_by_key(|(a, _)| *a);

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
    reddb_file::encode_native_len_prefixed_str(buf, value);
}

fn write_bytes(buf: &mut Vec<u8>, value: &[u8]) {
    reddb_file::encode_native_len_prefixed_bytes(buf, value);
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
            entries.sort_by_key(|(a, _)| *a);
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
    reddb_file::decode_native_len_prefixed_string(data, pos)
        .map_err(|err| StoreError::Serialization(err.to_string()))
}

fn read_bytes(data: &[u8], pos: &mut usize) -> Result<Vec<u8>, StoreError> {
    reddb_file::decode_native_len_prefixed_bytes(data, pos)
        .map(|bytes| bytes.to_vec())
        .map_err(|err| StoreError::Serialization(err.to_string()))
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
