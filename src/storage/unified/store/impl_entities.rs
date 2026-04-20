use super::*;

impl UnifiedStore {
    pub(crate) fn persist_entities_to_pager(
        &self,
        collection: &str,
        entities: &[UnifiedEntity],
    ) -> Result<(), StoreError> {
        if entities.is_empty() {
            return Ok(());
        }

        let Some(pager) = &self.pager else {
            return Ok(());
        };

        let fv = self.format_version();
        let manager = self
            .get_collection(collection)
            .ok_or_else(|| StoreError::CollectionNotFound(collection.to_string()))?;
        let mut serialized: Vec<(Vec<u8>, Vec<u8>)> = entities
            .iter()
            .map(|entity| {
                let metadata = manager.get_metadata(entity.id);
                (
                    entity.id.raw().to_be_bytes().to_vec(),
                    Self::serialize_entity_record(entity, metadata.as_ref(), fv),
                )
            })
            .collect();
        // u64 IDs encoded as big-endian — lex order = numeric order.
        serialized.sort_by(|a, b| a.0.cmp(&b.0));

        let mut btree_indices = self.btree_indices.write();
        let btree = btree_indices
            .entry(collection.to_string())
            .or_insert_with(|| Arc::new(BTree::new(Arc::clone(pager))));
        let root_before = btree.root_page_id();

        // Walks each distinct leaf once, applies all in-place overwrites
        // that belong there under one read+write. Keys that miss or grow
        // fall back to the per-key `upsert` path internally.
        btree.upsert_batch_sorted(&serialized).map_err(|e| {
            StoreError::Io(std::io::Error::other(format!("B-tree upsert error: {}", e)))
        })?;
        let root_after = btree.root_page_id();
        drop(btree_indices);
        if root_before != root_after {
            self.mark_paged_registry_dirty();
        }
        let actions = entities
            .iter()
            .map(|entity| {
                let metadata = manager.get_metadata(entity.id);
                StoreWalAction::upsert_entity(
                    collection,
                    entity,
                    metadata.as_ref(),
                    self.format_version(),
                )
            })
            .collect::<Vec<_>>();
        self.finish_paged_write(actions)?;

        Ok(())
    }

    /// Insert a label→entity_id mapping into the graph label index.
    pub(crate) fn update_graph_label_index(
        &self,
        collection: &str,
        label: &str,
        entity_id: EntityId,
    ) {
        let key = (collection.to_string(), label.to_string());
        let mut idx = self.graph_label_index.write();
        idx.entry(key).or_default().push(entity_id);
    }

    /// Remove a specific entity_id from the graph label index (called on delete).
    pub(crate) fn remove_from_graph_label_index(&self, collection: &str, entity_id: EntityId) {
        let mut idx = self.graph_label_index.write();
        for ((col, _), ids) in idx.iter_mut() {
            if col == collection {
                ids.retain(|&id| id != entity_id);
            }
        }
        // Prune empty entries to keep the index compact
        idx.retain(|_, ids| !ids.is_empty());
    }

    pub(crate) fn remove_from_graph_label_index_batch(
        &self,
        collection: &str,
        entity_ids: &[EntityId],
    ) {
        if entity_ids.is_empty() {
            return;
        }
        let id_set: std::collections::HashSet<EntityId> = entity_ids.iter().copied().collect();
        let mut idx = self.graph_label_index.write();
        for ((col, _), ids) in idx.iter_mut() {
            if col == collection {
                ids.retain(|id| !id_set.contains(id));
            }
        }
        idx.retain(|_, ids| !ids.is_empty());
    }

    /// Look up entity IDs for a graph node label across all collections.
    pub fn lookup_graph_nodes_by_label(&self, label: &str) -> Vec<EntityId> {
        let idx = self.graph_label_index.read();
        idx.iter()
            .filter(|((_, l), _)| l == label)
            .flat_map(|(_, ids)| ids.iter().copied())
            .collect()
    }

    pub fn create_collection(&self, name: impl Into<String>) -> Result<(), StoreError> {
        let name = name.into();
        let mut collections = self.collections.write();

        if collections.contains_key(&name) {
            return Err(StoreError::CollectionExists(name));
        }

        let manager = SegmentManager::with_config(&name, self.config.manager_config.clone());
        collections.insert(name.clone(), Arc::new(manager));
        drop(collections);
        self.mark_paged_registry_dirty();
        self.finish_paged_write([StoreWalAction::CreateCollection { name }])?;

        Ok(())
    }

    /// Get or create a collection
    pub fn get_or_create_collection(&self, name: impl Into<String>) -> Arc<SegmentManager> {
        let name = name.into();
        // Fast path: shared read lock — zero contention for existing collections
        {
            let collections = self.collections.read();
            if let Some(manager) = collections.get(&name) {
                return Arc::clone(manager);
            }
        }
        // Slow path: exclusive write lock — only when collection is missing
        let mut collections = self.collections.write();
        // Double-check after acquiring write lock (another thread may have created it)
        if let Some(manager) = collections.get(&name) {
            return Arc::clone(manager);
        }
        let manager = Arc::new(SegmentManager::with_config(
            &name,
            self.config.manager_config.clone(),
        ));
        collections.insert(name, Arc::clone(&manager));
        self.mark_paged_registry_dirty();
        manager
    }

    /// Get a collection
    pub fn get_collection(&self, name: &str) -> Option<Arc<SegmentManager>> {
        self.collections.read().get(name).map(Arc::clone)
    }

    /// Get the context index for cross-structure search.
    pub fn context_index(&self) -> &ContextIndex {
        &self.context_index
    }

    /// Set multiple config KV pairs at once from a JSON tree.
    /// Keys are flattened with dot-notation: `{"a":{"b":1}}` → `a.b = 1`.
    pub fn set_config_tree(&self, prefix: &str, json: &crate::serde_json::Value) -> usize {
        let _ = self.get_or_create_collection("red_config");
        let mut pairs = Vec::new();
        flatten_config_json(prefix, json, &mut pairs);
        let mut saved = 0;
        for (key, value) in pairs {
            let entity = UnifiedEntity::new(
                EntityId::new(0),
                EntityKind::TableRow {
                    table: Arc::from("red_config"),
                    row_id: 0,
                },
                EntityData::Row(RowData {
                    columns: Vec::new(),
                    named: Some(
                        [
                            ("key".to_string(), crate::storage::schema::Value::text(key)),
                            ("value".to_string(), value),
                        ]
                        .into_iter()
                        .collect(),
                    ),
                    schema: None,
                }),
            );
            if self.insert_auto("red_config", entity).is_ok() {
                saved += 1;
            }
        }
        saved
    }

    /// Read a single config value from `red_config` by dot-notation key.
    pub fn get_config(&self, key: &str) -> Option<crate::storage::schema::Value> {
        let manager = self.get_collection("red_config")?;
        for entity in manager.query_all(|_| true) {
            if let EntityData::Row(row) = &entity.data {
                if let Some(named) = &row.named {
                    let key_matches = named
                        .get("key")
                        .and_then(|v| match v {
                            crate::storage::schema::Value::Text(s) => Some(s.as_ref() == key),
                            _ => None,
                        })
                        .unwrap_or(false);
                    if key_matches {
                        return named.get("value").cloned();
                    }
                }
            }
        }
        None
    }

    /// List all collections
    pub fn list_collections(&self) -> Vec<String> {
        self.collections.read().keys().cloned().collect()
    }

    /// Drop a collection
    pub fn drop_collection(&self, name: &str) -> Result<(), StoreError> {
        let manager = {
            let mut collections = self.collections.write();

            collections
                .remove(name)
                .ok_or_else(|| StoreError::CollectionNotFound(name.to_string()))?
        };

        let entities = manager.query_all(|_| true);
        let entity_ids: Vec<EntityId> = entities.iter().map(|entity| entity.id).collect();

        for entity_id in &entity_ids {
            self.context_index.remove_entity(*entity_id);
            let _ = self.unindex_cross_refs(*entity_id);
        }

        self.btree_indices.write().remove(name);

        self.entity_cache
            .write()
            .retain(|entity_id, (collection, _)| {
                collection != name && !entity_ids.iter().any(|id| id.raw() == *entity_id)
            });

        self.cross_refs.write().retain(|source_id, refs| {
            refs.retain(|(target_id, _, target_collection)| {
                target_collection != name && !entity_ids.iter().any(|id| id == target_id)
            });
            !entity_ids.iter().any(|id| id == source_id)
        });

        self.reverse_refs.write().retain(|target_id, refs| {
            refs.retain(|(source_id, _, source_collection)| {
                source_collection != name && !entity_ids.iter().any(|id| id == source_id)
            });
            !entity_ids.iter().any(|id| id == target_id)
        });

        self.mark_paged_registry_dirty();
        self.finish_paged_write([StoreWalAction::DropCollection {
            name: name.to_string(),
        }])?;

        Ok(())
    }

    /// Insert an entity into a collection
    pub fn insert(&self, collection: &str, entity: UnifiedEntity) -> Result<EntityId, StoreError> {
        let manager = self
            .get_collection(collection)
            .ok_or_else(|| StoreError::CollectionNotFound(collection.to_string()))?;

        let mut entity = entity;
        if entity.id.raw() == 0 {
            entity.id = self.next_entity_id();
        } else {
            self.register_entity_id(entity.id);
        }
        // Assign per-table sequential row_id if not set
        if let EntityKind::TableRow { ref mut row_id, .. } = entity.kind {
            if *row_id == 0 {
                *row_id = manager.next_row_id();
            } else {
                manager.register_row_id(*row_id);
            }
        }
        // Capture graph node label before entity is moved into the manager
        let graph_node_label: Option<String> = if let EntityKind::GraphNode(ref node) = entity.kind
        {
            Some(node.label.clone())
        } else {
            None
        };

        let id = manager.insert(entity)?;
        self.register_entity_id(id);

        // Update graph label index for GraphNode entities
        if let Some(ref label) = graph_node_label {
            self.update_graph_label_index(collection, label, id);
        }

        // Also insert into B-tree index if pager is active
        let mut registry_dirty = false;
        if let Some(pager) = &self.pager {
            if let Some(entity) = manager.get(id) {
                let mut btree_indices = self.btree_indices.write();
                let btree = btree_indices
                    .entry(collection.to_string())
                    .or_insert_with(|| Arc::new(BTree::new(Arc::clone(pager))));
                let root_before = btree.root_page_id();

                let key = id.raw().to_be_bytes();
                let metadata = manager.get_metadata(id);
                let value = Self::serialize_entity_record(
                    &entity,
                    metadata.as_ref(),
                    self.format_version(),
                );
                // Ignore duplicate key errors (update scenario)
                let _ = btree.insert(&key, &value);
                registry_dirty = root_before != btree.root_page_id();
            }
        }

        // Index cross-references if enabled
        if self.config.auto_index_refs {
            if let Some(entity) = manager.get(id) {
                self.index_cross_refs(&entity, collection)?;
            }
        }

        // Perf: skip WAL-action construction when the store is
        // pagerless. For in-memory benchmarks this saved another
        // `manager.get(id)` + `serialize_entity_record` per call.
        if self.pager.is_some() {
            let actions = manager
                .get(id)
                .map(|entity| {
                    let metadata = manager.get_metadata(id);
                    vec![StoreWalAction::upsert_entity(
                        collection,
                        &entity,
                        metadata.as_ref(),
                        self.format_version(),
                    )]
                })
                .unwrap_or_default();
            if registry_dirty {
                self.mark_paged_registry_dirty();
            }
            self.finish_paged_write(actions)?;
        }

        Ok(id)
    }

    /// Turbo bulk insert — optimized fast path.
    ///
    /// Single lock for the entire batch. Skips bloom filter, memtable,
    /// context index, and cross-ref indexing. B-tree writes are batched.
    pub fn bulk_insert(
        &self,
        collection: &str,
        mut entities: Vec<UnifiedEntity>,
    ) -> Result<Vec<EntityId>, StoreError> {
        // REDDB_BULK_TIMING=1 prints a per-call breakdown of the bulk
        // insert path to stderr. Off by default — used by the reddb
        // benchmark harness to locate ingest bottlenecks.
        let trace = matches!(
            std::env::var("REDDB_BULK_TIMING").ok().as_deref(),
            Some("1") | Some("true") | Some("on")
        );
        let t_start = std::time::Instant::now();
        let n = entities.len();
        let manager = self.get_or_create_collection(collection);
        let t_get_coll = t_start.elapsed();

        // Assign IDs and per-table row_ids before serialization. Bulk insert
        // must follow the same global ID semantics as insert()/insert_auto().
        // `insert()`/`insert_auto()` already do this, but bulk_insert
        // needs the same guarantee or SQL/system fields like `row_id`
        // remain zero in the segment + serialized B-tree image.
        let t0 = std::time::Instant::now();
        for entity in &mut entities {
            if entity.id.raw() == 0 {
                entity.id = self.next_entity_id();
            } else {
                self.register_entity_id(entity.id);
            }
            if let EntityKind::TableRow { ref mut row_id, .. } = entity.kind {
                if *row_id == 0 {
                    *row_id = manager.next_row_id();
                } else {
                    manager.register_row_id(*row_id);
                }
            }
        }
        let t_assign_ids = t0.elapsed();

        // Capture graph node labels before entities are moved into the segment manager
        let graph_labels: Vec<Option<(String, EntityId)>> = entities
            .iter()
            .map(|e| {
                if let EntityKind::GraphNode(ref node) = e.kind {
                    Some((node.label.clone(), e.id))
                } else {
                    None
                }
            })
            .collect();

        // Pre-serialize for B-tree while we still have references
        let t0 = std::time::Instant::now();
        let serialized: Option<Vec<(Vec<u8>, Vec<u8>)>> = if self.pager.is_some() {
            let fv = self.format_version();
            Some(
                entities
                    .iter()
                    .map(|e| {
                        (
                            e.id.raw().to_be_bytes().to_vec(),
                            Self::serialize_entity_record(e, None, fv),
                        )
                    })
                    .collect(),
            )
        } else {
            None
        };
        let t_serialize = t0.elapsed();

        // Move entities into segment
        let t0 = std::time::Instant::now();
        let ids = manager.bulk_insert(entities)?;
        let t_manager = t0.elapsed();
        for id in &ids {
            self.register_entity_id(*id);
        }

        // Update graph label index for bulk-inserted GraphNode entities
        for label_entry in &graph_labels {
            if let Some((label, entity_id)) = label_entry {
                self.update_graph_label_index(collection, label, *entity_id);
            }
        }

        // REDDB_BULK_SKIP_PERSIST_UNSAFE=1 skips the persistent B-tree index
        // during bulk ingest.
        //
        // UNSAFE: for ephemeral benchmark containers ONLY.
        // This flag is silently ignored when a pager (durable storage) is active.
        // In persistent mode, bulk inserts ALWAYS write to the B-tree so the data
        // survives a cold restart without any manual rebuild step.
        //
        // The flag is only honoured when self.pager is None (in-memory / ephemeral).
        let skip_btree_requested = matches!(
            std::env::var("REDDB_BULK_SKIP_PERSIST_UNSAFE")
                .ok()
                .as_deref(),
            Some("1") | Some("true") | Some("on")
        );
        // Honour the flag only when there is no durable pager.
        // If a pager exists we are in persistent mode → always persist.
        let skip_btree = skip_btree_requested && self.pager.is_none();
        if skip_btree_requested && !skip_btree {
            // Flag was set but we ignored it because we have a real pager.
            static IGNORED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
            IGNORED.get_or_init(|| {
                tracing::warn!(
                    "REDDB_BULK_SKIP_PERSIST_UNSAFE set but durable pager is \
                     active — flag ignored; bulk inserts will be persisted normally"
                );
            });
        } else if skip_btree {
            // Ephemeral mode and flag is active — warn once.
            static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
            WARNED.get_or_init(|| {
                tracing::warn!(
                    "REDDB_BULK_SKIP_PERSIST_UNSAFE set (ephemeral/no-pager mode) — \
                     bulk inserts NOT durable; data will be lost on restart"
                );
            });
        }

        // Batch B-tree write from pre-serialized data.
        // Uses sorted bulk insert: walks to a leaf once, appends many entries,
        // writes each leaf exactly once per batch — O(N) instead of O(N²).
        let mut t_btree_lock = std::time::Duration::ZERO;
        let mut t_btree_insert = std::time::Duration::ZERO;
        let mut t_flush = std::time::Duration::ZERO;
        if !skip_btree {
            if let (Some(pager), Some(batch)) = (&self.pager, serialized.as_ref()) {
                let t0 = std::time::Instant::now();
                let mut btree_indices = self.btree_indices.write();
                let btree = btree_indices
                    .entry(collection.to_string())
                    .or_insert_with(|| Arc::new(BTree::new(Arc::clone(pager))));
                let root_before = btree.root_page_id();
                t_btree_lock = t0.elapsed();

                let t0 = std::time::Instant::now();
                let _ = btree.bulk_insert_sorted(&batch);
                t_btree_insert = t0.elapsed();
                let registry_dirty = root_before != btree.root_page_id();

                let t0 = std::time::Instant::now();
                if registry_dirty {
                    self.mark_paged_registry_dirty();
                }
                t_flush = t0.elapsed();
            }
        }

        let actions = serialized
            .as_ref()
            .map(|batch| {
                batch
                    .iter()
                    .map(|(_key, record)| StoreWalAction::UpsertEntityRecord {
                        collection: collection.to_string(),
                        record: record.clone(),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.finish_paged_write(actions)?;

        if trace {
            tracing::debug!(
                n,
                total = ?t_start.elapsed(),
                get_coll = ?t_get_coll,
                assign = ?t_assign_ids,
                serialize = ?t_serialize,
                manager = ?t_manager,
                btree_lock = ?t_btree_lock,
                btree = ?t_btree_insert,
                flush = ?t_flush,
                "bulk_insert timing"
            );
        }

        Ok(ids)
    }

    /// Insert an entity, creating collection if needed
    pub fn insert_auto(
        &self,
        collection: &str,
        entity: UnifiedEntity,
    ) -> Result<EntityId, StoreError> {
        let manager = self.get_or_create_collection(collection);
        let mut entity = entity;
        if entity.id.raw() == 0 {
            entity.id = self.next_entity_id();
        } else {
            self.register_entity_id(entity.id);
        }
        // Assign per-table sequential row_id if not set
        if let EntityKind::TableRow { ref mut row_id, .. } = entity.kind {
            if *row_id == 0 {
                *row_id = manager.next_row_id();
            } else {
                manager.register_row_id(*row_id);
            }
        }

        // Capture graph node label before entity is moved into the manager
        let graph_node_label: Option<String> = if let EntityKind::GraphNode(ref node) = entity.kind
        {
            Some(node.label.clone())
        } else {
            None
        };

        // Index into context index before consuming the entity
        self.context_index.index_entity(collection, &entity);

        let id = manager.insert(entity)?;
        // `register_entity_id` already advances the atomic counter on
        // the allocation path above (`self.next_entity_id()`), so the
        // second call here is a no-op CAS loop on the hot path. Only
        // needed for the caller-supplied-id branch which happens via
        // the `register_entity_id` call on line 573.

        // Update graph label index for GraphNode entities
        if let Some(ref label) = graph_node_label {
            self.update_graph_label_index(collection, label, id);
        }

        // Fetch the persisted entity once and reuse across btree insert,
        // cross-ref indexing, and WAL-action construction. Previously this
        // path did 3× manager.get(id), each cloning the entity — ~62 samples
        // of UnifiedEntity::clone in the insert hot path.
        let needs_entity = self.pager.is_some() || self.config.auto_index_refs;
        let persisted = if needs_entity { manager.get(id) } else { None };
        let persisted_metadata = if needs_entity {
            manager.get_metadata(id)
        } else {
            None
        };

        let mut registry_dirty = false;
        if let (Some(_pager), Some(entity)) = (&self.pager, persisted.as_ref()) {
            if let Some(btree) = self.get_or_create_btree(collection) {
                let root_before = btree.root_page_id();

                let key = id.raw().to_be_bytes();
                let value = Self::serialize_entity_record(
                    entity,
                    persisted_metadata.as_ref(),
                    self.format_version(),
                );
                btree.insert(&key, &value).map_err(|e| {
                    StoreError::Io(std::io::Error::other(format!(
                        "B-tree insert error while inserting '{collection}'/{id}: {e}"
                    )))
                })?;
                registry_dirty = root_before != btree.root_page_id();
            }
        }

        if self.config.auto_index_refs {
            if let Some(entity) = persisted.as_ref() {
                self.index_cross_refs(entity, collection)?;
            }
        }

        // Perf: pagerless → skip WAL-action construction (saves a
        // third manager.get + entity serialize per insert). For
        // in-memory runtimes finish_paged_write is a no-op.
        if self.pager.is_some() {
            let actions = persisted
                .as_ref()
                .map(|entity| {
                    vec![StoreWalAction::upsert_entity(
                        collection,
                        entity,
                        persisted_metadata.as_ref(),
                        self.format_version(),
                    )]
                })
                .unwrap_or_default();
            if registry_dirty {
                self.mark_paged_registry_dirty();
            }
            self.finish_paged_write(actions)?;
        }

        Ok(id)
    }

    /// Get an entity from a collection
    ///
    /// Prefers the live SegmentManager view so reads after update/delete observe
    /// the current in-memory state even when the paged B-tree image has not been
    /// refreshed yet. Falls back to the B-tree image for recovery-oriented reads.
    pub fn get(&self, collection: &str, id: EntityId) -> Option<UnifiedEntity> {
        // Prefer the live manager state to avoid stale reads after manager.update().
        if let Some(entity) = self
            .get_collection(collection)
            .and_then(|manager| manager.get(id))
        {
            return Some(entity);
        }

        // Fall back to the paged B-tree image if the manager does not currently hold the row.
        if self.pager.is_some() {
            let btree_indices = self.btree_indices.read();
            if let Some(btree) = btree_indices.get(collection) {
                let key = id.raw().to_be_bytes();
                if let Ok(Some(value)) = btree.get(&key) {
                    if let Ok((entity, _)) =
                        Self::deserialize_entity_record(&value, self.format_version())
                    {
                        return Some(entity);
                    }
                }
            }
        }

        None
    }

    /// Batch-fetch multiple entities from the same collection in minimal lock acquisitions.
    ///
    /// Preferred over N individual `get()` calls in indexed-scan loops (sorted index,
    /// bitmap, hash). Reduces lock acquisitions from N×3 to 2-3 total.
    /// Preserves input order: `result[i]` corresponds to `ids[i]`.
    pub fn get_batch(&self, collection: &str, ids: &[EntityId]) -> Vec<Option<UnifiedEntity>> {
        match self.get_collection(collection) {
            Some(manager) => manager.get_many(ids),
            None => vec![None; ids.len()],
        }
    }

    /// Get an entity from any collection
    pub fn get_any(&self, id: EntityId) -> Option<(String, UnifiedEntity)> {
        // Check entity cache first
        {
            let cache = self.entity_cache.read();
            if let Some(cached) = cache.get(&id.raw()) {
                return Some(cached.clone());
            }
        }

        // Full collection scan
        let collections = self.collections.read();
        for (name, manager) in collections.iter() {
            if let Some(entity) = manager.get(id) {
                let result = (name.clone(), entity);
                // Cache the result — drop read guard first to avoid deadlock
                drop(collections);
                {
                    let mut cache = self.entity_cache.write();
                    cache.insert(id.raw(), result.clone());
                    // Evict if too large
                    if cache.len() > 10_000 {
                        if let Some(&oldest_key) = cache.keys().next() {
                            cache.remove(&oldest_key);
                        }
                    }
                }
                return Some(result);
            }
        }
        None
    }

    /// Delete an entity
    pub fn delete(&self, collection: &str, id: EntityId) -> Result<bool, StoreError> {
        // Invalidate entity cache
        self.entity_cache.write().remove(&id.raw());
        let manager = self
            .get_collection(collection)
            .ok_or_else(|| StoreError::CollectionNotFound(collection.to_string()))?;

        let deleted = manager.delete(id)?;
        if !deleted {
            return Ok(false);
        }

        // Remove from B-tree index if active
        let mut registry_dirty = false;
        if self.pager.is_some() {
            let btree_indices = self.btree_indices.read();
            if let Some(btree) = btree_indices.get(collection) {
                let root_before = btree.root_page_id();
                let key = id.raw().to_be_bytes();
                let _ = btree.delete(&key);
                registry_dirty = root_before != btree.root_page_id();
            }
        }

        // Remove cross-references
        self.unindex_cross_refs(id)?;

        // Remove from graph label index
        self.remove_from_graph_label_index(collection, id);

        if registry_dirty {
            self.mark_paged_registry_dirty();
        }
        self.finish_paged_write([StoreWalAction::DeleteEntityRecord {
            collection: collection.to_string(),
            entity_id: id.raw(),
        }])?;

        Ok(true)
    }

    pub fn delete_batch(
        &self,
        collection: &str,
        ids: &[EntityId],
    ) -> Result<Vec<EntityId>, StoreError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        {
            let mut cache = self.entity_cache.write();
            for id in ids {
                cache.remove(&id.raw());
            }
        }

        let manager = self
            .get_collection(collection)
            .ok_or_else(|| StoreError::CollectionNotFound(collection.to_string()))?;

        let deleted_ids = manager.delete_batch(ids)?;
        if deleted_ids.is_empty() {
            return Ok(deleted_ids);
        }

        let mut registry_dirty = false;
        if self.pager.is_some() {
            let btree_indices = self.btree_indices.read();
            if let Some(btree) = btree_indices.get(collection) {
                let root_before = btree.root_page_id();
                for id in &deleted_ids {
                    let key = id.raw().to_be_bytes();
                    let _ = btree.delete(&key);
                }
                registry_dirty = root_before != btree.root_page_id();
            }
        }

        self.unindex_cross_refs_batch(&deleted_ids)?;
        self.remove_from_graph_label_index_batch(collection, &deleted_ids);
        if registry_dirty {
            self.mark_paged_registry_dirty();
        }
        let actions = deleted_ids
            .iter()
            .map(|id| StoreWalAction::DeleteEntityRecord {
                collection: collection.to_string(),
                entity_id: id.raw(),
            })
            .collect::<Vec<_>>();
        self.finish_paged_write(actions)?;

        Ok(deleted_ids)
    }

    /// Set metadata for an entity
    pub fn set_metadata(
        &self,
        collection: &str,
        id: EntityId,
        metadata: Metadata,
    ) -> Result<(), StoreError> {
        let manager = self
            .get_collection(collection)
            .ok_or_else(|| StoreError::CollectionNotFound(collection.to_string()))?;

        manager.set_metadata(id, metadata)?;
        if let Some(entity) = manager.get(id) {
            self.persist_entities_to_pager(collection, std::slice::from_ref(&entity))?;
        }
        Ok(())
    }

    /// Get metadata for an entity
    pub fn get_metadata(&self, collection: &str, id: EntityId) -> Option<Metadata> {
        self.get_collection(collection)?.get_metadata(id)
    }

    /// Add a cross-reference between entities
    pub fn add_cross_ref(
        &self,
        source_collection: &str,
        source_id: EntityId,
        target_collection: &str,
        target_id: EntityId,
        ref_type: RefType,
        weight: f32,
    ) -> Result<(), StoreError> {
        // Check source exists
        let source_manager = self
            .get_collection(source_collection)
            .ok_or_else(|| StoreError::CollectionNotFound(source_collection.to_string()))?;

        if source_manager.get(source_id).is_none() {
            return Err(StoreError::EntityNotFound(source_id));
        }

        // Check target exists
        let target_manager = self
            .get_collection(target_collection)
            .ok_or_else(|| StoreError::CollectionNotFound(target_collection.to_string()))?;

        if target_manager.get(target_id).is_none() {
            return Err(StoreError::EntityNotFound(target_id));
        }

        // Check limits
        let current_refs = self
            .cross_refs
            .read()
            .get(&source_id)
            .map_or(0, |v| v.len());

        if current_refs >= self.config.max_cross_refs {
            return Err(StoreError::TooManyRefs(source_id));
        }

        let mut registry_dirty = false;
        {
            let mut forward = self.cross_refs.write();
            let refs = forward.entry(source_id).or_default();
            let inserted = !refs.iter().any(|(id, kind, coll)| {
                *id == target_id && *kind == ref_type && coll == target_collection
            });
            if inserted {
                refs.push((target_id, ref_type, target_collection.to_string()));
                registry_dirty = true;
            }
        }

        {
            let mut reverse = self.reverse_refs.write();
            let refs = reverse.entry(target_id).or_default();
            let inserted = !refs.iter().any(|(id, kind, coll)| {
                *id == source_id && *kind == ref_type && coll == source_collection
            });
            if inserted {
                refs.push((source_id, ref_type, source_collection.to_string()));
                registry_dirty = true;
            }
        }

        if let Some(mut entity) = source_manager.get(source_id) {
            if !entity.cross_refs().iter().any(|xref| {
                xref.target == target_id
                    && xref.ref_type == ref_type
                    && xref.target_collection == target_collection
            }) {
                let cross_ref = CrossRef::with_weight(
                    source_id,
                    target_id,
                    target_collection,
                    ref_type,
                    weight,
                );
                entity.add_cross_ref(cross_ref);
                let _ = source_manager.update(entity.clone());
                registry_dirty = true;
                self.persist_entities_to_pager(source_collection, std::slice::from_ref(&entity))?;
            }
        }

        if registry_dirty {
            self.mark_paged_registry_dirty();
            if matches!(
                self.config.durability_mode,
                crate::api::DurabilityMode::Strict
            ) {
                self.flush_paged_state()?;
            }
        }

        Ok(())
    }

    /// Get cross-references from an entity
    pub fn get_refs_from(&self, id: EntityId) -> Vec<(EntityId, RefType, String)> {
        self.cross_refs.read().get(&id).cloned().unwrap_or_default()
    }

    /// Get cross-references to an entity
    pub fn get_refs_to(&self, id: EntityId) -> Vec<(EntityId, RefType, String)> {
        self.reverse_refs
            .read()
            .get(&id)
            .cloned()
            .unwrap_or_default()
    }

    /// Expand cross-references to get related entities
    pub fn expand_refs(
        &self,
        id: EntityId,
        depth: u32,
        ref_types: Option<&[RefType]>,
    ) -> Vec<(UnifiedEntity, u32, RefType)> {
        let mut results = Vec::new();
        let mut visited = std::collections::HashSet::new();
        visited.insert(id);

        self.expand_refs_recursive(id, depth, ref_types, &mut visited, &mut results, 1);

        results
    }

    fn expand_refs_recursive(
        &self,
        id: EntityId,
        max_depth: u32,
        ref_types: Option<&[RefType]>,
        visited: &mut std::collections::HashSet<EntityId>,
        results: &mut Vec<(UnifiedEntity, u32, RefType)>,
        current_depth: u32,
    ) {
        if current_depth > max_depth {
            return;
        }

        for (target_id, ref_type, target_collection) in self.get_refs_from(id) {
            if visited.contains(&target_id) {
                continue;
            }

            if let Some(types) = ref_types {
                if !types.contains(&ref_type) {
                    continue;
                }
            }

            visited.insert(target_id);

            if let Some(entity) = self.get(&target_collection, target_id) {
                results.push((entity, current_depth, ref_type));

                // Recurse
                self.expand_refs_recursive(
                    target_id,
                    max_depth,
                    ref_types,
                    visited,
                    results,
                    current_depth + 1,
                );
            }
        }
    }

    /// Index cross-references from an entity
    pub(crate) fn index_cross_refs(
        &self,
        entity: &UnifiedEntity,
        collection: &str,
    ) -> Result<(), StoreError> {
        let mut registry_dirty = false;
        for cross_ref in entity.cross_refs() {
            if cross_ref.target_collection.is_empty() {
                continue;
            }
            {
                let mut forward = self.cross_refs.write();
                let refs = forward.entry(cross_ref.source).or_default();
                let inserted = !refs.iter().any(|(id, kind, coll)| {
                    *id == cross_ref.target
                        && *kind == cross_ref.ref_type
                        && coll == &cross_ref.target_collection
                });
                if inserted {
                    refs.push((
                        cross_ref.target,
                        cross_ref.ref_type,
                        cross_ref.target_collection.clone(),
                    ));
                    registry_dirty = true;
                }
            }

            {
                let mut reverse = self.reverse_refs.write();
                let refs = reverse.entry(cross_ref.target).or_default();
                let inserted = !refs.iter().any(|(id, kind, coll)| {
                    *id == cross_ref.source && *kind == cross_ref.ref_type && coll == collection
                });
                if inserted {
                    refs.push((cross_ref.source, cross_ref.ref_type, collection.to_string()));
                    registry_dirty = true;
                }
            }
        }

        if registry_dirty {
            self.mark_paged_registry_dirty();
        }

        Ok(())
    }

    /// Remove cross-references for an entity
    pub(crate) fn unindex_cross_refs(&self, id: EntityId) -> Result<(), StoreError> {
        // Remove forward refs
        self.cross_refs.write().remove(&id);

        // Remove from reverse refs (scan all)
        let mut reverse = self.reverse_refs.write();
        for refs in reverse.values_mut() {
            refs.retain(|(source, _, _)| *source != id);
        }
        reverse.remove(&id);
        self.mark_paged_registry_dirty();

        Ok(())
    }

    pub(crate) fn unindex_cross_refs_batch(&self, ids: &[EntityId]) -> Result<(), StoreError> {
        if ids.is_empty() {
            return Ok(());
        }

        let id_set: std::collections::HashSet<EntityId> = ids.iter().copied().collect();

        {
            let mut forward = self.cross_refs.write();
            for id in &id_set {
                forward.remove(id);
            }
        }

        {
            let mut reverse = self.reverse_refs.write();
            for refs in reverse.values_mut() {
                refs.retain(|(source, _, _)| !id_set.contains(source));
            }
            reverse.retain(|target, refs| !id_set.contains(target) && !refs.is_empty());
        }
        self.mark_paged_registry_dirty();

        Ok(())
    }

    /// Query across all collections with a filter
    pub fn query_all<F>(&self, filter: F) -> Vec<(String, UnifiedEntity)>
    where
        F: Fn(&UnifiedEntity) -> bool + Clone + Send + Sync,
    {
        let collections = self.collections.read();
        let pairs: Vec<_> = collections.iter().collect();

        let use_parallel = pairs.len() > 1 && crate::runtime::SystemInfo::should_parallelize();
        if !use_parallel {
            // Single collection — no parallelism overhead
            return pairs
                .into_iter()
                .flat_map(|(name, mgr)| {
                    mgr.query_all(filter.clone())
                        .into_iter()
                        .map(move |e| (name.clone(), e))
                })
                .collect();
        }

        // Multiple collections — scan in parallel
        let filter_ref = &filter;
        let collection_results: Vec<Vec<(String, UnifiedEntity)>> = std::thread::scope(|s| {
            pairs
                .iter()
                .map(|(name, manager)| {
                    let name = (*name).clone();
                    s.spawn(move || {
                        manager
                            .query_all(|e| filter_ref(e))
                            .into_iter()
                            .map(|e| (name.clone(), e))
                            .collect::<Vec<_>>()
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|h| h.join().unwrap_or_default())
                .collect()
        });

        collection_results.into_iter().flatten().collect()
    }

    /// Filter by metadata across all collections
    pub fn filter_metadata_all(
        &self,
        filters: &[(String, MetadataFilter)],
    ) -> Vec<(String, EntityId)> {
        let mut results = Vec::new();
        let collections = self.collections.read();

        for (name, manager) in collections.iter() {
            for id in manager.filter_metadata(filters) {
                results.push((name.clone(), id));
            }
        }

        results
    }

    /// Get statistics
    pub fn stats(&self) -> StoreStats {
        let collections = self.collections.read();

        let mut stats = StoreStats {
            collection_count: collections.len(),
            ..Default::default()
        };

        for (name, manager) in collections.iter() {
            let manager_stats = manager.stats();
            stats.total_entities += manager_stats.total_entities;
            stats.total_memory_bytes += manager_stats.total_memory_bytes;
            stats.collections.insert(name.clone(), manager_stats);
        }

        stats
    }

    /// Run maintenance on all collections
    pub fn run_maintenance(&self) -> Result<(), StoreError> {
        let collections = self.collections.read();
        for manager in collections.values() {
            manager.run_maintenance()?;
        }
        Ok(())
    }
}

/// Flatten a JSON value into dot-notation key-value pairs for red_config.
fn flatten_config_json(
    prefix: &str,
    value: &crate::serde_json::Value,
    out: &mut Vec<(String, crate::storage::schema::Value)>,
) {
    use crate::storage::schema::Value;
    match value {
        crate::serde_json::Value::Object(map) => {
            for (k, v) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten_config_json(&key, v, out);
            }
        }
        crate::serde_json::Value::String(s) => {
            out.push((prefix.to_string(), Value::text(s.clone())));
        }
        crate::serde_json::Value::Number(n) => {
            if n.fract().abs() < f64::EPSILON {
                out.push((prefix.to_string(), Value::UnsignedInteger(*n as u64)));
            } else {
                out.push((prefix.to_string(), Value::Float(*n)));
            }
        }
        crate::serde_json::Value::Bool(b) => {
            out.push((prefix.to_string(), Value::Boolean(*b)));
        }
        crate::serde_json::Value::Null => {
            out.push((prefix.to_string(), Value::Null));
        }
        crate::serde_json::Value::Array(arr) => {
            let json_str = crate::serde_json::to_string(value).unwrap_or_default();
            out.push((prefix.to_string(), Value::text(json_str)));
        }
    }
}
