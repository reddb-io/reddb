//! Physical storage design primitives inspired by TigerBeetle's layout.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::{
    CatalogSnapshot, CollectionStats, RedDBOptions, SchemaManifest, StorageMode,
    REDDB_FORMAT_VERSION,
};
use crate::index::IndexKind;
use crate::json::parse_json;
use crate::serde_json::{Map, Value as JsonValue};

pub const DEFAULT_GRID_BLOCK_SIZE: usize = 512 * 1024;
pub const DEFAULT_PAGE_SIZE: usize = 4096;
pub const DEFAULT_SUPERBLOCK_COPIES: u8 = 4;
pub const PHYSICAL_METADATA_PROTOCOL_VERSION: &str = "reddb-physical-v1";
pub const DEFAULT_MANIFEST_EVENT_HISTORY: usize = 256;
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
            format_version: crate::api::REDDB_FORMAT_VERSION,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionPolicy {
    Incremental,
    Manual,
}

#[derive(Debug, Clone)]
pub struct WalPolicy {
    pub auto_checkpoint_pages: u32,
    pub fsync_on_commit: bool,
    pub ring_buffer_bytes: u64,
}

impl Default for WalPolicy {
    fn default() -> Self {
        Self {
            auto_checkpoint_pages: 1000,
            fsync_on_commit: true,
            ring_buffer_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GridLayout {
    pub block_size: usize,
    pub page_size: usize,
    pub superblock_copies: u8,
}

impl Default for GridLayout {
    fn default() -> Self {
        Self {
            block_size: DEFAULT_GRID_BLOCK_SIZE,
            page_size: DEFAULT_PAGE_SIZE,
            superblock_copies: DEFAULT_SUPERBLOCK_COPIES,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PhysicalLayout {
    pub mode: StorageMode,
    pub grid: GridLayout,
    pub wal: WalPolicy,
    pub compaction: CompactionPolicy,
}

impl PhysicalLayout {
    pub fn from_options(options: &RedDBOptions) -> Self {
        Self {
            mode: options.mode,
            grid: GridLayout::default(),
            wal: WalPolicy {
                auto_checkpoint_pages: options.auto_checkpoint_pages,
                ..WalPolicy::default()
            },
            compaction: CompactionPolicy::Incremental,
        }
    }

    pub fn is_persistent(&self) -> bool {
        self.mode == StorageMode::Persistent
    }
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
pub struct PhysicalIndexState {
    pub name: String,
    pub kind: IndexKind,
    pub collection: Option<String>,
    pub enabled: bool,
    pub entries: usize,
    pub estimated_memory_bytes: u64,
    pub last_refresh_ms: Option<u128>,
    pub backend: String,
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
pub struct PhysicalMetadataFile {
    pub protocol_version: String,
    pub generated_at_unix_ms: u128,
    pub manifest: SchemaManifest,
    pub catalog: CatalogSnapshot,
    pub manifest_events: Vec<ManifestEvent>,
    pub indexes: Vec<PhysicalIndexState>,
    pub graph_projections: Vec<PhysicalGraphProjection>,
    pub analytics_jobs: Vec<PhysicalAnalyticsJob>,
    pub exports: Vec<ExportDescriptor>,
    pub superblock: SuperblockHeader,
    pub snapshots: Vec<SnapshotDescriptor>,
}

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
            exports: previous.map(|previous| previous.exports.clone()).unwrap_or_default(),
            superblock,
            snapshots,
        }
    }

    pub fn metadata_path_for(data_path: &Path) -> PathBuf {
        let file_name = data_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "reddb.rdb".to_string());
        let meta_file = format!("{file_name}.meta.json");
        match data_path.parent() {
            Some(parent) => parent.join(meta_file),
            None => PathBuf::from(meta_file),
        }
    }

    pub fn export_data_path_for(data_path: &Path, name: &str) -> PathBuf {
        let file_name = data_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "reddb.rdb".to_string());
        let stem = file_name.strip_suffix(".rdb").unwrap_or(&file_name);
        let export_file = format!("{stem}.export.{}.rdb", sanitize_export_name(name));
        match data_path.parent() {
            Some(parent) => parent.join(export_file),
            None => PathBuf::from(export_file),
        }
    }

    pub fn load_for_data_path(data_path: &Path) -> io::Result<Self> {
        Self::load_from_path(&Self::metadata_path_for(data_path))
    }

    pub fn save_for_data_path(&self, data_path: &Path) -> io::Result<PathBuf> {
        let path = Self::metadata_path_for(data_path);
        self.save_to_path(&path)?;
        Ok(path)
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
            "exports".to_string(),
            JsonValue::Array(self.exports.iter().map(export_descriptor_to_json).collect()),
        );
        root.insert("superblock".to_string(), superblock_to_json(&self.superblock));
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

fn manifest_to_json(manifest: &SchemaManifest) -> JsonValue {
    let mut options = Map::new();
    options.insert(
        "mode".to_string(),
        JsonValue::String(match manifest.options.mode {
            StorageMode::Persistent => "persistent".to_string(),
            StorageMode::InMemory => "in_memory".to_string(),
        }),
    );
    options.insert(
        "data_path".to_string(),
        match &manifest.options.data_path {
            Some(path) => JsonValue::String(path.display().to_string()),
            None => JsonValue::Null,
        },
    );
    options.insert(
        "read_only".to_string(),
        JsonValue::Bool(manifest.options.read_only),
    );
    options.insert(
        "create_if_missing".to_string(),
        JsonValue::Bool(manifest.options.create_if_missing),
    );
    options.insert(
        "verify_checksums".to_string(),
        JsonValue::Bool(manifest.options.verify_checksums),
    );
    options.insert(
        "auto_checkpoint_pages".to_string(),
        JsonValue::Number(manifest.options.auto_checkpoint_pages as f64),
    );
    options.insert(
        "cache_pages".to_string(),
        JsonValue::Number(manifest.options.cache_pages as f64),
    );
    options.insert(
        "snapshot_retention".to_string(),
        JsonValue::Number(manifest.options.snapshot_retention as f64),
    );
    options.insert(
        "export_retention".to_string(),
        JsonValue::Number(manifest.options.export_retention as f64),
    );
    options.insert(
        "force_create".to_string(),
        JsonValue::Bool(manifest.options.force_create),
    );
    options.insert(
        "capabilities".to_string(),
        JsonValue::Array(
            manifest
                .options
                .feature_gates
                .as_slice()
                .into_iter()
                .map(|capability| JsonValue::String(capability.as_str().to_string()))
                .collect(),
        ),
    );
    options.insert(
        "metadata".to_string(),
        JsonValue::Object(
            manifest
                .options
                .metadata
                .iter()
                .map(|(key, value)| (key.clone(), JsonValue::String(value.clone())))
                .collect(),
        ),
    );

    let mut object = Map::new();
    object.insert(
        "format_version".to_string(),
        JsonValue::Number(manifest.format_version as f64),
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
        JsonValue::Number(manifest.collection_count as f64),
    );
    object.insert("options".to_string(), JsonValue::Object(options));
    JsonValue::Object(object)
}

fn manifest_from_json(value: &JsonValue) -> io::Result<SchemaManifest> {
    let object = expect_object(value, "manifest")?;
    let options_object = expect_object(json_required(object, "options")?, "manifest.options")?;
    let mut options = RedDBOptions::default();
    options.mode = match json_string_required(options_object, "mode")?.as_str() {
        "persistent" => StorageMode::Persistent,
        "in_memory" => StorageMode::InMemory,
        other => {
            return Err(invalid_data(format!(
                "unsupported storage mode in manifest: {other}"
            )))
        }
    };
    options.data_path = options_object
        .get("data_path")
        .and_then(JsonValue::as_str)
        .map(PathBuf::from);
    options.read_only = json_bool_required(options_object, "read_only")?;
    options.create_if_missing = json_bool_required(options_object, "create_if_missing")?;
    options.verify_checksums = json_bool_required(options_object, "verify_checksums")?;
    options.auto_checkpoint_pages =
        json_u32_required(options_object, "auto_checkpoint_pages")?;
    options.cache_pages = json_usize_required(options_object, "cache_pages")?;
    options.snapshot_retention = options_object
        .get("snapshot_retention")
        .map(|value| json_usize_required(options_object, "snapshot_retention"))
        .transpose()?
        .unwrap_or(crate::api::DEFAULT_SNAPSHOT_RETENTION)
        .max(1);
    options.export_retention = options_object
        .get("export_retention")
        .map(|value| json_usize_required(options_object, "export_retention"))
        .transpose()?
        .unwrap_or(crate::api::DEFAULT_EXPORT_RETENTION)
        .max(1);
    options.force_create = json_bool_required(options_object, "force_create")?;
    options.metadata = options_object
        .get("metadata")
        .and_then(JsonValue::as_object)
        .map(|metadata| {
            metadata
                .iter()
                .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_string())))
                .collect()
        })
        .unwrap_or_default();
    if let Some(capabilities) = options_object.get("capabilities").and_then(JsonValue::as_array) {
        options.feature_gates = capabilities.iter().fold(Default::default(), |set, value| {
            match value.as_str() {
                Some("table") => set.with(crate::api::Capability::Table),
                Some("graph") => set.with(crate::api::Capability::Graph),
                Some("vector") => set.with(crate::api::Capability::Vector),
                Some("fulltext") => set.with(crate::api::Capability::FullText),
                Some("security") => set.with(crate::api::Capability::Security),
                Some("encryption") => set.with(crate::api::Capability::Encryption),
                _ => set,
            }
        });
    }

    Ok(SchemaManifest {
        format_version: json_u32_required(object, "format_version")?,
        created_at_unix_ms: json_u128_required(object, "created_at_unix_ms")?,
        updated_at_unix_ms: json_u128_required(object, "updated_at_unix_ms")?,
        options,
        collection_count: json_usize_required(object, "collection_count")?,
    })
}

fn catalog_to_json(catalog: &CatalogSnapshot) -> JsonValue {
    let mut stats = Map::new();
    for (name, stat) in &catalog.stats_by_collection {
        let mut entry = Map::new();
        entry.insert("entities".to_string(), JsonValue::Number(stat.entities as f64));
        entry.insert(
            "cross_refs".to_string(),
            JsonValue::Number(stat.cross_refs as f64),
        );
        entry.insert("segments".to_string(), JsonValue::Number(stat.segments as f64));
        stats.insert(name.clone(), JsonValue::Object(entry));
    }

    let mut object = Map::new();
    object.insert("name".to_string(), JsonValue::String(catalog.name.clone()));
    object.insert(
        "total_entities".to_string(),
        JsonValue::Number(catalog.total_entities as f64),
    );
    object.insert(
        "total_collections".to_string(),
        JsonValue::Number(catalog.total_collections as f64),
    );
    object.insert(
        "updated_at_unix_ms".to_string(),
        json_u128(
            catalog
                .updated_at
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        ),
    );
    object.insert("stats_by_collection".to_string(), JsonValue::Object(stats));
    JsonValue::Object(object)
}

fn catalog_from_json(value: &JsonValue) -> io::Result<CatalogSnapshot> {
    let object = expect_object(value, "catalog")?;
    let stats = expect_object(json_required(object, "stats_by_collection")?, "catalog.stats")?;
    let mut stats_by_collection = BTreeMap::new();
    for (name, value) in stats {
        let entry = expect_object(value, "catalog.stats entry")?;
        stats_by_collection.insert(
            name.clone(),
            CollectionStats {
                entities: json_usize_required(entry, "entities")?,
                cross_refs: json_usize_required(entry, "cross_refs")?,
                segments: json_usize_required(entry, "segments")?,
            },
        );
    }

    Ok(CatalogSnapshot {
        name: json_string_required(object, "name")?,
        total_entities: json_usize_required(object, "total_entities")?,
        total_collections: json_usize_required(object, "total_collections")?,
        stats_by_collection,
        updated_at: UNIX_EPOCH + std::time::Duration::from_millis(
            json_u128_required(object, "updated_at_unix_ms")?
                .try_into()
                .unwrap_or(u64::MAX),
        ),
    })
}

fn superblock_to_json(superblock: &SuperblockHeader) -> JsonValue {
    let mut collection_roots = Map::new();
    for (name, root) in &superblock.collection_roots {
        collection_roots.insert(name.clone(), json_u64(*root));
    }

    let mut object = Map::new();
    object.insert(
        "format_version".to_string(),
        JsonValue::Number(superblock.format_version as f64),
    );
    object.insert("sequence".to_string(), json_u64(superblock.sequence));
    object.insert(
        "copies".to_string(),
        JsonValue::Number(superblock.copies as f64),
    );
    object.insert(
        "manifest".to_string(),
        manifest_pointers_to_json(&superblock.manifest),
    );
    object.insert(
        "free_set".to_string(),
        block_reference_to_json(superblock.free_set),
    );
    object.insert(
        "collection_roots".to_string(),
        JsonValue::Object(collection_roots),
    );
    JsonValue::Object(object)
}

fn superblock_from_json(value: &JsonValue) -> io::Result<SuperblockHeader> {
    let object = expect_object(value, "superblock")?;
    let roots = expect_object(json_required(object, "collection_roots")?, "superblock.roots")?;
    let mut collection_roots = BTreeMap::new();
    for (name, root) in roots {
        collection_roots.insert(name.clone(), json_u64_value(root)?);
    }

    Ok(SuperblockHeader {
        format_version: json_u32_required(object, "format_version")?,
        sequence: json_u64_required(object, "sequence")?,
        copies: json_u8_required(object, "copies")?,
        manifest: manifest_pointers_from_json(json_required(object, "manifest")?)?,
        free_set: block_reference_from_json(json_required(object, "free_set")?)?,
        collection_roots,
    })
}

fn manifest_event_to_json(event: &ManifestEvent) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "collection".to_string(),
        JsonValue::String(event.collection.clone()),
    );
    object.insert(
        "object_key".to_string(),
        JsonValue::String(event.object_key.clone()),
    );
    object.insert(
        "kind".to_string(),
        JsonValue::String(
            match event.kind {
                ManifestEventKind::Insert => "insert",
                ManifestEventKind::Update => "update",
                ManifestEventKind::Remove => "remove",
                ManifestEventKind::Checkpoint => "checkpoint",
            }
            .to_string(),
        ),
    );
    object.insert("block".to_string(), block_reference_to_json(event.block));
    object.insert("snapshot_min".to_string(), json_u64(event.snapshot_min));
    object.insert(
        "snapshot_max".to_string(),
        match event.snapshot_max {
            Some(value) => json_u64(value),
            None => JsonValue::Null,
        },
    );
    JsonValue::Object(object)
}

fn manifest_event_from_json(value: &JsonValue) -> io::Result<ManifestEvent> {
    let object = expect_object(value, "manifest event")?;
    Ok(ManifestEvent {
        collection: json_string_required(object, "collection")?,
        object_key: json_string_required(object, "object_key")?,
        kind: match json_string_required(object, "kind")?.as_str() {
            "insert" => ManifestEventKind::Insert,
            "update" => ManifestEventKind::Update,
            "remove" => ManifestEventKind::Remove,
            "checkpoint" => ManifestEventKind::Checkpoint,
            other => return Err(invalid_data(format!("unsupported manifest event kind '{other}'"))),
        },
        block: block_reference_from_json(json_required(object, "block")?)?,
        snapshot_min: json_u64_required(object, "snapshot_min")?,
        snapshot_max: object
            .get("snapshot_max")
            .and_then(|value| json_u64_value(value).ok()),
    })
}

fn manifest_pointers_to_json(pointers: &ManifestPointers) -> JsonValue {
    let mut object = Map::new();
    object.insert("oldest".to_string(), block_reference_to_json(pointers.oldest));
    object.insert("newest".to_string(), block_reference_to_json(pointers.newest));
    JsonValue::Object(object)
}

fn manifest_pointers_from_json(value: &JsonValue) -> io::Result<ManifestPointers> {
    let object = expect_object(value, "manifest pointers")?;
    Ok(ManifestPointers {
        oldest: block_reference_from_json(json_required(object, "oldest")?)?,
        newest: block_reference_from_json(json_required(object, "newest")?)?,
    })
}

fn block_reference_to_json(reference: BlockReference) -> JsonValue {
    let mut object = Map::new();
    object.insert("index".to_string(), json_u64(reference.index));
    object.insert("checksum".to_string(), json_u128(reference.checksum));
    JsonValue::Object(object)
}

fn block_reference_from_json(value: &JsonValue) -> io::Result<BlockReference> {
    let object = expect_object(value, "block reference")?;
    Ok(BlockReference {
        index: json_u64_required(object, "index")?,
        checksum: json_u128_required(object, "checksum")?,
    })
}

fn snapshot_descriptor_to_json(snapshot: &SnapshotDescriptor) -> JsonValue {
    let mut object = Map::new();
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
        JsonValue::Number(snapshot.collection_count as f64),
    );
    object.insert(
        "total_entities".to_string(),
        JsonValue::Number(snapshot.total_entities as f64),
    );
    JsonValue::Object(object)
}

fn snapshot_descriptor_from_json(value: &JsonValue) -> io::Result<SnapshotDescriptor> {
    let object = expect_object(value, "snapshot descriptor")?;
    Ok(SnapshotDescriptor {
        snapshot_id: json_u64_required(object, "snapshot_id")?,
        created_at_unix_ms: json_u128_required(object, "created_at_unix_ms")?,
        superblock_sequence: json_u64_required(object, "superblock_sequence")?,
        collection_count: json_usize_required(object, "collection_count")?,
        total_entities: json_usize_required(object, "total_entities")?,
    })
}

fn index_state_to_json(index: &PhysicalIndexState) -> JsonValue {
    let mut object = Map::new();
    object.insert("name".to_string(), JsonValue::String(index.name.clone()));
    object.insert(
        "kind".to_string(),
        JsonValue::String(index.kind.as_str().to_string()),
    );
    object.insert(
        "collection".to_string(),
        match &index.collection {
            Some(collection) => JsonValue::String(collection.clone()),
            None => JsonValue::Null,
        },
    );
    object.insert("enabled".to_string(), JsonValue::Bool(index.enabled));
    object.insert("entries".to_string(), JsonValue::Number(index.entries as f64));
    object.insert(
        "estimated_memory_bytes".to_string(),
        json_u64(index.estimated_memory_bytes),
    );
    object.insert(
        "last_refresh_ms".to_string(),
        match index.last_refresh_ms {
            Some(value) => json_u128(value),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "backend".to_string(),
        JsonValue::String(index.backend.clone()),
    );
    JsonValue::Object(object)
}

fn index_state_from_json(value: &JsonValue) -> io::Result<PhysicalIndexState> {
    let object = expect_object(value, "physical index state")?;
    Ok(PhysicalIndexState {
        name: json_string_required(object, "name")?,
        kind: index_kind_from_str(&json_string_required(object, "kind")?)?,
        collection: object
            .get("collection")
            .and_then(JsonValue::as_str)
            .map(|value| value.to_string()),
        enabled: json_bool_required(object, "enabled")?,
        entries: json_usize_required(object, "entries")?,
        estimated_memory_bytes: json_u64_required(object, "estimated_memory_bytes")?,
        last_refresh_ms: object
            .get("last_refresh_ms")
            .and_then(|value| json_u128_value(value).ok()),
        backend: json_string_required(object, "backend")?,
    })
}

fn graph_projection_to_json(projection: &PhysicalGraphProjection) -> JsonValue {
    let mut object = Map::new();
    object.insert("name".to_string(), JsonValue::String(projection.name.clone()));
    object.insert(
        "created_at_unix_ms".to_string(),
        json_u128(projection.created_at_unix_ms),
    );
    object.insert(
        "updated_at_unix_ms".to_string(),
        json_u128(projection.updated_at_unix_ms),
    );
    object.insert(
        "source".to_string(),
        JsonValue::String(projection.source.clone()),
    );
    object.insert(
        "node_labels".to_string(),
        JsonValue::Array(
            projection
                .node_labels
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "node_types".to_string(),
        JsonValue::Array(
            projection
                .node_types
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "edge_labels".to_string(),
        JsonValue::Array(
            projection
                .edge_labels
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "last_materialized_sequence".to_string(),
        match projection.last_materialized_sequence {
            Some(value) => json_u64(value),
            None => JsonValue::Null,
        },
    );
    JsonValue::Object(object)
}

fn graph_projection_from_json(value: &JsonValue) -> io::Result<PhysicalGraphProjection> {
    let object = expect_object(value, "graph projection")?;
    Ok(PhysicalGraphProjection {
        name: json_string_required(object, "name")?,
        created_at_unix_ms: json_u128_required(object, "created_at_unix_ms")?,
        updated_at_unix_ms: json_u128_required(object, "updated_at_unix_ms")?,
        source: json_string_required(object, "source")?,
        node_labels: object
            .get("node_labels")
            .and_then(JsonValue::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        node_types: object
            .get("node_types")
            .and_then(JsonValue::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        edge_labels: object
            .get("edge_labels")
            .and_then(JsonValue::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        last_materialized_sequence: object
            .get("last_materialized_sequence")
            .and_then(|value| json_u64_value(value).ok()),
    })
}

fn analytics_job_to_json(job: &PhysicalAnalyticsJob) -> JsonValue {
    let mut object = Map::new();
    object.insert("id".to_string(), JsonValue::String(job.id.clone()));
    object.insert("kind".to_string(), JsonValue::String(job.kind.clone()));
    object.insert("state".to_string(), JsonValue::String(job.state.clone()));
    object.insert(
        "projection".to_string(),
        match &job.projection {
            Some(value) => JsonValue::String(value.clone()),
            None => JsonValue::Null,
        },
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
        match job.last_run_sequence {
            Some(value) => json_u64(value),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "metadata".to_string(),
        JsonValue::Object(
            job.metadata
                .iter()
                .map(|(key, value)| (key.clone(), JsonValue::String(value.clone())))
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

fn analytics_job_from_json(value: &JsonValue) -> io::Result<PhysicalAnalyticsJob> {
    let object = expect_object(value, "analytics job")?;
    Ok(PhysicalAnalyticsJob {
        id: json_string_required(object, "id")?,
        kind: json_string_required(object, "kind")?,
        state: json_string_required(object, "state")?,
        projection: object
            .get("projection")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        created_at_unix_ms: json_u128_required(object, "created_at_unix_ms")?,
        updated_at_unix_ms: json_u128_required(object, "updated_at_unix_ms")?,
        last_run_sequence: object
            .get("last_run_sequence")
            .and_then(|value| json_u64_value(value).ok()),
        metadata: object
            .get("metadata")
            .and_then(JsonValue::as_object)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_string())))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn index_kind_from_str(value: &str) -> io::Result<IndexKind> {
    match value {
        "btree" => Ok(IndexKind::BTree),
        "vector.hnsw" => Ok(IndexKind::VectorHnsw),
        "vector.inverted" => Ok(IndexKind::VectorInverted),
        "graph.adjacency" => Ok(IndexKind::GraphAdjacency),
        "text.fulltext" => Ok(IndexKind::FullText),
        "search.hybrid" => Ok(IndexKind::HybridSearch),
        other => Err(invalid_data(format!("unsupported index kind '{other}'"))),
    }
}

fn export_descriptor_to_json(export: &ExportDescriptor) -> JsonValue {
    let mut object = Map::new();
    object.insert("name".to_string(), JsonValue::String(export.name.clone()));
    object.insert(
        "created_at_unix_ms".to_string(),
        json_u128(export.created_at_unix_ms),
    );
    object.insert(
        "snapshot_id".to_string(),
        match export.snapshot_id {
            Some(snapshot_id) => json_u64(snapshot_id),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "superblock_sequence".to_string(),
        json_u64(export.superblock_sequence),
    );
    object.insert(
        "data_path".to_string(),
        JsonValue::String(export.data_path.clone()),
    );
    object.insert(
        "metadata_path".to_string(),
        JsonValue::String(export.metadata_path.clone()),
    );
    object.insert(
        "collection_count".to_string(),
        JsonValue::Number(export.collection_count as f64),
    );
    object.insert(
        "total_entities".to_string(),
        JsonValue::Number(export.total_entities as f64),
    );
    JsonValue::Object(object)
}

fn export_descriptor_from_json(value: &JsonValue) -> io::Result<ExportDescriptor> {
    let object = expect_object(value, "export descriptor")?;
    Ok(ExportDescriptor {
        name: json_string_required(object, "name")?,
        created_at_unix_ms: json_u128_required(object, "created_at_unix_ms")?,
        snapshot_id: object
            .get("snapshot_id")
            .and_then(|value| json_u64_value(value).ok()),
        superblock_sequence: json_u64_required(object, "superblock_sequence")?,
        data_path: json_string_required(object, "data_path")?,
        metadata_path: json_string_required(object, "metadata_path")?,
        collection_count: json_usize_required(object, "collection_count")?,
        total_entities: json_usize_required(object, "total_entities")?,
    })
}

fn json_u64(value: u64) -> JsonValue {
    JsonValue::String(value.to_string())
}

fn json_u128(value: u128) -> JsonValue {
    JsonValue::String(value.to_string())
}

fn json_required<'a>(
    object: &'a Map<String, JsonValue>,
    key: &str,
) -> io::Result<&'a JsonValue> {
    object
        .get(key)
        .ok_or_else(|| invalid_data(format!("missing field '{key}'")))
}

fn json_string_required(object: &Map<String, JsonValue>, key: &str) -> io::Result<String> {
    json_required(object, key)?
        .as_str()
        .map(|value| value.to_string())
        .ok_or_else(|| invalid_data(format!("field '{key}' must be a string")))
}

fn json_bool_required(object: &Map<String, JsonValue>, key: &str) -> io::Result<bool> {
    json_required(object, key)?
        .as_bool()
        .ok_or_else(|| invalid_data(format!("field '{key}' must be a bool")))
}

fn json_u8_required(object: &Map<String, JsonValue>, key: &str) -> io::Result<u8> {
    json_u8_value(json_required(object, key)?)
}

fn json_u32_required(object: &Map<String, JsonValue>, key: &str) -> io::Result<u32> {
    json_u32_value(json_required(object, key)?)
}

fn json_u64_required(object: &Map<String, JsonValue>, key: &str) -> io::Result<u64> {
    json_u64_value(json_required(object, key)?)
}

fn json_u128_required(object: &Map<String, JsonValue>, key: &str) -> io::Result<u128> {
    json_u128_value(json_required(object, key)?)
}

fn json_usize_required(object: &Map<String, JsonValue>, key: &str) -> io::Result<usize> {
    json_usize_value(json_required(object, key)?)
}

fn json_u8_value(value: &JsonValue) -> io::Result<u8> {
    if let Some(text) = value.as_str() {
        return text
            .parse::<u8>()
            .map_err(|_| invalid_data("invalid u8 string value"));
    }
    value
        .as_i64()
        .and_then(|value| u8::try_from(value).ok())
        .ok_or_else(|| invalid_data("invalid u8 value"))
}

fn json_u32_value(value: &JsonValue) -> io::Result<u32> {
    if let Some(text) = value.as_str() {
        return text
            .parse::<u32>()
            .map_err(|_| invalid_data("invalid u32 string value"));
    }
    value
        .as_i64()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| invalid_data("invalid u32 value"))
}

fn json_u64_value(value: &JsonValue) -> io::Result<u64> {
    if let Some(text) = value.as_str() {
        return text
            .parse::<u64>()
            .map_err(|_| invalid_data("invalid u64 string value"));
    }
    value
        .as_i64()
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| invalid_data("invalid u64 value"))
}

fn json_u128_value(value: &JsonValue) -> io::Result<u128> {
    if let Some(text) = value.as_str() {
        return text
            .parse::<u128>()
            .map_err(|_| invalid_data("invalid u128 string value"));
    }
    value
        .as_i64()
        .and_then(|value| u128::try_from(value).ok())
        .ok_or_else(|| invalid_data("invalid u128 value"))
}

fn json_usize_value(value: &JsonValue) -> io::Result<usize> {
    if let Some(text) = value.as_str() {
        return text
            .parse::<usize>()
            .map_err(|_| invalid_data("invalid usize string value"));
    }
    value
        .as_i64()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| invalid_data("invalid usize value"))
}

fn expect_object<'a>(
    value: &'a JsonValue,
    context: &str,
) -> io::Result<&'a Map<String, JsonValue>> {
    value
        .as_object()
        .ok_or_else(|| invalid_data(format!("{context} must be an object")))
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn unix_ms_now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn sanitize_export_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "export".to_string()
    } else {
        sanitized
    }
}

fn trim_snapshot_history(snapshots: &mut Vec<SnapshotDescriptor>, retention: usize) {
    let retention = retention.max(1);
    if snapshots.len() > retention {
        let drop_count = snapshots.len() - retention;
        snapshots.drain(0..drop_count);
    }
}

fn trim_manifest_history(events: &mut Vec<ManifestEvent>) {
    if events.len() > DEFAULT_MANIFEST_EVENT_HISTORY {
        let drop_count = events.len() - DEFAULT_MANIFEST_EVENT_HISTORY;
        events.drain(0..drop_count);
    }
}

fn build_manifest_events(
    previous_roots: Option<&BTreeMap<String, u64>>,
    current_roots: &BTreeMap<String, u64>,
    sequence: u64,
) -> Vec<ManifestEvent> {
    let mut events = Vec::new();

    if let Some(previous_roots) = previous_roots {
        for (collection, previous_root) in previous_roots {
            if !current_roots.contains_key(collection) {
                events.push(ManifestEvent {
                    collection: collection.clone(),
                    object_key: collection.clone(),
                    kind: ManifestEventKind::Remove,
                    block: manifest_block_reference(*previous_root, sequence),
                    snapshot_min: sequence,
                    snapshot_max: Some(sequence),
                });
            }
        }
    }

    for (collection, root) in current_roots {
        let kind = match previous_roots.and_then(|roots| roots.get(collection)) {
            None => ManifestEventKind::Insert,
            Some(previous_root) if previous_root != root => ManifestEventKind::Update,
            Some(_) => continue,
        };

        events.push(ManifestEvent {
            collection: collection.clone(),
            object_key: collection.clone(),
            kind,
            block: manifest_block_reference(*root, sequence),
            snapshot_min: sequence,
            snapshot_max: None,
        });
    }

    events.push(ManifestEvent {
        collection: "__system__".to_string(),
        object_key: format!("superblock:{sequence}"),
        kind: ManifestEventKind::Checkpoint,
        block: manifest_block_reference(sequence, sequence),
        snapshot_min: sequence,
        snapshot_max: None,
    });

    events
}

fn manifest_block_reference(root: u64, sequence: u64) -> BlockReference {
    BlockReference {
        index: root,
        checksum: ((root as u128) << 64) | sequence as u128,
    }
}
