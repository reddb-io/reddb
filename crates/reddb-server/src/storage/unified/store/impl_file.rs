use super::*;

impl UnifiedStore {
    pub fn new() -> Self {
        Self::with_config(UnifiedStoreConfig::default())
    }

    /// Get the current storage format version
    pub fn format_version(&self) -> u32 {
        self.format_version.load(Ordering::SeqCst)
    }

    pub(crate) fn set_format_version(&self, version: u32) {
        self.format_version.store(version, Ordering::SeqCst);
    }

    /// Allocate a global entity ID
    pub fn next_entity_id(&self) -> EntityId {
        EntityId::new(self.next_entity_id.fetch_add(1, Ordering::SeqCst))
    }

    /// Reserve `n` contiguous global entity IDs with one fetch_add.
    /// Caller assigns `id = EntityId::new(start + i)` per entity.
    pub fn reserve_entity_ids(&self, n: u64) -> std::ops::Range<u64> {
        let start = self.next_entity_id.fetch_add(n, Ordering::SeqCst);
        start..start + n
    }

    pub(crate) fn register_entity_id(&self, id: EntityId) {
        let candidate = id.raw().saturating_add(1);
        let mut current = self.next_entity_id.load(Ordering::SeqCst);
        while candidate > current {
            match self.next_entity_id.compare_exchange(
                current,
                candidate,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(updated) => current = updated,
            }
        }
    }

    /// Load a persisted image and rebuild its derived indexes, retaining the
    /// stored sequence instead of assigning one from snapshot iteration order.
    fn restore_snapshot_entity(
        &self,
        collection: &str,
        mut entity: UnifiedEntity,
    ) -> Result<EntityId, StoreError> {
        let manager = self.get_or_create_collection(collection);
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
        entity.ensure_table_logical_id();
        self.context_index.index_entity(collection, &entity);
        if self.config.auto_index_refs {
            self.index_cross_refs(&entity, collection)?;
        }
        let graph_label = match &entity.kind {
            EntityKind::GraphNode(node) => Some(node.label.clone()),
            _ => None,
        };
        let id = manager.restore_snapshot_entity(entity)?;
        if let Some(label) = graph_label {
            self.update_graph_label_index(collection, &label, id);
        }
        Ok(id)
    }

    /// Load store from binary file
    ///
    /// Binary dump framing is defined by `reddb_file::native_store`.
    ///
    /// Store payload shape:
    /// ```text
    /// [collection_count: varu32]
    /// [collections...]
    /// [cross_ref_count: varu32]
    /// [cross_refs...]
    /// ```
    pub fn load_from_file(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;
        Self::load_from_bytes(&buf)
    }

    pub(crate) fn load_from_bytes(buf: &[u8]) -> Result<Self, Box<dyn std::error::Error>> {
        Self::load_from_bytes_with_config(buf, UnifiedStoreConfig::default())
    }

    pub(crate) fn load_from_bytes_with_config(
        buf: &[u8],
        config: UnifiedStoreConfig,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        use reddb_engine::storage::snapshot::{visit_snapshot, SnapshotVisitor};

        struct Restore<'a>(&'a UnifiedStore);
        impl SnapshotVisitor for Restore<'_> {
            fn collection(&mut self, name: &str) -> Result<(), String> {
                self.0.get_or_create_collection(name);
                Ok(())
            }

            fn entity(
                &mut self,
                collection: &str,
                entity: UnifiedEntity,
                metadata: Option<Metadata>,
            ) -> Result<(), String> {
                let id = entity.id;
                self.0
                    .restore_snapshot_entity(collection, entity)
                    .map_err(|err| err.to_string())?;
                if let Some(metadata) = metadata {
                    if let Some(manager) = self.0.get_collection(collection) {
                        let _ = manager.set_metadata(id, metadata);
                    }
                }
                Ok(())
            }

            fn cross_ref(
                &mut self,
                cross_ref: reddb_file::NativeDumpCrossRef,
            ) -> Result<(), String> {
                let source_collection = self
                    .0
                    .get_any(EntityId::new(cross_ref.source_id))
                    .map(|(name, _)| name)
                    .unwrap_or_else(|| cross_ref.target_collection.clone());
                let _ = self.0.add_cross_ref(
                    &source_collection,
                    EntityId::new(cross_ref.source_id),
                    &cross_ref.target_collection,
                    EntityId::new(cross_ref.target_id),
                    RefType::from_byte(cross_ref.ref_type),
                    1.0,
                );
                Ok(())
            }

            fn auxiliary(&mut self, bytes: &[u8]) -> Result<(), String> {
                self.0.set_aux_metadata(bytes.to_vec());
                Ok(())
            }
        }

        let version = reddb_file::decode_native_store_header(buf).map_err(|err| err.to_string())?;
        let mut store = Self::with_config(config);
        // Recovery is private until complete and must not append old images to WAL.
        let embedded_wal_path = store.config.embedded_wal_path.take();
        store.set_format_version(version);
        visit_snapshot(buf, &mut Restore(&store))?;

        if store.format_version() < STORE_VERSION_CURRENT {
            store.set_format_version(STORE_VERSION_CURRENT);
        }
        store.config.embedded_wal_path = embedded_wal_path;

        Ok(store)
    }

    /// Save store to binary file
    ///
    /// Uses compact binary encoding with varint for efficient storage.
    /// No JSON - pure binary with pages and indices.
    pub fn save_to_file(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let buf = self.to_binary_dump_bytes();
        reddb_file::write_native_store_bytes_atomically(path, &buf)?;
        Ok(())
    }

    pub(crate) fn to_binary_dump_bytes(&self) -> Vec<u8> {
        let _vault_guard = self.vault_publication_lock.lock();
        let pending_vault_versions = self.pending_vault_versions.read();
        let (_, excluded) = self.snapshot_manager.get().map_or_else(
            || (0, Default::default()),
            |manager| manager.checkpoint_excluded_state(),
        );
        let mut buf = Vec::new();

        // Version 9 includes explicit table-row logical identity plus MVCC
        // xmin/xmax alongside the V7 metadata envelope.
        buf.extend_from_slice(&reddb_file::encode_native_store_header(
            STORE_VERSION_CURRENT,
        ));

        // Get all collections
        let collections = self.collections.read();
        reddb_file::encode_native_dump_count(&mut buf, collections.len() as u32);

        let fv = STORE_VERSION_CURRENT;
        for (name, manager) in collections.iter() {
            // Get all entities from this collection
            let entities = manager.query_all(|entity| {
                if pending_vault_versions.contains(&entity.id) {
                    return false;
                }
                !matches!(entity.data, EntityData::Row(_) | EntityData::Vector(_))
                    || !excluded.contains(&entity.xmin)
            });
            reddb_file::encode_native_dump_collection_header(&mut buf, name, entities.len() as u32);

            // V7+: serialize entity+metadata as a length-prefixed record.
            // Each record: [u32 len][serialize_entity_record bytes]
            for mut entity in entities {
                // Unfinished and rolled-back deletions cannot become committed
                // tombstones when recovery reconstructs the xid floor.
                if matches!(entity.data, EntityData::Row(_)) && excluded.contains(&entity.xmax) {
                    entity.xmax = 0;
                }
                let metadata = manager.get_metadata(entity.id);
                let record = Self::serialize_entity_record(&entity, metadata.as_ref(), fv);
                reddb_file::encode_native_dump_entity_record(&mut buf, &record);
            }
        }

        // Write cross-references
        let cross_refs = self.cross_refs.read();
        let total_refs: usize = cross_refs.values().map(|v| v.len()).sum();
        reddb_file::encode_native_dump_count(&mut buf, total_refs as u32);

        for (source_id, refs) in cross_refs.iter() {
            for (target_id, ref_type, collection) in refs {
                reddb_file::encode_native_dump_cross_ref(
                    &mut buf,
                    source_id.raw(),
                    target_id.raw(),
                    ref_type.to_byte(),
                    collection,
                );
            }
        }

        // V10+: trailing opaque auxiliary-metadata blob (collection contracts
        // for the single-file artifact). Always written — empty when unused —
        // so the read side can unconditionally expect it for V10 dumps.
        {
            let aux = self.aux_metadata.read();
            reddb_file::encode_native_dump_entity_record(&mut buf, &aux);
        }

        self.set_format_version(STORE_VERSION_CURRENT);

        reddb_file::append_native_store_crc32_footer(&mut buf);
        buf
    }

    /// Read entity using the canonical native/browser logical codec.
    pub(crate) fn read_entity_binary(
        buf: &[u8],
        pos: &mut usize,
        format_version: u32,
    ) -> Result<UnifiedEntity, Box<dyn std::error::Error>> {
        Ok(reddb_engine::storage::entity_codec::read_entity_binary(
            buf,
            pos,
            format_version,
        )?)
    }

    /// Write entity using the canonical native/browser logical codec.
    pub(crate) fn write_entity_binary(
        buf: &mut Vec<u8>,
        entity: &UnifiedEntity,
        format_version: u32,
    ) {
        reddb_engine::storage::entity_codec::write_entity_binary(buf, entity, format_version);
    }
}

#[cfg(test)]
mod aux_metadata_dump_tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn snapshot_load_preserves_existing_wal_and_restores_future_appends() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("snapshot.rdb");
        let store = UnifiedStore::with_config(UnifiedStoreConfig::default());
        let old =
            UnifiedEntity::table_row(EntityId::new(1), "rows", 1, vec![Value::text("snapshot")]);
        store.insert_auto("rows", old).expect("snapshot row");
        let snapshot = store.to_binary_dump_bytes();
        crate::storage::EmbeddedRdbArtifact::create_with_snapshot(&path, &snapshot)
            .expect("artifact");
        let newer = UnifiedEntity::table_row(
            EntityId::new(1),
            "rows",
            1,
            vec![Value::text("durable WAL")],
        );
        let action = StoreWalAction::upsert_entity("rows", &newer, None, STORE_VERSION_CURRENT);
        UnifiedStore::with_config(UnifiedStoreConfig::default().with_embedded_wal_path(&path))
            .finish_paged_write([action])
            .expect("durable update after snapshot");
        let before = std::fs::read(&path).expect("original artifact bytes");
        let loaded = UnifiedStore::load_from_bytes_with_config(
            &snapshot,
            UnifiedStoreConfig::default().with_embedded_wal_path(&path),
        )
        .expect("reconstruct snapshot");
        assert_eq!(std::fs::read(&path).expect("artifact after load"), before);
        assert_eq!(
            loaded.config().embedded_wal_path.as_deref(),
            Some(path.as_path())
        );

        let generation = crate::storage::EmbeddedRdbArtifact::open(&path)
            .expect("boundary before future write")
            .selected_superblock
            .generation;
        loaded
            .insert_auto(
                "rows",
                UnifiedEntity::table_row(
                    EntityId::new(2),
                    "rows",
                    2,
                    vec![Value::text("future write")],
                ),
            )
            .expect("future writes still append WAL");
        assert_eq!(
            crate::storage::EmbeddedRdbArtifact::open(&path)
                .expect("boundary after future write")
                .selected_superblock
                .generation,
            generation + 1
        );
    }

    #[test]
    fn aux_metadata_round_trips_through_binary_dump() {
        let store = UnifiedStore::with_config(UnifiedStoreConfig::default());
        store.create_collection("k").expect("create collection");
        store.set_aux_metadata(b"contract-blob".to_vec());

        let bytes = store.to_binary_dump_bytes();
        let reloaded = UnifiedStore::load_from_bytes(&bytes).expect("reload dump");

        assert_eq!(reloaded.aux_metadata(), b"contract-blob".to_vec());
    }

    #[test]
    fn empty_aux_metadata_round_trips_as_empty() {
        let store = UnifiedStore::with_config(UnifiedStoreConfig::default());
        let bytes = store.to_binary_dump_bytes();
        let reloaded = UnifiedStore::load_from_bytes(&bytes).expect("reload dump");

        assert!(reloaded.aux_metadata().is_empty());
    }

    #[test]
    fn interned_timeseries_series_reduces_repeated_tag_storage() {
        let tags = HashMap::from([
            (
                "host".to_string(),
                "srv-very-long-hostname-0001".to_string(),
            ),
            ("region".to_string(), "us-east-1".to_string()),
            ("service".to_string(), "checkout-api".to_string()),
        ]);
        let inline_bytes: usize = (0..100)
            .map(|timestamp_ns| {
                let entity = UnifiedEntity::new(
                    EntityId::new(0),
                    EntityKind::TimeSeriesPoint(Box::new(TimeSeriesPointKind {
                        series: "cpu_metrics".to_string(),
                        metric: "cpu.idle".to_string(),
                    })),
                    EntityData::TimeSeries(crate::storage::TimeSeriesData {
                        metric: "cpu.idle".to_string(),
                        series_id: None,
                        timestamp_ns,
                        value: 94.8,
                        tags: tags.clone(),
                        fields: HashMap::new(),
                    }),
                );
                UnifiedStore::serialize_entity_record(&entity, None, reddb_file::STORE_VERSION_V10)
                    .len()
            })
            .sum();

        let mut dictionary_fields = HashMap::new();
        dictionary_fields.insert("collection".to_string(), Value::text("cpu_metrics"));
        dictionary_fields.insert("series_id".to_string(), Value::UnsignedInteger(0));
        dictionary_fields.insert("metric".to_string(), Value::text("cpu.idle"));
        dictionary_fields.insert(
            "canonical_tags".to_string(),
            Value::text(
                r#"{"host":"srv-very-long-hostname-0001","region":"us-east-1","service":"checkout-api"}"#,
            ),
        );
        dictionary_fields.insert(
            "tags".to_string(),
            Value::Json(
                br#"{"host":"srv-very-long-hostname-0001","region":"us-east-1","service":"checkout-api"}"#
                    .to_vec(),
            ),
        );
        let dictionary = UnifiedEntity::new(
            EntityId::new(0),
            EntityKind::TableRow {
                table: Arc::from("red_timeseries_series"),
                row_id: 0,
            },
            EntityData::Row(RowData {
                columns: Vec::new(),
                named: Some(dictionary_fields),
                schema: None,
            }),
        );
        let dictionary_bytes =
            UnifiedStore::serialize_entity_record(&dictionary, None, STORE_VERSION_V11).len();
        let interned_bytes = dictionary_bytes
            + (0..100)
                .map(|timestamp_ns| {
                    let entity = UnifiedEntity::new(
                        EntityId::new(0),
                        EntityKind::TimeSeriesPoint(Box::new(TimeSeriesPointKind {
                            series: "cpu_metrics".to_string(),
                            metric: "cpu.idle".to_string(),
                        })),
                        EntityData::TimeSeries(crate::storage::TimeSeriesData {
                            metric: "cpu.idle".to_string(),
                            series_id: Some(0),
                            timestamp_ns,
                            value: 94.8,
                            tags: HashMap::new(),
                            fields: HashMap::new(),
                        }),
                    );
                    UnifiedStore::serialize_entity_record(&entity, None, STORE_VERSION_V11).len()
                })
                .sum::<usize>();

        assert!(
            interned_bytes < inline_bytes,
            "interned series storage should shrink repeated tags: inline={inline_bytes}, interned={interned_bytes}"
        );
    }
}
