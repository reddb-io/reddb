use super::*;

impl RedDBRuntime {
    pub fn snapshots(&self) -> RedDBResult<Vec<SnapshotDescriptor>> {
        let snapshots = self.inner.db.snapshots();
        if snapshots.is_empty() {
            return Err(RedDBError::NotFound("physical metadata".to_string()));
        }
        Ok(snapshots)
    }

    pub fn create_snapshot(&self) -> RedDBResult<SnapshotDescriptor> {
        self.checkpoint()?;
        self.inner
            .db
            .snapshots()
            .last()
            .cloned()
            .ok_or_else(|| RedDBError::Internal("snapshot metadata was not recorded".to_string()))
    }

    pub fn exports(&self) -> RedDBResult<Vec<ExportDescriptor>> {
        Ok(self.inner.db.exports())
    }

    pub fn native_header(&self) -> RedDBResult<PhysicalFileHeader> {
        self.inner
            .db
            .store()
            .physical_file_header()
            .ok_or_else(|| RedDBError::NotFound("native physical header".to_string()))
    }

    pub fn native_collection_roots(&self) -> RedDBResult<std::collections::BTreeMap<String, u64>> {
        self.inner
            .db
            .native_collection_roots()
            .ok_or_else(|| RedDBError::NotFound("native collection roots".to_string()))
    }

    pub fn native_manifest_summary(&self) -> RedDBResult<NativeManifestSummary> {
        self.inner
            .db
            .native_manifest_summary()
            .ok_or_else(|| RedDBError::NotFound("native manifest summary".to_string()))
    }

    pub fn native_registry_summary(&self) -> RedDBResult<NativeRegistrySummary> {
        self.inner
            .db
            .native_registry_summary()
            .ok_or_else(|| RedDBError::NotFound("native registry summary".to_string()))
    }

    pub fn native_recovery_summary(&self) -> RedDBResult<NativeRecoverySummary> {
        self.inner
            .db
            .native_recovery_summary()
            .ok_or_else(|| RedDBError::NotFound("native recovery summary".to_string()))
    }

    pub fn native_catalog_summary(&self) -> RedDBResult<NativeCatalogSummary> {
        self.inner
            .db
            .native_catalog_summary()
            .ok_or_else(|| RedDBError::NotFound("native catalog summary".to_string()))
    }

    pub fn native_physical_state(&self) -> RedDBResult<NativePhysicalState> {
        self.inner
            .db
            .native_physical_state()
            .ok_or_else(|| RedDBError::NotFound("native physical state".to_string()))
    }

    pub fn native_vector_artifact_pages(
        &self,
    ) -> RedDBResult<Vec<crate::storage::unified::store::NativeVectorArtifactPageSummary>> {
        self.inner
            .db
            .native_vector_artifact_pages()
            .ok_or_else(|| RedDBError::NotFound("native vector artifact pages".to_string()))
    }

    pub fn inspect_native_vector_artifact(
        &self,
        collection: &str,
        artifact_kind: Option<&str>,
    ) -> RedDBResult<crate::storage::unified::devx::NativeVectorArtifactInspection> {
        self.inner
            .db
            .inspect_native_vector_artifact(collection, artifact_kind)
            .map_err(|err| {
                if err.contains("not found") || err.contains("not available") {
                    RedDBError::NotFound(err)
                } else {
                    RedDBError::Internal(err)
                }
            })
    }

    pub fn warmup_native_vector_artifact(
        &self,
        collection: &str,
        artifact_kind: Option<&str>,
    ) -> RedDBResult<crate::storage::unified::devx::NativeVectorArtifactInspection> {
        self.inner
            .db
            .warmup_native_vector_artifact(collection, artifact_kind)
            .map_err(|err| {
                if err.contains("not found") || err.contains("not available") {
                    RedDBError::NotFound(err)
                } else {
                    RedDBError::Internal(err)
                }
            })
    }

    pub fn inspect_native_vector_artifacts(
        &self,
    ) -> RedDBResult<crate::storage::unified::devx::NativeVectorArtifactBatchInspection> {
        self.inner
            .db
            .inspect_native_vector_artifacts()
            .map_err(|err| {
                if err.contains("not available") {
                    RedDBError::NotFound(err)
                } else {
                    RedDBError::Internal(err)
                }
            })
    }

    pub fn warmup_native_vector_artifacts(
        &self,
    ) -> RedDBResult<crate::storage::unified::devx::NativeVectorArtifactBatchInspection> {
        self.inner
            .db
            .warmup_native_vector_artifacts()
            .map_err(|err| {
                if err.contains("not available") {
                    RedDBError::NotFound(err)
                } else {
                    RedDBError::Internal(err)
                }
            })
    }

    pub fn native_header_repair_policy(&self) -> RedDBResult<String> {
        let policy = self.inner.db.native_header_repair_policy().ok_or_else(|| {
            RedDBError::NotFound("native physical header repair policy".to_string())
        })?;
        Ok(match policy {
            crate::storage::NativeHeaderRepairPolicy::InSync => "in_sync",
            crate::storage::NativeHeaderRepairPolicy::RepairNativeFromMetadata => {
                "repair_native_from_metadata"
            }
            crate::storage::NativeHeaderRepairPolicy::NativeAheadOfMetadata => {
                "native_ahead_of_metadata"
            }
        }
        .to_string())
    }

    pub fn repair_native_header_from_metadata(&self) -> RedDBResult<String> {
        let policy = self
            .inner
            .db
            .repair_native_header_from_metadata()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        Ok(match policy {
            crate::storage::NativeHeaderRepairPolicy::InSync => "in_sync",
            crate::storage::NativeHeaderRepairPolicy::RepairNativeFromMetadata => {
                "repair_native_from_metadata"
            }
            crate::storage::NativeHeaderRepairPolicy::NativeAheadOfMetadata => {
                "native_ahead_of_metadata"
            }
        }
        .to_string())
    }

    pub fn rebuild_physical_metadata_from_native_state(&self) -> RedDBResult<bool> {
        self.inner
            .db
            .rebuild_physical_metadata_from_native_state()
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn repair_native_physical_state_from_metadata(&self) -> RedDBResult<bool> {
        self.inner
            .db
            .repair_native_physical_state_from_metadata()
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn native_metadata_state_summary(
        &self,
    ) -> RedDBResult<crate::storage::unified::store::NativeMetadataStateSummary> {
        self.inner
            .db
            .native_metadata_state_summary()
            .ok_or_else(|| RedDBError::NotFound("native metadata state".to_string()))
    }

    pub fn physical_authority_status(
        &self,
    ) -> crate::storage::unified::devx::PhysicalAuthorityStatus {
        self.inner.db.physical_authority_status()
    }

    pub fn readiness_for_query(&self) -> bool {
        self.inner.db.readiness_for_query()
    }

    pub fn readiness_for_query_serverless(&self) -> bool {
        self.inner.db.readiness_for_query_serverless()
    }

    pub fn readiness_for_write(&self) -> bool {
        self.inner.db.readiness_for_write()
    }

    pub fn readiness_for_write_serverless(&self) -> bool {
        self.inner.db.readiness_for_write_serverless()
    }

    pub fn readiness_for_repair(&self) -> bool {
        self.inner.db.readiness_for_repair()
    }

    pub fn readiness_for_repair_serverless(&self) -> bool {
        self.inner.db.readiness_for_repair_serverless()
    }

    pub fn manifest_events(&self) -> RedDBResult<Vec<ManifestEvent>> {
        if let Some(metadata) = self.inner.db.physical_metadata() {
            return Ok(metadata.manifest_events);
        }
        if let Some(summary) = self.inner.db.native_manifest_summary() {
            return Ok(summary
                .recent_events
                .into_iter()
                .map(|event| ManifestEvent {
                    collection: event.collection,
                    object_key: event.object_key,
                    kind: match event.kind.as_str() {
                        "insert" => crate::physical::ManifestEventKind::Insert,
                        "update" => crate::physical::ManifestEventKind::Update,
                        "remove" => crate::physical::ManifestEventKind::Remove,
                        _ => crate::physical::ManifestEventKind::Checkpoint,
                    },
                    block: crate::physical::BlockReference {
                        index: event.block_index,
                        checksum: event.block_checksum,
                    },
                    snapshot_min: event.snapshot_min,
                    snapshot_max: event.snapshot_max,
                })
                .collect());
        }
        Err(RedDBError::NotFound("physical metadata".to_string()))
    }

    pub fn manifest_events_filtered(
        &self,
        collection: Option<&str>,
        kind: Option<&str>,
        since_snapshot: Option<u64>,
    ) -> RedDBResult<Vec<ManifestEvent>> {
        let mut events = self.manifest_events()?;
        if let Some(collection) = collection {
            events.retain(|event| event.collection == collection);
        }
        if let Some(kind) = kind {
            let kind = normalize_graph_token(kind);
            events.retain(|event| {
                normalize_graph_token(match event.kind {
                    crate::physical::ManifestEventKind::Insert => "insert",
                    crate::physical::ManifestEventKind::Update => "update",
                    crate::physical::ManifestEventKind::Remove => "remove",
                    crate::physical::ManifestEventKind::Checkpoint => "checkpoint",
                }) == kind
            });
        }
        if let Some(since_snapshot) = since_snapshot {
            events.retain(|event| event.snapshot_min >= since_snapshot);
        }
        Ok(events)
    }

    pub fn collection_roots(&self) -> RedDBResult<std::collections::BTreeMap<String, u64>> {
        if let Some(metadata) = self.inner.db.physical_metadata() {
            return Ok(metadata.superblock.collection_roots);
        }
        if let Some(state) = self.inner.db.native_physical_state() {
            return Ok(state.collection_roots);
        }
        Err(RedDBError::NotFound("physical metadata".to_string()))
    }
}
