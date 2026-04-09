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
            collection_ttl_defaults_ms: previous
                .map(|previous| previous.collection_ttl_defaults_ms.clone())
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
        if binary_path.exists() {
            let sequence = Self::load_from_binary_path(&binary_path)
                .map(|metadata| metadata.superblock.sequence)
                .unwrap_or(self.superblock.sequence);
            let journal_path = Self::metadata_journal_path_for(data_path, sequence);
            let _ = fs::copy(&binary_path, journal_path);
        }
        self.save_to_binary_path(&binary_path)?;
        self.prune_journal_for_data_path(data_path)?;
        let json_path = Self::metadata_path_for(data_path);
        self.save_to_path(&json_path)?;
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
        let mut paths = Self::journal_paths_for_data_path(data_path)?;
        if paths.len() <= DEFAULT_METADATA_JOURNAL_RETENTION {
            return Ok(());
        }
        let delete_count = paths.len() - DEFAULT_METADATA_JOURNAL_RETENTION;
        for path in paths.drain(0..delete_count) {
            let _ = fs::remove_file(path);
        }
        Ok(())
    }

    pub fn heal_primary_metadata_for_data_path(&self, data_path: &Path) -> io::Result<()> {
        let binary_path = Self::metadata_binary_path_for(data_path);
        self.save_to_binary_path(&binary_path)?;
        let json_path = Self::metadata_path_for(data_path);
        self.save_to_path(&json_path)?;
        Ok(())
    }

    pub fn mark_recovery(&self, source: PhysicalMetadataSource) -> Self {
        let mut metadata = self.clone();
        metadata.last_loaded_from = Some(source.as_str().to_string());
        metadata.last_healed_at_unix_ms = Some(unix_ms_now());
        metadata
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
            "collection_ttl_defaults_ms".to_string(),
            JsonValue::Object(
                self.collection_ttl_defaults_ms
                    .iter()
                    .map(|(collection, ttl_ms)| (collection.clone(), json_u64(*ttl_ms)))
                    .collect(),
            ),
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
