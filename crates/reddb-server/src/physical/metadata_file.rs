use super::*;

impl PhysicalMetadataFile {
    pub fn from_state(
        options: RedDBOptions,
        catalog: CatalogSnapshot,
        collection_roots: BTreeMap<String, u64>,
        indexes: Vec<PhysicalIndexState>,
        previous: Option<&PhysicalMetadataFile>,
    ) -> Self {
        let now = unix_ms_now();
        let mut manifest = SchemaManifest::now(options, catalog.total_collections);
        if let Some(previous) = previous {
            manifest.created_at_unix_ms = previous.manifest.created_at_unix_ms;
        }
        manifest.updated_at_unix_ms = now;

        let sequence = previous
            .map(|previous| previous.superblock.sequence.saturating_add(1))
            .unwrap_or(1);
        let mut manifest_events = previous
            .map(|previous| previous.manifest_events.clone())
            .unwrap_or_default();
        manifest_events.extend(build_manifest_events(
            previous.map(|previous| &previous.superblock.collection_roots),
            &collection_roots,
            sequence,
        ));
        trim_manifest_history(&mut manifest_events);

        let superblock = SuperblockHeader {
            format_version: REDDB_FORMAT_VERSION,
            sequence,
            copies: DEFAULT_SUPERBLOCK_COPIES,
            manifest: previous
                .map(|previous| previous.superblock.manifest.clone())
                .unwrap_or_default(),
            free_set: previous
                .map(|previous| previous.superblock.free_set)
                .unwrap_or_default(),
            collection_roots,
        };

        let mut snapshots = previous
            .map(|previous| previous.snapshots.clone())
            .unwrap_or_default();
        snapshots.push(SnapshotDescriptor {
            snapshot_id: sequence,
            created_at_unix_ms: now,
            superblock_sequence: sequence,
            collection_count: catalog.total_collections,
            total_entities: catalog.total_entities,
        });
        trim_snapshot_history(&mut snapshots, manifest.options.snapshot_retention);

        let collection_layouts = allocate_collection_layouts(
            &catalog,
            previous.map(|previous| &previous.collection_layouts),
            now,
        );
        let indexes = allocate_index_layouts(
            indexes,
            &collection_layouts,
            previous.map(|previous| &previous.indexes),
            now,
        );

        Self {
            protocol_version: PHYSICAL_METADATA_PROTOCOL_VERSION.to_string(),
            generated_at_unix_ms: now,
            last_loaded_from: previous.and_then(|previous| previous.last_loaded_from.clone()),
            last_healed_at_unix_ms: previous.and_then(|previous| previous.last_healed_at_unix_ms),
            manifest,
            catalog,
            manifest_events,
            indexes,
            graph_projections: previous
                .map(|previous| previous.graph_projections.clone())
                .unwrap_or_default(),
            analytics_jobs: previous
                .map(|previous| previous.analytics_jobs.clone())
                .unwrap_or_default(),
            tree_definitions: previous
                .map(|previous| previous.tree_definitions.clone())
                .unwrap_or_default(),
            collection_ttl_defaults_ms: previous
                .map(|previous| previous.collection_ttl_defaults_ms.clone())
                .unwrap_or_default(),
            collection_contracts: previous
                .map(|previous| previous.collection_contracts.clone())
                .unwrap_or_default(),
            collection_layouts,
            hypertables: previous
                .map(|previous| previous.hypertables.clone())
                .unwrap_or_default(),
            exports: previous
                .map(|previous| previous.exports.clone())
                .unwrap_or_default(),
            superblock,
            snapshots,
        }
    }

    pub fn metadata_path_for(data_path: &Path) -> PathBuf {
        let file_name = data_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "data.rdb".to_string());
        let meta_file = format!("{file_name}.meta.json");
        match data_path.parent() {
            Some(parent) => parent.join(meta_file),
            None => PathBuf::from(meta_file),
        }
    }

    pub fn metadata_binary_path_for(data_path: &Path) -> PathBuf {
        let file_name = data_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "data.rdb".to_string());
        let meta_file = format!("{file_name}.{PHYSICAL_METADATA_BINARY_EXTENSION}");
        match data_path.parent() {
            Some(parent) => parent.join(meta_file),
            None => PathBuf::from(meta_file),
        }
    }

    pub fn metadata_journal_path_for(data_path: &Path, sequence: u64) -> PathBuf {
        let file_name = data_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "data.rdb".to_string());
        let meta_file =
            format!("{file_name}.{PHYSICAL_METADATA_BINARY_EXTENSION}.seq-{sequence:020}");
        match data_path.parent() {
            Some(parent) => parent.join(meta_file),
            None => PathBuf::from(meta_file),
        }
    }

    pub fn export_data_path_for(data_path: &Path, name: &str) -> PathBuf {
        let file_name = data_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "data.rdb".to_string());
        let stem = file_name.strip_suffix(".rdb").unwrap_or(&file_name);
        let export_file = format!("{stem}.export.{}.rdb", sanitize_export_name(name));
        match data_path.parent() {
            Some(parent) => parent.join(export_file),
            None => PathBuf::from(export_file),
        }
    }

    pub fn load_for_data_path(data_path: &Path) -> io::Result<Self> {
        Self::load_for_data_path_with_source(data_path).map(|(metadata, _)| metadata)
    }

    pub fn load_for_data_path_with_source(
        data_path: &Path,
    ) -> io::Result<(Self, PhysicalMetadataSource)> {
        let binary_path = Self::metadata_binary_path_for(data_path);
        if binary_path.exists() {
            match Self::load_from_binary_path(&binary_path) {
                Ok(metadata) => {
                    return Ok((metadata, PhysicalMetadataSource::Binary));
                }
                Err(_) => {
                    let mut journal_paths = Self::journal_paths_for_data_path(data_path)?;
                    journal_paths.reverse();
                    for journal_path in journal_paths {
                        if let Ok(metadata) = Self::load_from_binary_path(&journal_path) {
                            let healed =
                                metadata.mark_recovery(PhysicalMetadataSource::BinaryJournal);
                            let _ = healed.heal_primary_metadata_for_data_path(data_path);
                            return Ok((healed, PhysicalMetadataSource::BinaryJournal));
                        }
                    }
                }
            }
        }
        Self::load_from_path(&Self::metadata_path_for(data_path)).map(|metadata| {
            let healed = metadata.mark_recovery(PhysicalMetadataSource::Json);
            let _ = healed.heal_primary_metadata_for_data_path(data_path);
            (healed, PhysicalMetadataSource::Json)
        })
    }

    pub fn save_for_data_path(&self, data_path: &Path) -> io::Result<PathBuf> {
        let binary_path = Self::metadata_binary_path_for(data_path);
        if binary_path.exists() && super::seqn_journal_enabled() {
            let sequence = Self::load_from_binary_path(&binary_path)
                .map(|metadata| metadata.superblock.sequence)
                .unwrap_or(self.superblock.sequence);
            let journal_path = Self::metadata_journal_path_for(data_path, sequence);
            let _ = fs::copy(&binary_path, journal_path);
        }
        self.save_to_binary_path(&binary_path)?;
        self.prune_journal_for_data_path(data_path)?;
        if super::meta_json_sidecar_enabled() {
            let json_path = Self::metadata_path_for(data_path);
            self.save_to_path(&json_path)?;
        }
        Ok(binary_path)
    }

    pub fn load_from_path(path: &Path) -> io::Result<Self> {
        let text = fs::read_to_string(path)?;
        let parsed = parse_json(&text).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid physical metadata JSON: {err}"),
            )
        })?;
        let json = JsonValue::from(parsed);
        Self::from_json_value(&json)
    }

    pub fn save_to_path(&self, path: &Path) -> io::Result<()> {
        let text = self.to_json_value().to_string_pretty();
        fs::write(path, text)
    }

    pub fn load_from_binary_path(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        let json = from_slice::<JsonValue>(&bytes).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid physical metadata binary: {err}"),
            )
        })?;
        Self::from_json_value(&json)
    }

    pub fn save_to_binary_path(&self, path: &Path) -> io::Result<()> {
        let bytes = to_vec(&self.to_json_value()).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("failed to encode physical metadata binary: {err}"),
            )
        })?;
        fs::write(path, bytes)
    }

    pub fn journal_paths_for_data_path(data_path: &Path) -> io::Result<Vec<PathBuf>> {
        let Some(parent) = data_path.parent() else {
            return Ok(Vec::new());
        };
        let file_name = data_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "data.rdb".to_string());
        let prefix = format!("{file_name}.{PHYSICAL_METADATA_BINARY_EXTENSION}.seq-");

        let mut paths = Vec::new();
        for entry in fs::read_dir(parent)? {
            let entry = entry?;
            let path = entry.path();
            let Some(name) = path.file_name().map(|name| name.to_string_lossy()) else {
                continue;
            };
            if name.starts_with(&prefix) {
                paths.push(path);
            }
        }
        paths.sort();
        Ok(paths)
    }

    pub fn prune_journal_for_data_path(&self, data_path: &Path) -> io::Result<()> {
        let retention = super::seqn_journal_retention();
        let mut paths = Self::journal_paths_for_data_path(data_path)?;
        if paths.len() <= retention {
            return Ok(());
        }
        let delete_count = paths.len() - retention;
        for path in paths.drain(0..delete_count) {
            let _ = fs::remove_file(path);
        }
        Ok(())
    }

    pub fn heal_primary_metadata_for_data_path(&self, data_path: &Path) -> io::Result<()> {
        let binary_path = Self::metadata_binary_path_for(data_path);
        self.save_to_binary_path(&binary_path)?;
        if super::meta_json_sidecar_enabled() {
            let json_path = Self::metadata_path_for(data_path);
            self.save_to_path(&json_path)?;
        }
        Ok(())
    }

    pub fn mark_recovery(&self, source: PhysicalMetadataSource) -> Self {
        let mut metadata = self.clone();
        metadata.last_loaded_from = Some(source.as_str().to_string());
        metadata.last_healed_at_unix_ms = Some(unix_ms_now());
        metadata
    }

    pub fn rename_collection_layout(&mut self, old_name: &str, new_name: &str) -> bool {
        let now = unix_ms_now();
        let mut renamed = false;
        for layout in &mut self.collection_layouts {
            if layout.name == old_name {
                layout.name = new_name.to_string();
                layout.updated_at_unix_ms = now;
                renamed = true;
            }
        }
        for index in &mut self.indexes {
            if index.collection.as_deref() == Some(old_name) {
                index.collection = Some(new_name.to_string());
            }
        }
        renamed
    }

    pub fn rename_index_layout(&mut self, old_name: &str, new_name: &str) -> bool {
        let mut renamed = false;
        for index in &mut self.indexes {
            if index.name == old_name {
                index.name = new_name.to_string();
                renamed = true;
            }
        }
        renamed
    }

    pub fn to_json_value(&self) -> JsonValue {
        let mut root = Map::new();
        root.insert(
            "protocol_version".to_string(),
            JsonValue::String(self.protocol_version.clone()),
        );
        root.insert(
            "generated_at_unix_ms".to_string(),
            json_u128(self.generated_at_unix_ms),
        );
        root.insert(
            "last_loaded_from".to_string(),
            self.last_loaded_from
                .clone()
                .map(JsonValue::String)
                .unwrap_or(JsonValue::Null),
        );
        root.insert(
            "last_healed_at_unix_ms".to_string(),
            self.last_healed_at_unix_ms
                .map(json_u128)
                .unwrap_or(JsonValue::Null),
        );
        root.insert("manifest".to_string(), manifest_to_json(&self.manifest));
        root.insert("catalog".to_string(), catalog_to_json(&self.catalog));
        root.insert(
            "manifest_events".to_string(),
            JsonValue::Array(
                self.manifest_events
                    .iter()
                    .map(manifest_event_to_json)
                    .collect(),
            ),
        );
        root.insert(
            "indexes".to_string(),
            JsonValue::Array(self.indexes.iter().map(index_state_to_json).collect()),
        );
        root.insert(
            "graph_projections".to_string(),
            JsonValue::Array(
                self.graph_projections
                    .iter()
                    .map(graph_projection_to_json)
                    .collect(),
            ),
        );
        root.insert(
            "analytics_jobs".to_string(),
            JsonValue::Array(
                self.analytics_jobs
                    .iter()
                    .map(analytics_job_to_json)
                    .collect(),
            ),
        );
        root.insert(
            "tree_definitions".to_string(),
            JsonValue::Array(
                self.tree_definitions
                    .iter()
                    .map(tree_definition_to_json)
                    .collect(),
            ),
        );
        root.insert(
            "collection_ttl_defaults_ms".to_string(),
            JsonValue::Object(
                self.collection_ttl_defaults_ms
                    .iter()
                    .map(|(collection, ttl_ms)| (collection.clone(), json_u64(*ttl_ms)))
                    .collect(),
            ),
        );
        root.insert(
            "collection_contracts".to_string(),
            JsonValue::Array(
                self.collection_contracts
                    .iter()
                    .map(collection_contract_to_json)
                    .collect(),
            ),
        );
        root.insert(
            "collection_layouts".to_string(),
            JsonValue::Array(
                self.collection_layouts
                    .iter()
                    .map(collection_layout_to_json)
                    .collect(),
            ),
        );
        root.insert(
            "hypertables".to_string(),
            JsonValue::Array(self.hypertables.iter().map(hypertable_to_json).collect()),
        );
        root.insert(
            "exports".to_string(),
            JsonValue::Array(self.exports.iter().map(export_descriptor_to_json).collect()),
        );
        root.insert(
            "superblock".to_string(),
            superblock_to_json(&self.superblock),
        );
        root.insert(
            "snapshots".to_string(),
            JsonValue::Array(
                self.snapshots
                    .iter()
                    .map(snapshot_descriptor_to_json)
                    .collect(),
            ),
        );
        JsonValue::Object(root)
    }

    pub fn from_json_value(value: &JsonValue) -> io::Result<Self> {
        let object = expect_object(value, "physical metadata root")?;
        Ok(Self {
            protocol_version: json_string_required(object, "protocol_version")?,
            generated_at_unix_ms: json_u128_required(object, "generated_at_unix_ms")?,
            last_loaded_from: object
                .get("last_loaded_from")
                .and_then(JsonValue::as_str)
                .map(|value| value.to_string()),
            last_healed_at_unix_ms: object
                .get("last_healed_at_unix_ms")
                .map(json_u128_value)
                .transpose()?,
            manifest: manifest_from_json(json_required(object, "manifest")?)?,
            catalog: catalog_from_json(json_required(object, "catalog")?)?,
            manifest_events: object
                .get("manifest_events")
                .and_then(JsonValue::as_array)
                .map(|values| {
                    values
                        .iter()
                        .map(manifest_event_from_json)
                        .collect::<io::Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default(),
            indexes: object
                .get("indexes")
                .and_then(JsonValue::as_array)
                .map(|values| {
                    values
                        .iter()
                        .map(index_state_from_json)
                        .collect::<io::Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default(),
            graph_projections: object
                .get("graph_projections")
                .and_then(JsonValue::as_array)
                .map(|values| {
                    values
                        .iter()
                        .map(graph_projection_from_json)
                        .collect::<io::Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default(),
            analytics_jobs: object
                .get("analytics_jobs")
                .and_then(JsonValue::as_array)
                .map(|values| {
                    values
                        .iter()
                        .map(analytics_job_from_json)
                        .collect::<io::Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default(),
            tree_definitions: object
                .get("tree_definitions")
                .and_then(JsonValue::as_array)
                .map(|values| {
                    values
                        .iter()
                        .map(tree_definition_from_json)
                        .collect::<io::Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default(),
            collection_ttl_defaults_ms: object
                .get("collection_ttl_defaults_ms")
                .and_then(JsonValue::as_object)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|(collection, ttl_ms)| {
                            json_u64_value(ttl_ms)
                                .ok()
                                .map(|ttl_ms| (collection.clone(), ttl_ms))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            collection_contracts: object
                .get("collection_contracts")
                .and_then(JsonValue::as_array)
                .map(|values| {
                    values
                        .iter()
                        .map(collection_contract_from_json)
                        .collect::<io::Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default(),
            collection_layouts: object
                .get("collection_layouts")
                .and_then(JsonValue::as_array)
                .map(|values| {
                    values
                        .iter()
                        .map(collection_layout_from_json)
                        .collect::<io::Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default(),
            hypertables: object
                .get("hypertables")
                .and_then(JsonValue::as_array)
                .map(|values| {
                    values
                        .iter()
                        .map(hypertable_from_json)
                        .collect::<io::Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default(),
            exports: object
                .get("exports")
                .and_then(JsonValue::as_array)
                .map(|values| {
                    values
                        .iter()
                        .map(export_descriptor_from_json)
                        .collect::<io::Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default(),
            superblock: superblock_from_json(json_required(object, "superblock")?)?,
            snapshots: json_required(object, "snapshots")?
                .as_array()
                .ok_or_else(|| invalid_data("field 'snapshots' must be an array"))?
                .iter()
                .map(snapshot_descriptor_from_json)
                .collect::<io::Result<Vec<_>>>()?,
        })
    }
}

fn allocate_collection_layouts(
    catalog: &CatalogSnapshot,
    previous: Option<&Vec<PhysicalCollectionLayout>>,
    now: u128,
) -> Vec<PhysicalCollectionLayout> {
    let previous = previous.unwrap_or_else(|| {
        static EMPTY: Vec<PhysicalCollectionLayout> = Vec::new();
        &EMPTY
    });
    let mut used_logical_ids: BTreeSet<u64> =
        previous.iter().map(|layout| layout.logical_id).collect();
    let mut used_physical_ids: BTreeSet<String> = previous
        .iter()
        .map(|layout| layout.physical_file_id.clone())
        .collect();
    let mut layouts = Vec::new();
    for name in catalog.stats_by_collection.keys() {
        if let Some(existing) = previous.iter().find(|layout| layout.name == *name) {
            layouts.push(existing.clone());
            continue;
        }
        let logical_id = allocate_compact_id(&mut used_logical_ids);
        let physical_file_id =
            allocate_physical_file_id("collection", logical_id, &mut used_physical_ids);
        layouts.push(PhysicalCollectionLayout {
            name: name.clone(),
            logical_id,
            physical_file_name: format!("{physical_file_id}.rdc"),
            physical_file_id,
            created_at_unix_ms: now,
            updated_at_unix_ms: now,
        });
    }
    layouts.sort_by_key(|layout| layout.logical_id);
    layouts
}

fn allocate_index_layouts(
    mut indexes: Vec<PhysicalIndexState>,
    collection_layouts: &[PhysicalCollectionLayout],
    previous: Option<&Vec<PhysicalIndexState>>,
    now: u128,
) -> Vec<PhysicalIndexState> {
    let previous = previous.unwrap_or_else(|| {
        static EMPTY: Vec<PhysicalIndexState> = Vec::new();
        &EMPTY
    });
    let mut used_logical_ids: BTreeSet<u64> = previous
        .iter()
        .filter_map(|index| (index.logical_id > 0).then_some(index.logical_id))
        .collect();
    let mut used_physical_ids: BTreeSet<String> = previous
        .iter()
        .filter(|index| !index.physical_file_id.is_empty())
        .map(|index| index.physical_file_id.clone())
        .collect();

    for index in &mut indexes {
        if let Some(existing) = previous
            .iter()
            .find(|candidate| candidate.name == index.name)
        {
            index.logical_id = existing.logical_id;
            index.physical_file_id = existing.physical_file_id.clone();
            index.physical_file_name = existing.physical_file_name.clone();
            index.collection_logical_id = existing.collection_logical_id;
            continue;
        }
        let logical_id = allocate_compact_id(&mut used_logical_ids);
        let physical_file_id =
            allocate_physical_file_id("index", logical_id, &mut used_physical_ids);
        index.logical_id = logical_id;
        index.physical_file_name = format!("{physical_file_id}.rdi");
        index.physical_file_id = physical_file_id;
        index.collection_logical_id = index.collection.as_ref().and_then(|collection| {
            collection_layouts
                .iter()
                .find(|layout| layout.name == *collection)
                .map(|layout| layout.logical_id)
        });
        if index.last_refresh_ms.is_none() {
            index.last_refresh_ms = Some(now);
        }
    }
    indexes
}

fn allocate_compact_id(used: &mut BTreeSet<u64>) -> u64 {
    let mut candidate = 1;
    while used.contains(&candidate) {
        candidate += 1;
    }
    used.insert(candidate);
    candidate
}

fn allocate_physical_file_id(prefix: &str, logical_id: u64, used: &mut BTreeSet<String>) -> String {
    let mut salt = unix_ms_now() as u64;
    loop {
        let candidate = format!("{prefix}-{logical_id:016x}-{salt:016x}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
        salt = salt.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{CatalogSnapshot, CollectionStats, RedDBOptions};
    use crate::index::IndexKind;
    use std::time::SystemTime;

    fn catalog(names: &[&str]) -> CatalogSnapshot {
        CatalogSnapshot {
            name: "reddb".to_string(),
            total_entities: 0,
            total_collections: names.len(),
            stats_by_collection: names
                .iter()
                .map(|name| {
                    (
                        (*name).to_string(),
                        CollectionStats {
                            entities: 0,
                            cross_refs: 0,
                            segments: 0,
                        },
                    )
                })
                .collect(),
            updated_at: SystemTime::now(),
        }
    }

    fn index(name: &str, collection: &str) -> PhysicalIndexState {
        PhysicalIndexState {
            name: name.to_string(),
            logical_id: 0,
            physical_file_id: String::new(),
            physical_file_name: String::new(),
            collection_logical_id: None,
            kind: IndexKind::Hash,
            collection: Some(collection.to_string()),
            enabled: true,
            entries: 0,
            estimated_memory_bytes: 0,
            last_refresh_ms: None,
            backend: "hash".to_string(),
            artifact_kind: None,
            artifact_root_page: None,
            artifact_checksum: None,
            build_state: "ready".to_string(),
        }
    }

    #[test]
    fn collection_and_index_creation_allocate_stable_physical_layouts() {
        let metadata = PhysicalMetadataFile::from_state(
            RedDBOptions::default(),
            catalog(&["orders", "users"]),
            BTreeMap::new(),
            vec![index("orders::idx_customer", "orders")],
            None,
        );

        assert_eq!(metadata.collection_layouts.len(), 2);
        assert_eq!(metadata.collection_layouts[0].logical_id, 1);
        assert_eq!(metadata.collection_layouts[1].logical_id, 2);
        assert_ne!(
            metadata.collection_layouts[0].physical_file_id,
            metadata.collection_layouts[1].physical_file_id
        );

        let index = metadata.indexes.first().expect("index layout");
        let orders_id = metadata
            .collection_layouts
            .iter()
            .find(|layout| layout.name == "orders")
            .expect("orders layout")
            .logical_id;
        assert_eq!(index.logical_id, 1);
        assert_eq!(index.collection_logical_id, Some(orders_id));
        assert!(index.physical_file_name.ends_with(".rdi"));
    }

    #[test]
    fn rename_updates_human_metadata_without_moving_physical_files() {
        let mut metadata = PhysicalMetadataFile::from_state(
            RedDBOptions::default(),
            catalog(&["orders"]),
            BTreeMap::new(),
            vec![index("orders::idx_customer", "orders")],
            None,
        );
        let collection_file = metadata.collection_layouts[0].physical_file_id.clone();
        let index_file = metadata.indexes[0].physical_file_id.clone();

        assert!(metadata.rename_collection_layout("orders", "purchases"));
        assert!(metadata.rename_index_layout("orders::idx_customer", "purchases::idx_customer"));

        assert_eq!(metadata.collection_layouts[0].name, "purchases");
        assert_eq!(
            metadata.collection_layouts[0].physical_file_id,
            collection_file
        );
        assert_eq!(metadata.indexes[0].name, "purchases::idx_customer");
        assert_eq!(metadata.indexes[0].collection.as_deref(), Some("purchases"));
        assert_eq!(metadata.indexes[0].physical_file_id, index_file);
    }

    #[test]
    fn recovery_preserves_existing_identities_and_avoids_new_collisions() {
        let first = PhysicalMetadataFile::from_state(
            RedDBOptions::default(),
            catalog(&["orders"]),
            BTreeMap::new(),
            vec![index("orders::idx_customer", "orders")],
            None,
        );
        let recovered = PhysicalMetadataFile::from_state(
            RedDBOptions::default(),
            catalog(&["orders", "users"]),
            BTreeMap::new(),
            vec![
                index("orders::idx_customer", "orders"),
                index("users::idx_email", "users"),
            ],
            Some(&first),
        );

        let first_orders = &first.collection_layouts[0];
        let recovered_orders = recovered
            .collection_layouts
            .iter()
            .find(|layout| layout.name == "orders")
            .expect("orders recovered");
        assert_eq!(recovered_orders.logical_id, first_orders.logical_id);
        assert_eq!(
            recovered_orders.physical_file_id,
            first_orders.physical_file_id
        );

        let ids: BTreeSet<_> = recovered
            .collection_layouts
            .iter()
            .map(|layout| layout.physical_file_id.clone())
            .chain(
                recovered
                    .indexes
                    .iter()
                    .map(|index| index.physical_file_id.clone()),
            )
            .collect();
        assert_eq!(
            ids.len(),
            recovered.collection_layouts.len() + recovered.indexes.len()
        );
    }
}
