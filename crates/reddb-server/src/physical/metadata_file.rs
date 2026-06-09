use super::*;
use crate::api::REDDB_FORMAT_VERSION;

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
            tree_definitions: previous
                .map(|previous| previous.tree_definitions.clone())
                .unwrap_or_default(),
            collection_ttl_defaults_ms: previous
                .map(|previous| previous.collection_ttl_defaults_ms.clone())
                .unwrap_or_default(),
            collection_contracts: previous
                .map(|previous| previous.collection_contracts.clone())
                .unwrap_or_default(),
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
        reddb_file::layout::physical_metadata_json_path(data_path)
    }

    pub fn metadata_binary_path_for(data_path: &Path) -> PathBuf {
        reddb_file::layout::physical_metadata_binary_path(data_path)
    }

    pub fn metadata_journal_path_for(data_path: &Path, sequence: u64) -> PathBuf {
        reddb_file::layout::physical_metadata_journal_path(data_path, sequence)
    }

    pub fn export_data_path_for(data_path: &Path, name: &str) -> PathBuf {
        reddb_file::layout::physical_export_data_path(data_path, name)
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
            let _ = reddb_file::copy_physical_metadata_binary_to_journal(
                data_path,
                &binary_path,
                sequence,
            );
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
        let text = reddb_file::read_physical_metadata_document(path).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid physical metadata JSON: {err}"),
            )
        })?;
        Self::from_document_json(&text, "invalid physical metadata JSON")
    }

    pub fn save_to_path(&self, path: &Path) -> io::Result<()> {
        let text = self.to_document_json(true)?;
        reddb_file::write_physical_metadata_json_document(path, &text)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))
    }

    pub fn load_from_binary_path(path: &Path) -> io::Result<Self> {
        let text = reddb_file::read_physical_metadata_document(path).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid physical metadata binary: {err}"),
            )
        })?;
        Self::from_document_json(&text, "invalid physical metadata binary")
    }

    pub fn save_to_binary_path(&self, path: &Path) -> io::Result<()> {
        let text = self.to_document_json(false)?;
        reddb_file::write_physical_metadata_binary_document(path, &text)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))
    }

    pub fn journal_paths_for_data_path(data_path: &Path) -> io::Result<Vec<PathBuf>> {
        reddb_file::list_physical_metadata_journal_paths(data_path)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))
    }

    pub fn prune_journal_for_data_path(&self, data_path: &Path) -> io::Result<()> {
        let retention = super::seqn_journal_retention();
        reddb_file::prune_physical_metadata_journal_paths(data_path, retention)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))
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

    pub fn to_json_value(&self) -> JsonValue {
        let json = self
            .to_document_json(false)
            .expect("physical metadata must encode as JSON");
        crate::json::from_str::<JsonValue>(&json)
            .expect("reddb-file emitted JSON the server can parse")
    }

    pub fn from_json_value(value: &JsonValue) -> io::Result<Self> {
        Self::from_document_json(&value.to_string_compact(), "invalid physical metadata JSON")
    }

    fn to_document_json(&self, pretty: bool) -> io::Result<String> {
        reddb_file::encode_physical_metadata_document_root_json(
            &self.to_document_envelope(),
            pretty,
        )
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))
    }

    fn from_document_json(text: &str, context: &'static str) -> io::Result<Self> {
        let envelope =
            reddb_file::decode_physical_metadata_document_root_json(text).map_err(|err| {
                io::Error::new(io::ErrorKind::InvalidData, format!("{context}: {err}"))
            })?;
        Self::from_document_envelope(envelope)
    }

    fn to_document_envelope(&self) -> reddb_file::PhysicalMetadataDocumentEnvelope {
        reddb_file::PhysicalMetadataDocumentEnvelope {
            protocol_version: self.protocol_version.clone(),
            generated_at_unix_ms: self.generated_at_unix_ms,
            last_loaded_from: self.last_loaded_from.clone(),
            last_healed_at_unix_ms: self.last_healed_at_unix_ms,
            manifest_json: manifest_to_json(&self.manifest).to_string_compact(),
            catalog_json: catalog_to_json(&self.catalog).to_string_compact(),
            manifest_events_json: self
                .manifest_events
                .iter()
                .map(|event| manifest_event_to_json(event).to_string_compact())
                .collect(),
            indexes_json: self
                .indexes
                .iter()
                .map(|index| index_state_to_json(index).to_string_compact())
                .collect(),
            graph_projections_json: self
                .graph_projections
                .iter()
                .map(|projection| graph_projection_to_json(projection).to_string_compact())
                .collect(),
            analytics_jobs_json: self
                .analytics_jobs
                .iter()
                .map(|job| analytics_job_to_json(job).to_string_compact())
                .collect(),
            tree_definitions_json: self
                .tree_definitions
                .iter()
                .map(|definition| tree_definition_to_json(definition).to_string_compact())
                .collect(),
            collection_ttl_defaults_ms: self.collection_ttl_defaults_ms.clone(),
            collection_contracts_json: self
                .collection_contracts
                .iter()
                .map(|contract| collection_contract_to_json(contract).to_string_compact())
                .collect(),
            hypertables_json: self
                .hypertables
                .iter()
                .map(|hypertable| hypertable_to_json(hypertable).to_string_compact())
                .collect(),
            exports_json: self
                .exports
                .iter()
                .map(|export| export_descriptor_to_json(export).to_string_compact())
                .collect(),
            superblock_json: superblock_to_json(&self.superblock).to_string_compact(),
            snapshots_json: self
                .snapshots
                .iter()
                .map(|snapshot| snapshot_descriptor_to_json(snapshot).to_string_compact())
                .collect(),
        }
    }

    fn from_document_envelope(
        envelope: reddb_file::PhysicalMetadataDocumentEnvelope,
    ) -> io::Result<Self> {
        Ok(Self {
            protocol_version: envelope.protocol_version,
            generated_at_unix_ms: envelope.generated_at_unix_ms,
            last_loaded_from: envelope.last_loaded_from,
            last_healed_at_unix_ms: envelope.last_healed_at_unix_ms,
            manifest: manifest_from_json(&parse_document_fragment(
                &envelope.manifest_json,
                "manifest",
            )?)?,
            catalog: catalog_from_json(&parse_document_fragment(
                &envelope.catalog_json,
                "catalog",
            )?)?,
            manifest_events: parse_document_fragments(
                &envelope.manifest_events_json,
                "manifest_events",
                manifest_event_from_json,
            )?,
            indexes: parse_document_fragments(
                &envelope.indexes_json,
                "indexes",
                index_state_from_json,
            )?,
            graph_projections: parse_document_fragments(
                &envelope.graph_projections_json,
                "graph_projections",
                graph_projection_from_json,
            )?,
            analytics_jobs: parse_document_fragments(
                &envelope.analytics_jobs_json,
                "analytics_jobs",
                analytics_job_from_json,
            )?,
            tree_definitions: parse_document_fragments(
                &envelope.tree_definitions_json,
                "tree_definitions",
                tree_definition_from_json,
            )?,
            collection_ttl_defaults_ms: envelope.collection_ttl_defaults_ms,
            collection_contracts: parse_document_fragments(
                &envelope.collection_contracts_json,
                "collection_contracts",
                collection_contract_from_json,
            )?,
            hypertables: parse_document_fragments(
                &envelope.hypertables_json,
                "hypertables",
                hypertable_from_json,
            )?,
            exports: parse_document_fragments(
                &envelope.exports_json,
                "exports",
                export_descriptor_from_json,
            )?,
            superblock: superblock_from_json(&parse_document_fragment(
                &envelope.superblock_json,
                "superblock",
            )?)?,
            snapshots: parse_document_fragments(
                &envelope.snapshots_json,
                "snapshots",
                snapshot_descriptor_from_json,
            )?,
        })
    }
}

fn parse_document_fragment(json: &str, field: &'static str) -> io::Result<JsonValue> {
    crate::json::from_str::<JsonValue>(json)
        .map_err(|err| invalid_data(format!("invalid physical metadata field '{field}': {err}")))
}

fn parse_document_fragments<T>(
    fragments: &[String],
    field: &'static str,
    decode: fn(&JsonValue) -> io::Result<T>,
) -> io::Result<Vec<T>> {
    fragments
        .iter()
        .map(|fragment| decode(&parse_document_fragment(fragment, field)?))
        .collect()
}
