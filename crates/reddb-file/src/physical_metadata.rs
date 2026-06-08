//! Physical metadata document file contract.
//!
//! Runtime crates own the domain model that becomes physical metadata. This
//! module owns the persisted document envelope: JSON validation, byte encoding,
//! and path I/O for the JSON sidecar and compact binary sidecar.

use crate::{RdbFileError, RdbFileResult};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub const DEFAULT_PHYSICAL_FORMAT_VERSION: u32 = 2;
pub const DEFAULT_SUPERBLOCK_COPIES: u8 = 4;
pub const PHYSICAL_METADATA_PROTOCOL_VERSION: &str = "reddb-physical-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlockReference {
    pub index: u64,
    pub checksum: u128,
}

#[derive(Debug, Clone, Default)]
pub struct ManifestPointers {
    pub oldest: BlockReference,
    pub newest: BlockReference,
}

#[derive(Debug, Clone)]
pub struct SuperblockHeader {
    pub format_version: u32,
    pub sequence: u64,
    pub copies: u8,
    pub manifest: ManifestPointers,
    pub free_set: BlockReference,
    pub collection_roots: BTreeMap<String, u64>,
}

impl Default for SuperblockHeader {
    fn default() -> Self {
        Self {
            format_version: DEFAULT_PHYSICAL_FORMAT_VERSION,
            sequence: 0,
            copies: DEFAULT_SUPERBLOCK_COPIES,
            manifest: ManifestPointers::default(),
            free_set: BlockReference::default(),
            collection_roots: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestEventKind {
    Insert,
    Update,
    Remove,
    Checkpoint,
}

#[derive(Debug, Clone)]
pub struct ManifestEvent {
    pub collection: String,
    pub object_key: String,
    pub kind: ManifestEventKind,
    pub block: BlockReference,
    pub snapshot_min: u64,
    pub snapshot_max: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct SnapshotDescriptor {
    pub snapshot_id: u64,
    pub created_at_unix_ms: u128,
    pub superblock_sequence: u64,
    pub collection_count: usize,
    pub total_entities: usize,
}

#[derive(Debug, Clone)]
pub struct ExportDescriptor {
    pub name: String,
    pub created_at_unix_ms: u128,
    pub snapshot_id: Option<u64>,
    pub superblock_sequence: u64,
    pub data_path: String,
    pub metadata_path: String,
    pub collection_count: usize,
    pub total_entities: usize,
}

#[derive(Debug, Clone)]
pub struct PhysicalGraphProjection {
    pub name: String,
    pub created_at_unix_ms: u128,
    pub updated_at_unix_ms: u128,
    pub state: String,
    pub source: String,
    pub node_labels: Vec<String>,
    pub node_types: Vec<String>,
    pub edge_labels: Vec<String>,
    pub last_materialized_sequence: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct PhysicalAnalyticsJob {
    pub id: String,
    pub kind: String,
    pub state: String,
    pub projection: Option<String>,
    pub created_at_unix_ms: u128,
    pub updated_at_unix_ms: u128,
    pub last_run_sequence: Option<u64>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct PhysicalTreeDefinition {
    pub collection: String,
    pub name: String,
    pub root_id: u64,
    pub default_max_children: usize,
    pub ordered_children: bool,
    pub ownership: String,
    pub auto_fix_mode: String,
    pub created_at_unix_ms: u128,
    pub updated_at_unix_ms: u128,
}

#[derive(Debug, Clone)]
pub struct PersistedPhysicalIndexState {
    pub name: String,
    pub kind: String,
    pub collection: Option<String>,
    pub enabled: bool,
    pub entries: usize,
    pub estimated_memory_bytes: u64,
    pub last_refresh_ms: Option<u128>,
    pub backend: String,
    pub artifact_kind: Option<String>,
    pub artifact_root_page: Option<u32>,
    pub artifact_checksum: Option<u64>,
    pub build_state: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalPageLocation {
    pub page_id: u32,
    pub offset: u32,
    pub length: u32,
}

#[derive(Debug, Clone)]
pub struct PersistedPhysicalHypertableChunk {
    pub start_ns: u64,
    pub end_ns_exclusive: u64,
    pub row_count: u64,
    pub min_ts_ns: u64,
    pub max_ts_ns: u64,
    pub sealed: bool,
    pub ttl_override_ns: Option<u64>,
    pub columnar_page: Option<PhysicalPageLocation>,
}

#[derive(Debug, Clone)]
pub struct PersistedPhysicalHypertable {
    pub name: String,
    pub time_column: String,
    pub chunk_interval_ns: u64,
    pub default_ttl_ns: Option<u64>,
    pub chunks: Vec<PersistedPhysicalHypertableChunk>,
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

fn parse_json_value(json: &str, label: &'static str) -> RdbFileResult<serde_json::Value> {
    serde_json::from_str(json)
        .map_err(|err| RdbFileError::InvalidOperation(format!("invalid {label}: {err}")))
}

fn expect_object<'a>(
    value: &'a serde_json::Value,
    context: &'static str,
) -> RdbFileResult<&'a serde_json::Map<String, serde_json::Value>> {
    value
        .as_object()
        .ok_or_else(|| invalid(format!("{context} must be an object")))
}

fn required<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<&'a serde_json::Value> {
    object
        .get(key)
        .ok_or_else(|| invalid(format!("missing field '{key}'")))
}

fn json_string_required(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<String> {
    required(object, key)?
        .as_str()
        .map(ToString::to_string)
        .ok_or_else(|| invalid(format!("field '{key}' must be a string")))
}

fn json_bool_required(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<bool> {
    required(object, key)?
        .as_bool()
        .ok_or_else(|| invalid(format!("field '{key}' must be a bool")))
}

fn json_u8_required(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<u8> {
    let value = required(object, key)?;
    if let Some(text) = value.as_str() {
        return text
            .parse::<u8>()
            .map_err(|_| invalid("invalid u8 string value"));
    }
    value
        .as_u64()
        .and_then(|value| u8::try_from(value).ok())
        .ok_or_else(|| invalid("invalid u8 value"))
}

fn json_u32_required(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<u32> {
    let value = required(object, key)?;
    if let Some(text) = value.as_str() {
        return text
            .parse::<u32>()
            .map_err(|_| invalid("invalid u32 string value"));
    }
    value
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| invalid("invalid u32 value"))
}

fn json_u64_required(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<u64> {
    json_u64_value(required(object, key)?)
}

fn json_u128_required(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<u128> {
    json_u128_value(required(object, key)?)
}

fn json_usize_required(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> RdbFileResult<usize> {
    let value = required(object, key)?;
    if let Some(text) = value.as_str() {
        return text
            .parse::<usize>()
            .map_err(|_| invalid("invalid usize string value"));
    }
    value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| invalid("invalid usize value"))
}

fn json_u64_value(value: &serde_json::Value) -> RdbFileResult<u64> {
    if let Some(text) = value.as_str() {
        return text
            .parse::<u64>()
            .map_err(|_| invalid("invalid u64 string value"));
    }
    value.as_u64().ok_or_else(|| invalid("invalid u64 value"))
}

fn json_u128_value(value: &serde_json::Value) -> RdbFileResult<u128> {
    if let Some(text) = value.as_str() {
        return text
            .parse::<u128>()
            .map_err(|_| invalid("invalid u128 string value"));
    }
    value
        .as_u64()
        .map(u128::from)
        .ok_or_else(|| invalid("invalid u128 value"))
}

fn json_u64(value: u64) -> serde_json::Value {
    serde_json::Value::String(value.to_string())
}

fn json_u128(value: u128) -> serde_json::Value {
    serde_json::Value::String(value.to_string())
}

fn json_usize(value: usize) -> serde_json::Value {
    serde_json::Value::Number((value as u64).into())
}

fn string_array_json(values: &[String]) -> serde_json::Value {
    serde_json::Value::Array(
        values
            .iter()
            .cloned()
            .map(serde_json::Value::String)
            .collect(),
    )
}

fn string_array_from_json(value: Option<&serde_json::Value>) -> Option<Vec<String>> {
    value.and_then(serde_json::Value::as_array).map(|values| {
        values
            .iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect()
    })
}

fn invalid(message: impl Into<String>) -> RdbFileError {
    RdbFileError::InvalidOperation(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_metadata_documents_validate_json_before_publish() {
        assert!(encode_physical_metadata_json_document(r#"{"ok":true}"#).is_ok());
        assert!(encode_physical_metadata_binary_document(r#"{"ok":true}"#).is_ok());
        assert!(encode_physical_metadata_json_document("{").is_err());
        assert!(encode_physical_metadata_binary_document("{").is_err());
    }

    #[test]
    fn physical_metadata_document_decode_rejects_invalid_json() {
        assert_eq!(
            decode_physical_metadata_document(br#"{"sequence":1}"#).unwrap(),
            r#"{"sequence":1}"#
        );
        assert!(decode_physical_metadata_document(b"not-json").is_err());
    }

    #[test]
    fn physical_metadata_core_contracts_round_trip() {
        let mut roots = BTreeMap::new();
        roots.insert("docs".to_string(), 42);
        let superblock = SuperblockHeader {
            format_version: 2,
            sequence: u64::MAX,
            copies: 4,
            manifest: ManifestPointers {
                oldest: BlockReference {
                    index: 1,
                    checksum: u128::MAX,
                },
                newest: BlockReference {
                    index: 2,
                    checksum: 99,
                },
            },
            free_set: BlockReference {
                index: 3,
                checksum: 100,
            },
            collection_roots: roots,
        };
        let json = encode_physical_superblock_json(&superblock).unwrap();
        assert!(
            json.contains(&format!("\"sequence\":\"{}\"", u64::MAX)),
            "large u64 values stay string-encoded for legacy compatibility: {json}"
        );
        assert!(
            json.contains(&format!("\"checksum\":\"{}\"", u128::MAX)),
            "large u128 values stay string-encoded for legacy compatibility: {json}"
        );
        assert_eq!(
            decode_physical_superblock_json(&json)
                .unwrap()
                .manifest
                .oldest
                .checksum,
            u128::MAX
        );

        let event = ManifestEvent {
            collection: "docs".to_string(),
            object_key: "a".to_string(),
            kind: ManifestEventKind::Checkpoint,
            block: BlockReference {
                index: 7,
                checksum: 8,
            },
            snapshot_min: 9,
            snapshot_max: Some(10),
        };
        let event_json = encode_physical_manifest_event_json(&event).unwrap();
        let decoded = decode_physical_manifest_event_json(&event_json).unwrap();
        assert_eq!(decoded.collection, "docs");
        assert_eq!(decoded.kind, ManifestEventKind::Checkpoint);
        assert_eq!(decoded.snapshot_max, Some(10));

        let snapshot = SnapshotDescriptor {
            snapshot_id: 11,
            created_at_unix_ms: 12,
            superblock_sequence: 13,
            collection_count: 14,
            total_entities: 15,
        };
        let snapshot_json = encode_physical_snapshot_descriptor_json(&snapshot).unwrap();
        assert_eq!(
            decode_physical_snapshot_descriptor_json(&snapshot_json)
                .unwrap()
                .snapshot_id,
            11
        );

        let export = ExportDescriptor {
            name: "daily".to_string(),
            created_at_unix_ms: 16,
            snapshot_id: Some(17),
            superblock_sequence: 18,
            data_path: "data.rdb".to_string(),
            metadata_path: "data.meta.rdbx".to_string(),
            collection_count: 19,
            total_entities: 20,
        };
        let export_json = encode_physical_export_descriptor_json(&export).unwrap();
        let decoded_export = decode_physical_export_descriptor_json(&export_json).unwrap();
        assert_eq!(decoded_export.name, "daily");
        assert_eq!(decoded_export.snapshot_id, Some(17));

        let projection = PhysicalGraphProjection {
            name: "g".to_string(),
            created_at_unix_ms: 21,
            updated_at_unix_ms: 22,
            state: "ready".to_string(),
            source: "docs".to_string(),
            node_labels: vec!["Person".to_string()],
            node_types: vec!["person".to_string()],
            edge_labels: vec!["KNOWS".to_string()],
            last_materialized_sequence: Some(23),
        };
        let projection_json = encode_physical_graph_projection_json(&projection).unwrap();
        let decoded_projection = decode_physical_graph_projection_json(&projection_json).unwrap();
        assert_eq!(decoded_projection.node_labels, vec!["Person"]);
        assert_eq!(decoded_projection.last_materialized_sequence, Some(23));

        let mut metadata = BTreeMap::new();
        metadata.insert("k".to_string(), "v".to_string());
        let job = PhysicalAnalyticsJob {
            id: "job".to_string(),
            kind: "materialize".to_string(),
            state: "queued".to_string(),
            projection: Some("g".to_string()),
            created_at_unix_ms: 24,
            updated_at_unix_ms: 25,
            last_run_sequence: Some(26),
            metadata,
        };
        let job_json = encode_physical_analytics_job_json(&job).unwrap();
        let decoded_job = decode_physical_analytics_job_json(&job_json).unwrap();
        assert_eq!(decoded_job.projection.as_deref(), Some("g"));
        assert_eq!(decoded_job.metadata.get("k").map(String::as_str), Some("v"));

        let tree = PhysicalTreeDefinition {
            collection: "docs".to_string(),
            name: "comments".to_string(),
            root_id: 27,
            default_max_children: 28,
            ordered_children: true,
            ownership: "owned".to_string(),
            auto_fix_mode: "conservative".to_string(),
            created_at_unix_ms: 29,
            updated_at_unix_ms: 30,
        };
        let tree_json = encode_physical_tree_definition_json(&tree).unwrap();
        let decoded_tree = decode_physical_tree_definition_json(&tree_json).unwrap();
        assert_eq!(decoded_tree.root_id, 27);
        assert!(decoded_tree.ordered_children);

        let index = PersistedPhysicalIndexState {
            name: "idx_docs".to_string(),
            kind: "btree".to_string(),
            collection: Some("docs".to_string()),
            enabled: true,
            entries: 31,
            estimated_memory_bytes: 32,
            last_refresh_ms: Some(33),
            backend: "native".to_string(),
            artifact_kind: Some("btree".to_string()),
            artifact_root_page: Some(34),
            artifact_checksum: Some(35),
            build_state: "ready".to_string(),
        };
        let index_json = encode_persisted_physical_index_state_json(&index).unwrap();
        let decoded_index = decode_persisted_physical_index_state_json(&index_json).unwrap();
        assert_eq!(decoded_index.kind, "btree");
        assert_eq!(decoded_index.artifact_checksum, Some(35));

        let hypertable = PersistedPhysicalHypertable {
            name: "metrics".to_string(),
            time_column: "ts".to_string(),
            chunk_interval_ns: 36,
            default_ttl_ns: Some(37),
            chunks: vec![PersistedPhysicalHypertableChunk {
                start_ns: 38,
                end_ns_exclusive: 39,
                row_count: 40,
                min_ts_ns: 41,
                max_ts_ns: 42,
                sealed: true,
                ttl_override_ns: Some(43),
                columnar_page: Some(PhysicalPageLocation {
                    page_id: 44,
                    offset: 45,
                    length: 46,
                }),
            }],
        };
        let hypertable_json = encode_persisted_physical_hypertable_json(&hypertable).unwrap();
        let decoded_hypertable =
            decode_persisted_physical_hypertable_json(&hypertable_json).unwrap();
        assert_eq!(decoded_hypertable.name, "metrics");
        assert_eq!(
            decoded_hypertable.chunks[0].columnar_page,
            Some(PhysicalPageLocation {
                page_id: 44,
                offset: 45,
                length: 46,
            })
        );
    }
}
