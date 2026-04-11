use super::*;

impl UnifiedStore {
    pub fn create_collection(&self, name: impl Into<String>) -> Result<(), StoreError> {
        let name = name.into();
        let mut collections = self
            .collections
            .write()
            .map_err(|_| StoreError::Internal("collections lock poisoned".into()))?;

        if collections.contains_key(&name) {
            return Err(StoreError::CollectionExists(name));
        }

        let manager = SegmentManager::with_config(&name, self.config.manager_config.clone());
        collections.insert(name, Arc::new(manager));

        Ok(())
    }

    /// Get or create a collection
    pub fn get_or_create_collection(&self, name: impl Into<String>) -> Arc<SegmentManager> {
        let name = name.into();
        let mut collections = self.collections.write().unwrap_or_else(|e| e.into_inner());

        if let Some(manager) = collections.get(&name) {
            return Arc::clone(manager);
        }

        let manager = Arc::new(SegmentManager::with_config(
            &name,
            self.config.manager_config.clone(),
        ));
        collections.insert(name, Arc::clone(&manager));
        manager
    }

    /// Get a collection
    pub fn get_collection(&self, name: &str) -> Option<Arc<SegmentManager>> {
        self.collections
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .map(Arc::clone)
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
                    table: "red_config".to_string(),
                    row_id: 0,
                },
                EntityData::Row(RowData {
                    columns: Vec::new(),
                    named: Some(
                        [
                            ("key".to_string(), crate::storage::schema::Value::Text(key)),
                            ("value".to_string(), value),
                        ]
                        .into_iter()
                        .collect(),
                    ),
                }),
            );
            if self.insert_auto("red_config", entity).is_ok() {
                saved += 1;
            }
        }
        saved
    }

    /// List all collections
    pub fn list_collections(&self) -> Vec<String> {
        self.collections
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect()
    }

    /// Drop a collection
    pub fn drop_collection(&self, name: &str) -> Result<(), StoreError> {
        let mut collections = self
            .collections
            .write()
            .map_err(|_| StoreError::Internal("collections lock poisoned".into()))?;

        if collections.remove(name).is_none() {
            return Err(StoreError::CollectionNotFound(name.to_string()));
        }

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
        let id = manager.insert(entity)?;
        self.register_entity_id(id);

        // Also insert into B-tree index if pager is active
        if let Some(pager) = &self.pager {
            if let Some(entity) = manager.get(id) {
                let mut btree_indices = self
                    .btree_indices
                    .write()
                    .map_err(|_| StoreError::Internal("btree_indices lock poisoned".into()))?;
                let btree = btree_indices
                    .entry(collection.to_string())
                    .or_insert_with(|| BTree::new(Arc::clone(pager)));

                let key = id.raw().to_le_bytes();
                let value = Self::serialize_entity(&entity, self.format_version());
                // Ignore duplicate key errors (update scenario)
                let _ = btree.insert(&key, &value);
            }
        }

        // Index cross-references if enabled
        if self.config.auto_index_refs {
            if let Some(entity) = manager.get(id) {
                self.index_cross_refs(&entity, collection)?;
            }
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
        entities: Vec<UnifiedEntity>,
    ) -> Result<Vec<EntityId>, StoreError> {
        let manager = self.get_or_create_collection(collection);

        // Single lock bulk insert (skips bloom/memtable/cross-refs)
        let ids = manager.bulk_insert(entities)?;

        // Batch B-tree writes if pager is active
        if let Some(pager) = &self.pager {
            let mut btree_indices = self
                .btree_indices
                .write()
                .map_err(|_| StoreError::Internal("btree_indices lock poisoned".into()))?;
            let btree = btree_indices
                .entry(collection.to_string())
                .or_insert_with(|| BTree::new(Arc::clone(pager)));

            let format_version = self.format_version();
            for id in &ids {
                if let Some(entity) = manager.get(*id) {
                    let key = id.raw().to_le_bytes();
                    let value = Self::serialize_entity(&entity, format_version);
                    let _ = btree.insert(&key, &value);
                }
            }
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

        // Index into context index before consuming the entity
        self.context_index.index_entity(collection, &entity);

        let id = manager.insert(entity)?;
        self.register_entity_id(id);

        // Also insert into B-tree index if pager is active
        if let Some(pager) = &self.pager {
            if let Some(entity) = manager.get(id) {
                let mut btree_indices = self
                    .btree_indices
                    .write()
                    .map_err(|_| StoreError::Internal("btree_indices lock poisoned".into()))?;
                let btree = btree_indices
                    .entry(collection.to_string())
                    .or_insert_with(|| BTree::new(Arc::clone(pager)));

                let key = id.raw().to_le_bytes();
                let value = Self::serialize_entity(&entity, self.format_version());
                let _ = btree.insert(&key, &value);
            }
        }

        if self.config.auto_index_refs {
            if let Some(entity) = manager.get(id) {
                self.index_cross_refs(&entity, collection)?;
            }
        }

        Ok(id)
    }

    /// Get an entity from a collection
    ///
    /// Uses B-tree index for O(log n) lookup when page-based storage is active.
    /// Falls back to linear scan through SegmentManager otherwise.
    pub fn get(&self, collection: &str, id: EntityId) -> Option<UnifiedEntity> {
        // Try B-tree index first for O(log n) lookup
        if self.pager.is_some() {
            let btree_indices = self.btree_indices.read().unwrap_or_else(|e| e.into_inner());
            if let Some(btree) = btree_indices.get(collection) {
                let key = id.raw().to_le_bytes();
                if let Ok(Some(value)) = btree.get(&key) {
                    if let Ok(entity) = Self::deserialize_entity(&value, self.format_version()) {
                        return Some(entity);
                    }
                }
            }
        }

        // Fall back to SegmentManager
        self.get_collection(collection)?.get(id)
    }

    /// Get an entity from any collection
    pub fn get_any(&self, id: EntityId) -> Option<(String, UnifiedEntity)> {
        // Check entity cache first
        if let Ok(cache) = self.entity_cache.read() {
            if let Some(cached) = cache.get(&id.raw()) {
                return Some(cached.clone());
            }
        }

        // Full collection scan
        let collections = self.collections.read().unwrap_or_else(|e| e.into_inner());
        for (name, manager) in collections.iter() {
            if let Some(entity) = manager.get(id) {
                let result = (name.clone(), entity);
                // Cache the result
                if let Ok(mut cache) = self.entity_cache.write() {
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
        if let Ok(mut cache) = self.entity_cache.write() {
            cache.remove(&id.raw());
        }
        let manager = self
            .get_collection(collection)
            .ok_or_else(|| StoreError::CollectionNotFound(collection.to_string()))?;

        // Remove from B-tree index if active
        if self.pager.is_some() {
            let btree_indices = self
                .btree_indices
                .read()
                .map_err(|_| StoreError::Internal("btree_indices lock poisoned".into()))?;
            if let Some(btree) = btree_indices.get(collection) {
                let key = id.raw().to_le_bytes();
                let _ = btree.delete(&key);
            }
        }

        // Remove cross-references
        self.unindex_cross_refs(id)?;

        Ok(manager.delete(id)?)
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

        Ok(manager.set_metadata(id, metadata)?)
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
            .map_err(|_| StoreError::Internal("cross_refs lock poisoned".into()))?
            .get(&source_id)
            .map_or(0, |v| v.len());

        if current_refs >= self.config.max_cross_refs {
            return Err(StoreError::TooManyRefs(source_id));
        }

        {
            let mut forward = self
                .cross_refs
                .write()
                .map_err(|_| StoreError::Internal("cross_refs lock poisoned".into()))?;
            let refs = forward.entry(source_id).or_default();
            if !refs.iter().any(|(id, kind, coll)| {
                *id == target_id && *kind == ref_type && coll == target_collection
            }) {
                refs.push((target_id, ref_type, target_collection.to_string()));
            }
        }

        {
            let mut reverse = self
                .reverse_refs
                .write()
                .map_err(|_| StoreError::Internal("reverse_refs lock poisoned".into()))?;
            let refs = reverse.entry(target_id).or_default();
            if !refs.iter().any(|(id, kind, coll)| {
                *id == source_id && *kind == ref_type && coll == source_collection
            }) {
                refs.push((source_id, ref_type, source_collection.to_string()));
            }
        }

        if let Some(mut entity) = source_manager.get(source_id) {
            if !entity.cross_refs.iter().any(|xref| {
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
                let _ = source_manager.update(entity);
            }
        }

        Ok(())
    }

    /// Get cross-references from an entity
    pub fn get_refs_from(&self, id: EntityId) -> Vec<(EntityId, RefType, String)> {
        self.cross_refs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .cloned()
            .unwrap_or_default()
    }

    /// Get cross-references to an entity
    pub fn get_refs_to(&self, id: EntityId) -> Vec<(EntityId, RefType, String)> {
        self.reverse_refs
            .read()
            .unwrap_or_else(|e| e.into_inner())
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
        for cross_ref in &entity.cross_refs {
            if cross_ref.target_collection.is_empty() {
                continue;
            }
            {
                let mut forward = self
                    .cross_refs
                    .write()
                    .map_err(|_| StoreError::Internal("cross_refs lock poisoned".into()))?;
                let refs = forward.entry(cross_ref.source).or_default();
                if !refs.iter().any(|(id, kind, coll)| {
                    *id == cross_ref.target
                        && *kind == cross_ref.ref_type
                        && coll == &cross_ref.target_collection
                }) {
                    refs.push((
                        cross_ref.target,
                        cross_ref.ref_type,
                        cross_ref.target_collection.clone(),
                    ));
                }
            }

            {
                let mut reverse = self
                    .reverse_refs
                    .write()
                    .map_err(|_| StoreError::Internal("reverse_refs lock poisoned".into()))?;
                let refs = reverse.entry(cross_ref.target).or_default();
                if !refs.iter().any(|(id, kind, coll)| {
                    *id == cross_ref.source && *kind == cross_ref.ref_type && coll == collection
                }) {
                    refs.push((cross_ref.source, cross_ref.ref_type, collection.to_string()));
                }
            }
        }

        Ok(())
    }

    /// Remove cross-references for an entity
    fn unindex_cross_refs(&self, id: EntityId) -> Result<(), StoreError> {
        // Remove forward refs
        self.cross_refs
            .write()
            .map_err(|_| StoreError::Internal("cross_refs lock poisoned".into()))?
            .remove(&id);

        // Remove from reverse refs (scan all)
        let mut reverse = self
            .reverse_refs
            .write()
            .map_err(|_| StoreError::Internal("reverse_refs lock poisoned".into()))?;
        for refs in reverse.values_mut() {
            refs.retain(|(source, _, _)| *source != id);
        }
        reverse.remove(&id);

        Ok(())
    }

    /// Query across all collections with a filter
    pub fn query_all<F>(&self, filter: F) -> Vec<(String, UnifiedEntity)>
    where
        F: Fn(&UnifiedEntity) -> bool + Clone + Send + Sync,
    {
        let collections = self.collections.read().unwrap_or_else(|e| e.into_inner());
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
        let collections = self.collections.read().unwrap_or_else(|e| e.into_inner());

        for (name, manager) in collections.iter() {
            for id in manager.filter_metadata(filters) {
                results.push((name.clone(), id));
            }
        }

        results
    }

    /// Get statistics
    pub fn stats(&self) -> StoreStats {
        let collections = self.collections.read().unwrap_or_else(|e| e.into_inner());

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
        let collections = self
            .collections
            .read()
            .map_err(|_| StoreError::Internal("collections lock poisoned".into()))?;
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
            out.push((prefix.to_string(), Value::Text(s.clone())));
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
            out.push((prefix.to_string(), Value::Text(json_str)));
        }
    }
}
