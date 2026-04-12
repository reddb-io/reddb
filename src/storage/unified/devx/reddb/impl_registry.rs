use super::*;

impl RedDB {
    pub fn collection_default_ttl_ms(&self, collection: &str) -> Option<u64> {
        self.collection_ttl_defaults_ms
            .read()
            .ok()
            .and_then(|defaults| defaults.get(collection).copied())
    }

    pub fn set_collection_default_ttl_ms(&self, collection: impl Into<String>, ttl_ms: u64) {
        if let Ok(mut defaults) = self.collection_ttl_defaults_ms.write() {
            defaults.insert(collection.into(), ttl_ms);
        }
    }

    pub fn clear_collection_default_ttl_ms(&self, collection: &str) {
        if let Ok(mut defaults) = self.collection_ttl_defaults_ms.write() {
            defaults.remove(collection);
        }
    }

    pub fn collection_contracts(&self) -> Vec<crate::physical::CollectionContract> {
        self.physical_metadata()
            .map(|metadata| metadata.collection_contracts)
            .unwrap_or_default()
    }

    pub fn collection_contract(
        &self,
        collection: &str,
    ) -> Option<crate::physical::CollectionContract> {
        self.collection_contracts()
            .into_iter()
            .find(|contract| contract.name == collection)
    }

    pub fn save_collection_contract(
        &self,
        contract: crate::physical::CollectionContract,
    ) -> Result<crate::physical::CollectionContract, Box<dyn std::error::Error>> {
        if let Ok(mut defaults) = self.collection_ttl_defaults_ms.write() {
            if let Some(ttl_ms) = contract.default_ttl_ms {
                defaults.insert(contract.name.clone(), ttl_ms);
            } else {
                defaults.remove(&contract.name);
            }
        }

        self.update_physical_metadata(|metadata| {
            if let Some(existing) = metadata
                .collection_contracts
                .iter_mut()
                .find(|existing| existing.name == contract.name)
            {
                *existing = contract.clone();
            } else {
                metadata.collection_contracts.push(contract.clone());
            }
            metadata
                .collection_contracts
                .sort_by(|left, right| left.name.cmp(&right.name));

            if let Some(ttl_ms) = contract.default_ttl_ms {
                metadata
                    .collection_ttl_defaults_ms
                    .insert(contract.name.clone(), ttl_ms);
            } else {
                metadata.collection_ttl_defaults_ms.remove(&contract.name);
            }

            contract.clone()
        })
    }

    pub fn remove_collection_contract(
        &self,
        collection: &str,
    ) -> Result<Option<crate::physical::CollectionContract>, Box<dyn std::error::Error>> {
        if let Ok(mut defaults) = self.collection_ttl_defaults_ms.write() {
            defaults.remove(collection);
        }

        self.update_physical_metadata(|metadata| {
            let removed = metadata
                .collection_contracts
                .iter()
                .position(|contract| contract.name == collection)
                .map(|index| metadata.collection_contracts.remove(index));
            metadata.collection_ttl_defaults_ms.remove(collection);
            metadata
                .indexes
                .retain(|index| index.collection.as_deref() != Some(collection));
            removed
        })
    }

    pub(crate) fn collection_ttl_defaults_snapshot(&self) -> BTreeMap<String, u64> {
        self.collection_ttl_defaults_ms
            .read()
            .map(|defaults| {
                defaults
                    .iter()
                    .map(|(collection, ttl_ms)| (collection.clone(), *ttl_ms))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn load_collection_ttl_defaults_from_metadata(&self) {
        let defaults = self
            .physical_metadata()
            .map(|metadata| metadata.collection_ttl_defaults_ms)
            .unwrap_or_default();

        if let Ok(mut current) = self.collection_ttl_defaults_ms.write() {
            current.clear();
            current.extend(defaults);
        }
    }

    pub fn run_maintenance(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.store.run_maintenance()?;
        self.persist_metadata()?;
        Ok(())
    }

    /// Path to the physical metadata sidecar, if persistent.
    pub fn metadata_path(&self) -> Option<PathBuf> {
        self.path
            .as_ref()
            .map(|path| PhysicalMetadataFile::metadata_path_for(path))
    }

    /// Load the current physical metadata view, bootstrapping from native state when needed.
    pub fn physical_metadata(&self) -> Option<PhysicalMetadataFile> {
        self.load_or_bootstrap_physical_metadata(!self.options.read_only)
            .ok()
    }

    /// Physical index registry derived for the current database state.
    pub fn physical_indexes(&self) -> Vec<PhysicalIndexState> {
        let indexes = self
            .physical_metadata()
            .map(|metadata| metadata.indexes)
            .filter(|indexes| !indexes.is_empty())
            .or_else(|| {
                self.native_physical_state()
                    .map(|state| self.physical_index_state_from_native_state(&state, None))
            })
            .unwrap_or_else(|| self.physical_index_state());
        self.reconcile_index_states_with_native_artifacts(indexes)
    }

    /// List registered named exports from the current physical metadata view.
    pub fn exports(&self) -> Vec<ExportDescriptor> {
        self.physical_metadata()
            .map(|metadata| metadata.exports)
            .or_else(|| {
                self.native_physical_state()
                    .map(|state| self.exports_from_native_state(&state))
            })
            .unwrap_or_default()
    }

    /// List recorded snapshots from the current physical metadata view.
    pub fn snapshots(&self) -> Vec<crate::physical::SnapshotDescriptor> {
        self.physical_metadata()
            .map(|metadata| metadata.snapshots)
            .or_else(|| {
                self.native_physical_state()
                    .map(|state| self.snapshots_from_native_state(&state))
            })
            .unwrap_or_default()
    }

    /// List persisted named graph projections from the current physical metadata view.
    pub fn graph_projections(&self) -> Vec<PhysicalGraphProjection> {
        self.physical_metadata()
            .map(|metadata| metadata.graph_projections)
            .or_else(|| {
                self.native_physical_state()
                    .map(|state| self.graph_projections_from_native_state(&state))
            })
            .unwrap_or_default()
    }

    /// List graph projections declared in the catalog view.
    pub fn declared_graph_projections(&self) -> Vec<PhysicalGraphProjection> {
        self.catalog_model_snapshot().declared_graph_projections
    }

    /// List graph projections currently observed in the operational view.
    pub fn operational_graph_projections(&self) -> Vec<PhysicalGraphProjection> {
        self.graph_projections()
            .into_iter()
            .filter(|projection| {
                projection.last_materialized_sequence.is_some()
                    || matches!(projection.state.as_str(), "materialized" | "stale")
            })
            .collect()
    }

    /// List persisted analytics job metadata from the current physical metadata view.
    pub fn analytics_jobs(&self) -> Vec<PhysicalAnalyticsJob> {
        self.physical_metadata()
            .map(|metadata| metadata.analytics_jobs)
            .or_else(|| {
                self.native_physical_state()
                    .map(|state| self.analytics_jobs_from_native_state(&state))
            })
            .unwrap_or_default()
    }

    /// List analytics jobs declared in the catalog view.
    pub fn declared_analytics_jobs(&self) -> Vec<PhysicalAnalyticsJob> {
        self.catalog_model_snapshot().declared_analytics_jobs
    }

    /// List analytics jobs currently observed in the operational view.
    pub fn operational_analytics_jobs(&self) -> Vec<PhysicalAnalyticsJob> {
        self.analytics_jobs()
            .into_iter()
            .filter(|job| {
                job.last_run_sequence.is_some()
                    || matches!(
                        job.state.as_str(),
                        "running" | "completed" | "failed" | "queued" | "stale"
                    )
            })
            .collect()
    }

    /// List indexes declared in the catalog view.
    pub fn declared_indexes(&self) -> Vec<PhysicalIndexState> {
        self.catalog_model_snapshot().declared_indexes
    }

    /// List indexes currently observed in the operational view.
    pub fn operational_indexes(&self) -> Vec<PhysicalIndexState> {
        self.catalog_model_snapshot().operational_indexes
    }

    /// List reconciled index status entries from the catalog snapshot.
    pub fn index_statuses(&self) -> Vec<CatalogIndexStatus> {
        self.catalog_model_snapshot().index_statuses
    }

    /// Resolve one index status entry from the catalog snapshot.
    pub fn index_status(&self, name: &str) -> Option<CatalogIndexStatus> {
        self.catalog_model_snapshot()
            .index_statuses
            .into_iter()
            .find(|status| status.name == name)
    }

    /// Upsert a named graph projection in the persisted physical metadata.
    pub fn save_graph_projection(
        &self,
        name: impl Into<String>,
        node_labels: Vec<String>,
        node_types: Vec<String>,
        edge_labels: Vec<String>,
        source: impl Into<String>,
    ) -> Result<PhysicalGraphProjection, Box<dyn std::error::Error>> {
        let name = name.into();
        let source = source.into();
        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let projection = if let Some(existing) = metadata
                .graph_projections
                .iter_mut()
                .find(|projection| projection.name == name)
            {
                existing.updated_at_unix_ms = now;
                existing.state = "declared".to_string();
                existing.source = source.clone();
                existing.node_labels = node_labels.clone();
                existing.node_types = node_types.clone();
                existing.edge_labels = edge_labels.clone();
                existing.last_materialized_sequence = None;
                existing.clone()
            } else {
                let projection = PhysicalGraphProjection {
                    name: name.clone(),
                    created_at_unix_ms: now,
                    updated_at_unix_ms: now,
                    state: "declared".to_string(),
                    source: source.clone(),
                    node_labels: node_labels.clone(),
                    node_types: node_types.clone(),
                    edge_labels: edge_labels.clone(),
                    last_materialized_sequence: None,
                };
                metadata.graph_projections.push(projection.clone());
                projection
            };

            Self::mark_projection_dependent_jobs_stale(metadata, &name, now);

            metadata
                .graph_projections
                .sort_by(|left, right| left.name.cmp(&right.name));
            projection
        })
    }

    /// Mark a declared graph projection as materialized in the current physical metadata.
    pub fn materialize_graph_projection(
        &self,
        name: &str,
    ) -> Result<Option<PhysicalGraphProjection>, Box<dyn std::error::Error>> {
        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let idx = metadata
                .graph_projections
                .iter()
                .position(|projection| projection.name == name);
            if let Some(idx) = idx {
                metadata.graph_projections[idx].updated_at_unix_ms = now;
                metadata.graph_projections[idx].state = "materialized".to_string();
                metadata.graph_projections[idx].last_materialized_sequence =
                    Some(metadata.superblock.sequence);
                let result = metadata.graph_projections[idx].clone();
                Self::rearm_projection_dependent_jobs_declared(metadata, name, now);
                return Some(result);
            }
            None
        })
    }

    /// Mark a declared graph projection as materializing.
    pub fn mark_graph_projection_materializing(
        &self,
        name: &str,
    ) -> Result<Option<PhysicalGraphProjection>, Box<dyn std::error::Error>> {
        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let idx = metadata
                .graph_projections
                .iter()
                .position(|projection| projection.name == name);
            if let Some(idx) = idx {
                metadata.graph_projections[idx].updated_at_unix_ms = now;
                metadata.graph_projections[idx].state = "materializing".to_string();
                metadata.graph_projections[idx].last_materialized_sequence = None;
                let result = metadata.graph_projections[idx].clone();
                Self::mark_projection_dependent_jobs_stale(metadata, name, now);
                return Some(result);
            }
            None
        })
    }

    /// Mark a graph projection as failed.
    pub fn fail_graph_projection(
        &self,
        name: &str,
    ) -> Result<Option<PhysicalGraphProjection>, Box<dyn std::error::Error>> {
        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let idx = metadata
                .graph_projections
                .iter()
                .position(|projection| projection.name == name);
            if let Some(idx) = idx {
                metadata.graph_projections[idx].updated_at_unix_ms = now;
                metadata.graph_projections[idx].state = "failed".to_string();
                metadata.graph_projections[idx].last_materialized_sequence = None;
                let result = metadata.graph_projections[idx].clone();
                Self::mark_projection_dependent_jobs_stale(metadata, name, now);
                return Some(result);
            }
            None
        })
    }

    /// Mark a graph projection as stale while preserving any last materialized sequence.
    pub fn mark_graph_projection_stale(
        &self,
        name: &str,
    ) -> Result<Option<PhysicalGraphProjection>, Box<dyn std::error::Error>> {
        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let idx = metadata
                .graph_projections
                .iter()
                .position(|projection| projection.name == name);
            if let Some(idx) = idx {
                metadata.graph_projections[idx].updated_at_unix_ms = now;
                metadata.graph_projections[idx].state = "stale".to_string();
                let result = metadata.graph_projections[idx].clone();
                Self::mark_projection_dependent_jobs_stale(metadata, name, now);
                return Some(result);
            }
            None
        })
    }

    fn mark_projection_dependent_jobs_stale(
        metadata: &mut PhysicalMetadataFile,
        projection_name: &str,
        now: u128,
    ) {
        for job in metadata.analytics_jobs.iter_mut() {
            if job.projection.as_deref() == Some(projection_name) && job.state != "declared" {
                job.state = "stale".to_string();
                job.updated_at_unix_ms = now;
            }
        }
    }

    fn rearm_projection_dependent_jobs_declared(
        metadata: &mut PhysicalMetadataFile,
        projection_name: &str,
        now: u128,
    ) {
        for job in metadata.analytics_jobs.iter_mut() {
            if job.projection.as_deref() == Some(projection_name) && job.state == "stale" {
                job.state = "declared".to_string();
                job.last_run_sequence = None;
                job.updated_at_unix_ms = now;
            }
        }
    }

    /// Declare or update analytics job metadata without marking it as executed.
    pub fn save_analytics_job(
        &self,
        kind: impl Into<String>,
        projection: Option<String>,
        metadata_entries: BTreeMap<String, String>,
    ) -> Result<PhysicalAnalyticsJob, Box<dyn std::error::Error>> {
        let kind = kind.into();
        let job_id = Self::analytics_job_id(&kind, projection.as_deref());
        if let Some(projection_name) = projection.as_deref() {
            if !self.graph_projection_is_declared(projection_name) {
                return Err(format!(
                    "graph projection '{projection_name}' is not declared in physical metadata"
                )
                .into());
            }
        }

        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();

            let job = if let Some(existing) = metadata
                .analytics_jobs
                .iter_mut()
                .find(|job| job.id == job_id)
            {
                existing.kind = kind.clone();
                existing.projection = projection.clone();
                existing.updated_at_unix_ms = now;
                existing.metadata = metadata_entries.clone();
                if existing.last_run_sequence.is_none() {
                    existing.state = "declared".to_string();
                }
                existing.clone()
            } else {
                let job = PhysicalAnalyticsJob {
                    id: job_id.clone(),
                    kind: kind.clone(),
                    state: "declared".to_string(),
                    projection: projection.clone(),
                    created_at_unix_ms: now,
                    updated_at_unix_ms: now,
                    last_run_sequence: None,
                    metadata: metadata_entries.clone(),
                };
                metadata.analytics_jobs.push(job.clone());
                job
            };

            metadata
                .analytics_jobs
                .sort_by(|left, right| left.id.cmp(&right.id));
            job
        })
    }

    /// Record or update analytics job metadata in the persisted physical metadata.
    pub fn record_analytics_job(
        &self,
        kind: impl Into<String>,
        projection: Option<String>,
        metadata_entries: BTreeMap<String, String>,
    ) -> Result<PhysicalAnalyticsJob, Box<dyn std::error::Error>> {
        let kind = kind.into();
        let job_id = Self::analytics_job_id(&kind, projection.as_deref());
        if let Some(projection_name) = projection.as_deref() {
            if !self.graph_projection_is_declared(projection_name) {
                return Err(format!(
                    "graph projection '{projection_name}' is not declared in physical metadata"
                )
                .into());
            }
            if !self.graph_projection_is_operational(projection_name) {
                return Err(format!(
                    "graph projection '{projection_name}' is declared but not operationally materialized"
                )
                .into());
            }
        }

        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();

            let existing = metadata
                .analytics_jobs
                .iter_mut()
                .find(|job| job.id == job_id)?;
            existing.state = "completed".to_string();
            existing.kind = kind.clone();
            existing.projection = projection.clone();
            existing.updated_at_unix_ms = now;
            existing.last_run_sequence = Some(metadata.superblock.sequence);
            existing.metadata = metadata_entries.clone();
            let job = existing.clone();

            metadata
                .analytics_jobs
                .sort_by(|left, right| left.id.cmp(&right.id));
            Some(job)
        })
        .and_then(|job| {
            job.ok_or_else(|| {
                format!("analytics job '{job_id}' is not declared in physical metadata").into()
            })
        })
    }

    /// Mark a declared analytics job as running.
    pub fn queue_analytics_job(
        &self,
        kind: impl Into<String>,
        projection: Option<String>,
        metadata_entries: BTreeMap<String, String>,
    ) -> Result<PhysicalAnalyticsJob, Box<dyn std::error::Error>> {
        let kind = kind.into();
        let job_id = Self::analytics_job_id(&kind, projection.as_deref());
        if let Some(projection_name) = projection.as_deref() {
            if !self.graph_projection_is_declared(projection_name) {
                return Err(format!(
                    "graph projection '{projection_name}' is not declared in physical metadata"
                )
                .into());
            }
            if !self.graph_projection_is_operational(projection_name) {
                return Err(format!(
                    "graph projection '{projection_name}' is declared but not operationally materialized"
                )
                .into());
            }
        }

        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();

            let existing = metadata
                .analytics_jobs
                .iter_mut()
                .find(|job| job.id == job_id)?;
            existing.state = "queued".to_string();
            existing.kind = kind.clone();
            existing.projection = projection.clone();
            existing.updated_at_unix_ms = now;
            existing.metadata = metadata_entries.clone();
            let job = existing.clone();

            metadata
                .analytics_jobs
                .sort_by(|left, right| left.id.cmp(&right.id));
            Some(job)
        })
        .and_then(|job| {
            job.ok_or_else(|| {
                format!("analytics job '{job_id}' is not declared in physical metadata").into()
            })
        })
    }

    /// Mark a declared analytics job as running.
    pub fn start_analytics_job(
        &self,
        kind: impl Into<String>,
        projection: Option<String>,
        metadata_entries: BTreeMap<String, String>,
    ) -> Result<PhysicalAnalyticsJob, Box<dyn std::error::Error>> {
        let kind = kind.into();
        let job_id = Self::analytics_job_id(&kind, projection.as_deref());
        if let Some(projection_name) = projection.as_deref() {
            if !self.graph_projection_is_declared(projection_name) {
                return Err(format!(
                    "graph projection '{projection_name}' is not declared in physical metadata"
                )
                .into());
            }
            if !self.graph_projection_is_operational(projection_name) {
                return Err(format!(
                    "graph projection '{projection_name}' is declared but not operationally materialized"
                )
                .into());
            }
        }

        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();

            let existing = metadata
                .analytics_jobs
                .iter_mut()
                .find(|job| job.id == job_id)?;
            existing.state = "running".to_string();
            existing.kind = kind.clone();
            existing.projection = projection.clone();
            existing.updated_at_unix_ms = now;
            existing.metadata = metadata_entries.clone();
            let job = existing.clone();

            metadata
                .analytics_jobs
                .sort_by(|left, right| left.id.cmp(&right.id));
            Some(job)
        })
        .and_then(|job| {
            job.ok_or_else(|| {
                format!("analytics job '{job_id}' is not declared in physical metadata").into()
            })
        })
    }

    /// Mark a declared analytics job as failed.
    pub fn fail_analytics_job(
        &self,
        kind: impl Into<String>,
        projection: Option<String>,
        metadata_entries: BTreeMap<String, String>,
    ) -> Result<PhysicalAnalyticsJob, Box<dyn std::error::Error>> {
        let kind = kind.into();
        let job_id = Self::analytics_job_id(&kind, projection.as_deref());

        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();

            let existing = metadata
                .analytics_jobs
                .iter_mut()
                .find(|job| job.id == job_id)?;
            existing.state = "failed".to_string();
            existing.kind = kind.clone();
            existing.projection = projection.clone();
            existing.updated_at_unix_ms = now;
            existing.metadata = metadata_entries.clone();
            let job = existing.clone();

            metadata
                .analytics_jobs
                .sort_by(|left, right| left.id.cmp(&right.id));
            Some(job)
        })
        .and_then(|job| {
            job.ok_or_else(|| {
                format!("analytics job '{job_id}' is not declared in physical metadata").into()
            })
        })
    }

    /// Mark a declared analytics job as stale.
    pub fn mark_analytics_job_stale(
        &self,
        kind: impl Into<String>,
        projection: Option<String>,
        metadata_entries: BTreeMap<String, String>,
    ) -> Result<PhysicalAnalyticsJob, Box<dyn std::error::Error>> {
        let kind = kind.into();
        let job_id = Self::analytics_job_id(&kind, projection.as_deref());

        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();

            let existing = metadata
                .analytics_jobs
                .iter_mut()
                .find(|job| job.id == job_id)?;
            existing.state = "stale".to_string();
            existing.kind = kind.clone();
            existing.projection = projection.clone();
            existing.updated_at_unix_ms = now;
            existing.metadata = metadata_entries.clone();
            let job = existing.clone();

            metadata
                .analytics_jobs
                .sort_by(|left, right| left.id.cmp(&right.id));
            Some(job)
        })
        .and_then(|job| {
            job.ok_or_else(|| {
                format!("analytics job '{job_id}' is not declared in physical metadata").into()
            })
        })
    }

    /// Create a named export by copying the current database file and metadata sidecar.
    pub fn create_named_export(
        &self,
        name: impl Into<String>,
    ) -> Result<ExportDescriptor, Box<dyn std::error::Error>> {
        let name = name.into();
        if self.options.mode != StorageMode::Persistent {
            return Err("exports require persistent mode".into());
        }
        let Some(path) = self.path() else {
            return Err("database path is not available".into());
        };

        self.flush()?;

        let mut metadata = self.load_or_bootstrap_physical_metadata(true)?;
        let export_data_path = PhysicalMetadataFile::export_data_path_for(path, &name);
        let export_metadata_path = PhysicalMetadataFile::metadata_path_for(&export_data_path);
        let export_metadata_binary_path =
            PhysicalMetadataFile::metadata_binary_path_for(&export_data_path);

        fs::copy(path, &export_data_path)?;

        let descriptor = ExportDescriptor {
            name: name.clone(),
            created_at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            snapshot_id: metadata
                .snapshots
                .last()
                .map(|snapshot| snapshot.snapshot_id),
            superblock_sequence: metadata.superblock.sequence,
            data_path: export_data_path.display().to_string(),
            metadata_path: export_metadata_path.display().to_string(),
            collection_count: metadata.catalog.total_collections,
            total_entities: metadata.catalog.total_entities,
        };

        metadata
            .exports
            .retain(|export| export.name != descriptor.name);
        metadata.exports.push(descriptor.clone());
        self.prune_export_registry(&mut metadata.exports);
        metadata.save_for_data_path(path)?;
        metadata.save_to_binary_path(&export_metadata_binary_path)?;
        metadata.save_to_path(&export_metadata_path)?;

        Ok(descriptor)
    }

    /// Enable or disable a physical index entry in the persisted registry.
    pub fn set_index_enabled(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<Option<PhysicalIndexState>, Box<dyn std::error::Error>> {
        let Some(status) = self.index_status(name) else {
            return Err(format!("index '{name}' is not present in catalog status").into());
        };
        if !status.declared {
            return Err(format!("index '{name}' is not declared in physical metadata").into());
        }
        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            if let Some(index) = metadata.indexes.iter_mut().find(|index| index.name == name) {
                index.enabled = enabled;
                if !enabled {
                    index.build_state = "disabled".to_string();
                } else if index.build_state == "disabled" {
                    index.build_state = if index.artifact_root_page.is_some() {
                        "ready".to_string()
                    } else {
                        "declared-unbuilt".to_string()
                    };
                }
                index.last_refresh_ms = Some(now);
                return Some(index.clone());
            }
            None
        })
    }

    /// Mark a declared physical index as building in the persisted registry.
    pub fn mark_index_building(
        &self,
        name: &str,
    ) -> Result<Option<PhysicalIndexState>, Box<dyn std::error::Error>> {
        let Some(status) = self.index_status(name) else {
            return Err(format!("index '{name}' is not present in catalog status").into());
        };
        if !status.declared {
            return Err(format!("index '{name}' is not declared in physical metadata").into());
        }
        if status.lifecycle_state == "disabled" {
            return Err(format!("index '{name}' is disabled").into());
        }
        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            if let Some(index) = metadata.indexes.iter_mut().find(|index| index.name == name) {
                index.build_state = "building".to_string();
                index.last_refresh_ms = Some(now);
                return Some(index.clone());
            }
            None
        })
    }

    /// Mark a declared physical index as failed in the persisted registry.
    pub fn fail_index(
        &self,
        name: &str,
    ) -> Result<Option<PhysicalIndexState>, Box<dyn std::error::Error>> {
        let Some(status) = self.index_status(name) else {
            return Err(format!("index '{name}' is not present in catalog status").into());
        };
        if !status.declared {
            return Err(format!("index '{name}' is not declared in physical metadata").into());
        }
        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            if let Some(index) = metadata.indexes.iter_mut().find(|index| index.name == name) {
                index.build_state = "failed".to_string();
                index.last_refresh_ms = Some(now);
                return Some(index.clone());
            }
            None
        })
    }

    /// Mark a declared physical index as stale in the persisted registry.
    pub fn mark_index_stale(
        &self,
        name: &str,
    ) -> Result<Option<PhysicalIndexState>, Box<dyn std::error::Error>> {
        let Some(status) = self.index_status(name) else {
            return Err(format!("index '{name}' is not present in catalog status").into());
        };
        if !status.declared {
            return Err(format!("index '{name}' is not declared in physical metadata").into());
        }
        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            if let Some(index) = metadata.indexes.iter_mut().find(|index| index.name == name) {
                index.build_state = if index.enabled {
                    "stale".to_string()
                } else {
                    "disabled".to_string()
                };
                index.last_refresh_ms = Some(now);
                return Some(index.clone());
            }
            None
        })
    }

    /// Mark a declared physical index as ready in the persisted registry.
    pub fn mark_index_ready(
        &self,
        name: &str,
    ) -> Result<Option<PhysicalIndexState>, Box<dyn std::error::Error>> {
        self.warmup_index(name)
    }

    /// Mark a physical index as warmed up/refreshed in the persisted registry.
    pub fn warmup_index(
        &self,
        name: &str,
    ) -> Result<Option<PhysicalIndexState>, Box<dyn std::error::Error>> {
        let Some(status) = self.index_status(name) else {
            return Err(format!("index '{name}' is not present in catalog status").into());
        };
        if !status.declared {
            return Err(format!("index '{name}' is not declared in physical metadata").into());
        }
        if status.lifecycle_state == "disabled" {
            return Err(format!("index '{name}' is disabled").into());
        }
        if !status.operational {
            return Err(
                format!("index '{name}' is declared but not operationally materialized").into(),
            );
        }
        let warmed_artifact = self
            .physical_indexes()
            .into_iter()
            .find(|index| index.name == name)
            .map(|mut index| {
                self.warmup_native_vector_artifact_for_index(&index)?;
                self.apply_runtime_native_artifact_to_index_state(&mut index)?;
                Ok::<_, String>(index)
            })
            .transpose()
            .map_err(|err| -> Box<dyn std::error::Error> { err.into() })?;

        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            if let Some(index) = metadata.indexes.iter_mut().find(|index| index.name == name) {
                if let Some(warmed) = warmed_artifact.as_ref() {
                    index.entries = warmed.entries;
                    index.estimated_memory_bytes = warmed.estimated_memory_bytes;
                    index.backend = warmed.backend.clone();
                    index.build_state = "ready".to_string();
                }
                index.last_refresh_ms = Some(now);
                return Some(index.clone());
            }
            None
        })
    }

    /// Rebuild physical index metadata from the current catalog, optionally restricted to one collection.
    pub fn rebuild_index_registry(
        &self,
        collection: Option<&str>,
    ) -> Result<Vec<PhysicalIndexState>, Box<dyn std::error::Error>> {
        let fresh = self.reconcile_index_states_with_native_artifacts(self.physical_index_state());
        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();

            let mut affected = Vec::new();
            let declared = metadata.indexes.clone();
            for declared_index in declared {
                let matches_collection = collection.is_none_or(|collection_name| {
                    declared_index.collection.as_deref() == Some(collection_name)
                });
                if !matches_collection {
                    continue;
                }

                let mut rebuilt = fresh
                    .iter()
                    .find(|index| index.name == declared_index.name)
                    .cloned()
                    .unwrap_or_else(|| {
                        let mut index = declared_index.clone();
                        index.build_state = "declared-unbuilt".to_string();
                        index
                    });
                rebuilt.enabled = declared_index.enabled;
                rebuilt.artifact_kind = rebuilt
                    .artifact_kind
                    .or_else(|| declared_index.artifact_kind.clone());
                rebuilt.artifact_root_page = rebuilt
                    .artifact_root_page
                    .or(declared_index.artifact_root_page);
                rebuilt.artifact_checksum = rebuilt
                    .artifact_checksum
                    .or(declared_index.artifact_checksum);
                rebuilt.build_state =
                    Self::finalize_rebuilt_index_build_state(&declared_index, &rebuilt);
                rebuilt.last_refresh_ms = Some(now);

                if let Some(existing) = metadata
                    .indexes
                    .iter_mut()
                    .find(|index| index.name == rebuilt.name)
                {
                    *existing = rebuilt.clone();
                } else {
                    metadata.indexes.push(rebuilt.clone());
                }

                affected.push(rebuilt);
            }

            affected
        })
    }

    fn finalize_rebuilt_index_build_state(
        declared: &PhysicalIndexState,
        rebuilt: &PhysicalIndexState,
    ) -> String {
        if !rebuilt.enabled {
            return "disabled".to_string();
        }

        if declared.build_state == "failed" || rebuilt.build_state == "failed" {
            return "failed".to_string();
        }

        let native_artifact_family = Self::native_artifact_kind_for_index(rebuilt.kind).is_some();
        if native_artifact_family {
            if rebuilt.artifact_root_page.is_some() && rebuilt.artifact_checksum.is_some() {
                return "ready".to_string();
            }
            if declared.artifact_root_page.is_some()
                || declared.artifact_checksum.is_some()
                || declared.artifact_kind.is_some()
            {
                return "stale".to_string();
            }
            return "declared-unbuilt".to_string();
        }

        if rebuilt.entries > 0 {
            return "ready".to_string();
        }

        if matches!(
            declared.build_state.as_str(),
            "stale" | "artifact-published" | "registry-loaded"
        ) {
            return "stale".to_string();
        }

        "declared-unbuilt".to_string()
    }
}
