//! Unified Store
//!
//! High-level API for the unified storage layer that combines tables, graphs,
//! and vectors into a single coherent interface.
//!
//! # Features
//!
//! - Multi-collection management
//! - Cross-collection queries
//! - Unified entity access
//! - Automatic ID generation
//! - Cross-reference management
//! - **Binary persistence** with pages, indices, and efficient encoding
//! - **Page-based storage** via Pager for ACID durability
//!
//! # Persistence Modes
//!
//! 1. **File Mode** (`save_to_file`/`load_from_file`): Simple binary dump
//! 2. **Paged Mode** (`open`/`persist`): Full page-based storage with B-tree indices

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use super::entity::{
    CrossRef, EmbeddingSlot, EntityData, EntityId, EntityKind, RefType, UnifiedEntity,
};
use super::manager::{ManagerConfig, ManagerStats, SegmentManager};
use super::metadata::{Metadata, MetadataFilter, MetadataValue};
use super::segment::SegmentError;
use crate::storage::engine::pager::PagerError;
use crate::storage::engine::{BTree, BTreeError, Pager, PagerConfig};
use crate::storage::primitives::encoding::{read_varu32, read_varu64, write_varu32, write_varu64};
use crate::storage::schema::types::Value;

const STORE_MAGIC: &[u8; 4] = b"RDST";
const STORE_VERSION_V1: u32 = 1;
const STORE_VERSION_V2: u32 = 2;
const METADATA_MAGIC: &[u8; 4] = b"RDM2";

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for UnifiedStore
#[derive(Debug, Clone)]
pub struct UnifiedStoreConfig {
    /// Configuration for segment managers
    pub manager_config: ManagerConfig,
    /// Automatically index cross-references on insert
    pub auto_index_refs: bool,
    /// Maximum cross-references per entity
    pub max_cross_refs: usize,
    /// Enable write-ahead logging
    pub enable_wal: bool,
    /// Data directory path
    pub data_dir: Option<std::path::PathBuf>,
}

impl Default for UnifiedStoreConfig {
    fn default() -> Self {
        Self {
            manager_config: ManagerConfig::default(),
            auto_index_refs: true,
            max_cross_refs: 1000,
            enable_wal: false,
            data_dir: None,
        }
    }
}

impl UnifiedStoreConfig {
    /// Create config with data directory
    pub fn with_data_dir(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.data_dir = Some(path.into());
        self
    }

    /// Enable WAL
    pub fn with_wal(mut self) -> Self {
        self.enable_wal = true;
        self
    }

    /// Set max cross-references
    pub fn with_max_refs(mut self, max: usize) -> Self {
        self.max_cross_refs = max;
        self
    }
}

// ============================================================================
// Error Types
// ============================================================================

/// Errors from UnifiedStore operations
#[derive(Debug)]
pub enum StoreError {
    /// Collection already exists
    CollectionExists(String),
    /// Collection not found
    CollectionNotFound(String),
    /// Entity not found
    EntityNotFound(EntityId),
    /// Too many cross-references
    TooManyRefs(EntityId),
    /// Segment error
    Segment(SegmentError),
    /// I/O error
    Io(std::io::Error),
    /// Serialization error
    Serialization(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CollectionExists(name) => write!(f, "Collection already exists: {}", name),
            Self::CollectionNotFound(name) => write!(f, "Collection not found: {}", name),
            Self::EntityNotFound(id) => write!(f, "Entity not found: {}", id),
            Self::TooManyRefs(id) => write!(f, "Too many cross-references for entity: {}", id),
            Self::Segment(e) => write!(f, "Segment error: {:?}", e),
            Self::Io(e) => write!(f, "I/O error: {}", e),
            Self::Serialization(msg) => write!(f, "Serialization error: {}", msg),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<SegmentError> for StoreError {
    fn from(e: SegmentError) -> Self {
        Self::Segment(e)
    }
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ============================================================================
// Statistics
// ============================================================================

/// Statistics for UnifiedStore
#[derive(Debug, Clone, Default)]
pub struct StoreStats {
    /// Number of collections
    pub collection_count: usize,
    /// Total entities across all collections
    pub total_entities: usize,
    /// Total memory usage in bytes
    pub total_memory_bytes: usize,
    /// Per-collection statistics
    pub collections: HashMap<String, ManagerStats>,
    /// Total cross-references
    pub cross_ref_count: usize,
}

impl StoreStats {
    /// Get average entities per collection
    pub fn avg_entities_per_collection(&self) -> f64 {
        if self.collection_count == 0 {
            0.0
        } else {
            self.total_entities as f64 / self.collection_count as f64
        }
    }

    /// Get memory in MB
    pub fn memory_mb(&self) -> f64 {
        self.total_memory_bytes as f64 / (1024.0 * 1024.0)
    }
}

// ============================================================================
// UnifiedStore - The Main API
// ============================================================================

/// Unified storage for tables, graphs, and vectors
///
/// UnifiedStore provides a single coherent interface for all data types:
/// - **Tables**: Row-based data with columns
/// - **Graphs**: Nodes and edges with labels
/// - **Vectors**: Embeddings for similarity search
///
/// # Features
///
/// - Multi-collection management
/// - Cross-collection queries
/// - Cross-reference tracking between entities
/// - Automatic ID generation
/// - Segment-based storage with growing/sealed lifecycle
///
/// # Example
///
/// ```ignore
/// use redblue::storage::{Entity, Store};
///
/// let store = Store::new();
///
/// // Create a collection
/// store.create_collection("hosts")?;
///
/// // Insert an entity
/// let entity = Entity::table_row(1, "hosts", 1, vec![]);
/// let id = store.insert("hosts", entity)?;
///
/// // Query
/// let found = store.get("hosts", id);
/// ```
pub struct UnifiedStore {
    /// Store configuration
    config: UnifiedStoreConfig,
    /// File format version for serialization
    format_version: AtomicU32,
    /// Global entity ID counter
    next_entity_id: AtomicU64,
    /// Collections by name
    collections: RwLock<HashMap<String, Arc<SegmentManager>>>,
    /// Forward cross-references: source_id → [(target_id, ref_type, target_collection)]
    cross_refs: RwLock<HashMap<EntityId, Vec<(EntityId, RefType, String)>>>,
    /// Reverse cross-references: target_id → [(source_id, ref_type, source_collection)]
    reverse_refs: RwLock<HashMap<EntityId, Vec<(EntityId, RefType, String)>>>,
    /// Optional page-based storage via Pager
    pager: Option<Arc<Pager>>,
    /// Database file path (for paged mode)
    db_path: Option<PathBuf>,
    /// B-tree indices for O(log n) entity lookups by ID (per collection)
    btree_indices: RwLock<HashMap<String, BTree>>,
}

impl UnifiedStore {
    /// Create a new unified store
    pub fn new() -> Self {
        Self::with_config(UnifiedStoreConfig::default())
    }

    /// Get the current storage format version
    pub fn format_version(&self) -> u32 {
        self.format_version.load(Ordering::SeqCst)
    }

    fn set_format_version(&self, version: u32) {
        self.format_version.store(version, Ordering::SeqCst);
    }

    /// Allocate a global entity ID
    pub fn next_entity_id(&self) -> EntityId {
        EntityId::new(self.next_entity_id.fetch_add(1, Ordering::SeqCst))
    }

    fn register_entity_id(&self, id: EntityId) {
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

    /// Load store from binary file
    ///
    /// Binary format:
    /// ```text
    /// [magic: 4 bytes "RDST"]
    /// [version: u32]
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

        // Verify magic bytes "RDST" (RedDB Store)
        if buf.len() < 8 {
            return Err("File too small".into());
        }
        if &buf[0..4] != STORE_MAGIC {
            return Err("Invalid magic bytes - expected RDST".into());
        }
        let mut pos = 4;

        // Version check
        let version = u32::from_le_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]);
        pos += 4;
        if version != STORE_VERSION_V1 && version != STORE_VERSION_V2 {
            return Err(format!("Unsupported version: {}", version).into());
        }

        let store = Self::with_config(UnifiedStoreConfig::default());
        store.set_format_version(version);

        // Read collection count
        let collection_count = read_varu32(&buf, &mut pos)
            .map_err(|e| format!("Failed to read collection count: {:?}", e))?;

        // Read each collection
        for _ in 0..collection_count {
            // Collection name
            let name_len = read_varu32(&buf, &mut pos)
                .map_err(|e| format!("Failed to read name length: {:?}", e))?
                as usize;
            let name = String::from_utf8(buf[pos..pos + name_len].to_vec())
                .map_err(|e| format!("Invalid UTF-8 in collection name: {}", e))?;
            pos += name_len;

            // Entity count
            let entity_count = read_varu32(&buf, &mut pos)
                .map_err(|e| format!("Failed to read entity count: {:?}", e))?;

            // Read each entity
            for _ in 0..entity_count {
                let entity = Self::read_entity_binary(&buf, &mut pos, version)?;
                store.insert_auto(&name, entity)?;
            }
        }

        if pos < buf.len() {
            // Read cross-references section
            let cross_ref_count = read_varu32(&buf, &mut pos)
                .map_err(|e| format!("Failed to read cross-ref count: {:?}", e))?;

            for _ in 0..cross_ref_count {
                let source_id = read_varu64(&buf, &mut pos)
                    .map_err(|e| format!("Failed to read source_id: {:?}", e))?;
                let target_id = read_varu64(&buf, &mut pos)
                    .map_err(|e| format!("Failed to read target_id: {:?}", e))?;
                let ref_type_byte = buf[pos];
                pos += 1;
                let ref_type = RefType::from_byte(ref_type_byte);

                let coll_len = read_varu32(&buf, &mut pos)
                    .map_err(|e| format!("Failed to read collection length: {:?}", e))?
                    as usize;
                let collection = String::from_utf8(buf[pos..pos + coll_len].to_vec())
                    .map_err(|e| format!("Invalid UTF-8 in collection: {}", e))?;
                pos += coll_len;

                let source_collection = store
                    .get_any(EntityId::new(source_id))
                    .map(|(name, _)| name)
                    .unwrap_or_else(|| collection.clone());
                let _ = store.add_cross_ref(
                    &source_collection,
                    EntityId::new(source_id),
                    &collection,
                    EntityId::new(target_id),
                    ref_type,
                    1.0,
                );
            }
        }

        Ok(store)
    }

    /// Save store to binary file
    ///
    /// Uses compact binary encoding with varint for efficient storage.
    /// No JSON - pure binary with pages and indices.
    pub fn save_to_file(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);
        let mut buf = Vec::new();

        // Magic bytes "RDST"
        buf.extend_from_slice(STORE_MAGIC);

        // Version (2)
        buf.extend_from_slice(&STORE_VERSION_V2.to_le_bytes());

        // Get all collections
        let collections = self.collections.read().unwrap();
        write_varu32(&mut buf, collections.len() as u32);

        for (name, manager) in collections.iter() {
            // Collection name
            write_varu32(&mut buf, name.len() as u32);
            buf.extend_from_slice(name.as_bytes());

            // Get all entities from this collection
            let entities = manager.query_all(|_| true);
            write_varu32(&mut buf, entities.len() as u32);

            for entity in entities {
                Self::write_entity_binary(&mut buf, &entity, STORE_VERSION_V2);
            }
        }

        // Write cross-references
        let cross_refs = self.cross_refs.read().unwrap();
        let total_refs: usize = cross_refs.values().map(|v| v.len()).sum();
        write_varu32(&mut buf, total_refs as u32);

        for (source_id, refs) in cross_refs.iter() {
            for (target_id, ref_type, collection) in refs {
                write_varu64(&mut buf, source_id.raw());
                write_varu64(&mut buf, target_id.raw());
                buf.push(ref_type.to_byte());
                write_varu32(&mut buf, collection.len() as u32);
                buf.extend_from_slice(collection.as_bytes());
            }
        }

        self.set_format_version(STORE_VERSION_V2);

        writer.write_all(&buf)?;
        writer.flush()?;

        Ok(())
    }

    /// Read entity from binary buffer
    fn read_entity_binary(
        buf: &[u8],
        pos: &mut usize,
        format_version: u32,
    ) -> Result<UnifiedEntity, Box<dyn std::error::Error>> {
        // Entity ID
        let id = read_varu64(buf, pos).map_err(|e| format!("Failed to read entity id: {:?}", e))?;

        // EntityKind type byte
        let kind_type = buf[*pos];
        *pos += 1;

        // EntityKind details
        let kind = match kind_type {
            0 => {
                // TableRow
                let table_len = Self::read_varu32_safe(buf, pos)?;
                let table = String::from_utf8(buf[*pos..*pos + table_len].to_vec())?;
                *pos += table_len;
                let row_id = Self::read_varu64_safe(buf, pos)?;
                EntityKind::TableRow { table, row_id }
            }
            1 => {
                // GraphNode
                let label_len = Self::read_varu32_safe(buf, pos)?;
                let label = String::from_utf8(buf[*pos..*pos + label_len].to_vec())?;
                *pos += label_len;
                let node_type_len = Self::read_varu32_safe(buf, pos)?;
                let node_type = String::from_utf8(buf[*pos..*pos + node_type_len].to_vec())?;
                *pos += node_type_len;
                EntityKind::GraphNode { label, node_type }
            }
            2 => {
                // GraphEdge
                let label_len = Self::read_varu32_safe(buf, pos)?;
                let label = String::from_utf8(buf[*pos..*pos + label_len].to_vec())?;
                *pos += label_len;
                let from_node_len = Self::read_varu32_safe(buf, pos)?;
                let from_node = String::from_utf8(buf[*pos..*pos + from_node_len].to_vec())?;
                *pos += from_node_len;
                let to_node_len = Self::read_varu32_safe(buf, pos)?;
                let to_node = String::from_utf8(buf[*pos..*pos + to_node_len].to_vec())?;
                *pos += to_node_len;
                let weight =
                    u32::from_le_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
                *pos += 4;
                EntityKind::GraphEdge {
                    label,
                    from_node,
                    to_node,
                    weight,
                }
            }
            3 => {
                // Vector
                let collection_len = Self::read_varu32_safe(buf, pos)?;
                let collection = String::from_utf8(buf[*pos..*pos + collection_len].to_vec())?;
                *pos += collection_len;
                EntityKind::Vector { collection }
            }
            _ => return Err(format!("Unknown EntityKind type: {}", kind_type).into()),
        };

        // EntityData type byte
        let data_type = buf[*pos];
        *pos += 1;

        // EntityData
        let data = match data_type {
            0 => {
                // Row
                let col_count = Self::read_varu32_safe(buf, pos)?;
                let mut columns = Vec::with_capacity(col_count);
                for _ in 0..col_count {
                    columns.push(Self::read_value_binary(buf, pos)?);
                }
                EntityData::Row(super::entity::RowData::new(columns))
            }
            1 => {
                // Node
                let prop_count = Self::read_varu32_safe(buf, pos)?;
                let mut properties = HashMap::new();
                for _ in 0..prop_count {
                    let key_len = Self::read_varu32_safe(buf, pos)?;
                    let key = String::from_utf8(buf[*pos..*pos + key_len].to_vec())?;
                    *pos += key_len;
                    let value = Self::read_value_binary(buf, pos)?;
                    properties.insert(key, value);
                }
                EntityData::Node(super::entity::NodeData::with_properties(properties))
            }
            2 => {
                // Edge
                let weight_bytes = [buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]];
                *pos += 4;
                let weight = f32::from_le_bytes(weight_bytes);
                let prop_count = Self::read_varu32_safe(buf, pos)?;
                let mut properties = HashMap::new();
                for _ in 0..prop_count {
                    let key_len = Self::read_varu32_safe(buf, pos)?;
                    let key = String::from_utf8(buf[*pos..*pos + key_len].to_vec())?;
                    *pos += key_len;
                    let value = Self::read_value_binary(buf, pos)?;
                    properties.insert(key, value);
                }
                let mut edge = super::entity::EdgeData::new(weight);
                edge.properties = properties;
                EntityData::Edge(edge)
            }
            3 => {
                // Vector
                let dim = Self::read_varu32_safe(buf, pos)?;
                let mut dense = Vec::with_capacity(dim);
                for _ in 0..dim {
                    let bytes = [buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]];
                    *pos += 4;
                    dense.push(f32::from_le_bytes(bytes));
                }
                EntityData::Vector(super::entity::VectorData::new(dense))
            }
            _ => return Err(format!("Unknown EntityData type: {}", data_type).into()),
        };

        // Timestamps
        let created_at = Self::read_varu64_safe(buf, pos)?;
        let updated_at = Self::read_varu64_safe(buf, pos)?;

        // Embeddings count
        let embedding_count = Self::read_varu32_safe(buf, pos)?;
        let mut embeddings = Vec::with_capacity(embedding_count);
        for _ in 0..embedding_count {
            let name_len = Self::read_varu32_safe(buf, pos)?;
            let name = String::from_utf8(buf[*pos..*pos + name_len].to_vec())?;
            *pos += name_len;

            let dim = Self::read_varu32_safe(buf, pos)?;
            let mut vector = Vec::with_capacity(dim);
            for _ in 0..dim {
                let bytes = [buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]];
                *pos += 4;
                vector.push(f32::from_le_bytes(bytes));
            }

            let model_len = Self::read_varu32_safe(buf, pos)?;
            let model = String::from_utf8(buf[*pos..*pos + model_len].to_vec())?;
            *pos += model_len;

            embeddings.push(EmbeddingSlot::new(name, vector, model));
        }

        // Cross-refs count
        let cross_ref_count = Self::read_varu32_safe(buf, pos)?;
        let mut cross_refs = Vec::with_capacity(cross_ref_count);
        for _ in 0..cross_ref_count {
            let source = Self::read_varu64_safe(buf, pos)?;
            let target = Self::read_varu64_safe(buf, pos)?;
            let ref_type_byte = buf[*pos];
            *pos += 1;
            let (target_collection, weight, created_at) = if format_version >= STORE_VERSION_V2 {
                let coll_len = Self::read_varu32_safe(buf, pos)?;
                let collection = String::from_utf8(buf[*pos..*pos + coll_len].to_vec())?;
                *pos += coll_len;
                let weight_bytes = [buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]];
                *pos += 4;
                let weight = f32::from_le_bytes(weight_bytes);
                let created_at = Self::read_varu64_safe(buf, pos)?;
                (collection, weight, created_at)
            } else {
                (String::new(), 1.0, 0)
            };

            let mut cross_ref = CrossRef::new(
                EntityId::new(source),
                EntityId::new(target),
                target_collection,
                RefType::from_byte(ref_type_byte),
            );
            cross_ref.weight = weight;
            cross_ref.created_at = created_at;
            cross_refs.push(cross_ref);
        }

        // Sequence ID
        let sequence_id = Self::read_varu64_safe(buf, pos)?;

        let entity = UnifiedEntity {
            id: EntityId::new(id),
            kind,
            created_at,
            updated_at,
            data,
            embeddings,
            cross_refs,
            sequence_id,
        };

        Ok(entity)
    }

    /// Safe varu32 reader that converts DecodeError to Box<dyn Error>
    fn read_varu32_safe(buf: &[u8], pos: &mut usize) -> Result<usize, Box<dyn std::error::Error>> {
        read_varu32(buf, pos)
            .map(|v| v as usize)
            .map_err(|e| format!("Decode error: {:?}", e).into())
    }

    /// Safe varu64 reader that converts DecodeError to Box<dyn Error>
    fn read_varu64_safe(buf: &[u8], pos: &mut usize) -> Result<u64, Box<dyn std::error::Error>> {
        read_varu64(buf, pos).map_err(|e| format!("Decode error: {:?}", e).into())
    }

    /// Write entity to binary buffer
    fn write_entity_binary(buf: &mut Vec<u8>, entity: &UnifiedEntity, format_version: u32) {
        // Entity ID
        write_varu64(buf, entity.id.raw());

        // EntityKind
        match &entity.kind {
            EntityKind::TableRow { table, row_id } => {
                buf.push(0);
                write_varu32(buf, table.len() as u32);
                buf.extend_from_slice(table.as_bytes());
                write_varu64(buf, *row_id);
            }
            EntityKind::GraphNode { label, node_type } => {
                buf.push(1);
                write_varu32(buf, label.len() as u32);
                buf.extend_from_slice(label.as_bytes());
                write_varu32(buf, node_type.len() as u32);
                buf.extend_from_slice(node_type.as_bytes());
            }
            EntityKind::GraphEdge {
                label,
                from_node,
                to_node,
                weight,
            } => {
                buf.push(2);
                write_varu32(buf, label.len() as u32);
                buf.extend_from_slice(label.as_bytes());
                write_varu32(buf, from_node.len() as u32);
                buf.extend_from_slice(from_node.as_bytes());
                write_varu32(buf, to_node.len() as u32);
                buf.extend_from_slice(to_node.as_bytes());
                buf.extend_from_slice(&weight.to_le_bytes());
            }
            EntityKind::Vector { collection } => {
                buf.push(3);
                write_varu32(buf, collection.len() as u32);
                buf.extend_from_slice(collection.as_bytes());
            }
        }

        // EntityData
        match &entity.data {
            EntityData::Row(row) => {
                buf.push(0);
                write_varu32(buf, row.columns.len() as u32);
                for col in &row.columns {
                    Self::write_value_binary(buf, col);
                }
            }
            EntityData::Node(node) => {
                buf.push(1);
                write_varu32(buf, node.properties.len() as u32);
                for (key, value) in &node.properties {
                    write_varu32(buf, key.len() as u32);
                    buf.extend_from_slice(key.as_bytes());
                    Self::write_value_binary(buf, value);
                }
            }
            EntityData::Edge(edge) => {
                buf.push(2);
                buf.extend_from_slice(&edge.weight.to_le_bytes());
                write_varu32(buf, edge.properties.len() as u32);
                for (key, value) in &edge.properties {
                    write_varu32(buf, key.len() as u32);
                    buf.extend_from_slice(key.as_bytes());
                    Self::write_value_binary(buf, value);
                }
            }
            EntityData::Vector(vec) => {
                buf.push(3);
                write_varu32(buf, vec.dense.len() as u32);
                for f in &vec.dense {
                    buf.extend_from_slice(&f.to_le_bytes());
                }
            }
        }

        // Timestamps
        write_varu64(buf, entity.created_at);
        write_varu64(buf, entity.updated_at);

        // Embeddings
        write_varu32(buf, entity.embeddings.len() as u32);
        for emb in &entity.embeddings {
            write_varu32(buf, emb.name.len() as u32);
            buf.extend_from_slice(emb.name.as_bytes());
            write_varu32(buf, emb.vector.len() as u32);
            for f in &emb.vector {
                buf.extend_from_slice(&f.to_le_bytes());
            }
            write_varu32(buf, emb.model.len() as u32);
            buf.extend_from_slice(emb.model.as_bytes());
        }

        // Cross-refs
        write_varu32(buf, entity.cross_refs.len() as u32);
        for cross_ref in &entity.cross_refs {
            write_varu64(buf, cross_ref.source.raw());
            write_varu64(buf, cross_ref.target.raw());
            buf.push(cross_ref.ref_type.to_byte());
            if format_version >= STORE_VERSION_V2 {
                write_varu32(buf, cross_ref.target_collection.len() as u32);
                buf.extend_from_slice(cross_ref.target_collection.as_bytes());
                buf.extend_from_slice(&cross_ref.weight.to_le_bytes());
                write_varu64(buf, cross_ref.created_at);
            }
        }

        // Sequence ID
        write_varu64(buf, entity.sequence_id);
    }

    /// Read a Value from binary buffer
    /// Type bytes: 0=Null, 1=Boolean, 2=Integer, 3=UnsignedInteger, 4=Float,
    /// 5=Text, 6=Blob, 7=Timestamp, 8=Duration, 9=IpAddr, 10=MacAddr,
    /// 11=Vector, 12=Json, 13=Uuid, 14=NodeRef, 15=EdgeRef, 16=VectorRef, 17=RowRef
    fn read_value_binary(buf: &[u8], pos: &mut usize) -> Result<Value, Box<dyn std::error::Error>> {
        use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

        let type_byte = buf[*pos];
        *pos += 1;

        Ok(match type_byte {
            0 => Value::Null,
            1 => {
                let b = buf[*pos] != 0;
                *pos += 1;
                Value::Boolean(b)
            }
            2 => {
                let val = i64::from_le_bytes([
                    buf[*pos],
                    buf[*pos + 1],
                    buf[*pos + 2],
                    buf[*pos + 3],
                    buf[*pos + 4],
                    buf[*pos + 5],
                    buf[*pos + 6],
                    buf[*pos + 7],
                ]);
                *pos += 8;
                Value::Integer(val)
            }
            3 => {
                let val = u64::from_le_bytes([
                    buf[*pos],
                    buf[*pos + 1],
                    buf[*pos + 2],
                    buf[*pos + 3],
                    buf[*pos + 4],
                    buf[*pos + 5],
                    buf[*pos + 6],
                    buf[*pos + 7],
                ]);
                *pos += 8;
                Value::UnsignedInteger(val)
            }
            4 => {
                let val = f64::from_le_bytes([
                    buf[*pos],
                    buf[*pos + 1],
                    buf[*pos + 2],
                    buf[*pos + 3],
                    buf[*pos + 4],
                    buf[*pos + 5],
                    buf[*pos + 6],
                    buf[*pos + 7],
                ]);
                *pos += 8;
                Value::Float(val)
            }
            5 => {
                let len = Self::read_varu32_safe(buf, pos)?;
                let s = String::from_utf8(buf[*pos..*pos + len].to_vec())?;
                *pos += len;
                Value::Text(s)
            }
            6 => {
                let len = Self::read_varu32_safe(buf, pos)?;
                let bytes = buf[*pos..*pos + len].to_vec();
                *pos += len;
                Value::Blob(bytes)
            }
            7 => {
                let val = i64::from_le_bytes([
                    buf[*pos],
                    buf[*pos + 1],
                    buf[*pos + 2],
                    buf[*pos + 3],
                    buf[*pos + 4],
                    buf[*pos + 5],
                    buf[*pos + 6],
                    buf[*pos + 7],
                ]);
                *pos += 8;
                Value::Timestamp(val)
            }
            8 => {
                let val = i64::from_le_bytes([
                    buf[*pos],
                    buf[*pos + 1],
                    buf[*pos + 2],
                    buf[*pos + 3],
                    buf[*pos + 4],
                    buf[*pos + 5],
                    buf[*pos + 6],
                    buf[*pos + 7],
                ]);
                *pos += 8;
                Value::Duration(val)
            }
            9 => {
                // IpAddr: first byte = version (4 or 6)
                let version = buf[*pos];
                *pos += 1;
                if version == 4 {
                    let octets = [buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]];
                    *pos += 4;
                    Value::IpAddr(IpAddr::V4(Ipv4Addr::from(octets)))
                } else {
                    let mut octets = [0u8; 16];
                    octets.copy_from_slice(&buf[*pos..*pos + 16]);
                    *pos += 16;
                    Value::IpAddr(IpAddr::V6(Ipv6Addr::from(octets)))
                }
            }
            10 => {
                let mut mac = [0u8; 6];
                mac.copy_from_slice(&buf[*pos..*pos + 6]);
                *pos += 6;
                Value::MacAddr(mac)
            }
            11 => {
                let len = Self::read_varu32_safe(buf, pos)?;
                let mut vector = Vec::with_capacity(len);
                for _ in 0..len {
                    let bytes = [buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]];
                    *pos += 4;
                    vector.push(f32::from_le_bytes(bytes));
                }
                Value::Vector(vector)
            }
            12 => {
                let len = Self::read_varu32_safe(buf, pos)?;
                let bytes = buf[*pos..*pos + len].to_vec();
                *pos += len;
                Value::Json(bytes)
            }
            13 => {
                let mut uuid = [0u8; 16];
                uuid.copy_from_slice(&buf[*pos..*pos + 16]);
                *pos += 16;
                Value::Uuid(uuid)
            }
            14 => {
                let len = Self::read_varu32_safe(buf, pos)?;
                let s = String::from_utf8(buf[*pos..*pos + len].to_vec())?;
                *pos += len;
                Value::NodeRef(s)
            }
            15 => {
                let len = Self::read_varu32_safe(buf, pos)?;
                let s = String::from_utf8(buf[*pos..*pos + len].to_vec())?;
                *pos += len;
                Value::EdgeRef(s)
            }
            16 => {
                let len = Self::read_varu32_safe(buf, pos)?;
                let s = String::from_utf8(buf[*pos..*pos + len].to_vec())?;
                *pos += len;
                let id = u64::from_le_bytes([
                    buf[*pos],
                    buf[*pos + 1],
                    buf[*pos + 2],
                    buf[*pos + 3],
                    buf[*pos + 4],
                    buf[*pos + 5],
                    buf[*pos + 6],
                    buf[*pos + 7],
                ]);
                *pos += 8;
                Value::VectorRef(s, id)
            }
            17 => {
                let len = Self::read_varu32_safe(buf, pos)?;
                let s = String::from_utf8(buf[*pos..*pos + len].to_vec())?;
                *pos += len;
                let id = u64::from_le_bytes([
                    buf[*pos],
                    buf[*pos + 1],
                    buf[*pos + 2],
                    buf[*pos + 3],
                    buf[*pos + 4],
                    buf[*pos + 5],
                    buf[*pos + 6],
                    buf[*pos + 7],
                ]);
                *pos += 8;
                Value::RowRef(s, id)
            }
            _ => return Err(format!("Unknown Value type: {}", type_byte).into()),
        })
    }

    /// Write a Value to binary buffer
    /// Type bytes: 0=Null, 1=Boolean, 2=Integer, 3=UnsignedInteger, 4=Float,
    /// 5=Text, 6=Blob, 7=Timestamp, 8=Duration, 9=IpAddr, 10=MacAddr,
    /// 11=Vector, 12=Json, 13=Uuid, 14=NodeRef, 15=EdgeRef, 16=VectorRef, 17=RowRef
    fn write_value_binary(buf: &mut Vec<u8>, value: &Value) {
        use std::net::IpAddr;

        match value {
            Value::Null => buf.push(0),
            Value::Boolean(b) => {
                buf.push(1);
                buf.push(if *b { 1 } else { 0 });
            }
            Value::Integer(i) => {
                buf.push(2);
                buf.extend_from_slice(&i.to_le_bytes());
            }
            Value::UnsignedInteger(u) => {
                buf.push(3);
                buf.extend_from_slice(&u.to_le_bytes());
            }
            Value::Float(f) => {
                buf.push(4);
                buf.extend_from_slice(&f.to_le_bytes());
            }
            Value::Text(s) => {
                buf.push(5);
                write_varu32(buf, s.len() as u32);
                buf.extend_from_slice(s.as_bytes());
            }
            Value::Blob(bytes) => {
                buf.push(6);
                write_varu32(buf, bytes.len() as u32);
                buf.extend_from_slice(bytes);
            }
            Value::Timestamp(t) => {
                buf.push(7);
                buf.extend_from_slice(&t.to_le_bytes());
            }
            Value::Duration(d) => {
                buf.push(8);
                buf.extend_from_slice(&d.to_le_bytes());
            }
            Value::IpAddr(ip) => {
                buf.push(9);
                match ip {
                    IpAddr::V4(v4) => {
                        buf.push(4);
                        buf.extend_from_slice(&v4.octets());
                    }
                    IpAddr::V6(v6) => {
                        buf.push(6);
                        buf.extend_from_slice(&v6.octets());
                    }
                }
            }
            Value::MacAddr(mac) => {
                buf.push(10);
                buf.extend_from_slice(mac);
            }
            Value::Vector(vec) => {
                buf.push(11);
                write_varu32(buf, vec.len() as u32);
                for f in vec {
                    buf.extend_from_slice(&f.to_le_bytes());
                }
            }
            Value::Json(bytes) => {
                buf.push(12);
                write_varu32(buf, bytes.len() as u32);
                buf.extend_from_slice(bytes);
            }
            Value::Uuid(uuid) => {
                buf.push(13);
                buf.extend_from_slice(uuid);
            }
            Value::NodeRef(s) => {
                buf.push(14);
                write_varu32(buf, s.len() as u32);
                buf.extend_from_slice(s.as_bytes());
            }
            Value::EdgeRef(s) => {
                buf.push(15);
                write_varu32(buf, s.len() as u32);
                buf.extend_from_slice(s.as_bytes());
            }
            Value::VectorRef(s, id) => {
                buf.push(16);
                write_varu32(buf, s.len() as u32);
                buf.extend_from_slice(s.as_bytes());
                buf.extend_from_slice(&id.to_le_bytes());
            }
            Value::RowRef(s, id) => {
                buf.push(17);
                write_varu32(buf, s.len() as u32);
                buf.extend_from_slice(s.as_bytes());
                buf.extend_from_slice(&id.to_le_bytes());
            }
        }
    }

    /// Create with custom configuration
    pub fn with_config(config: UnifiedStoreConfig) -> Self {
        Self {
            config,
            format_version: AtomicU32::new(STORE_VERSION_V2),
            next_entity_id: AtomicU64::new(1),
            collections: RwLock::new(HashMap::new()),
            cross_refs: RwLock::new(HashMap::new()),
            reverse_refs: RwLock::new(HashMap::new()),
            pager: None,
            db_path: None,
            btree_indices: RwLock::new(HashMap::new()),
        }
    }

    /// Open or create a page-based database
    ///
    /// This uses the page engine for ACID durability with B-tree indices.
    /// The database file uses 4KB pages with checksums and efficient caching.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the database file (e.g., "data.rdb")
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let store = UnifiedStore::open("security.rdb")?;
    /// store.create_collection("hosts")?;
    /// // ... operations ...
    /// store.persist()?; // Flush to disk
    /// ```
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        let pager_config = PagerConfig::default();
        let pager = Pager::open(path, pager_config).map_err(|e| {
            StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                e.to_string(),
            ))
        })?;

        let store = Self {
            config: UnifiedStoreConfig::default(),
            format_version: AtomicU32::new(STORE_VERSION_V2),
            next_entity_id: AtomicU64::new(1),
            collections: RwLock::new(HashMap::new()),
            cross_refs: RwLock::new(HashMap::new()),
            reverse_refs: RwLock::new(HashMap::new()),
            pager: Some(Arc::new(pager)),
            db_path: Some(path.to_path_buf()),
            btree_indices: RwLock::new(HashMap::new()),
        };

        // Load existing data from pages if database exists
        store.load_from_pages()?;

        Ok(store)
    }

    /// Load data from page-based storage
    ///
    /// Reads the B-tree indices and reconstructs collections from pages.
    fn load_from_pages(&self) -> Result<(), StoreError> {
        let pager = match &self.pager {
            Some(p) => p,
            None => return Ok(()), // No pager, nothing to load
        };

        // Get page count
        let page_count = pager.page_count();
        if page_count <= 1 {
            // Empty database (only header page)
            return Ok(());
        }

        // Read metadata from page 1 (collections registry)
        // Format: [collection_count: u32][names...][root_page_ids...]
        if let Ok(meta_page) = pager.read_page(1) {
            let data = meta_page.as_bytes();
            // Skip header (32 bytes), read content area
            let content = &data[crate::storage::engine::HEADER_SIZE..];
            if content.len() >= 4 {
                let mut pos = 0;
                let mut format_version = STORE_VERSION_V1;

                if content.len() >= 8 && &content[0..4] == METADATA_MAGIC {
                    format_version =
                        u32::from_le_bytes([content[4], content[5], content[6], content[7]]);
                    pos += 8;
                }

                self.set_format_version(format_version);

                // Collection count
                let collection_count = u32::from_le_bytes([
                    content[pos],
                    content[pos + 1],
                    content[pos + 2],
                    content[pos + 3],
                ]) as usize;
                pos += 4;

                // Read collection names and their B-tree root pages
                for _ in 0..collection_count {
                    if pos + 4 > content.len() {
                        break;
                    }

                    let name_len = u32::from_le_bytes([
                        content[pos],
                        content[pos + 1],
                        content[pos + 2],
                        content[pos + 3],
                    ]) as usize;
                    pos += 4;

                    if pos + name_len + 4 > content.len() {
                        break;
                    }

                    if let Ok(name) = String::from_utf8(content[pos..pos + name_len].to_vec()) {
                        pos += name_len;

                        // Root page ID for this collection's B-tree
                        let root_page = u32::from_le_bytes([
                            content[pos],
                            content[pos + 1],
                            content[pos + 2],
                            content[pos + 3],
                        ]);
                        pos += 4;

                        // Create the collection
                        let _ = self.create_collection(&name);

                        // Load B-tree with root page if it exists
                        if root_page > 0 {
                            let btree = BTree::with_root(Arc::clone(pager), root_page);

                            // Load all entities from B-tree into the collection
                            if let Ok(mut cursor) = btree.cursor_first() {
                                let manager = self.get_collection(&name);
                                while let Ok(Some((key, value))) = cursor.next() {
                                    // Deserialize entity from value bytes
                                    if let Ok(entity) =
                                        Self::deserialize_entity(&value, self.format_version())
                                    {
                                        if let Some(m) = &manager {
                                            let id = entity.id;
                                            let _ = m.insert(entity.clone());
                                            self.register_entity_id(id);
                                            if self.config.auto_index_refs {
                                                self.index_cross_refs(&entity, &name);
                                            }
                                        }
                                    }
                                }
                            }

                            // Store the B-tree for future lookups
                            self.btree_indices.write().unwrap().insert(name, btree);
                        }
                    } else {
                        pos += name_len + 4;
                    }
                }

                if format_version >= STORE_VERSION_V2 && pos + 4 <= content.len() {
                    let cross_ref_count = u32::from_le_bytes([
                        content[pos],
                        content[pos + 1],
                        content[pos + 2],
                        content[pos + 3],
                    ]) as usize;
                    pos += 4;

                    for _ in 0..cross_ref_count {
                        if pos + 17 > content.len() {
                            break;
                        }
                        let source_id = u64::from_le_bytes([
                            content[pos],
                            content[pos + 1],
                            content[pos + 2],
                            content[pos + 3],
                            content[pos + 4],
                            content[pos + 5],
                            content[pos + 6],
                            content[pos + 7],
                        ]);
                        pos += 8;
                        let target_id = u64::from_le_bytes([
                            content[pos],
                            content[pos + 1],
                            content[pos + 2],
                            content[pos + 3],
                            content[pos + 4],
                            content[pos + 5],
                            content[pos + 6],
                            content[pos + 7],
                        ]);
                        pos += 8;
                        let ref_type = RefType::from_byte(content[pos]);
                        pos += 1;

                        if pos + 4 > content.len() {
                            break;
                        }
                        let name_len = u32::from_le_bytes([
                            content[pos],
                            content[pos + 1],
                            content[pos + 2],
                            content[pos + 3],
                        ]) as usize;
                        pos += 4;
                        if pos + name_len > content.len() {
                            break;
                        }
                        let target_collection =
                            String::from_utf8_lossy(&content[pos..pos + name_len]).to_string();
                        pos += name_len;

                        let source_id = EntityId::new(source_id);
                        let target_id = EntityId::new(target_id);

                        self.cross_refs
                            .write()
                            .unwrap()
                            .entry(source_id)
                            .or_default()
                            .push((target_id, ref_type, target_collection.clone()));

                        if let Some((collection, mut entity)) = self.get_any(source_id) {
                            let exists = entity.cross_refs.iter().any(|xref| {
                                xref.target == target_id
                                    && xref.ref_type == ref_type
                                    && xref.target_collection == target_collection
                            });
                            if !exists {
                                entity.cross_refs.push(CrossRef::new(
                                    source_id,
                                    target_id,
                                    target_collection.clone(),
                                    ref_type,
                                ));
                                if let Some(manager) = self.get_collection(&collection) {
                                    let _ = manager.update(entity);
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Deserialize an entity from binary bytes
    fn deserialize_entity(data: &[u8], format_version: u32) -> Result<UnifiedEntity, StoreError> {
        let mut pos = 0;
        Self::read_entity_binary(data, &mut pos, format_version)
            .map_err(|e| StoreError::Serialization(e.to_string()))
    }

    /// Serialize an entity to binary bytes
    fn serialize_entity(entity: &UnifiedEntity, format_version: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        Self::write_entity_binary(&mut buf, entity, format_version);
        buf
    }

    /// Persist all data to page-based storage
    ///
    /// Writes all entities to B-tree pages and flushes to disk.
    /// This provides ACID durability guarantees.
    pub fn persist(&self) -> Result<(), StoreError> {
        let pager = match &self.pager {
            Some(p) => p,
            None => {
                // No pager attached - use binary file fallback if path available
                if let Some(path) = &self.db_path {
                    return self
                        .save_to_file(path)
                        .map_err(|e| StoreError::Serialization(e.to_string()));
                }
                return Err(StoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "No pager or path configured for persistence",
                )));
            }
        };

        match pager.read_page(1) {
            Ok(_) => {}
            Err(PagerError::PageNotFound(_)) => {
                let meta_page = pager
                    .allocate_page(crate::storage::engine::PageType::Header)
                    .map_err(|e| {
                        StoreError::Io(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            e.to_string(),
                        ))
                    })?;
                pager
                    .write_page(meta_page.page_id(), meta_page)
                    .map_err(|e| {
                        StoreError::Io(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            e.to_string(),
                        ))
                    })?;
            }
            Err(e) => {
                return Err(StoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    e.to_string(),
                )));
            }
        }

        let collections = self.collections.read().unwrap();
        let mut btree_indices = self.btree_indices.write().unwrap();

        // Collect collection names and their B-tree root pages
        let mut collection_roots: Vec<(String, u32)> = Vec::new();

        // For each collection, create/populate a B-tree and get its root page
        for (name, manager) in collections.iter() {
            // Get or create B-tree for this collection
            let btree = btree_indices
                .entry(name.clone())
                .or_insert_with(|| BTree::new(Arc::clone(pager)));

            // Insert all entities into the B-tree
            for entity in manager.query_all(|_| true) {
                let key = entity.id.raw().to_le_bytes();
                let value = Self::serialize_entity(&entity, self.format_version());

                // Ignore errors if key already exists (update scenario)
                match btree.insert(&key, &value) {
                    Ok(_) => {}
                    Err(BTreeError::DuplicateKey) => {
                        // Key exists - delete and re-insert for update
                        let _ = btree.delete(&key);
                        let _ = btree.insert(&key, &value);
                    }
                    Err(e) => {
                        return Err(StoreError::Io(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            format!("B-tree insert error: {}", e),
                        )));
                    }
                }
            }

            collection_roots.push((name.clone(), btree.root_page_id()));
        }

        // Write collection metadata to page 1
        let mut meta_data = Vec::with_capacity(4096);

        let format_version = STORE_VERSION_V2;
        self.set_format_version(format_version);

        // Metadata header: magic + version + collection count
        meta_data.extend_from_slice(METADATA_MAGIC);
        meta_data.extend_from_slice(&format_version.to_le_bytes());
        meta_data.extend_from_slice(&(collection_roots.len() as u32).to_le_bytes());

        // Write each collection's name and B-tree root page
        for (name, root_page) in &collection_roots {
            // Name length
            meta_data.extend_from_slice(&(name.len() as u32).to_le_bytes());
            // Name
            meta_data.extend_from_slice(name.as_bytes());
            // Root page ID from actual B-tree
            meta_data.extend_from_slice(&root_page.to_le_bytes());
        }

        // Write cross-reference metadata
        let cross_refs = self.cross_refs.read().unwrap();
        let total_refs: usize = cross_refs.values().map(|v| v.len()).sum();
        meta_data.extend_from_slice(&(total_refs as u32).to_le_bytes());
        for (source_id, refs) in cross_refs.iter() {
            for (target_id, ref_type, collection) in refs {
                meta_data.extend_from_slice(&source_id.raw().to_le_bytes());
                meta_data.extend_from_slice(&target_id.raw().to_le_bytes());
                meta_data.push(ref_type.to_byte());
                meta_data.extend_from_slice(&(collection.len() as u32).to_le_bytes());
                meta_data.extend_from_slice(collection.as_bytes());
            }
        }

        // Create metadata page with Header type
        let mut meta_page = crate::storage::engine::Page::new(
            crate::storage::engine::PageType::Header,
            1, // page_id = 1
        );
        // Copy metadata into page content area (after header)
        let page_data = meta_page.as_bytes_mut();
        let content_start = crate::storage::engine::HEADER_SIZE;
        let copy_len = meta_data.len().min(page_data.len() - content_start);
        page_data[content_start..content_start + copy_len].copy_from_slice(&meta_data[..copy_len]);

        // Write page
        pager.write_page(1, meta_page).map_err(|e| {
            StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                e.to_string(),
            ))
        })?;

        // Flush all pages to disk
        pager.flush().map_err(|e| {
            StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                e.to_string(),
            ))
        })?;

        Ok(())
    }

    /// Check if the store is using page-based persistence
    pub fn is_paged(&self) -> bool {
        self.pager.is_some()
    }

    /// Get the database file path (if using paged mode)
    pub fn db_path(&self) -> Option<&Path> {
        self.db_path.as_deref()
    }

    /// Create a new collection
    pub fn create_collection(&self, name: impl Into<String>) -> Result<(), StoreError> {
        let name = name.into();
        let mut collections = self.collections.write().unwrap();

        if collections.contains_key(&name) {
            return Err(StoreError::CollectionExists(name));
        }

        let manager = SegmentManager::with_config(&name, self.config.manager_config.clone());
        collections.insert(name, Arc::new(manager));

        Ok(())
    }

    /// Get or create a collection
    pub fn get_or_create_collection(&self, name: impl Into<String>) -> Arc<SegmentManager> {
        let name = name.into();
        let mut collections = self.collections.write().unwrap();

        if let Some(manager) = collections.get(&name) {
            return Arc::clone(manager);
        }

        let manager = Arc::new(SegmentManager::with_config(
            &name,
            self.config.manager_config.clone(),
        ));
        collections.insert(name, Arc::clone(&manager));
        manager
    }

    /// Get a collection
    pub fn get_collection(&self, name: &str) -> Option<Arc<SegmentManager>> {
        self.collections.read().unwrap().get(name).map(Arc::clone)
    }

    /// List all collections
    pub fn list_collections(&self) -> Vec<String> {
        self.collections.read().unwrap().keys().cloned().collect()
    }

    /// Drop a collection
    pub fn drop_collection(&self, name: &str) -> Result<(), StoreError> {
        let mut collections = self.collections.write().unwrap();

        if collections.remove(name).is_none() {
            return Err(StoreError::CollectionNotFound(name.to_string()));
        }

        Ok(())
    }

    /// Insert an entity into a collection
    pub fn insert(&self, collection: &str, entity: UnifiedEntity) -> Result<EntityId, StoreError> {
        let manager = self
            .get_collection(collection)
            .ok_or_else(|| StoreError::CollectionNotFound(collection.to_string()))?;

        let mut entity = entity;
        if entity.id.raw() == 0 {
            entity.id = self.next_entity_id();
        } else {
            self.register_entity_id(entity.id);
        }
        let id = manager.insert(entity)?;
        self.register_entity_id(id);

        // Also insert into B-tree index if pager is active
        if let Some(pager) = &self.pager {
            if let Some(entity) = manager.get(id) {
                let mut btree_indices = self.btree_indices.write().unwrap();
                let btree = btree_indices
                    .entry(collection.to_string())
                    .or_insert_with(|| BTree::new(Arc::clone(pager)));

                let key = id.raw().to_le_bytes();
                let value = Self::serialize_entity(&entity, self.format_version());
                // Ignore duplicate key errors (update scenario)
                let _ = btree.insert(&key, &value);
            }
        }

        // Index cross-references if enabled
        if self.config.auto_index_refs {
            if let Some(entity) = manager.get(id) {
                self.index_cross_refs(&entity, collection);
            }
        }

        Ok(id)
    }

    /// Insert an entity, creating collection if needed
    pub fn insert_auto(
        &self,
        collection: &str,
        entity: UnifiedEntity,
    ) -> Result<EntityId, StoreError> {
        let manager = self.get_or_create_collection(collection);
        let mut entity = entity;
        if entity.id.raw() == 0 {
            entity.id = self.next_entity_id();
        } else {
            self.register_entity_id(entity.id);
        }
        let id = manager.insert(entity)?;
        self.register_entity_id(id);

        // Also insert into B-tree index if pager is active
        if let Some(pager) = &self.pager {
            if let Some(entity) = manager.get(id) {
                let mut btree_indices = self.btree_indices.write().unwrap();
                let btree = btree_indices
                    .entry(collection.to_string())
                    .or_insert_with(|| BTree::new(Arc::clone(pager)));

                let key = id.raw().to_le_bytes();
                let value = Self::serialize_entity(&entity, self.format_version());
                let _ = btree.insert(&key, &value);
            }
        }

        if self.config.auto_index_refs {
            if let Some(entity) = manager.get(id) {
                self.index_cross_refs(&entity, collection);
            }
        }

        Ok(id)
    }

    /// Get an entity from a collection
    ///
    /// Uses B-tree index for O(log n) lookup when page-based storage is active.
    /// Falls back to linear scan through SegmentManager otherwise.
    pub fn get(&self, collection: &str, id: EntityId) -> Option<UnifiedEntity> {
        // Try B-tree index first for O(log n) lookup
        if self.pager.is_some() {
            let btree_indices = self.btree_indices.read().unwrap();
            if let Some(btree) = btree_indices.get(collection) {
                let key = id.raw().to_le_bytes();
                if let Ok(Some(value)) = btree.get(&key) {
                    if let Ok(entity) = Self::deserialize_entity(&value, self.format_version()) {
                        return Some(entity);
                    }
                }
            }
        }

        // Fall back to SegmentManager
        self.get_collection(collection)?.get(id)
    }

    /// Get an entity from any collection
    pub fn get_any(&self, id: EntityId) -> Option<(String, UnifiedEntity)> {
        let collections = self.collections.read().unwrap();
        for (name, manager) in collections.iter() {
            if let Some(entity) = manager.get(id) {
                return Some((name.clone(), entity));
            }
        }
        None
    }

    /// Delete an entity
    pub fn delete(&self, collection: &str, id: EntityId) -> Result<bool, StoreError> {
        let manager = self
            .get_collection(collection)
            .ok_or_else(|| StoreError::CollectionNotFound(collection.to_string()))?;

        // Remove from B-tree index if active
        if self.pager.is_some() {
            let btree_indices = self.btree_indices.read().unwrap();
            if let Some(btree) = btree_indices.get(collection) {
                let key = id.raw().to_le_bytes();
                let _ = btree.delete(&key);
            }
        }

        // Remove cross-references
        self.unindex_cross_refs(id);

        Ok(manager.delete(id)?)
    }

    /// Set metadata for an entity
    pub fn set_metadata(
        &self,
        collection: &str,
        id: EntityId,
        metadata: Metadata,
    ) -> Result<(), StoreError> {
        let manager = self
            .get_collection(collection)
            .ok_or_else(|| StoreError::CollectionNotFound(collection.to_string()))?;

        Ok(manager.set_metadata(id, metadata)?)
    }

    /// Get metadata for an entity
    pub fn get_metadata(&self, collection: &str, id: EntityId) -> Option<Metadata> {
        self.get_collection(collection)?.get_metadata(id)
    }

    /// Add a cross-reference between entities
    pub fn add_cross_ref(
        &self,
        source_collection: &str,
        source_id: EntityId,
        target_collection: &str,
        target_id: EntityId,
        ref_type: RefType,
        weight: f32,
    ) -> Result<(), StoreError> {
        // Check source exists
        let source_manager = self
            .get_collection(source_collection)
            .ok_or_else(|| StoreError::CollectionNotFound(source_collection.to_string()))?;

        if source_manager.get(source_id).is_none() {
            return Err(StoreError::EntityNotFound(source_id));
        }

        // Check target exists
        let target_manager = self
            .get_collection(target_collection)
            .ok_or_else(|| StoreError::CollectionNotFound(target_collection.to_string()))?;

        if target_manager.get(target_id).is_none() {
            return Err(StoreError::EntityNotFound(target_id));
        }

        // Check limits
        let current_refs = self
            .cross_refs
            .read()
            .unwrap()
            .get(&source_id)
            .map_or(0, |v| v.len());

        if current_refs >= self.config.max_cross_refs {
            return Err(StoreError::TooManyRefs(source_id));
        }

        {
            let mut forward = self.cross_refs.write().unwrap();
            let refs = forward.entry(source_id).or_default();
            if !refs.iter().any(|(id, kind, coll)| {
                *id == target_id && *kind == ref_type && coll == target_collection
            }) {
                refs.push((target_id, ref_type, target_collection.to_string()));
            }
        }

        {
            let mut reverse = self.reverse_refs.write().unwrap();
            let refs = reverse.entry(target_id).or_default();
            if !refs.iter().any(|(id, kind, coll)| {
                *id == source_id && *kind == ref_type && coll == source_collection
            }) {
                refs.push((source_id, ref_type, source_collection.to_string()));
            }
        }

        if let Some(mut entity) = source_manager.get(source_id) {
            if !entity.cross_refs.iter().any(|xref| {
                xref.target == target_id
                    && xref.ref_type == ref_type
                    && xref.target_collection == target_collection
            }) {
                let cross_ref = CrossRef::with_weight(
                    source_id,
                    target_id,
                    target_collection,
                    ref_type,
                    weight,
                );
                entity.add_cross_ref(cross_ref);
                let _ = source_manager.update(entity);
            }
        }

        Ok(())
    }

    /// Get cross-references from an entity
    pub fn get_refs_from(&self, id: EntityId) -> Vec<(EntityId, RefType, String)> {
        self.cross_refs
            .read()
            .unwrap()
            .get(&id)
            .cloned()
            .unwrap_or_default()
    }

    /// Get cross-references to an entity
    pub fn get_refs_to(&self, id: EntityId) -> Vec<(EntityId, RefType, String)> {
        self.reverse_refs
            .read()
            .unwrap()
            .get(&id)
            .cloned()
            .unwrap_or_default()
    }

    /// Expand cross-references to get related entities
    pub fn expand_refs(
        &self,
        id: EntityId,
        depth: u32,
        ref_types: Option<&[RefType]>,
    ) -> Vec<(UnifiedEntity, u32, RefType)> {
        let mut results = Vec::new();
        let mut visited = std::collections::HashSet::new();
        visited.insert(id);

        self.expand_refs_recursive(id, depth, ref_types, &mut visited, &mut results, 1);

        results
    }

    fn expand_refs_recursive(
        &self,
        id: EntityId,
        max_depth: u32,
        ref_types: Option<&[RefType]>,
        visited: &mut std::collections::HashSet<EntityId>,
        results: &mut Vec<(UnifiedEntity, u32, RefType)>,
        current_depth: u32,
    ) {
        if current_depth > max_depth {
            return;
        }

        for (target_id, ref_type, target_collection) in self.get_refs_from(id) {
            if visited.contains(&target_id) {
                continue;
            }

            if let Some(types) = ref_types {
                if !types.contains(&ref_type) {
                    continue;
                }
            }

            visited.insert(target_id);

            if let Some(entity) = self.get(&target_collection, target_id) {
                results.push((entity, current_depth, ref_type));

                // Recurse
                self.expand_refs_recursive(
                    target_id,
                    max_depth,
                    ref_types,
                    visited,
                    results,
                    current_depth + 1,
                );
            }
        }
    }

    /// Index cross-references from an entity
    fn index_cross_refs(&self, entity: &UnifiedEntity, collection: &str) {
        for cross_ref in &entity.cross_refs {
            if cross_ref.target_collection.is_empty() {
                continue;
            }
            {
                let mut forward = self.cross_refs.write().unwrap();
                let refs = forward.entry(cross_ref.source).or_default();
                if !refs.iter().any(|(id, kind, coll)| {
                    *id == cross_ref.target
                        && *kind == cross_ref.ref_type
                        && coll == &cross_ref.target_collection
                }) {
                    refs.push((
                        cross_ref.target,
                        cross_ref.ref_type,
                        cross_ref.target_collection.clone(),
                    ));
                }
            }

            {
                let mut reverse = self.reverse_refs.write().unwrap();
                let refs = reverse.entry(cross_ref.target).or_default();
                if !refs.iter().any(|(id, kind, coll)| {
                    *id == cross_ref.source && *kind == cross_ref.ref_type && coll == collection
                }) {
                    refs.push((cross_ref.source, cross_ref.ref_type, collection.to_string()));
                }
            }
        }
    }

    /// Remove cross-references for an entity
    fn unindex_cross_refs(&self, id: EntityId) {
        // Remove forward refs
        self.cross_refs.write().unwrap().remove(&id);

        // Remove from reverse refs (scan all)
        let mut reverse = self.reverse_refs.write().unwrap();
        for refs in reverse.values_mut() {
            refs.retain(|(source, _, _)| *source != id);
        }
        reverse.remove(&id);
    }

    /// Query across all collections with a filter
    pub fn query_all<F>(&self, filter: F) -> Vec<(String, UnifiedEntity)>
    where
        F: Fn(&UnifiedEntity) -> bool + Clone,
    {
        let mut results = Vec::new();
        let collections = self.collections.read().unwrap();

        for (name, manager) in collections.iter() {
            for entity in manager.query_all(filter.clone()) {
                results.push((name.clone(), entity));
            }
        }

        results
    }

    /// Filter by metadata across all collections
    pub fn filter_metadata_all(
        &self,
        filters: &[(String, MetadataFilter)],
    ) -> Vec<(String, EntityId)> {
        let mut results = Vec::new();
        let collections = self.collections.read().unwrap();

        for (name, manager) in collections.iter() {
            for id in manager.filter_metadata(filters) {
                results.push((name.clone(), id));
            }
        }

        results
    }

    /// Get statistics
    pub fn stats(&self) -> StoreStats {
        let collections = self.collections.read().unwrap();

        let mut stats = StoreStats {
            collection_count: collections.len(),
            ..Default::default()
        };

        for (name, manager) in collections.iter() {
            let manager_stats = manager.stats();
            stats.total_entities += manager_stats.total_entities;
            stats.total_memory_bytes += manager_stats.total_memory_bytes;
            stats.collections.insert(name.clone(), manager_stats);
        }

        stats
    }

    /// Run maintenance on all collections
    pub fn run_maintenance(&self) -> Result<(), StoreError> {
        let collections = self.collections.read().unwrap();
        for manager in collections.values() {
            manager.run_maintenance()?;
        }
        Ok(())
    }
}

impl Default for UnifiedStore {
    fn default() -> Self {
        Self::new()
    }
}

// Builder for creating entities with a fluent API
pub struct EntityBuilder {
    store: Arc<UnifiedStore>,
    collection: String,
    entity: UnifiedEntity,
}

impl EntityBuilder {
    /// Start building an entity
    pub fn new(
        store: Arc<UnifiedStore>,
        collection: impl Into<String>,
        kind: EntityKind,
        data: EntityData,
    ) -> Self {
        let collection_name = collection.into();
        let _ = store.get_or_create_collection(&collection_name);
        let id = store.next_entity_id();

        Self {
            store,
            collection: collection_name,
            entity: UnifiedEntity::new(id, kind, data),
        }
    }

    /// Add metadata
    pub fn metadata(self, key: impl Into<String>, value: MetadataValue) -> Self {
        // Store metadata separately via manager after insert
        self
    }

    /// Add an embedding
    pub fn embedding(
        mut self,
        name: impl Into<String>,
        vector: Vec<f32>,
        model: impl Into<String>,
    ) -> Self {
        self.entity
            .add_embedding(EmbeddingSlot::new(name, vector, model));
        self
    }

    /// Add a cross-reference
    pub fn cross_ref(
        mut self,
        target: EntityId,
        target_collection: impl Into<String>,
        ref_type: RefType,
    ) -> Self {
        self.entity.add_cross_ref(CrossRef::new(
            self.entity.id,
            target,
            target_collection,
            ref_type,
        ));
        self
    }

    /// Build and insert the entity
    pub fn insert(self) -> Result<EntityId, StoreError> {
        self.store.insert(&self.collection, self.entity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::schema::Value;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn test_store_basic() {
        let store = UnifiedStore::new();
        store.create_collection("hosts").unwrap();

        let entity = UnifiedEntity::table_row(
            store.next_entity_id(),
            "hosts",
            1,
            vec![Value::Text("192.168.1.1".to_string())],
        );

        let id = store.insert("hosts", entity).unwrap();
        assert!(store.get("hosts", id).is_some());
    }

    #[test]
    fn test_store_auto_create() {
        let store = UnifiedStore::new();

        let entity =
            UnifiedEntity::vector(store.next_entity_id(), "embeddings", vec![0.1, 0.2, 0.3]);

        let id = store.insert_auto("new_collection", entity).unwrap();
        assert!(store.get("new_collection", id).is_some());
    }

    #[test]
    fn test_cross_references() {
        let store = UnifiedStore::new();

        // Create hosts collection
        let host_entity = UnifiedEntity::table_row(
            store.next_entity_id(),
            "hosts",
            1,
            vec![Value::Text("192.168.1.1".to_string())],
        );
        let host_id = store.insert_auto("hosts", host_entity).unwrap();

        // Create vulns collection
        let vuln_entity = UnifiedEntity::table_row(
            store.next_entity_id(),
            "vulns",
            1,
            vec![Value::Text("CVE-2024-1234".to_string())],
        );
        let vuln_id = store.insert_auto("vulns", vuln_entity).unwrap();

        // Add cross-reference
        store
            .add_cross_ref("hosts", host_id, "vulns", vuln_id, RefType::RelatedTo, 1.0)
            .unwrap();

        // Verify forward reference
        let refs = store.get_refs_from(host_id);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].0, vuln_id);

        // Verify reverse reference
        let back_refs = store.get_refs_to(vuln_id);
        assert_eq!(back_refs.len(), 1);
        assert_eq!(back_refs[0].0, host_id);
    }

    #[test]
    fn test_expand_refs() {
        let store = UnifiedStore::new();

        // Create a chain: A → B → C
        let _ = store.get_or_create_collection("test");

        let a = UnifiedEntity::vector(store.next_entity_id(), "v", vec![0.1]);
        let a_id = store.insert_auto("test", a).unwrap();

        let b = UnifiedEntity::vector(store.next_entity_id(), "v", vec![0.2]);
        let b_id = store.insert_auto("test", b).unwrap();

        let c = UnifiedEntity::vector(store.next_entity_id(), "v", vec![0.3]);
        let c_id = store.insert_auto("test", c).unwrap();

        store
            .add_cross_ref("test", a_id, "test", b_id, RefType::SimilarTo, 0.9)
            .unwrap();
        store
            .add_cross_ref("test", b_id, "test", c_id, RefType::SimilarTo, 0.8)
            .unwrap();

        // Expand from A with depth 2
        let expanded = store.expand_refs(a_id, 2, None);
        assert_eq!(expanded.len(), 2); // Should find B and C
    }

    #[test]
    fn test_query_all_collections() {
        let store = UnifiedStore::new();

        // Insert into multiple collections
        store
            .insert_auto(
                "hosts",
                UnifiedEntity::table_row(store.next_entity_id(), "hosts", 1, vec![]),
            )
            .unwrap();

        store
            .insert_auto(
                "vulns",
                UnifiedEntity::table_row(store.next_entity_id(), "vulns", 1, vec![]),
            )
            .unwrap();

        // Query all
        let results = store.query_all(|_| true);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_stats() {
        let store = UnifiedStore::new();

        let _ = store.get_or_create_collection("test");
        for i in 0..5 {
            store
                .insert_auto(
                    "test",
                    UnifiedEntity::vector(store.next_entity_id(), "v", vec![i as f32]),
                )
                .unwrap();
        }

        let stats = store.stats();
        assert_eq!(stats.collection_count, 1);
        assert_eq!(stats.total_entities, 5);
    }

    struct FileGuard {
        path: PathBuf,
    }

    impl Drop for FileGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn temp_path(name: &str) -> (FileGuard, PathBuf) {
        let path =
            std::env::temp_dir().join(format!("rb_store_{}_{}.rdb", name, std::process::id()));
        let guard = FileGuard { path: path.clone() };
        let _ = std::fs::remove_file(&path);
        (guard, path)
    }

    #[test]
    fn test_cross_refs_persist_file_mode() {
        let (_guard, path) = temp_path("file");
        let store = UnifiedStore::new();

        let row = UnifiedEntity::table_row(
            store.next_entity_id(),
            "hosts",
            1,
            vec![Value::Text("10.0.0.1".to_string())],
        );
        let row_id = store.insert_auto("hosts", row).unwrap();

        let node =
            UnifiedEntity::graph_node(store.next_entity_id(), "host", "asset", HashMap::new());
        let node_id = store.insert_auto("graph", node).unwrap();

        let vector =
            UnifiedEntity::vector(store.next_entity_id(), "embeddings", vec![0.1, 0.2, 0.3]);
        let vector_id = store.insert_auto("embeddings", vector).unwrap();

        store
            .add_cross_ref("hosts", row_id, "graph", node_id, RefType::RowToNode, 1.0)
            .unwrap();
        store
            .add_cross_ref(
                "graph",
                node_id,
                "embeddings",
                vector_id,
                RefType::NodeToVector,
                1.0,
            )
            .unwrap();

        store.save_to_file(&path).unwrap();

        let loaded = UnifiedStore::load_from_file(&path).unwrap();
        let refs = loaded.get_refs_from(row_id);
        assert!(refs.iter().any(|(id, kind, coll)| {
            *id == node_id && *kind == RefType::RowToNode && coll == "graph"
        }));

        let graph_refs = loaded.get_refs_from(node_id);
        assert!(graph_refs.iter().any(|(id, kind, coll)| {
            *id == vector_id && *kind == RefType::NodeToVector && coll == "embeddings"
        }));

        let expanded = loaded.expand_refs(row_id, 2, None);
        assert!(expanded
            .iter()
            .any(|(entity, depth, _)| { entity.id == node_id && *depth == 1 }));
        assert!(expanded
            .iter()
            .any(|(entity, depth, _)| { entity.id == vector_id && *depth == 2 }));
    }

    #[test]
    fn test_cross_refs_persist_paged_mode() {
        let (_guard, path) = temp_path("paged");
        let store = UnifiedStore::open(&path).unwrap();

        let row = UnifiedEntity::table_row(store.next_entity_id(), "hosts", 1, vec![]);
        let row_id = store.insert_auto("hosts", row).unwrap();

        let node =
            UnifiedEntity::graph_node(store.next_entity_id(), "host", "asset", HashMap::new());
        let node_id = store.insert_auto("graph", node).unwrap();

        store
            .add_cross_ref("hosts", row_id, "graph", node_id, RefType::RowToNode, 1.0)
            .unwrap();

        store.persist().unwrap();

        drop(store);

        let loaded = UnifiedStore::open(&path).unwrap();
        let refs = loaded.get_refs_from(row_id);
        assert!(refs.iter().any(|(id, kind, coll)| {
            *id == node_id && *kind == RefType::RowToNode && coll == "graph"
        }));
    }

    #[test]
    fn test_global_ids_unique_across_collections() {
        let store = UnifiedStore::new();

        let entity_a = UnifiedEntity::table_row(EntityId::new(0), "alpha", 1, vec![]);
        let entity_b = UnifiedEntity::table_row(EntityId::new(0), "beta", 1, vec![]);

        let id_a = store.insert_auto("alpha", entity_a).unwrap();
        let id_b = store.insert_auto("beta", entity_b).unwrap();

        assert_ne!(id_a, id_b);

        store
            .add_cross_ref("alpha", id_a, "beta", id_b, RefType::RelatedTo, 1.0)
            .unwrap();

        let expanded = store.expand_refs(id_a, 1, None);
        assert!(expanded.iter().any(|(entity, _, _)| entity.id == id_b));
    }
}
