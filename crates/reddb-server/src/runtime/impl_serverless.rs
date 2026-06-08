use super::*;

impl RedDBRuntime {
    pub fn serverless_file_plan(&self) -> Option<reddb_file::ServerlessFilePlan> {
        let data_path = self.inner.db.options().data_path.as_ref()?;
        let generation = self
            .primary_logical_head_lsn()
            .max(self.cdc_current_lsn())
            .max(1);
        Some(reddb_file::ServerlessFilePlan::for_data_path(
            data_path, generation,
        ))
    }

    fn serverless_file_plan_for_generation(
        &self,
        generation: u64,
    ) -> Option<reddb_file::ServerlessFilePlan> {
        let plan = self.serverless_file_plan()?;
        Some(plan.for_generation(generation))
    }

    fn serverless_local_cache_for_generation(
        &self,
        generation: u64,
    ) -> Option<reddb_file::ServerlessLocalCache> {
        let plan = self.serverless_file_plan_for_generation(generation)?;
        Some(plan.local_cache())
    }

    fn serverless_collection_snapshot_bytes(&self, collection: &str) -> RedDBResult<Vec<u8>> {
        let source = self
            .inner
            .db
            .store()
            .get_collection(collection)
            .ok_or_else(|| {
                RedDBError::Internal(format!("serverless collection not found: {collection}"))
            })?;
        let snapshot = crate::storage::unified::UnifiedStore::with_config(
            crate::storage::unified::UnifiedStoreConfig::default(),
        );
        let mut error: Option<RedDBError> = None;
        source.for_each_entity(|entity| {
            let cloned = entity.clone();
            match snapshot.insert_auto(collection, cloned) {
                Ok(id) => {
                    if let Some(metadata) = source.get_metadata(entity.id) {
                        if let Err(err) = snapshot.set_metadata(collection, id, metadata) {
                            error = Some(RedDBError::Internal(err.to_string()));
                            return false;
                        }
                    }
                    true
                }
                Err(err) => {
                    error = Some(RedDBError::Internal(err.to_string()));
                    false
                }
            }
        });
        if let Some(error) = error {
            return Err(error);
        }
        Ok(snapshot.to_binary_dump_bytes())
    }

    pub fn publish_serverless_generation(
        &self,
    ) -> RedDBResult<Option<reddb_file::ServerlessGenerationPointer>> {
        let Some(base_plan) = self.serverless_file_plan() else {
            return Ok(None);
        };
        self.flush()?;
        let next_generation = match base_plan.read_current_pointer_verified() {
            Ok(pointer) => base_plan
                .generation
                .max(pointer.generation.saturating_add(1)),
            Err(reddb_file::RdbFileError::Io(err))
                if err.kind() == std::io::ErrorKind::NotFound =>
            {
                base_plan.generation
            }
            Err(err) => {
                return Err(RedDBError::InvalidOperation(format!(
                    "corrupt serverless generation: {err}"
                )));
            }
        };
        let plan = reddb_file::ServerlessFilePlan::new(
            base_plan.root,
            base_plan.namespace,
            next_generation,
        )
        .with_cache_policy(base_plan.cache_policy);
        let mut extent_index = reddb_file::ServerlessExtentIndex::new(plan.generation);
        let mut collections = self.inner.db.store().list_collections();
        collections.sort();
        collections.dedup();
        if collections.is_empty() {
            collections.push("__database__".to_string());
        }
        let mut collection_data = Vec::new();
        for collection in collections {
            let payload = if collection == "__database__" {
                self.inner.db.store().to_binary_dump_bytes()
            } else {
                self.serverless_collection_snapshot_bytes(&collection)?
            };
            let offset = collection_data.len() as u64;
            collection_data.extend_from_slice(&payload);
            extent_index.push(
                plan.collection_data_extent_ref(collection, offset, &payload, true)
                    .map_err(|err| RedDBError::Internal(err.to_string()))?,
            );
        }
        let secondary_index =
            reddb_file::ServerlessSecondaryIndex::from_extent_index(&extent_index);
        let pointer = plan
            .publish_core_generation(&extent_index, &collection_data, &secondary_index.encode())
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        Ok(Some(pointer))
    }

    pub fn read_current_serverless_generation_verified(
        &self,
    ) -> RedDBResult<Option<reddb_file::ServerlessGenerationPointer>> {
        let Some(plan) = self.serverless_file_plan() else {
            return Ok(None);
        };
        match plan.read_current_pointer_verified() {
            Ok(pointer) => Ok(Some(pointer)),
            Err(reddb_file::RdbFileError::Io(err))
                if err.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(None)
            }
            Err(err) => Err(RedDBError::InvalidOperation(format!(
                "corrupt serverless generation: {err}"
            ))),
        }
    }

    pub fn hydrate_current_serverless_collection(
        &self,
        collection: &str,
    ) -> RedDBResult<Option<Vec<reddb_file::ServerlessHydratedRange>>> {
        let Some(pointer) = self.read_current_serverless_generation_verified()? else {
            return Ok(None);
        };
        let Some(plan) = self.serverless_file_plan_for_generation(pointer.generation) else {
            return Ok(None);
        };
        let secondary =
            reddb_file::ServerlessSecondaryIndex::read_from_path(plan.secondary_index_path())
                .map_err(|err| {
                    RedDBError::InvalidOperation(format!("corrupt serverless generation: {err}"))
                })?;
        let hydration = secondary.hydration_plan_for_collection(collection);
        plan.hydrate_local_plan(&hydration)
            .map(Some)
            .map_err(|err| {
                RedDBError::InvalidOperation(format!("serverless hydrate failed: {err}"))
            })
    }

    pub fn hydrate_current_serverless_key(
        &self,
        collection: &str,
        key: &[u8],
    ) -> RedDBResult<Option<Vec<reddb_file::ServerlessHydratedRange>>> {
        let Some(pointer) = self.read_current_serverless_generation_verified()? else {
            return Ok(None);
        };
        let Some(plan) = self.serverless_file_plan_for_generation(pointer.generation) else {
            return Ok(None);
        };
        let index = reddb_file::ServerlessExtentIndex::read_from_path(plan.extent_index_path())
            .map_err(|err| {
                RedDBError::InvalidOperation(format!("corrupt serverless generation: {err}"))
            })?;
        let hydration = index.hydration_plan_for_key(collection, key);
        plan.hydrate_local_plan(&hydration)
            .map(Some)
            .map_err(|err| {
                RedDBError::InvalidOperation(format!("serverless hydrate failed: {err}"))
            })
    }

    pub fn hydrate_current_serverless_range(
        &self,
        collection: &str,
        range_start: &[u8],
        range_end: &[u8],
    ) -> RedDBResult<Option<Vec<reddb_file::ServerlessHydratedRange>>> {
        let Some(pointer) = self.read_current_serverless_generation_verified()? else {
            return Ok(None);
        };
        let Some(plan) = self.serverless_file_plan_for_generation(pointer.generation) else {
            return Ok(None);
        };
        let index = reddb_file::ServerlessExtentIndex::read_from_path(plan.extent_index_path())
            .map_err(|err| {
                RedDBError::InvalidOperation(format!("corrupt serverless generation: {err}"))
            })?;
        let hydration = index
            .hydration_plan_for_range(collection, range_start, range_end)
            .map_err(|err| RedDBError::InvalidOperation(err.to_string()))?;
        plan.hydrate_local_plan(&hydration)
            .map(Some)
            .map_err(|err| {
                RedDBError::InvalidOperation(format!("serverless hydrate failed: {err}"))
            })
    }

    pub fn hydrate_current_serverless_key_cached(
        &self,
        collection: &str,
        key: &[u8],
    ) -> RedDBResult<Option<Vec<reddb_file::ServerlessHydratedRange>>> {
        let Some(pointer) = self.read_current_serverless_generation_verified()? else {
            return Ok(None);
        };
        let Some(plan) = self.serverless_file_plan_for_generation(pointer.generation) else {
            return Ok(None);
        };
        let Some(cache) = self.serverless_local_cache_for_generation(pointer.generation) else {
            return Ok(None);
        };
        let index = reddb_file::ServerlessExtentIndex::read_from_path(plan.extent_index_path())
            .map_err(|err| {
                RedDBError::InvalidOperation(format!("corrupt serverless generation: {err}"))
            })?;
        let hydration = index.hydration_plan_for_key(collection, key);
        plan.hydrate_local_plan_cached(&hydration, &cache)
            .map(Some)
            .map_err(|err| {
                RedDBError::InvalidOperation(format!("serverless hydrate failed: {err}"))
            })
    }

    pub fn prefetch_current_serverless_hot_extents_cached(
        &self,
    ) -> RedDBResult<Option<Vec<reddb_file::ServerlessHydratedRange>>> {
        let Some(pointer) = self.read_current_serverless_generation_verified()? else {
            return Ok(None);
        };
        let Some(plan) = self.serverless_file_plan_for_generation(pointer.generation) else {
            return Ok(None);
        };
        let Some(cache) = self.serverless_local_cache_for_generation(pointer.generation) else {
            return Ok(None);
        };
        let index = reddb_file::ServerlessExtentIndex::read_from_path(plan.extent_index_path())
            .map_err(|err| {
                RedDBError::InvalidOperation(format!("corrupt serverless generation: {err}"))
            })?;
        plan.prefetch_hot_extents_cached(&index, &cache)
            .map(Some)
            .map_err(|err| {
                RedDBError::InvalidOperation(format!("serverless hydrate failed: {err}"))
            })
    }
}
