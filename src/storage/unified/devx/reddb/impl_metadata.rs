use super::*;
use crate::storage::unified::metadata::{MetadataFilter, MetadataValue};

impl RedDB {
    pub fn enforce_retention_policy(&self) -> Result<(), Box<dyn std::error::Error>> {
        if self.options.read_only {
            return Ok(());
        }

        // Export pruning is only meaningful for persistent mode where we
        // have a metadata sidecar that tracks file-backed export artifacts.
        if self.options.mode == StorageMode::Persistent {
            let Some(path) = self.path() else {
                return Ok(());
            };

            let Ok(mut metadata) = self.load_or_bootstrap_physical_metadata(true) else {
                return Ok(());
            };

            self.prune_export_registry(&mut metadata.exports);
            metadata.save_for_data_path(path)?;
        }

        let _ = self.sweep_ttl_expired_entities()?;

        Ok(())
    }

    fn sweep_ttl_expired_entities(&self) -> Result<usize, Box<dyn std::error::Error>> {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;

        let mut to_delete = Vec::<(String, EntityId)>::new();

        let mut absolute_expired = self.expired_entities_by_expires_at(now_ms)?;
        to_delete.append(&mut absolute_expired);

        let mut relative_expired = self.expired_entities_by_ttl(now_ms)?;
        to_delete.append(&mut relative_expired);

        to_delete.sort_unstable();
        to_delete.dedup();

        let mut deleted = 0usize;
        for (collection, id) in to_delete {
            match self.store.delete(&collection, id) {
                Ok(true) => deleted = deleted.saturating_add(1),
                Ok(false) => {}
                Err(err) => {
                    return Err(format!(
                        "failed deleting expired entity {id} from collection '{collection}': {err:?}"
                    )
                    .into());
                }
            }
        }

        Ok(deleted)
    }

    fn expired_entities_by_expires_at(
        &self,
        now_ms: u64,
    ) -> Result<Vec<(String, EntityId)>, Box<dyn std::error::Error>> {
        let mut ids = self.store.filter_metadata_all(&[(
            "_expires_at".to_string(),
            MetadataFilter::Le(MetadataValue::Timestamp(now_ms)),
        )]);

        if let Ok(now_ms_i64) = i64::try_from(now_ms) {
            ids.extend(self.store.filter_metadata_all(&[(
                "_expires_at".to_string(),
                MetadataFilter::Le(MetadataValue::Int(now_ms_i64)),
            )]));
        }

        let now_ms_f64 = now_ms as f64;
        if now_ms_f64.is_finite() {
            ids.extend(self.store.filter_metadata_all(&[(
                "_expires_at".to_string(),
                MetadataFilter::Le(MetadataValue::Float(now_ms_f64)),
            )]));
        }

        Ok(ids)
    }

    fn expired_entities_by_ttl(
        &self,
        now_ms: u64,
    ) -> Result<Vec<(String, EntityId)>, Box<dyn std::error::Error>> {
        let mut candidates = Vec::<(String, EntityId)>::new();

        let ttl_ms_candidates = self
            .store
            .filter_metadata_all(&[("_ttl_ms".to_string(), MetadataFilter::IsNotNull)]);
        candidates.extend(ttl_ms_candidates);

        let ttl_candidates = self
            .store
            .filter_metadata_all(&[("_ttl".to_string(), MetadataFilter::IsNotNull)]);
        candidates.extend(ttl_candidates);

        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        candidates.sort_unstable();
        candidates.dedup();

        let mut expired = Vec::<(String, EntityId)>::new();
        for (collection, entity_id) in candidates {
            let Some(entity) = self.store.get(&collection, entity_id) else {
                continue;
            };

            let Some(metadata) = self.store.get_metadata(&collection, entity_id) else {
                continue;
            };

            let ttl_ms = metadata.get("_ttl_ms").and_then(Self::metadata_u64);
            let ttl_secs = if ttl_ms.is_none() {
                metadata.get("_ttl").and_then(|value| {
                    Self::metadata_u64(value).and_then(|value_secs| value_secs.checked_mul(1000))
                })
            } else {
                None
            };

            let Some(ttl_ms) = ttl_ms.or(ttl_secs) else {
                continue;
            };

            let created_at_ms = entity.created_at.saturating_mul(1000);
            let expiry_ms = created_at_ms.saturating_add(ttl_ms);
            if expiry_ms <= now_ms {
                expired.push((collection, entity_id));
            }
        }

        Ok(expired)
    }

    fn metadata_u64(value: &MetadataValue) -> Option<u64> {
        match value {
            MetadataValue::Int(v) if *v >= 0 => Some(*v as u64),
            MetadataValue::Timestamp(v) => Some(*v),
            MetadataValue::Float(v) => {
                if !v.is_finite() || !v.is_sign_positive() || v.fract().abs() >= f64::EPSILON {
                    return None;
                }
                if *v > u64::MAX as f64 {
                    return None;
                }
                Some(v.trunc() as u64)
            }
            MetadataValue::String(v) => v.parse::<u64>().ok(),
            _ => None,
        }
    }

    // ========================================================================
    // Builder Methods - Create Entities
    // ========================================================================

    /// Start building a graph node
    ///
    /// # Example
    /// ```ignore
    /// let host = db.node("hosts", "Host")
    ///     .property("ip", "192.168.1.1")
    ///     .save()?;
    /// ```
    pub fn node(&self, collection: impl Into<String>, label: impl Into<String>) -> NodeBuilder {
        NodeBuilder::new(self.store.clone(), collection, label)
    }

    /// Start building a graph edge
    ///
    /// # Example
    /// ```ignore
    /// let edge = db.edge("connections", "CONNECTS_TO")
    ///     .from(host_a)
    ///     .to(host_b)
    ///     .weight(0.95)
    ///     .property("protocol", "TCP")
    ///     .save()?;
    /// ```
    pub fn edge(&self, collection: impl Into<String>, label: impl Into<String>) -> EdgeBuilder {
        EdgeBuilder::new(self.store.clone(), collection, label)
    }

    /// Start building a vector entry
    ///
    /// # Example
    /// ```ignore
    /// let vec = db.vector("embeddings")
    ///     .dense(embedding)
    ///     .content("Original text content")
    ///     .metadata("source", "document.pdf")
    ///     .save()?;
    /// ```
    pub fn vector(&self, collection: impl Into<String>) -> VectorBuilder {
        VectorBuilder::new(self.store.clone(), collection)
    }

    /// Start building a table row
    ///
    /// # Example
    /// ```ignore
    /// let row = db.row("scans", vec![
    ///     ("timestamp", Value::Timestamp(now)),
    ///     ("target", Value::Text("192.168.1.0/24".into())),
    ///     ("findings", Value::Integer(42)),
    /// ]).save()?;
    /// ```
    pub fn row(&self, table: impl Into<String>, columns: Vec<(&str, Value)>) -> RowBuilder {
        RowBuilder::new(self.store.clone(), table, columns)
    }

    /// Start building a document
    ///
    /// Documents are stored as enriched table rows with a full JSON body
    /// field and flattened top-level keys for filtering.
    ///
    /// # Example
    /// ```ignore
    /// let doc = db.doc("articles")
    ///     .field("title", "Hello World")
    ///     .field("views", 42)
    ///     .metadata("source", "web")
    ///     .save()?;
    /// ```
    pub fn doc(&self, collection: impl Into<String>) -> DocumentBuilder {
        DocumentBuilder::new(self.store.clone(), collection)
    }

    /// Start building a key-value pair
    ///
    /// KV pairs are stored as table rows with named fields `key` and `value`.
    ///
    /// # Example
    /// ```ignore
    /// let id = db.kv("config", "theme", Value::Text("dark".into()))
    ///     .metadata("updated_by", "admin")
    ///     .save()?;
    /// ```
    pub fn kv(
        &self,
        collection: impl Into<String>,
        key: impl Into<String>,
        value: Value,
    ) -> KvBuilder {
        KvBuilder::new(self.store.clone(), collection, key, value)
    }

    /// Get a key-value pair by key, returning the value and entity id
    ///
    /// Scans the collection for an entity whose named field `key` matches.
    pub fn get_kv(&self, collection: &str, key: &str) -> Option<(Value, EntityId)> {
        let manager = self.store.get_collection(collection)?;
        let entities = manager.query_all(|_| true);
        for entity in entities {
            if let EntityData::Row(ref row) = entity.data {
                if let Some(ref named) = row.named {
                    if let Some(Value::Text(ref k)) = named.get("key") {
                        if k == key {
                            let value = named.get("value").cloned().unwrap_or(Value::Null);
                            return Some((value, entity.id));
                        }
                    }
                }
            }
        }
        None
    }

    /// Delete a key-value pair by key, returning whether it was found and removed
    pub fn delete_kv(
        &self,
        collection: &str,
        key: &str,
    ) -> Result<bool, super::super::error::DevXError> {
        let Some((_, id)) = self.get_kv(collection, key) else {
            return Ok(false);
        };
        self.store
            .delete(collection, id)
            .map_err(|err| super::super::error::DevXError::Storage(format!("{err:?}")))?;
        Ok(true)
    }

    pub(crate) fn with_initialized_metadata(self) -> Result<Self, Box<dyn std::error::Error>> {
        if self.options.mode == StorageMode::Persistent && !self.options.read_only {
            // Load metadata without persisting (avoids blocking catalog snapshot on boot)
            let _ = self.load_or_bootstrap_physical_metadata(false);
            // Skip repair on boot — deferred to first explicit persist_metadata() call.
            // This avoids the recursive catalog_model_snapshot → physical_metadata loop
            // that caused stack overflow / 12-second hang on startup.
        }
        self.load_collection_ttl_defaults_from_metadata();
        Ok(self)
    }

    pub(crate) fn persist_metadata(&self) -> Result<(), Box<dyn std::error::Error>> {
        if self.options.mode != StorageMode::Persistent || self.options.read_only {
            return Ok(());
        }
        let Some(path) = self.path() else {
            return Ok(());
        };

        let previous = self.load_or_bootstrap_physical_metadata(false).ok();
        let collection_roots = self.physical_collection_roots();
        let indexes = self
            .native_physical_state()
            .map(|state| self.physical_index_state_from_native_state(&state, previous.as_ref()))
            .unwrap_or_else(|| self.physical_index_state());
        let mut metadata = PhysicalMetadataFile::from_state(
            self.options.clone(),
            self.catalog_snapshot(),
            collection_roots,
            indexes,
            previous.as_ref(),
        );
        metadata.collection_ttl_defaults_ms = self.collection_ttl_defaults_snapshot();
        metadata.save_for_data_path(path)?;
        self.persist_native_physical_header(&metadata)?;
        Ok(())
    }

    fn bootstrap_metadata_from_native_state(&self) -> Result<bool, Box<dyn std::error::Error>> {
        if self.options.mode != StorageMode::Persistent || self.options.read_only {
            return Ok(false);
        }
        let Some(path) = self.path() else {
            return Ok(false);
        };
        let Some(native_state) = self.native_physical_state() else {
            return Ok(false);
        };
        if !Self::native_state_is_bootstrap_complete(&native_state) {
            return Ok(false);
        }

        let previous = PhysicalMetadataFile::load_for_data_path(path).ok();
        let metadata = self.metadata_from_native_state(&native_state, previous.as_ref());
        metadata.save_for_data_path(path)?;
        self.persist_native_physical_header(&metadata)?;
        Ok(true)
    }

    /// Rebuild the external physical metadata view from the native state published in the
    /// paged database file.
    pub fn rebuild_physical_metadata_from_native_state(
        &self,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        self.bootstrap_metadata_from_native_state()
    }

    pub(crate) fn native_state_is_bootstrap_complete(native_state: &NativePhysicalState) -> bool {
        let registry_complete = native_state.registry.as_ref().map(|registry| {
            registry.collections_complete
                && registry.indexes_complete
                && registry.graph_projections_complete
                && registry.analytics_jobs_complete
                && registry.vector_artifacts_complete
        });
        let recovery_complete = native_state
            .recovery
            .as_ref()
            .map(|recovery| recovery.snapshots_complete && recovery.exports_complete);
        let catalog_complete = native_state
            .catalog
            .as_ref()
            .map(|catalog| catalog.collections_complete);

        registry_complete == Some(true)
            && recovery_complete == Some(true)
            && catalog_complete == Some(true)
    }

    pub(crate) fn load_or_bootstrap_physical_metadata(
        &self,
        persist_bootstrapped: bool,
    ) -> Result<PhysicalMetadataFile, Box<dyn std::error::Error>> {
        if self.options.mode != StorageMode::Persistent {
            return Err("physical metadata requires persistent mode".into());
        }
        let Some(path) = self.path() else {
            return Err("database path is not available".into());
        };
        let native_state = self.native_physical_state();

        match PhysicalMetadataFile::load_for_data_path(path) {
            Ok(metadata) => {
                if let Some(native_state) = native_state.as_ref() {
                    let inspection = Self::inspect_native_header_against_metadata(
                        native_state.header,
                        &metadata,
                    );
                    if Self::repair_policy_for_inspection(&inspection)
                        == NativeHeaderRepairPolicy::NativeAheadOfMetadata
                    {
                        let bootstrapped =
                            self.metadata_from_native_state(native_state, Some(&metadata));
                        if persist_bootstrapped && !self.options.read_only {
                            bootstrapped.save_for_data_path(path)?;
                            self.persist_native_physical_header(&bootstrapped)?;
                        }
                        return Ok(bootstrapped);
                    }
                }
                Ok(metadata)
            }
            Err(err) => {
                let Some(native_state) = native_state else {
                    return Err(err.into());
                };
                // Accept the bootstrap when the native state is either
                // (a) fully populated and consistent (the original
                // contract), or (b) trivially empty — a freshly created
                // database with no collections written yet. Without (b)
                // a brand-new data file can never reach
                // `readiness_for_query = true`, because the bootstrap
                // refuses to run until the registry/catalog/recovery
                // structures are "complete", which they never become
                // until the bootstrap has already run once.
                //
                // The emptiness check is conservative: header.sequence
                // must still be at its initial value AND all three
                // physical state summaries must be absent. Anything
                // else falls through to the original error so we never
                // paper over partially corrupted files.
                let is_fresh_empty = native_state.header.sequence == 0
                    && native_state.registry.is_none()
                    && native_state.catalog.is_none()
                    && native_state.recovery.is_none();
                if !is_fresh_empty && !Self::native_state_is_bootstrap_complete(&native_state) {
                    return Err(err.into());
                }
                let metadata = self.metadata_from_native_state(&native_state, None);
                if persist_bootstrapped && !self.options.read_only {
                    metadata.save_for_data_path(path)?;
                    self.persist_native_physical_header(&metadata)?;
                }
                Ok(metadata)
            }
        }
    }

    pub(crate) fn physical_metadata_preference(&self) -> Option<&'static str> {
        let path = self.path()?;
        let native_state = self.native_physical_state();
        let metadata = PhysicalMetadataFile::load_for_data_path(path).ok();

        match (metadata, native_state) {
            (Some(metadata), Some(native_state)) => {
                let inspection =
                    Self::inspect_native_header_against_metadata(native_state.header, &metadata);
                match Self::repair_policy_for_inspection(&inspection) {
                    NativeHeaderRepairPolicy::InSync => Some("sidecar_current"),
                    NativeHeaderRepairPolicy::RepairNativeFromMetadata => Some("sidecar_current"),
                    NativeHeaderRepairPolicy::NativeAheadOfMetadata => Some("native_ahead"),
                }
            }
            (Some(_), None) => Some("sidecar_only"),
            (None, Some(_)) => Some("sidecar_missing_native_available"),
            (None, None) => Some("sidecar_missing_no_native"),
        }
    }

    fn metadata_from_native_state(
        &self,
        native_state: &NativePhysicalState,
        previous: Option<&PhysicalMetadataFile>,
    ) -> PhysicalMetadataFile {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let catalog = self.catalog_snapshot();
        let catalog_name = catalog.name.clone();
        let catalog_total_entities = catalog.total_entities;
        let catalog_total_collections = catalog.total_collections;
        let indexes = self.physical_index_state();

        let mut manifest =
            crate::api::SchemaManifest::now(self.options.clone(), catalog.total_collections);
        manifest.updated_at_unix_ms = now;

        let manifest_events = native_state
            .manifest
            .as_ref()
            .map(|summary| {
                summary
                    .recent_events
                    .iter()
                    .map(|event| crate::physical::ManifestEvent {
                        collection: event.collection.clone(),
                        object_key: event.object_key.clone(),
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
                    .collect()
            })
            .unwrap_or_default();

        let graph_projections = native_state
            .registry
            .as_ref()
            .and_then(|registry| {
                registry.graph_projections_complete.then(|| {
                    registry
                        .graph_projections
                        .iter()
                        .map(|projection| crate::physical::PhysicalGraphProjection {
                            name: projection.name.clone(),
                            created_at_unix_ms: projection.created_at_unix_ms,
                            updated_at_unix_ms: projection.updated_at_unix_ms,
                            state: "materialized".to_string(),
                            source: projection.source.clone(),
                            node_labels: projection.node_labels.clone(),
                            node_types: projection.node_types.clone(),
                            edge_labels: projection.edge_labels.clone(),
                            last_materialized_sequence: projection.last_materialized_sequence,
                        })
                        .collect()
                })
            })
            .or_else(|| previous.map(|metadata| metadata.graph_projections.clone()))
            .unwrap_or_default();

        let analytics_jobs = native_state
            .registry
            .as_ref()
            .and_then(|registry| {
                registry.analytics_jobs_complete.then(|| {
                    registry
                        .analytics_jobs
                        .iter()
                        .map(|job| crate::physical::PhysicalAnalyticsJob {
                            id: job.id.clone(),
                            kind: job.kind.clone(),
                            state: job.state.clone(),
                            projection: job.projection.clone(),
                            created_at_unix_ms: job.created_at_unix_ms,
                            updated_at_unix_ms: job.updated_at_unix_ms,
                            last_run_sequence: job.last_run_sequence,
                            metadata: job.metadata.clone(),
                        })
                        .collect()
                })
            })
            .or_else(|| previous.map(|metadata| metadata.analytics_jobs.clone()))
            .unwrap_or_default();

        let exports = native_state
            .recovery
            .as_ref()
            .and_then(|recovery| {
                recovery.exports_complete.then(|| {
                    recovery
                        .exports
                        .iter()
                        .map(|export| crate::physical::ExportDescriptor {
                            name: export.name.clone(),
                            created_at_unix_ms: export.created_at_unix_ms,
                            snapshot_id: export.snapshot_id,
                            superblock_sequence: export.superblock_sequence,
                            data_path: self
                                .path()
                                .map(|path| {
                                    crate::physical::PhysicalMetadataFile::export_data_path_for(
                                        path,
                                        &export.name,
                                    )
                                    .display()
                                    .to_string()
                                })
                                .unwrap_or_default(),
                            metadata_path: self
                                .path()
                                .map(|path| {
                                    let export_data_path =
                                        crate::physical::PhysicalMetadataFile::export_data_path_for(
                                            path,
                                            &export.name,
                                        );
                                    crate::physical::PhysicalMetadataFile::metadata_path_for(
                                        &export_data_path,
                                    )
                                    .display()
                                    .to_string()
                                })
                                .unwrap_or_default(),
                            collection_count: export.collection_count as usize,
                            total_entities: export.total_entities as usize,
                        })
                        .collect()
                })
            })
            .or_else(|| previous.map(|metadata| metadata.exports.clone()))
            .unwrap_or_default();

        let snapshots = native_state
            .recovery
            .as_ref()
            .and_then(|recovery| {
                recovery.snapshots_complete.then(|| {
                    recovery
                        .snapshots
                        .iter()
                        .map(|snapshot| crate::physical::SnapshotDescriptor {
                            snapshot_id: snapshot.snapshot_id,
                            created_at_unix_ms: snapshot.created_at_unix_ms,
                            superblock_sequence: snapshot.superblock_sequence,
                            collection_count: snapshot.collection_count as usize,
                            total_entities: snapshot.total_entities as usize,
                        })
                        .collect()
                })
            })
            .or_else(|| previous.map(|metadata| metadata.snapshots.clone()))
            .unwrap_or_else(|| {
                vec![crate::physical::SnapshotDescriptor {
                    snapshot_id: native_state.header.sequence,
                    created_at_unix_ms: now,
                    superblock_sequence: native_state.header.sequence,
                    collection_count: catalog_total_collections,
                    total_entities: catalog_total_entities,
                }]
            });

        let catalog_stats = native_state
            .catalog
            .as_ref()
            .and_then(|native_catalog| {
                native_catalog.collections_complete.then(|| {
                    native_catalog
                        .collections
                        .iter()
                        .map(|collection| {
                            (
                                collection.name.clone(),
                                crate::api::CollectionStats {
                                    entities: collection.entities as usize,
                                    cross_refs: collection.cross_refs as usize,
                                    segments: collection.segments as usize,
                                },
                            )
                        })
                        .collect::<BTreeMap<_, _>>()
                })
            })
            .or_else(|| previous.map(|metadata| metadata.catalog.stats_by_collection.clone()))
            .unwrap_or_else(|| catalog.stats_by_collection.clone());

        PhysicalMetadataFile {
            protocol_version: crate::physical::PHYSICAL_METADATA_PROTOCOL_VERSION.to_string(),
            generated_at_unix_ms: now,
            last_loaded_from: Some("native_bootstrap".to_string()),
            last_healed_at_unix_ms: Some(now),
            manifest,
            catalog: crate::api::CatalogSnapshot {
                name: catalog_name,
                total_entities: native_state
                    .catalog
                    .as_ref()
                    .map(|summary| summary.total_entities as usize)
                    .unwrap_or(catalog_total_entities),
                total_collections: native_state
                    .catalog
                    .as_ref()
                    .map(|summary| summary.collection_count as usize)
                    .unwrap_or(catalog_total_collections),
                stats_by_collection: catalog_stats,
                updated_at: SystemTime::now(),
            },
            manifest_events,
            collection_ttl_defaults_ms: previous
                .map(|metadata| metadata.collection_ttl_defaults_ms.clone())
                .unwrap_or_default(),
            collection_contracts: previous
                .map(|metadata| metadata.collection_contracts.clone())
                .unwrap_or_default(),
            indexes,
            graph_projections,
            analytics_jobs,
            exports,
            superblock: crate::physical::SuperblockHeader {
                format_version: native_state.header.format_version,
                sequence: native_state.header.sequence,
                copies: crate::physical::DEFAULT_SUPERBLOCK_COPIES,
                manifest: crate::physical::ManifestPointers {
                    oldest: crate::physical::BlockReference {
                        index: native_state.header.manifest_oldest_root,
                        checksum: 0,
                    },
                    newest: crate::physical::BlockReference {
                        index: native_state.header.manifest_root,
                        checksum: 0,
                    },
                },
                free_set: crate::physical::BlockReference {
                    index: native_state.header.free_set_root,
                    checksum: 0,
                },
                collection_roots: native_state.collection_roots.clone(),
            },
            snapshots,
        }
    }

    pub(crate) fn reconcile_index_states_with_native_artifacts(
        &self,
        mut indexes: Vec<PhysicalIndexState>,
    ) -> Vec<PhysicalIndexState> {
        let native_artifacts = self
            .native_physical_state()
            .and_then(|state| state.registry)
            .map(|registry| registry.vector_artifacts)
            .unwrap_or_default();
        for index in &mut indexes {
            let Some(collection) = index.collection.as_deref() else {
                continue;
            };
            let Some(artifact_kind) = Self::native_artifact_kind_for_index(index.kind) else {
                continue;
            };
            let Some(artifact) = native_artifacts.iter().find(|artifact| {
                artifact.collection == collection && artifact.artifact_kind == artifact_kind
            }) else {
                index.build_state = "metadata-only".to_string();
                continue;
            };
            index.entries = artifact.vector_count as usize;
            index.estimated_memory_bytes = artifact.serialized_bytes;
            index.backend = format!("{}+native-artifact", index_backend_name(index.kind));
            index.artifact_kind = Some(artifact.artifact_kind.clone());
            index.artifact_checksum = Some(artifact.checksum);
            index.build_state = "artifact-published".to_string();
            if let Some(pages) = self.native_vector_artifact_pages() {
                index.artifact_root_page = pages
                    .into_iter()
                    .find(|page| {
                        page.collection == artifact.collection
                            && page.artifact_kind == artifact.artifact_kind
                    })
                    .map(|page| page.root_page);
            }
        }
        indexes
    }

    pub(crate) fn warmup_native_vector_artifact_for_index(
        &self,
        index: &PhysicalIndexState,
    ) -> Result<(), String> {
        let Some(collection) = index.collection.as_deref() else {
            return Ok(());
        };
        let Some(artifact_kind) = Self::native_artifact_kind_for_index(index.kind) else {
            return Ok(());
        };
        self.warmup_native_vector_artifact(collection, Some(artifact_kind))?;
        Ok(())
    }

    pub(crate) fn apply_runtime_native_artifact_to_index_state(
        &self,
        index: &mut PhysicalIndexState,
    ) -> Result<(), String> {
        let Some(collection) = index.collection.as_deref() else {
            return Ok(());
        };
        let Some(artifact_kind) = Self::native_artifact_kind_for_index(index.kind) else {
            return Ok(());
        };
        let artifact = self.inspect_native_vector_artifact(collection, Some(artifact_kind))?;
        index.entries = artifact
            .graph_edge_count
            .or(artifact.text_posting_count)
            .unwrap_or(artifact.node_count) as usize;
        index.estimated_memory_bytes = artifact.byte_len;
        index.backend = format!("{}+native-artifact", index_backend_name(index.kind));
        index.artifact_kind = Some(artifact.artifact_kind.clone());
        index.artifact_checksum = Some(artifact.checksum);
        index.build_state = "ready".to_string();
        index.artifact_root_page = self
            .native_vector_artifact_pages()
            .and_then(|pages| {
                pages.into_iter().find(|page| {
                    page.collection == artifact.collection
                        && page.artifact_kind == artifact.artifact_kind
                })
            })
            .map(|page| page.root_page);
        Ok(())
    }

    pub(crate) fn physical_index_state_from_native_state(
        &self,
        native_state: &NativePhysicalState,
        previous: Option<&PhysicalMetadataFile>,
    ) -> Vec<PhysicalIndexState> {
        let mut fresh = self.physical_index_state();
        let Some(registry) = native_state.registry.as_ref() else {
            if let Some(previous) = previous {
                for index in &previous.indexes {
                    if !fresh.iter().any(|candidate| candidate.name == index.name) {
                        fresh.push(index.clone());
                    }
                }
            }
            return fresh;
        };

        for index in &mut fresh {
            if let Some(native) = registry
                .indexes
                .iter()
                .find(|candidate| candidate.name == index.name)
            {
                index.enabled = native.enabled;
                index.last_refresh_ms = native.last_refresh_ms;
                index.backend = native.backend.clone();
                index.entries = native.entries as usize;
                index.estimated_memory_bytes = native.estimated_memory_bytes;
                if index.artifact_kind.is_none() {
                    index.artifact_kind = Self::native_artifact_kind_for_index(index.kind)
                        .map(|value| value.to_string());
                }
                if index.build_state == "catalog-derived" {
                    index.build_state = "registry-loaded".to_string();
                }
            }
        }

        for native in &registry.indexes {
            if fresh.iter().any(|index| index.name == native.name) {
                continue;
            }
            let Some(kind) = Self::index_kind_from_str(&native.kind) else {
                continue;
            };
            fresh.push(PhysicalIndexState {
                name: native.name.clone(),
                kind,
                collection: native.collection.clone(),
                enabled: native.enabled,
                entries: native.entries as usize,
                estimated_memory_bytes: native.estimated_memory_bytes,
                last_refresh_ms: native.last_refresh_ms,
                backend: native.backend.clone(),
                artifact_kind: Self::native_artifact_kind_for_index(kind)
                    .map(|value| value.to_string()),
                artifact_root_page: None,
                artifact_checksum: None,
                build_state: "registry-loaded".to_string(),
            });
        }

        if !registry.indexes_complete {
            if let Some(previous) = previous {
                for index in &previous.indexes {
                    if !fresh.iter().any(|candidate| candidate.name == index.name) {
                        fresh.push(index.clone());
                    }
                }
            }
        }

        fresh
    }

    pub(crate) fn graph_projections_from_native_state(
        &self,
        native_state: &NativePhysicalState,
    ) -> Vec<PhysicalGraphProjection> {
        native_state
            .registry
            .as_ref()
            .map(|registry| {
                registry
                    .graph_projections
                    .iter()
                    .map(|projection| PhysicalGraphProjection {
                        name: projection.name.clone(),
                        created_at_unix_ms: projection.created_at_unix_ms,
                        updated_at_unix_ms: projection.updated_at_unix_ms,
                        state: "materialized".to_string(),
                        source: projection.source.clone(),
                        node_labels: projection.node_labels.clone(),
                        node_types: projection.node_types.clone(),
                        edge_labels: projection.edge_labels.clone(),
                        last_materialized_sequence: projection.last_materialized_sequence,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn analytics_jobs_from_native_state(
        &self,
        native_state: &NativePhysicalState,
    ) -> Vec<PhysicalAnalyticsJob> {
        native_state
            .registry
            .as_ref()
            .map(|registry| {
                registry
                    .analytics_jobs
                    .iter()
                    .map(|job| PhysicalAnalyticsJob {
                        id: job.id.clone(),
                        kind: job.kind.clone(),
                        state: job.state.clone(),
                        projection: job.projection.clone(),
                        created_at_unix_ms: job.created_at_unix_ms,
                        updated_at_unix_ms: job.updated_at_unix_ms,
                        last_run_sequence: job.last_run_sequence,
                        metadata: job.metadata.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn exports_from_native_state(
        &self,
        native_state: &NativePhysicalState,
    ) -> Vec<ExportDescriptor> {
        native_state
            .recovery
            .as_ref()
            .map(|recovery| {
                recovery
                    .exports
                    .iter()
                    .map(|export| ExportDescriptor {
                        name: export.name.clone(),
                        created_at_unix_ms: export.created_at_unix_ms,
                        snapshot_id: export.snapshot_id,
                        superblock_sequence: export.superblock_sequence,
                        data_path: self
                            .path()
                            .map(|path| {
                                crate::physical::PhysicalMetadataFile::export_data_path_for(
                                    path,
                                    &export.name,
                                )
                                .display()
                                .to_string()
                            })
                            .unwrap_or_default(),
                        metadata_path: self
                            .path()
                            .map(|path| {
                                let export_data_path =
                                    crate::physical::PhysicalMetadataFile::export_data_path_for(
                                        path,
                                        &export.name,
                                    );
                                crate::physical::PhysicalMetadataFile::metadata_path_for(
                                    &export_data_path,
                                )
                                .display()
                                .to_string()
                            })
                            .unwrap_or_default(),
                        collection_count: export.collection_count as usize,
                        total_entities: export.total_entities as usize,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn snapshots_from_native_state(
        &self,
        native_state: &NativePhysicalState,
    ) -> Vec<crate::physical::SnapshotDescriptor> {
        native_state
            .recovery
            .as_ref()
            .map(|recovery| {
                recovery
                    .snapshots
                    .iter()
                    .map(|snapshot| crate::physical::SnapshotDescriptor {
                        snapshot_id: snapshot.snapshot_id,
                        created_at_unix_ms: snapshot.created_at_unix_ms,
                        superblock_sequence: snapshot.superblock_sequence,
                        collection_count: snapshot.collection_count as usize,
                        total_entities: snapshot.total_entities as usize,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn index_kind_from_str(value: &str) -> Option<crate::index::IndexKind> {
        match value {
            "btree" => Some(crate::index::IndexKind::BTree),
            "vector.hnsw" => Some(crate::index::IndexKind::VectorHnsw),
            "vector.inverted" => Some(crate::index::IndexKind::VectorInverted),
            "graph.adjacency" => Some(crate::index::IndexKind::GraphAdjacency),
            "text.fulltext" => Some(crate::index::IndexKind::FullText),
            "document.pathvalue" => Some(crate::index::IndexKind::DocumentPathValue),
            "search.hybrid" => Some(crate::index::IndexKind::HybridSearch),
            _ => None,
        }
    }

    pub(crate) fn native_artifact_kind_for_index(kind: IndexKind) -> Option<&'static str> {
        match kind {
            IndexKind::VectorHnsw => Some("hnsw"),
            IndexKind::VectorInverted => Some("ivf"),
            IndexKind::GraphAdjacency => Some("graph.adjacency"),
            IndexKind::FullText => Some("text.fulltext"),
            IndexKind::DocumentPathValue => Some("document.pathvalue"),
            _ => None,
        }
    }

    fn index_is_declared(&self, name: &str) -> bool {
        self.physical_metadata()
            .map(|metadata| metadata.indexes.iter().any(|index| index.name == name))
            .unwrap_or(false)
    }

    pub(crate) fn graph_projection_is_declared(&self, name: &str) -> bool {
        self.physical_metadata()
            .map(|metadata| {
                metadata
                    .graph_projections
                    .iter()
                    .any(|projection| projection.name == name)
            })
            .unwrap_or(false)
    }

    pub(crate) fn graph_projection_is_operational(&self, name: &str) -> bool {
        self.operational_graph_projections()
            .into_iter()
            .any(|projection| projection.name == name && projection.state == "materialized")
    }

    pub(crate) fn analytics_job_id(kind: &str, projection: Option<&str>) -> String {
        match projection {
            Some(projection) => format!("{kind}::{projection}"),
            None => format!("{kind}::global"),
        }
    }

    pub(crate) fn update_physical_metadata<T, F>(
        &self,
        mutator: F,
    ) -> Result<T, Box<dyn std::error::Error>>
    where
        F: FnOnce(&mut PhysicalMetadataFile) -> T,
    {
        if self.options.mode != StorageMode::Persistent {
            return Err("physical metadata operations require persistent mode".into());
        }
        if self.options.read_only {
            return Err("physical metadata operations are not allowed in read-only mode".into());
        }
        let Some(path) = self.path() else {
            return Err("database path is not available".into());
        };

        let mut metadata = self.load_or_bootstrap_physical_metadata(true)?;

        if metadata.indexes.is_empty() {
            metadata.indexes = self.physical_index_state();
        }
        metadata.superblock.collection_roots = self.physical_collection_roots();

        let result = mutator(&mut metadata);
        metadata.save_for_data_path(path)?;
        self.persist_native_physical_header(&metadata)?;
        Ok(result)
    }

    pub(crate) fn persist_native_physical_header(
        &self,
        metadata: &PhysicalMetadataFile,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !self.paged_mode {
            return Ok(());
        }

        let existing_page = self
            .store
            .physical_file_header()
            .map(|header| header.collection_roots_page)
            .filter(|page| *page != 0);
        let existing_registry_page = self
            .store
            .physical_file_header()
            .map(|header| header.registry_page)
            .filter(|page| *page != 0);
        let existing_recovery_page = self
            .store
            .physical_file_header()
            .map(|header| header.recovery_page)
            .filter(|page| *page != 0);
        let existing_catalog_page = self
            .store
            .physical_file_header()
            .map(|header| header.catalog_page)
            .filter(|page| *page != 0);
        let existing_metadata_state_page = self
            .store
            .physical_file_header()
            .map(|header| header.metadata_state_page)
            .filter(|page| *page != 0);
        let existing_vector_artifact_page = self
            .store
            .physical_file_header()
            .map(|header| header.vector_artifact_page)
            .filter(|page| *page != 0);
        let existing_manifest_page = self
            .store
            .physical_file_header()
            .map(|header| header.manifest_page)
            .filter(|page| *page != 0);
        let (manifest_page, manifest_checksum) = self.store.write_native_manifest_summary(
            metadata.superblock.sequence,
            &metadata.manifest_events,
            existing_manifest_page,
        )?;
        let (collection_roots_page, collection_roots_checksum) = self
            .store
            .write_native_collection_roots(&metadata.superblock.collection_roots, existing_page)?;
        let registry_summary = self.native_registry_summary_from_metadata(metadata);
        let (registry_page, registry_checksum) = self
            .store
            .write_native_registry_summary(&registry_summary, existing_registry_page)?;
        let recovery_summary = Self::native_recovery_summary_from_metadata(metadata);
        let (recovery_page, recovery_checksum) = self
            .store
            .write_native_recovery_summary(&recovery_summary, existing_recovery_page)?;
        let catalog_summary = Self::native_catalog_summary_from_metadata(metadata);
        let (catalog_page, catalog_checksum) = self
            .store
            .write_native_catalog_summary(&catalog_summary, existing_catalog_page)?;
        let metadata_state_summary = Self::native_metadata_state_summary_from_metadata(metadata);
        let (metadata_state_page, metadata_state_checksum) =
            self.store.write_native_metadata_state_summary(
                &metadata_state_summary,
                existing_metadata_state_page,
            )?;
        let vector_artifact_records = self.native_vector_artifact_records();
        let vector_artifact_payloads = vector_artifact_records
            .iter()
            .map(|(summary, bytes)| {
                (
                    summary.collection.clone(),
                    summary.artifact_kind.clone(),
                    bytes.clone(),
                )
            })
            .collect::<Vec<_>>();
        let (vector_artifact_page, vector_artifact_checksum, _vector_artifact_pages) =
            self.store.write_native_vector_artifact_store(
                &vector_artifact_payloads,
                existing_vector_artifact_page,
            )?;
        let mut header = Self::native_header_from_metadata(metadata);
        header.manifest_page = manifest_page;
        header.manifest_checksum = manifest_checksum;
        header.collection_roots_page = collection_roots_page;
        header.collection_roots_checksum = collection_roots_checksum;
        header.registry_page = registry_page;
        header.registry_checksum = registry_checksum;
        header.recovery_page = recovery_page;
        header.recovery_checksum = recovery_checksum;
        header.catalog_page = catalog_page;
        header.catalog_checksum = catalog_checksum;
        header.metadata_state_page = metadata_state_page;
        header.metadata_state_checksum = metadata_state_checksum;
        header.vector_artifact_page = vector_artifact_page;
        header.vector_artifact_checksum = vector_artifact_checksum;
        self.store.update_physical_file_header(header)?;
        self.store.persist()?;
        Ok(())
    }

    pub(crate) fn native_header_from_metadata(
        metadata: &PhysicalMetadataFile,
    ) -> PhysicalFileHeader {
        PhysicalFileHeader {
            format_version: metadata.superblock.format_version,
            sequence: metadata.superblock.sequence,
            manifest_oldest_root: metadata.superblock.manifest.oldest.index,
            manifest_root: metadata.superblock.manifest.newest.index,
            free_set_root: metadata.superblock.free_set.index,
            manifest_page: 0,
            manifest_checksum: 0,
            collection_roots_page: 0,
            collection_roots_checksum: 0,
            collection_root_count: metadata.superblock.collection_roots.len() as u32,
            snapshot_count: metadata.snapshots.len() as u32,
            index_count: metadata.indexes.len() as u32,
            catalog_collection_count: metadata.catalog.total_collections as u32,
            catalog_total_entities: metadata.catalog.total_entities as u64,
            export_count: metadata.exports.len() as u32,
            graph_projection_count: metadata.graph_projections.len() as u32,
            analytics_job_count: metadata.analytics_jobs.len() as u32,
            manifest_event_count: metadata.manifest_events.len() as u32,
            registry_page: 0,
            registry_checksum: 0,
            recovery_page: 0,
            recovery_checksum: 0,
            catalog_page: 0,
            catalog_checksum: 0,
            metadata_state_page: 0,
            metadata_state_checksum: 0,
            vector_artifact_page: 0,
            vector_artifact_checksum: 0,
        }
    }
}
