//! Physical metadata document file contract.
//!
//! Runtime crates own the domain model that becomes physical metadata. This
//! module owns the persisted document envelope: JSON validation, byte encoding,
//! and path I/O for the JSON sidecar and compact binary sidecar.

use crate::{RdbFileError, RdbFileResult};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

mod json_helpers;
mod types;

use json_helpers::*;
pub use types::*;

pub fn encode_physical_metadata_document_root_json(
    document: &PhysicalMetadataDocumentEnvelope,
    pretty: bool,
) -> RdbFileResult<String> {
    let mut root = serde_json::Map::new();
    root.insert(
        "protocol_version".to_string(),
        serde_json::Value::String(document.protocol_version.clone()),
    );
    root.insert(
        "generated_at_unix_ms".to_string(),
        json_u128(document.generated_at_unix_ms),
    );
    root.insert(
        "last_loaded_from".to_string(),
        document
            .last_loaded_from
            .clone()
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null),
    );
    root.insert(
        "last_healed_at_unix_ms".to_string(),
        document
            .last_healed_at_unix_ms
            .map(json_u128)
            .unwrap_or(serde_json::Value::Null),
    );
    root.insert(
        "manifest".to_string(),
        parse_json_fragment("physical metadata manifest", &document.manifest_json)?,
    );
    root.insert(
        "catalog".to_string(),
        parse_json_fragment("physical metadata catalog", &document.catalog_json)?,
    );
    root.insert(
        "manifest_events".to_string(),
        parse_json_fragment_array(
            "physical metadata manifest event",
            &document.manifest_events_json,
        )?,
    );
    root.insert(
        "indexes".to_string(),
        parse_json_fragment_array("physical metadata index", &document.indexes_json)?,
    );
    root.insert(
        "graph_projections".to_string(),
        parse_json_fragment_array(
            "physical metadata graph projection",
            &document.graph_projections_json,
        )?,
    );
    root.insert(
        "analytics_jobs".to_string(),
        parse_json_fragment_array(
            "physical metadata analytics job",
            &document.analytics_jobs_json,
        )?,
    );
    root.insert(
        "tree_definitions".to_string(),
        parse_json_fragment_array(
            "physical metadata tree definition",
            &document.tree_definitions_json,
        )?,
    );
    root.insert(
        "collection_ttl_defaults_ms".to_string(),
        serde_json::Value::Object(
            document
                .collection_ttl_defaults_ms
                .iter()
                .map(|(collection, ttl_ms)| (collection.clone(), json_u64(*ttl_ms)))
                .collect(),
        ),
    );
    root.insert(
        "collection_contracts".to_string(),
        parse_json_fragment_array(
            "physical metadata collection contract",
            &document.collection_contracts_json,
        )?,
    );
    root.insert(
        "hypertables".to_string(),
        parse_json_fragment_array("physical metadata hypertable", &document.hypertables_json)?,
    );
    root.insert(
        "exports".to_string(),
        parse_json_fragment_array("physical metadata export", &document.exports_json)?,
    );
    root.insert(
        "superblock".to_string(),
        parse_json_fragment("physical metadata superblock", &document.superblock_json)?,
    );
    root.insert(
        "snapshots".to_string(),
        parse_json_fragment_array("physical metadata snapshot", &document.snapshots_json)?,
    );

    let value = serde_json::Value::Object(root);
    if pretty {
        serde_json::to_string_pretty(&value)
    } else {
        serde_json::to_string(&value)
    }
    .map_err(|err| invalid(format!("encode physical metadata document root: {err}")))
}

pub fn decode_physical_metadata_document_root_json(
    json: &str,
) -> RdbFileResult<PhysicalMetadataDocumentEnvelope> {
    let value = parse_json_value(json, "physical metadata document root")?;
    let root = expect_object(&value, "physical metadata root")?;
    Ok(PhysicalMetadataDocumentEnvelope {
        protocol_version: json_string_required(root, "protocol_version")?,
        generated_at_unix_ms: json_u128_required(root, "generated_at_unix_ms")?,
        last_loaded_from: root
            .get("last_loaded_from")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
        last_healed_at_unix_ms: root
            .get("last_healed_at_unix_ms")
            .filter(|value| !value.is_null())
            .map(json_u128_value)
            .transpose()?,
        manifest_json: required_json_fragment(root, "manifest")?,
        catalog_json: required_json_fragment(root, "catalog")?,
        manifest_events_json: optional_json_fragment_array(root, "manifest_events")?,
        indexes_json: optional_json_fragment_array(root, "indexes")?,
        graph_projections_json: optional_json_fragment_array(root, "graph_projections")?,
        analytics_jobs_json: optional_json_fragment_array(root, "analytics_jobs")?,
        tree_definitions_json: optional_json_fragment_array(root, "tree_definitions")?,
        collection_ttl_defaults_ms: optional_u64_map(root, "collection_ttl_defaults_ms")?,
        collection_contracts_json: optional_json_fragment_array(root, "collection_contracts")?,
        hypertables_json: optional_json_fragment_array(root, "hypertables")?,
        exports_json: optional_json_fragment_array(root, "exports")?,
        superblock_json: required_json_fragment(root, "superblock")?,
        snapshots_json: required_json_fragment_array(root, "snapshots")?,
    })
}

pub fn encode_physical_schema_manifest_json(
    manifest: &PhysicalSchemaManifest,
) -> RdbFileResult<String> {
    Ok(schema_manifest_json_value(manifest).to_string())
}

pub fn decode_physical_schema_manifest_json(json: &str) -> RdbFileResult<PhysicalSchemaManifest> {
    let value = parse_json_value(json, "physical schema manifest")?;
    schema_manifest_from_json_value(&value)
}

pub fn encode_physical_catalog_snapshot_json(
    catalog: &PhysicalCatalogSnapshot,
) -> RdbFileResult<String> {
    Ok(catalog_snapshot_json_value(catalog).to_string())
}

pub fn decode_physical_catalog_snapshot_json(json: &str) -> RdbFileResult<PhysicalCatalogSnapshot> {
    let value = parse_json_value(json, "physical catalog snapshot")?;
    catalog_snapshot_from_json_value(&value)
}

pub fn encode_physical_analytical_storage_json(
    config: &PhysicalAnalyticalStorageConfig,
) -> RdbFileResult<String> {
    Ok(analytical_storage_json_value(config).to_string())
}

pub fn decode_physical_analytical_storage_json(
    json: &str,
) -> RdbFileResult<PhysicalAnalyticalStorageConfig> {
    let value = parse_json_value(json, "physical analytical storage")?;
    analytical_storage_from_json_value(&value)
}

pub fn encode_physical_subscription_descriptor_json(
    subscription: &PhysicalSubscriptionDescriptor,
) -> RdbFileResult<String> {
    Ok(subscription_descriptor_json_value(subscription).to_string())
}

pub fn decode_physical_subscription_descriptor_json(
    json: &str,
) -> RdbFileResult<PhysicalSubscriptionDescriptor> {
    let value = parse_json_value(json, "physical subscription descriptor")?;
    subscription_descriptor_from_json_value(&value)
}

pub fn encode_physical_analytics_view_descriptor_json(
    view: &PhysicalAnalyticsViewDescriptor,
) -> RdbFileResult<String> {
    Ok(analytics_view_descriptor_json_value(view).to_string())
}

pub fn decode_physical_analytics_view_descriptor_json(
    json: &str,
) -> RdbFileResult<PhysicalAnalyticsViewDescriptor> {
    let value = parse_json_value(json, "physical analytics view descriptor")?;
    analytics_view_descriptor_from_json_value(&value)
}

pub fn encode_physical_declared_column_contract_json(
    column: &PhysicalDeclaredColumnContract,
) -> RdbFileResult<String> {
    Ok(declared_column_contract_json_value(column).to_string())
}

pub fn decode_physical_declared_column_contract_json(
    json: &str,
) -> RdbFileResult<PhysicalDeclaredColumnContract> {
    let value = parse_json_value(json, "physical declared column contract")?;
    declared_column_contract_from_json_value(&value)
}

pub fn encode_physical_collection_contract_json(
    contract: &PhysicalCollectionContract,
) -> RdbFileResult<String> {
    Ok(collection_contract_json_value(contract).to_string())
}

pub fn decode_physical_collection_contract_json(
    json: &str,
) -> RdbFileResult<PhysicalCollectionContract> {
    let value = parse_json_value(json, "physical collection contract")?;
    collection_contract_from_json_value(&value)
}

pub fn encode_physical_superblock_json(superblock: &SuperblockHeader) -> RdbFileResult<String> {
    Ok(superblock_json_value(superblock).to_string())
}

pub fn decode_physical_superblock_json(json: &str) -> RdbFileResult<SuperblockHeader> {
    let value = parse_json_value(json, "physical superblock")?;
    superblock_from_json_value(&value)
}

pub fn encode_physical_manifest_event_json(event: &ManifestEvent) -> RdbFileResult<String> {
    Ok(manifest_event_json_value(event).to_string())
}

pub fn decode_physical_manifest_event_json(json: &str) -> RdbFileResult<ManifestEvent> {
    let value = parse_json_value(json, "physical manifest event")?;
    manifest_event_from_json_value(&value)
}

pub fn encode_physical_manifest_pointers_json(
    pointers: &ManifestPointers,
) -> RdbFileResult<String> {
    Ok(manifest_pointers_json_value(pointers).to_string())
}

pub fn decode_physical_manifest_pointers_json(json: &str) -> RdbFileResult<ManifestPointers> {
    let value = parse_json_value(json, "physical manifest pointers")?;
    manifest_pointers_from_json_value(&value)
}

pub fn encode_physical_block_reference_json(reference: BlockReference) -> RdbFileResult<String> {
    Ok(block_reference_json_value(reference).to_string())
}

pub fn decode_physical_block_reference_json(json: &str) -> RdbFileResult<BlockReference> {
    let value = parse_json_value(json, "physical block reference")?;
    block_reference_from_json_value(&value)
}

pub fn encode_physical_snapshot_descriptor_json(
    snapshot: &SnapshotDescriptor,
) -> RdbFileResult<String> {
    Ok(snapshot_descriptor_json_value(snapshot).to_string())
}

pub fn decode_physical_snapshot_descriptor_json(json: &str) -> RdbFileResult<SnapshotDescriptor> {
    let value = parse_json_value(json, "physical snapshot descriptor")?;
    snapshot_descriptor_from_json_value(&value)
}

pub fn encode_physical_export_descriptor_json(export: &ExportDescriptor) -> RdbFileResult<String> {
    Ok(export_descriptor_json_value(export).to_string())
}

pub fn decode_physical_export_descriptor_json(json: &str) -> RdbFileResult<ExportDescriptor> {
    let value = parse_json_value(json, "physical export descriptor")?;
    export_descriptor_from_json_value(&value)
}

pub fn encode_physical_graph_projection_json(
    projection: &PhysicalGraphProjection,
) -> RdbFileResult<String> {
    Ok(graph_projection_json_value(projection).to_string())
}

pub fn decode_physical_graph_projection_json(json: &str) -> RdbFileResult<PhysicalGraphProjection> {
    let value = parse_json_value(json, "physical graph projection")?;
    graph_projection_from_json_value(&value)
}

pub fn encode_physical_analytics_job_json(job: &PhysicalAnalyticsJob) -> RdbFileResult<String> {
    Ok(analytics_job_json_value(job).to_string())
}

pub fn decode_physical_analytics_job_json(json: &str) -> RdbFileResult<PhysicalAnalyticsJob> {
    let value = parse_json_value(json, "physical analytics job")?;
    analytics_job_from_json_value(&value)
}

pub fn encode_physical_tree_definition_json(
    definition: &PhysicalTreeDefinition,
) -> RdbFileResult<String> {
    Ok(tree_definition_json_value(definition).to_string())
}

pub fn decode_physical_tree_definition_json(json: &str) -> RdbFileResult<PhysicalTreeDefinition> {
    let value = parse_json_value(json, "physical tree definition")?;
    tree_definition_from_json_value(&value)
}

pub fn encode_persisted_physical_index_state_json(
    index: &PersistedPhysicalIndexState,
) -> RdbFileResult<String> {
    Ok(index_state_json_value(index).to_string())
}

pub fn decode_persisted_physical_index_state_json(
    json: &str,
) -> RdbFileResult<PersistedPhysicalIndexState> {
    let value = parse_json_value(json, "physical index state")?;
    index_state_from_json_value(&value)
}

pub fn encode_persisted_physical_hypertable_json(
    hypertable: &PersistedPhysicalHypertable,
) -> RdbFileResult<String> {
    Ok(hypertable_json_value(hypertable).to_string())
}

pub fn decode_persisted_physical_hypertable_json(
    json: &str,
) -> RdbFileResult<PersistedPhysicalHypertable> {
    let value = parse_json_value(json, "physical hypertable")?;
    hypertable_from_json_value(&value)
}

pub fn encode_persisted_physical_hypertable_chunk_json(
    chunk: &PersistedPhysicalHypertableChunk,
) -> RdbFileResult<String> {
    Ok(hypertable_chunk_json_value(chunk).to_string())
}

pub fn decode_persisted_physical_hypertable_chunk_json(
    json: &str,
) -> RdbFileResult<PersistedPhysicalHypertableChunk> {
    let value = parse_json_value(json, "physical hypertable chunk")?;
    hypertable_chunk_from_json_value(&value)
}

pub fn encode_physical_metadata_json_document(pretty_json: &str) -> RdbFileResult<Vec<u8>> {
    validate_json(pretty_json.as_bytes(), "physical metadata JSON")?;
    Ok(pretty_json.as_bytes().to_vec())
}

pub fn encode_physical_metadata_binary_document(compact_json: &str) -> RdbFileResult<Vec<u8>> {
    validate_json(compact_json.as_bytes(), "physical metadata binary")?;
    Ok(compact_json.as_bytes().to_vec())
}

pub fn decode_physical_metadata_document(bytes: &[u8]) -> RdbFileResult<String> {
    validate_json(bytes, "physical metadata document")?;
    String::from_utf8(bytes.to_vec()).map_err(|err| {
        RdbFileError::InvalidOperation(format!("physical metadata document is not UTF-8: {err}"))
    })
}

pub fn read_physical_metadata_document(path: &Path) -> RdbFileResult<String> {
    let bytes = fs::read(path)?;
    decode_physical_metadata_document(&bytes)
}

pub fn write_physical_metadata_json_document(path: &Path, pretty_json: &str) -> RdbFileResult<()> {
    let bytes = encode_physical_metadata_json_document(pretty_json)?;
    fs::write(path, bytes)?;
    Ok(())
}

pub fn write_physical_metadata_binary_document(
    path: &Path,
    compact_json: &str,
) -> RdbFileResult<()> {
    let bytes = encode_physical_metadata_binary_document(compact_json)?;
    fs::write(path, bytes)?;
    Ok(())
}

pub fn copy_physical_metadata_binary_to_journal(
    data_path: &Path,
    source_path: &Path,
    sequence: u64,
) -> RdbFileResult<PathBuf> {
    let journal_path = crate::layout::physical_metadata_journal_path(data_path, sequence);
    fs::copy(source_path, &journal_path)?;
    Ok(journal_path)
}

pub fn copy_physical_export_data_file(data_path: &Path, name: &str) -> RdbFileResult<PathBuf> {
    let export_data_path = crate::layout::physical_export_data_path(data_path, name);
    fs::copy(data_path, &export_data_path)?;
    Ok(export_data_path)
}

pub fn list_physical_metadata_journal_paths(data_path: &Path) -> RdbFileResult<Vec<PathBuf>> {
    let Some(parent) = data_path.parent() else {
        return Ok(Vec::new());
    };
    let prefix = crate::layout::physical_metadata_journal_prefix(data_path);

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

pub fn prune_physical_metadata_journal_paths(
    data_path: &Path,
    retention: usize,
) -> RdbFileResult<()> {
    let mut paths = list_physical_metadata_journal_paths(data_path)?;
    if paths.len() <= retention {
        return Ok(());
    }
    let delete_count = paths.len() - retention;
    for path in paths.drain(0..delete_count) {
        let _ = fs::remove_file(path);
    }
    Ok(())
}

fn validate_json(bytes: &[u8], label: &'static str) -> RdbFileResult<()> {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .map(|_| ())
        .map_err(|err| RdbFileError::InvalidOperation(format!("invalid {label}: {err}")))
}

fn superblock_json_value(superblock: &SuperblockHeader) -> serde_json::Value {
    let mut collection_roots = serde_json::Map::new();
    for (name, root) in &superblock.collection_roots {
        collection_roots.insert(name.clone(), json_u64(*root));
    }

    let mut object = serde_json::Map::new();
    object.insert(
        "format_version".to_string(),
        serde_json::Value::Number(superblock.format_version.into()),
    );
    object.insert("sequence".to_string(), json_u64(superblock.sequence));
    object.insert(
        "copies".to_string(),
        serde_json::Value::Number(superblock.copies.into()),
    );
    object.insert(
        "manifest".to_string(),
        manifest_pointers_json_value(&superblock.manifest),
    );
    object.insert(
        "free_set".to_string(),
        block_reference_json_value(superblock.free_set),
    );
    object.insert(
        "collection_roots".to_string(),
        serde_json::Value::Object(collection_roots),
    );
    serde_json::Value::Object(object)
}

fn superblock_from_json_value(value: &serde_json::Value) -> RdbFileResult<SuperblockHeader> {
    let object = expect_object(value, "superblock")?;
    let roots = expect_object(required(object, "collection_roots")?, "superblock.roots")?;
    let mut collection_roots = BTreeMap::new();
    for (name, root) in roots {
        collection_roots.insert(name.clone(), json_u64_value(root)?);
    }

    Ok(SuperblockHeader {
        format_version: json_u32_required(object, "format_version")?,
        sequence: json_u64_required(object, "sequence")?,
        copies: json_u8_required(object, "copies")?,
        manifest: manifest_pointers_from_json_value(required(object, "manifest")?)?,
        free_set: block_reference_from_json_value(required(object, "free_set")?)?,
        collection_roots,
    })
}

fn manifest_event_json_value(event: &ManifestEvent) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "collection".to_string(),
        serde_json::Value::String(event.collection.clone()),
    );
    object.insert(
        "object_key".to_string(),
        serde_json::Value::String(event.object_key.clone()),
    );
    object.insert(
        "kind".to_string(),
        serde_json::Value::String(manifest_event_kind_as_str(event.kind).to_string()),
    );
    object.insert("block".to_string(), block_reference_json_value(event.block));
    object.insert("snapshot_min".to_string(), json_u64(event.snapshot_min));
    object.insert(
        "snapshot_max".to_string(),
        match event.snapshot_max {
            Some(value) => json_u64(value),
            None => serde_json::Value::Null,
        },
    );
    serde_json::Value::Object(object)
}

fn manifest_event_from_json_value(value: &serde_json::Value) -> RdbFileResult<ManifestEvent> {
    let object = expect_object(value, "manifest event")?;
    Ok(ManifestEvent {
        collection: json_string_required(object, "collection")?,
        object_key: json_string_required(object, "object_key")?,
        kind: manifest_event_kind_from_str(&json_string_required(object, "kind")?)?,
        block: block_reference_from_json_value(required(object, "block")?)?,
        snapshot_min: json_u64_required(object, "snapshot_min")?,
        snapshot_max: object.get("snapshot_max").and_then(|value| {
            if value.is_null() {
                None
            } else {
                json_u64_value(value).ok()
            }
        }),
    })
}

fn manifest_pointers_json_value(pointers: &ManifestPointers) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "oldest".to_string(),
        block_reference_json_value(pointers.oldest),
    );
    object.insert(
        "newest".to_string(),
        block_reference_json_value(pointers.newest),
    );
    serde_json::Value::Object(object)
}

fn manifest_pointers_from_json_value(value: &serde_json::Value) -> RdbFileResult<ManifestPointers> {
    let object = expect_object(value, "manifest pointers")?;
    Ok(ManifestPointers {
        oldest: block_reference_from_json_value(required(object, "oldest")?)?,
        newest: block_reference_from_json_value(required(object, "newest")?)?,
    })
}

fn block_reference_json_value(reference: BlockReference) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert("index".to_string(), json_u64(reference.index));
    object.insert("checksum".to_string(), json_u128(reference.checksum));
    serde_json::Value::Object(object)
}

fn block_reference_from_json_value(value: &serde_json::Value) -> RdbFileResult<BlockReference> {
    let object = expect_object(value, "block reference")?;
    Ok(BlockReference {
        index: json_u64_required(object, "index")?,
        checksum: json_u128_required(object, "checksum")?,
    })
}

fn schema_manifest_json_value(manifest: &PhysicalSchemaManifest) -> serde_json::Value {
    let mut options = serde_json::Map::new();
    options.insert(
        "mode".to_string(),
        serde_json::Value::String(manifest.options.mode.clone()),
    );
    options.insert(
        "data_path".to_string(),
        manifest
            .options
            .data_path
            .clone()
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null),
    );
    options.insert(
        "read_only".to_string(),
        serde_json::Value::Bool(manifest.options.read_only),
    );
    options.insert(
        "create_if_missing".to_string(),
        serde_json::Value::Bool(manifest.options.create_if_missing),
    );
    options.insert(
        "verify_checksums".to_string(),
        serde_json::Value::Bool(manifest.options.verify_checksums),
    );
    options.insert(
        "durability_mode".to_string(),
        manifest
            .options
            .durability_mode
            .clone()
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null),
    );
    options.insert(
        "group_commit_window_ms".to_string(),
        manifest
            .options
            .group_commit_window_ms
            .map(|value| serde_json::Value::Number(value.into()))
            .unwrap_or(serde_json::Value::Null),
    );
    options.insert(
        "group_commit_max_statements".to_string(),
        manifest
            .options
            .group_commit_max_statements
            .map(json_usize)
            .unwrap_or(serde_json::Value::Null),
    );
    options.insert(
        "group_commit_max_wal_bytes".to_string(),
        manifest
            .options
            .group_commit_max_wal_bytes
            .map(|value| serde_json::Value::Number(value.into()))
            .unwrap_or(serde_json::Value::Null),
    );
    options.insert(
        "auto_checkpoint_pages".to_string(),
        serde_json::Value::Number(manifest.options.auto_checkpoint_pages.into()),
    );
    options.insert(
        "cache_pages".to_string(),
        json_usize(manifest.options.cache_pages),
    );
    options.insert(
        "snapshot_retention".to_string(),
        manifest
            .options
            .snapshot_retention
            .map(json_usize)
            .unwrap_or(serde_json::Value::Null),
    );
    options.insert(
        "export_retention".to_string(),
        manifest
            .options
            .export_retention
            .map(json_usize)
            .unwrap_or(serde_json::Value::Null),
    );
    options.insert(
        "force_create".to_string(),
        serde_json::Value::Bool(manifest.options.force_create),
    );
    options.insert(
        "capabilities".to_string(),
        string_array_json(&manifest.options.capabilities),
    );
    options.insert(
        "metadata".to_string(),
        serde_json::Value::Object(
            manifest
                .options
                .metadata
                .iter()
                .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
                .collect(),
        ),
    );

    let mut object = serde_json::Map::new();
    object.insert(
        "format_version".to_string(),
        serde_json::Value::Number(manifest.format_version.into()),
    );
    object.insert(
        "created_at_unix_ms".to_string(),
        json_u128(manifest.created_at_unix_ms),
    );
    object.insert(
        "updated_at_unix_ms".to_string(),
        json_u128(manifest.updated_at_unix_ms),
    );
    object.insert(
        "collection_count".to_string(),
        json_usize(manifest.collection_count),
    );
    object.insert("options".to_string(), serde_json::Value::Object(options));
    serde_json::Value::Object(object)
}

fn schema_manifest_from_json_value(
    value: &serde_json::Value,
) -> RdbFileResult<PhysicalSchemaManifest> {
    let object = expect_object(value, "manifest")?;
    let options_object = expect_object(required(object, "options")?, "manifest.options")?;
    let options = PhysicalSchemaOptions {
        mode: json_string_required(options_object, "mode")?,
        data_path: options_object
            .get("data_path")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
        read_only: json_bool_required(options_object, "read_only")?,
        create_if_missing: json_bool_required(options_object, "create_if_missing")?,
        verify_checksums: json_bool_required(options_object, "verify_checksums")?,
        durability_mode: options_object
            .get("durability_mode")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
        group_commit_window_ms: options_object
            .get("group_commit_window_ms")
            .filter(|value| !value.is_null())
            .map(json_u64_value)
            .transpose()?,
        group_commit_max_statements: options_object
            .get("group_commit_max_statements")
            .filter(|value| !value.is_null())
            .map(json_usize_value)
            .transpose()?,
        group_commit_max_wal_bytes: options_object
            .get("group_commit_max_wal_bytes")
            .filter(|value| !value.is_null())
            .map(json_u64_value)
            .transpose()?,
        auto_checkpoint_pages: json_u32_required(options_object, "auto_checkpoint_pages")?,
        cache_pages: json_usize_required(options_object, "cache_pages")?,
        snapshot_retention: options_object
            .get("snapshot_retention")
            .filter(|value| !value.is_null())
            .map(json_usize_value)
            .transpose()?,
        export_retention: options_object
            .get("export_retention")
            .filter(|value| !value.is_null())
            .map(json_usize_value)
            .transpose()?,
        force_create: json_bool_required(options_object, "force_create")?,
        capabilities: options_object
            .get("capabilities")
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        metadata: options_object
            .get("metadata")
            .and_then(serde_json::Value::as_object)
            .map(|metadata| {
                metadata
                    .iter()
                    .filter_map(|(key, value)| {
                        value.as_str().map(|value| (key.clone(), value.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default(),
    };

    Ok(PhysicalSchemaManifest {
        format_version: json_u32_required(object, "format_version")?,
        created_at_unix_ms: json_u128_required(object, "created_at_unix_ms")?,
        updated_at_unix_ms: json_u128_required(object, "updated_at_unix_ms")?,
        collection_count: json_usize_required(object, "collection_count")?,
        options,
    })
}

fn catalog_snapshot_json_value(catalog: &PhysicalCatalogSnapshot) -> serde_json::Value {
    let mut stats = serde_json::Map::new();
    for (name, stat) in &catalog.stats_by_collection {
        let mut entry = serde_json::Map::new();
        entry.insert("entities".to_string(), json_usize(stat.entities));
        entry.insert("cross_refs".to_string(), json_usize(stat.cross_refs));
        entry.insert("segments".to_string(), json_usize(stat.segments));
        stats.insert(name.clone(), serde_json::Value::Object(entry));
    }

    let mut object = serde_json::Map::new();
    object.insert(
        "name".to_string(),
        serde_json::Value::String(catalog.name.clone()),
    );
    object.insert(
        "total_entities".to_string(),
        json_usize(catalog.total_entities),
    );
    object.insert(
        "total_collections".to_string(),
        json_usize(catalog.total_collections),
    );
    object.insert(
        "updated_at_unix_ms".to_string(),
        json_u128(catalog.updated_at_unix_ms),
    );
    object.insert(
        "stats_by_collection".to_string(),
        serde_json::Value::Object(stats),
    );
    serde_json::Value::Object(object)
}

fn catalog_snapshot_from_json_value(
    value: &serde_json::Value,
) -> RdbFileResult<PhysicalCatalogSnapshot> {
    let object = expect_object(value, "physical catalog snapshot")?;
    let stats = expect_object(
        required(object, "stats_by_collection")?,
        "physical catalog stats",
    )?;
    let mut stats_by_collection = BTreeMap::new();
    for (name, value) in stats {
        let entry = expect_object(value, "physical catalog stats entry")?;
        stats_by_collection.insert(
            name.clone(),
            PhysicalCatalogCollectionStats {
                entities: json_usize_required(entry, "entities")?,
                cross_refs: json_usize_required(entry, "cross_refs")?,
                segments: json_usize_required(entry, "segments")?,
            },
        );
    }

    Ok(PhysicalCatalogSnapshot {
        name: json_string_required(object, "name")?,
        total_entities: json_usize_required(object, "total_entities")?,
        total_collections: json_usize_required(object, "total_collections")?,
        updated_at_unix_ms: json_u128_required(object, "updated_at_unix_ms")?,
        stats_by_collection,
    })
}

fn analytical_storage_json_value(config: &PhysicalAnalyticalStorageConfig) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "columnar".to_string(),
        serde_json::Value::Bool(config.columnar),
    );
    object.insert(
        "time_key".to_string(),
        serde_json::Value::String(config.time_key.clone()),
    );
    object.insert(
        "order_by_key".to_string(),
        config
            .order_by_key
            .clone()
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null),
    );
    serde_json::Value::Object(object)
}

fn analytical_storage_from_json_value(
    value: &serde_json::Value,
) -> RdbFileResult<PhysicalAnalyticalStorageConfig> {
    let object = expect_object(value, "physical analytical storage")?;
    Ok(PhysicalAnalyticalStorageConfig {
        columnar: object
            .get("columnar")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        time_key: json_string_required(object, "time_key")?,
        order_by_key: object
            .get("order_by_key")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
    })
}

fn subscription_descriptor_json_value(
    subscription: &PhysicalSubscriptionDescriptor,
) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "name".to_string(),
        serde_json::Value::String(subscription.name.clone()),
    );
    object.insert(
        "source".to_string(),
        serde_json::Value::String(subscription.source.clone()),
    );
    object.insert(
        "target_queue".to_string(),
        serde_json::Value::String(subscription.target_queue.clone()),
    );
    object.insert(
        "ops_filter".to_string(),
        string_array_json(&subscription.ops_filter),
    );
    object.insert(
        "where_filter".to_string(),
        subscription
            .where_filter
            .clone()
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null),
    );
    object.insert(
        "redact_fields".to_string(),
        string_array_json(&subscription.redact_fields),
    );
    object.insert(
        "enabled".to_string(),
        serde_json::Value::Bool(subscription.enabled),
    );
    object.insert(
        "all_tenants".to_string(),
        serde_json::Value::Bool(subscription.all_tenants),
    );
    serde_json::Value::Object(object)
}

fn subscription_descriptor_from_json_value(
    value: &serde_json::Value,
) -> RdbFileResult<PhysicalSubscriptionDescriptor> {
    let object = expect_object(value, "physical subscription descriptor")?;
    Ok(PhysicalSubscriptionDescriptor {
        name: object
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        source: json_string_required(object, "source")?,
        target_queue: json_string_required(object, "target_queue")?,
        ops_filter: string_array_from_json(object.get("ops_filter")).unwrap_or_default(),
        where_filter: match object.get("where_filter") {
            Some(serde_json::Value::String(value)) => Some(value.clone()),
            Some(serde_json::Value::Null) | None => None,
            Some(_) => {
                return Err(invalid(
                    "physical subscription where_filter must be a string or null",
                ))
            }
        },
        redact_fields: string_array_from_json(object.get("redact_fields")).unwrap_or_default(),
        enabled: object
            .get("enabled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        all_tenants: object
            .get("all_tenants")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

fn analytics_view_descriptor_json_value(
    view: &PhysicalAnalyticsViewDescriptor,
) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "output".to_string(),
        serde_json::Value::String(view.output.clone()),
    );
    object.insert(
        "algorithm".to_string(),
        view.algorithm
            .clone()
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null),
    );
    object.insert("resolution".to_string(), optional_f64_json(view.resolution));
    object.insert(
        "max_iterations".to_string(),
        view.max_iterations
            .map(serde_json::Value::from)
            .unwrap_or(serde_json::Value::Null),
    );
    object.insert("tolerance".to_string(), optional_f64_json(view.tolerance));
    serde_json::Value::Object(object)
}

fn analytics_view_descriptor_from_json_value(
    value: &serde_json::Value,
) -> RdbFileResult<PhysicalAnalyticsViewDescriptor> {
    let object = expect_object(value, "physical analytics view descriptor")?;
    Ok(PhysicalAnalyticsViewDescriptor {
        output: json_string_required(object, "output")?,
        algorithm: optional_string_field(object, "algorithm")?,
        resolution: optional_f64_field(object, "resolution")?,
        max_iterations: optional_i64_field(object, "max_iterations")?,
        tolerance: optional_f64_field(object, "tolerance")?,
    })
}

fn declared_column_contract_json_value(
    column: &PhysicalDeclaredColumnContract,
) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "name".to_string(),
        serde_json::Value::String(column.name.clone()),
    );
    object.insert(
        "data_type".to_string(),
        serde_json::Value::String(column.data_type.clone()),
    );
    object.insert(
        "sql_type".to_string(),
        column
            .sql_type
            .as_ref()
            .map(sql_type_name_json_value)
            .unwrap_or(serde_json::Value::Null),
    );
    object.insert(
        "not_null".to_string(),
        serde_json::Value::Bool(column.not_null),
    );
    object.insert(
        "default".to_string(),
        column
            .default
            .clone()
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null),
    );
    object.insert("compress".to_string(), optional_u8_json(column.compress));
    object.insert("unique".to_string(), serde_json::Value::Bool(column.unique));
    object.insert(
        "primary_key".to_string(),
        serde_json::Value::Bool(column.primary_key),
    );
    object.insert(
        "enum_variants".to_string(),
        string_array_json(&column.enum_variants),
    );
    object.insert(
        "array_element".to_string(),
        column
            .array_element
            .clone()
            .map(serde_json::Value::String)
            .unwrap_or(serde_json::Value::Null),
    );
    object.insert(
        "decimal_precision".to_string(),
        optional_u8_json(column.decimal_precision),
    );
    serde_json::Value::Object(object)
}

fn declared_column_contract_from_json_value(
    value: &serde_json::Value,
) -> RdbFileResult<PhysicalDeclaredColumnContract> {
    let object = expect_object(value, "physical declared column contract")?;
    Ok(PhysicalDeclaredColumnContract {
        name: json_string_required(object, "name")?,
        data_type: json_string_required(object, "data_type")?,
        sql_type: optional_sql_type_name_from_json_value(object.get("sql_type"))?,
        not_null: json_bool_required(object, "not_null")?,
        default: optional_string_field(object, "default")?,
        compress: optional_u8_field(object, "compress")?,
        unique: json_bool_required(object, "unique")?,
        primary_key: json_bool_required(object, "primary_key")?,
        enum_variants: string_array_from_json(object.get("enum_variants")).unwrap_or_default(),
        array_element: optional_string_field(object, "array_element")?,
        decimal_precision: optional_u8_field(object, "decimal_precision")?,
    })
}

fn collection_contract_json_value(contract: &PhysicalCollectionContract) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "name".to_string(),
        serde_json::Value::String(contract.name.clone()),
    );
    object.insert(
        "declared_model".to_string(),
        serde_json::Value::String(contract.declared_model.clone()),
    );
    object.insert(
        "schema_mode".to_string(),
        serde_json::Value::String(contract.schema_mode.clone()),
    );
    object.insert(
        "origin".to_string(),
        serde_json::Value::String(contract.origin.clone()),
    );
    object.insert(
        "version".to_string(),
        serde_json::Value::from(contract.version),
    );
    object.insert(
        "created_at_unix_ms".to_string(),
        json_u128(contract.created_at_unix_ms),
    );
    object.insert(
        "updated_at_unix_ms".to_string(),
        json_u128(contract.updated_at_unix_ms),
    );
    object.insert(
        "default_ttl_ms".to_string(),
        optional_u64_json(contract.default_ttl_ms),
    );
    object.insert(
        "vector_dimension".to_string(),
        optional_usize_json(contract.vector_dimension),
    );
    object.insert(
        "vector_metric".to_string(),
        optional_string_json(contract.vector_metric.as_ref()),
    );
    object.insert(
        "context_index_fields".to_string(),
        string_array_json(&contract.context_index_fields),
    );
    object.insert(
        "declared_columns".to_string(),
        serde_json::Value::Array(
            contract
                .declared_columns
                .iter()
                .map(declared_column_contract_json_value)
                .collect(),
        ),
    );
    object.insert(
        "timestamps_enabled".to_string(),
        serde_json::Value::Bool(contract.timestamps_enabled),
    );
    object.insert(
        "context_index_enabled".to_string(),
        serde_json::Value::Bool(contract.context_index_enabled),
    );
    object.insert(
        "metrics_raw_retention_ms".to_string(),
        optional_u64_json(contract.metrics_raw_retention_ms),
    );
    object.insert(
        "metrics_rollup_policies".to_string(),
        string_array_json(&contract.metrics_rollup_policies),
    );
    object.insert(
        "metrics_tenant_identity".to_string(),
        optional_string_json(contract.metrics_tenant_identity.as_ref()),
    );
    object.insert(
        "metrics_namespace".to_string(),
        optional_string_json(contract.metrics_namespace.as_ref()),
    );
    object.insert(
        "append_only".to_string(),
        serde_json::Value::Bool(contract.append_only),
    );
    object.insert(
        "subscriptions".to_string(),
        serde_json::Value::Array(
            contract
                .subscriptions
                .iter()
                .map(subscription_descriptor_json_value)
                .collect(),
        ),
    );
    object.insert(
        "analytics_config".to_string(),
        serde_json::Value::Array(
            contract
                .analytics_config
                .iter()
                .map(analytics_view_descriptor_json_value)
                .collect(),
        ),
    );
    object.insert(
        "session_key".to_string(),
        optional_string_json(contract.session_key.as_ref()),
    );
    object.insert(
        "session_gap_ms".to_string(),
        optional_u64_json(contract.session_gap_ms),
    );
    object.insert(
        "retention_duration_ms".to_string(),
        optional_u64_json(contract.retention_duration_ms),
    );
    object.insert(
        "analytical_storage".to_string(),
        contract
            .analytical_storage
            .as_ref()
            .map(analytical_storage_json_value)
            .unwrap_or(serde_json::Value::Null),
    );
    object.insert(
        "table_def".to_string(),
        optional_string_json(contract.table_def_hex.as_ref()),
    );
    serde_json::Value::Object(object)
}

fn collection_contract_from_json_value(
    value: &serde_json::Value,
) -> RdbFileResult<PhysicalCollectionContract> {
    let object = expect_object(value, "physical collection contract")?;
    Ok(PhysicalCollectionContract {
        name: json_string_required(object, "name")?,
        declared_model: json_string_required(object, "declared_model")?,
        schema_mode: json_string_required(object, "schema_mode")?,
        origin: json_string_required(object, "origin")?,
        version: json_u32_required(object, "version")?,
        created_at_unix_ms: json_u128_required(object, "created_at_unix_ms")?,
        updated_at_unix_ms: json_u128_required(object, "updated_at_unix_ms")?,
        default_ttl_ms: optional_u64_field(object, "default_ttl_ms")?,
        vector_dimension: optional_usize_field(object, "vector_dimension")?,
        vector_metric: optional_string_field(object, "vector_metric")?,
        context_index_fields: string_array_from_json(object.get("context_index_fields"))
            .unwrap_or_default(),
        declared_columns: physical_array_from_json(
            object.get("declared_columns"),
            declared_column_contract_from_json_value,
        )?,
        table_def_hex: optional_string_field(object, "table_def")?,
        timestamps_enabled: object
            .get("timestamps_enabled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        context_index_enabled: object
            .get("context_index_enabled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        metrics_raw_retention_ms: optional_u64_field(object, "metrics_raw_retention_ms")?,
        metrics_rollup_policies: string_array_from_json(object.get("metrics_rollup_policies"))
            .unwrap_or_default(),
        metrics_tenant_identity: optional_string_field(object, "metrics_tenant_identity")?,
        metrics_namespace: optional_string_field(object, "metrics_namespace")?,
        append_only: object
            .get("append_only")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        subscriptions: physical_array_from_json(
            object.get("subscriptions"),
            subscription_descriptor_from_json_value,
        )?,
        analytics_config: physical_array_from_json(
            object.get("analytics_config"),
            analytics_view_descriptor_from_json_value,
        )?,
        session_key: optional_string_field(object, "session_key")?,
        session_gap_ms: optional_u64_field(object, "session_gap_ms")?,
        retention_duration_ms: optional_u64_field(object, "retention_duration_ms")?,
        analytical_storage: match object.get("analytical_storage") {
            Some(serde_json::Value::Null) | None => None,
            Some(value) => Some(analytical_storage_from_json_value(value)?),
        },
    })
}

fn sql_type_name_json_value(sql_type: &PhysicalSqlTypeName) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "name".to_string(),
        serde_json::Value::String(sql_type.name.clone()),
    );
    object.insert(
        "modifiers".to_string(),
        serde_json::Value::Array(
            sql_type
                .modifiers
                .iter()
                .map(type_modifier_json_value)
                .collect(),
        ),
    );
    serde_json::Value::Object(object)
}

fn optional_sql_type_name_from_json_value(
    value: Option<&serde_json::Value>,
) -> RdbFileResult<Option<PhysicalSqlTypeName>> {
    match value {
        Some(serde_json::Value::Null) | None => Ok(None),
        Some(value) => sql_type_name_from_json_value(value).map(Some),
    }
}

fn sql_type_name_from_json_value(value: &serde_json::Value) -> RdbFileResult<PhysicalSqlTypeName> {
    let object = expect_object(value, "physical sql type name")?;
    let modifiers = object
        .get("modifiers")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(type_modifier_from_json_value)
                .collect::<RdbFileResult<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok(PhysicalSqlTypeName {
        name: json_string_required(object, "name")?,
        modifiers,
    })
}

fn type_modifier_json_value(modifier: &PhysicalTypeModifier) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    match modifier {
        PhysicalTypeModifier::Number(value) => {
            object.insert(
                "kind".to_string(),
                serde_json::Value::String("number".to_string()),
            );
            object.insert("value".to_string(), serde_json::Value::from(*value));
        }
        PhysicalTypeModifier::Ident(value) => {
            object.insert(
                "kind".to_string(),
                serde_json::Value::String("ident".to_string()),
            );
            object.insert(
                "value".to_string(),
                serde_json::Value::String(value.clone()),
            );
        }
        PhysicalTypeModifier::StringLiteral(value) => {
            object.insert(
                "kind".to_string(),
                serde_json::Value::String("string".to_string()),
            );
            object.insert(
                "value".to_string(),
                serde_json::Value::String(value.clone()),
            );
        }
        PhysicalTypeModifier::Type(value) => {
            object.insert(
                "kind".to_string(),
                serde_json::Value::String("type".to_string()),
            );
            object.insert("value".to_string(), sql_type_name_json_value(value));
        }
    }
    serde_json::Value::Object(object)
}

fn type_modifier_from_json_value(value: &serde_json::Value) -> RdbFileResult<PhysicalTypeModifier> {
    let object = expect_object(value, "physical type modifier")?;
    match json_string_required(object, "kind")?.as_str() {
        "number" => Ok(PhysicalTypeModifier::Number(json_u32_required(
            object, "value",
        )?)),
        "ident" => Ok(PhysicalTypeModifier::Ident(json_string_required(
            object, "value",
        )?)),
        "string" => Ok(PhysicalTypeModifier::StringLiteral(json_string_required(
            object, "value",
        )?)),
        "type" => Ok(PhysicalTypeModifier::Type(Box::new(
            sql_type_name_from_json_value(required(object, "value")?)?,
        ))),
        other => Err(invalid(format!("unsupported type modifier kind: {other}"))),
    }
}

fn snapshot_descriptor_json_value(snapshot: &SnapshotDescriptor) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert("snapshot_id".to_string(), json_u64(snapshot.snapshot_id));
    object.insert(
        "created_at_unix_ms".to_string(),
        json_u128(snapshot.created_at_unix_ms),
    );
    object.insert(
        "superblock_sequence".to_string(),
        json_u64(snapshot.superblock_sequence),
    );
    object.insert(
        "collection_count".to_string(),
        json_usize(snapshot.collection_count),
    );
    object.insert(
        "total_entities".to_string(),
        json_usize(snapshot.total_entities),
    );
    serde_json::Value::Object(object)
}

fn snapshot_descriptor_from_json_value(
    value: &serde_json::Value,
) -> RdbFileResult<SnapshotDescriptor> {
    let object = expect_object(value, "snapshot descriptor")?;
    Ok(SnapshotDescriptor {
        snapshot_id: json_u64_required(object, "snapshot_id")?,
        created_at_unix_ms: json_u128_required(object, "created_at_unix_ms")?,
        superblock_sequence: json_u64_required(object, "superblock_sequence")?,
        collection_count: json_usize_required(object, "collection_count")?,
        total_entities: json_usize_required(object, "total_entities")?,
    })
}

fn export_descriptor_json_value(export: &ExportDescriptor) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "name".to_string(),
        serde_json::Value::String(export.name.clone()),
    );
    object.insert(
        "created_at_unix_ms".to_string(),
        json_u128(export.created_at_unix_ms),
    );
    object.insert(
        "snapshot_id".to_string(),
        match export.snapshot_id {
            Some(snapshot_id) => json_u64(snapshot_id),
            None => serde_json::Value::Null,
        },
    );
    object.insert(
        "superblock_sequence".to_string(),
        json_u64(export.superblock_sequence),
    );
    object.insert(
        "data_path".to_string(),
        serde_json::Value::String(export.data_path.clone()),
    );
    object.insert(
        "metadata_path".to_string(),
        serde_json::Value::String(export.metadata_path.clone()),
    );
    object.insert(
        "collection_count".to_string(),
        json_usize(export.collection_count),
    );
    object.insert(
        "total_entities".to_string(),
        json_usize(export.total_entities),
    );
    serde_json::Value::Object(object)
}

fn export_descriptor_from_json_value(value: &serde_json::Value) -> RdbFileResult<ExportDescriptor> {
    let object = expect_object(value, "export descriptor")?;
    Ok(ExportDescriptor {
        name: json_string_required(object, "name")?,
        created_at_unix_ms: json_u128_required(object, "created_at_unix_ms")?,
        snapshot_id: object.get("snapshot_id").and_then(|value| {
            if value.is_null() {
                None
            } else {
                json_u64_value(value).ok()
            }
        }),
        superblock_sequence: json_u64_required(object, "superblock_sequence")?,
        data_path: json_string_required(object, "data_path")?,
        metadata_path: json_string_required(object, "metadata_path")?,
        collection_count: json_usize_required(object, "collection_count")?,
        total_entities: json_usize_required(object, "total_entities")?,
    })
}

fn graph_projection_json_value(projection: &PhysicalGraphProjection) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "name".to_string(),
        serde_json::Value::String(projection.name.clone()),
    );
    object.insert(
        "created_at_unix_ms".to_string(),
        json_u128(projection.created_at_unix_ms),
    );
    object.insert(
        "updated_at_unix_ms".to_string(),
        json_u128(projection.updated_at_unix_ms),
    );
    object.insert(
        "state".to_string(),
        serde_json::Value::String(projection.state.clone()),
    );
    object.insert(
        "source".to_string(),
        serde_json::Value::String(projection.source.clone()),
    );
    object.insert(
        "node_labels".to_string(),
        string_array_json(&projection.node_labels),
    );
    object.insert(
        "node_types".to_string(),
        string_array_json(&projection.node_types),
    );
    object.insert(
        "edge_labels".to_string(),
        string_array_json(&projection.edge_labels),
    );
    object.insert(
        "last_materialized_sequence".to_string(),
        projection
            .last_materialized_sequence
            .map(json_u64)
            .unwrap_or(serde_json::Value::Null),
    );
    serde_json::Value::Object(object)
}

fn graph_projection_from_json_value(
    value: &serde_json::Value,
) -> RdbFileResult<PhysicalGraphProjection> {
    let object = expect_object(value, "graph projection")?;
    Ok(PhysicalGraphProjection {
        name: json_string_required(object, "name")?,
        created_at_unix_ms: json_u128_required(object, "created_at_unix_ms")?,
        updated_at_unix_ms: json_u128_required(object, "updated_at_unix_ms")?,
        state: object
            .get("state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("declared")
            .to_string(),
        source: json_string_required(object, "source")?,
        node_labels: string_array_from_json(object.get("node_labels")).unwrap_or_default(),
        node_types: string_array_from_json(object.get("node_types")).unwrap_or_default(),
        edge_labels: string_array_from_json(object.get("edge_labels")).unwrap_or_default(),
        last_materialized_sequence: object
            .get("last_materialized_sequence")
            .and_then(|value| json_u64_value(value).ok()),
    })
}

fn analytics_job_json_value(job: &PhysicalAnalyticsJob) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert("id".to_string(), serde_json::Value::String(job.id.clone()));
    object.insert(
        "kind".to_string(),
        serde_json::Value::String(job.kind.clone()),
    );
    object.insert(
        "state".to_string(),
        serde_json::Value::String(job.state.clone()),
    );
    object.insert(
        "projection".to_string(),
        job.projection
            .as_ref()
            .map(|value| serde_json::Value::String(value.clone()))
            .unwrap_or(serde_json::Value::Null),
    );
    object.insert(
        "created_at_unix_ms".to_string(),
        json_u128(job.created_at_unix_ms),
    );
    object.insert(
        "updated_at_unix_ms".to_string(),
        json_u128(job.updated_at_unix_ms),
    );
    object.insert(
        "last_run_sequence".to_string(),
        job.last_run_sequence
            .map(json_u64)
            .unwrap_or(serde_json::Value::Null),
    );
    object.insert(
        "metadata".to_string(),
        serde_json::Value::Object(
            job.metadata
                .iter()
                .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
                .collect(),
        ),
    );
    serde_json::Value::Object(object)
}

fn analytics_job_from_json_value(value: &serde_json::Value) -> RdbFileResult<PhysicalAnalyticsJob> {
    let object = expect_object(value, "analytics job")?;
    Ok(PhysicalAnalyticsJob {
        id: json_string_required(object, "id")?,
        kind: json_string_required(object, "kind")?,
        state: json_string_required(object, "state")?,
        projection: object
            .get("projection")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        created_at_unix_ms: json_u128_required(object, "created_at_unix_ms")?,
        updated_at_unix_ms: json_u128_required(object, "updated_at_unix_ms")?,
        last_run_sequence: object
            .get("last_run_sequence")
            .and_then(|value| json_u64_value(value).ok()),
        metadata: object
            .get("metadata")
            .and_then(serde_json::Value::as_object)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|(key, value)| {
                        value.as_str().map(|value| (key.clone(), value.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn tree_definition_json_value(definition: &PhysicalTreeDefinition) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "collection".to_string(),
        serde_json::Value::String(definition.collection.clone()),
    );
    object.insert(
        "name".to_string(),
        serde_json::Value::String(definition.name.clone()),
    );
    object.insert("root_id".to_string(), json_u64(definition.root_id));
    object.insert(
        "default_max_children".to_string(),
        json_usize(definition.default_max_children),
    );
    object.insert(
        "ordered_children".to_string(),
        serde_json::Value::Bool(definition.ordered_children),
    );
    object.insert(
        "ownership".to_string(),
        serde_json::Value::String(definition.ownership.clone()),
    );
    object.insert(
        "auto_fix_mode".to_string(),
        serde_json::Value::String(definition.auto_fix_mode.clone()),
    );
    object.insert(
        "created_at_unix_ms".to_string(),
        json_u128(definition.created_at_unix_ms),
    );
    object.insert(
        "updated_at_unix_ms".to_string(),
        json_u128(definition.updated_at_unix_ms),
    );
    serde_json::Value::Object(object)
}

fn tree_definition_from_json_value(
    value: &serde_json::Value,
) -> RdbFileResult<PhysicalTreeDefinition> {
    let object = expect_object(value, "tree definition")?;
    Ok(PhysicalTreeDefinition {
        collection: json_string_required(object, "collection")?,
        name: json_string_required(object, "name")?,
        root_id: json_u64_required(object, "root_id")?,
        default_max_children: json_usize_required(object, "default_max_children")?,
        ordered_children: object
            .get("ordered_children")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        ownership: object
            .get("ownership")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("owned")
            .to_string(),
        auto_fix_mode: object
            .get("auto_fix_mode")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("conservative")
            .to_string(),
        created_at_unix_ms: json_u128_required(object, "created_at_unix_ms")?,
        updated_at_unix_ms: json_u128_required(object, "updated_at_unix_ms")?,
    })
}

fn index_state_json_value(index: &PersistedPhysicalIndexState) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "name".to_string(),
        serde_json::Value::String(index.name.clone()),
    );
    object.insert(
        "kind".to_string(),
        serde_json::Value::String(index.kind.clone()),
    );
    object.insert(
        "collection".to_string(),
        index
            .collection
            .as_ref()
            .map(|value| serde_json::Value::String(value.clone()))
            .unwrap_or(serde_json::Value::Null),
    );
    object.insert(
        "enabled".to_string(),
        serde_json::Value::Bool(index.enabled),
    );
    object.insert("entries".to_string(), json_usize(index.entries));
    object.insert(
        "estimated_memory_bytes".to_string(),
        json_u64(index.estimated_memory_bytes),
    );
    object.insert(
        "last_refresh_ms".to_string(),
        index
            .last_refresh_ms
            .map(json_u128)
            .unwrap_or(serde_json::Value::Null),
    );
    object.insert(
        "backend".to_string(),
        serde_json::Value::String(index.backend.clone()),
    );
    object.insert(
        "artifact_kind".to_string(),
        index
            .artifact_kind
            .as_ref()
            .map(|value| serde_json::Value::String(value.clone()))
            .unwrap_or(serde_json::Value::Null),
    );
    object.insert(
        "artifact_root_page".to_string(),
        index
            .artifact_root_page
            .map(|value| serde_json::Value::Number(value.into()))
            .unwrap_or(serde_json::Value::Null),
    );
    object.insert(
        "artifact_checksum".to_string(),
        index
            .artifact_checksum
            .map(json_u64)
            .unwrap_or(serde_json::Value::Null),
    );
    object.insert(
        "build_state".to_string(),
        serde_json::Value::String(index.build_state.clone()),
    );
    serde_json::Value::Object(object)
}

fn index_state_from_json_value(
    value: &serde_json::Value,
) -> RdbFileResult<PersistedPhysicalIndexState> {
    let object = expect_object(value, "physical index state")?;
    Ok(PersistedPhysicalIndexState {
        name: json_string_required(object, "name")?,
        kind: json_string_required(object, "kind")?,
        collection: object
            .get("collection")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        enabled: json_bool_required(object, "enabled")?,
        entries: json_usize_required(object, "entries")?,
        estimated_memory_bytes: json_u64_required(object, "estimated_memory_bytes")?,
        last_refresh_ms: object
            .get("last_refresh_ms")
            .and_then(|value| json_u128_value(value).ok()),
        backend: json_string_required(object, "backend")?,
        artifact_kind: object
            .get("artifact_kind")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        artifact_root_page: object
            .get("artifact_root_page")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u32::try_from(value).ok()),
        artifact_checksum: object
            .get("artifact_checksum")
            .and_then(|value| json_u64_value(value).ok()),
        build_state: object
            .get("build_state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
    })
}

fn hypertable_chunk_json_value(chunk: &PersistedPhysicalHypertableChunk) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert("start_ns".to_string(), json_u64(chunk.start_ns));
    object.insert(
        "end_ns_exclusive".to_string(),
        json_u64(chunk.end_ns_exclusive),
    );
    object.insert("row_count".to_string(), json_u64(chunk.row_count));
    object.insert("min_ts_ns".to_string(), json_u64(chunk.min_ts_ns));
    object.insert("max_ts_ns".to_string(), json_u64(chunk.max_ts_ns));
    object.insert("sealed".to_string(), serde_json::Value::Bool(chunk.sealed));
    object.insert(
        "ttl_override_ns".to_string(),
        chunk
            .ttl_override_ns
            .map(json_u64)
            .unwrap_or(serde_json::Value::Null),
    );
    object.insert(
        "columnar_page".to_string(),
        chunk
            .columnar_page
            .map(page_location_json_value)
            .unwrap_or(serde_json::Value::Null),
    );
    serde_json::Value::Object(object)
}

fn hypertable_chunk_from_json_value(
    value: &serde_json::Value,
) -> RdbFileResult<PersistedPhysicalHypertableChunk> {
    let object = expect_object(value, "hypertable chunk")?;
    Ok(PersistedPhysicalHypertableChunk {
        start_ns: json_u64_required(object, "start_ns")?,
        end_ns_exclusive: json_u64_required(object, "end_ns_exclusive")?,
        row_count: json_u64_required(object, "row_count")?,
        min_ts_ns: json_u64_required(object, "min_ts_ns")?,
        max_ts_ns: json_u64_required(object, "max_ts_ns")?,
        sealed: object
            .get("sealed")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        ttl_override_ns: match object.get("ttl_override_ns") {
            Some(serde_json::Value::Null) | None => None,
            Some(value) => Some(json_u64_value(value)?),
        },
        columnar_page: match object.get("columnar_page") {
            Some(serde_json::Value::Null) | None => None,
            Some(value) => Some(page_location_from_json_value(value)?),
        },
    })
}

fn hypertable_json_value(hypertable: &PersistedPhysicalHypertable) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "name".to_string(),
        serde_json::Value::String(hypertable.name.clone()),
    );
    object.insert(
        "time_column".to_string(),
        serde_json::Value::String(hypertable.time_column.clone()),
    );
    object.insert(
        "chunk_interval_ns".to_string(),
        json_u64(hypertable.chunk_interval_ns),
    );
    object.insert(
        "default_ttl_ns".to_string(),
        hypertable
            .default_ttl_ns
            .map(json_u64)
            .unwrap_or(serde_json::Value::Null),
    );
    object.insert(
        "chunks".to_string(),
        serde_json::Value::Array(
            hypertable
                .chunks
                .iter()
                .map(hypertable_chunk_json_value)
                .collect(),
        ),
    );
    serde_json::Value::Object(object)
}

fn hypertable_from_json_value(
    value: &serde_json::Value,
) -> RdbFileResult<PersistedPhysicalHypertable> {
    let object = expect_object(value, "hypertable")?;
    Ok(PersistedPhysicalHypertable {
        name: json_string_required(object, "name")?,
        time_column: json_string_required(object, "time_column")?,
        chunk_interval_ns: json_u64_required(object, "chunk_interval_ns")?,
        default_ttl_ns: match object.get("default_ttl_ns") {
            Some(serde_json::Value::Null) | None => None,
            Some(value) => Some(json_u64_value(value)?),
        },
        chunks: object
            .get("chunks")
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .map(hypertable_chunk_from_json_value)
                    .collect::<RdbFileResult<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default(),
    })
}

fn page_location_json_value(loc: PhysicalPageLocation) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    object.insert("page_id".to_string(), json_u64(loc.page_id as u64));
    object.insert("offset".to_string(), json_u64(loc.offset as u64));
    object.insert("length".to_string(), json_u64(loc.length as u64));
    serde_json::Value::Object(object)
}

fn page_location_from_json_value(value: &serde_json::Value) -> RdbFileResult<PhysicalPageLocation> {
    let object = expect_object(value, "page location")?;
    Ok(PhysicalPageLocation {
        page_id: json_u64_required(object, "page_id")? as u32,
        offset: json_u64_required(object, "offset")? as u32,
        length: json_u64_required(object, "length")? as u32,
    })
}

fn manifest_event_kind_as_str(kind: ManifestEventKind) -> &'static str {
    match kind {
        ManifestEventKind::Insert => "insert",
        ManifestEventKind::Update => "update",
        ManifestEventKind::Remove => "remove",
        ManifestEventKind::Checkpoint => "checkpoint",
    }
}

fn manifest_event_kind_from_str(value: &str) -> RdbFileResult<ManifestEventKind> {
    match value {
        "insert" => Ok(ManifestEventKind::Insert),
        "update" => Ok(ManifestEventKind::Update),
        "remove" => Ok(ManifestEventKind::Remove),
        "checkpoint" => Ok(ManifestEventKind::Checkpoint),
        other => Err(invalid(format!(
            "unsupported manifest event kind '{other}'"
        ))),
    }
}

#[cfg(test)]
mod tests;
