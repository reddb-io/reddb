use std::collections::BTreeMap;

pub const DEFAULT_PHYSICAL_FORMAT_VERSION: u32 = 2;
pub const DEFAULT_SUPERBLOCK_COPIES: u8 = 4;
pub const PHYSICAL_METADATA_PROTOCOL_VERSION: &str = "reddb-physical-v1";
pub const PHYSICAL_SYSTEM_COLLECTION: &str = "__system__";

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

pub fn physical_manifest_block_reference(root: u64, sequence: u64) -> BlockReference {
    BlockReference {
        index: root,
        checksum: ((root as u128) << 64) | sequence as u128,
    }
}

pub fn physical_superblock_object_key(sequence: u64) -> String {
    format!("superblock:{sequence}")
}

pub fn physical_superblock_checkpoint_event(sequence: u64) -> ManifestEvent {
    ManifestEvent {
        collection: PHYSICAL_SYSTEM_COLLECTION.to_string(),
        object_key: physical_superblock_object_key(sequence),
        kind: ManifestEventKind::Checkpoint,
        block: physical_manifest_block_reference(sequence, sequence),
        snapshot_min: sequence,
        snapshot_max: None,
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
    pub columnar_derived: bool,
}

#[derive(Debug, Clone)]
pub struct PersistedPhysicalHypertable {
    pub name: String,
    pub time_column: String,
    pub chunk_interval_ns: u64,
    pub default_ttl_ns: Option<u64>,
    pub chunks: Vec<PersistedPhysicalHypertableChunk>,
}

#[derive(Debug, Clone, Default)]
pub struct PhysicalMetadataDocumentEnvelope {
    pub protocol_version: String,
    pub generated_at_unix_ms: u128,
    pub last_loaded_from: Option<String>,
    pub last_healed_at_unix_ms: Option<u128>,
    pub manifest_json: String,
    pub catalog_json: String,
    pub manifest_events_json: Vec<String>,
    pub indexes_json: Vec<String>,
    pub graph_projections_json: Vec<String>,
    pub analytics_jobs_json: Vec<String>,
    pub tree_definitions_json: Vec<String>,
    pub collection_ttl_defaults_ms: BTreeMap<String, u64>,
    pub collection_contracts_json: Vec<String>,
    pub hypertables_json: Vec<String>,
    pub exports_json: Vec<String>,
    pub superblock_json: String,
    pub snapshots_json: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalSchemaOptions {
    pub mode: String,
    pub data_path: Option<String>,
    pub read_only: bool,
    pub create_if_missing: bool,
    pub verify_checksums: bool,
    pub durability_mode: Option<String>,
    pub group_commit_window_ms: Option<u64>,
    pub group_commit_max_statements: Option<usize>,
    pub group_commit_max_wal_bytes: Option<u64>,
    pub auto_checkpoint_pages: u32,
    pub cache_pages: usize,
    pub snapshot_retention: Option<usize>,
    pub export_retention: Option<usize>,
    pub force_create: bool,
    pub capabilities: Vec<String>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalSchemaManifest {
    pub format_version: u32,
    pub created_at_unix_ms: u128,
    pub updated_at_unix_ms: u128,
    pub collection_count: usize,
    pub options: PhysicalSchemaOptions,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhysicalCatalogCollectionStats {
    pub entities: usize,
    pub cross_refs: usize,
    pub segments: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhysicalCatalogSnapshot {
    pub name: String,
    pub total_entities: usize,
    pub total_collections: usize,
    pub updated_at_unix_ms: u128,
    pub stats_by_collection: BTreeMap<String, PhysicalCatalogCollectionStats>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhysicalAnalyticalStorageConfig {
    pub columnar: bool,
    pub time_key: String,
    pub order_by_key: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhysicalSubscriptionDescriptor {
    pub name: String,
    pub source: String,
    pub target_queue: String,
    pub ops_filter: Vec<String>,
    pub where_filter: Option<String>,
    pub redact_fields: Vec<String>,
    pub enabled: bool,
    pub all_tenants: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PhysicalAnalyticsViewDescriptor {
    pub output: String,
    pub algorithm: Option<String>,
    pub resolution: Option<f64>,
    pub max_iterations: Option<i64>,
    pub tolerance: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhysicalDeclaredColumnContract {
    pub name: String,
    pub data_type: String,
    pub sql_type: Option<PhysicalSqlTypeName>,
    pub not_null: bool,
    pub default: Option<String>,
    pub compress: Option<u8>,
    pub unique: bool,
    pub primary_key: bool,
    pub enum_variants: Vec<String>,
    pub array_element: Option<String>,
    pub decimal_precision: Option<u8>,
}

/// Persisted form of an `EMBED (...)` AI-policy block (issue #1271).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhysicalAiEmbedPolicy {
    pub fields: Vec<String>,
    pub provider: String,
    pub model: String,
}

/// Persisted form of a `MODERATE (...)` AI-policy block (issue #1271).
/// `degraded_mode` / `reject_action` are stored as their canonical
/// lower-case tokens (`open`/`closed`, `reject`/`flag`/`redact`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhysicalAiModeratePolicy {
    pub fields: Vec<String>,
    pub provider: String,
    pub model: String,
    pub sync_gate: bool,
    pub degraded_mode: String,
    pub reject_action: String,
    /// When true, a quarantined row that re-moderates to a reject is
    /// hard-deleted rather than tombstoned-and-retained. Defaults to
    /// false (audit-retaining tombstone) on sidecars written before this
    /// field existed.
    pub hard_delete_on_reject: bool,
}

/// Persisted form of a `VISION (...)` AI-policy block (issue #1271).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhysicalAiVisionPolicy {
    pub image_field: String,
    pub output_kinds: Vec<String>,
    pub provider: String,
    pub model: String,
}

/// Persisted per-collection AI policy (issue #1271). Each block is
/// optional; the whole policy is `None` on sidecars written before the
/// feature, so reopen is migration-safe.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhysicalAiPolicy {
    pub embed: Option<PhysicalAiEmbedPolicy>,
    pub moderate: Option<PhysicalAiModeratePolicy>,
    pub vision: Option<PhysicalAiVisionPolicy>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PhysicalCollectionContract {
    pub name: String,
    pub declared_model: String,
    pub schema_mode: String,
    pub origin: String,
    pub version: u32,
    pub created_at_unix_ms: u128,
    pub updated_at_unix_ms: u128,
    pub default_ttl_ms: Option<u64>,
    pub vector_dimension: Option<usize>,
    pub vector_metric: Option<String>,
    pub context_index_fields: Vec<String>,
    pub declared_columns: Vec<PhysicalDeclaredColumnContract>,
    pub table_def_hex: Option<String>,
    pub timestamps_enabled: bool,
    pub context_index_enabled: bool,
    pub metrics_raw_retention_ms: Option<u64>,
    pub metrics_rollup_policies: Vec<String>,
    pub metrics_tenant_identity: Option<String>,
    pub metrics_namespace: Option<String>,
    pub append_only: bool,
    pub subscriptions: Vec<PhysicalSubscriptionDescriptor>,
    pub analytics_config: Vec<PhysicalAnalyticsViewDescriptor>,
    pub session_key: Option<String>,
    pub session_gap_ms: Option<u64>,
    pub retention_duration_ms: Option<u64>,
    pub analytical_storage: Option<PhysicalAnalyticalStorageConfig>,
    /// Per-collection AI policy (issue #1271). `None` on sidecars written
    /// before the feature — decode defaults to `None` for migration
    /// safety.
    pub ai_policy: Option<PhysicalAiPolicy>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhysicalSqlTypeName {
    pub name: String,
    pub modifiers: Vec<PhysicalTypeModifier>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhysicalTypeModifier {
    Number(u32),
    Ident(String),
    StringLiteral(String),
    Type(Box<PhysicalSqlTypeName>),
}
