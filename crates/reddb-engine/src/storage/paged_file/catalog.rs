//! Canonical physical metadata document and collection contract codecs.
use super::*;
use reddb_file::physical_metadata as physical;

impl<F: VfsFile> PagedFileStore<F> {
    pub(super) fn contract_document(
        &self,
        extra: Option<&CollectionContract>,
        now_ms: u64,
    ) -> io::Result<String> {
        let contracts: Vec<_> = self.contracts().chain(extra).collect();
        let manifest = physical::PhysicalSchemaManifest {
            format_version: physical::DEFAULT_PHYSICAL_FORMAT_VERSION,
            created_at_unix_ms: u128::from(now_ms),
            updated_at_unix_ms: u128::from(now_ms),
            collection_count: contracts.len(),
            options: physical::PhysicalSchemaOptions {
                mode: "persistent".into(),
                data_path: None,
                read_only: false,
                create_if_missing: false,
                verify_checksums: true,
                durability_mode: None,
                group_commit_window_ms: None,
                group_commit_max_statements: None,
                group_commit_max_wal_bytes: None,
                auto_checkpoint_pages: 0,
                cache_pages: self.cache_pages(),
                snapshot_retention: None,
                export_retention: None,
                force_create: false,
                capabilities: Vec::new(),
                metadata: BTreeMap::new(),
            },
        };
        let catalog = physical::PhysicalCatalogSnapshot {
            name: "database".into(),
            total_entities: usize::try_from(self.count()?).map_err(error)?,
            total_collections: contracts.len(),
            updated_at_unix_ms: u128::from(now_ms),
            stats_by_collection: BTreeMap::new(),
        };
        let envelope = physical::PhysicalMetadataDocumentEnvelope {
            protocol_version: physical::PHYSICAL_METADATA_PROTOCOL_VERSION.into(),
            generated_at_unix_ms: u128::from(now_ms),
            manifest_json: physical::encode_physical_schema_manifest_json(&manifest)
                .map_err(error)?,
            catalog_json: physical::encode_physical_catalog_snapshot_json(&catalog)
                .map_err(error)?,
            collection_contracts_json: contracts
                .into_iter()
                .map(|contract| {
                    crate::catalog::codec::collection_contract_to_json(contract).to_string_compact()
                })
                .collect(),
            superblock_json: physical::encode_physical_superblock_json(
                &physical::SuperblockHeader::default(),
            )
            .map_err(error)?,
            ..Default::default()
        };
        physical::encode_physical_metadata_document_root_json(&envelope, false).map_err(error)
    }
}
