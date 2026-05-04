//! High-Performance Disk-Backed Graph Storage Engine
//!
//! A concurrent, page-based graph storage optimized for:
//! - Lock-free reads with RwLock for concurrent traversal
//! - Cache-friendly packed layouts for nodes and edges
//! - B+ tree index for O(log n) edge lookups
//! - Streaming iteration for large graphs
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                       GraphStore                                 │
//! ├─────────────────────────────────────────────────────────────────┤
//! │  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐        │
//! │  │ NodeIndex│  │EdgeIndex │  │ NodePages│  │ EdgePages│        │
//! │  │ (B+ Tree)│  │ (B+ Tree)│  │ (Packed) │  │ (Packed) │        │
//! │  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬─────┘        │
//! │       │              │             │             │              │
//! │  ┌────▼──────────────▼─────────────▼─────────────▼────┐        │
//! │  │                    Pager (4KB Pages)                │        │
//! │  └────────────────────────────────────────────────────┘        │
//! │                              │                                  │
//! │  ┌───────────────────────────▼────────────────────────┐        │
//! │  │              SIEVE PageCache (lock-free reads)      │        │
//! │  └────────────────────────────────────────────────────┘        │
//! └─────────────────────────────────────────────────────────────────┘
//! ```

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::RwLock;

use super::page::{Page, PageType, PAGE_SIZE};

/// Maximum key size for node/edge IDs
pub const MAX_ID_SIZE: usize = 256;

/// Maximum label size
pub const MAX_LABEL_SIZE: usize = 512;

/// V1 node record header size: id_len(2) + label_len(2) + type(1) + flags(1) + edge_count(4).
/// Kept for [`StoredNode::decode_v1`]; new writes use [`NODE_HEADER_SIZE`].
pub const NODE_HEADER_SIZE_V1: usize = 10;

/// Node record header size: id_len(2) + label_len(2) + label_id(4) + flags(1) + edge_count(4).
/// The 1-byte legacy `node_type` discriminant has been replaced by a 4-byte
/// dynamic [`LabelId`] resolved through [`LabelRegistry`].
pub const NODE_HEADER_SIZE: usize = 13;

/// TableRef size: table_id(2) + row_id(8)
pub const TABLE_REF_SIZE: usize = 10;

/// Node flag: has table reference
pub const NODE_FLAG_HAS_TABLE_REF: u8 = 0x01;
/// Node flag: has vector embedding reference
pub const NODE_FLAG_HAS_VECTOR_REF: u8 = 0x02;

/// VectorRef size: collection_len(2) + vector_id(8) = 10 bytes header + variable collection name
pub const VECTOR_REF_HEADER_SIZE: usize = 10;

/// Reference to a table row (for linking graph nodes to tabular data)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TableRef {
    /// Table identifier (index into table registry)
    pub table_id: u16,
    /// Row ID within the table
    pub row_id: u64,
}

impl TableRef {
    /// Create a new table reference
    pub fn new(table_id: u16, row_id: u64) -> Self {
        Self { table_id, row_id }
    }

    /// Encode to bytes (10 bytes total)
    pub fn encode(&self) -> [u8; TABLE_REF_SIZE] {
        let mut buf = [0u8; TABLE_REF_SIZE];
        buf[0..2].copy_from_slice(&self.table_id.to_le_bytes());
        buf[2..10].copy_from_slice(&self.row_id.to_le_bytes());
        buf
    }

    /// Decode from bytes
    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < TABLE_REF_SIZE {
            return None;
        }
        Some(Self {
            table_id: u16::from_le_bytes([data[0], data[1]]),
            row_id: u64::from_le_bytes([
                data[2], data[3], data[4], data[5], data[6], data[7], data[8], data[9],
            ]),
        })
    }
}

/// V1 edge record header size: source_len(2) + target_len(2) + type(1) + weight(4).
/// Kept for [`StoredEdge::decode_v1`]; new writes use [`EDGE_HEADER_SIZE`].
pub const EDGE_HEADER_SIZE_V1: usize = 9;

/// Edge record header size: source_len(2) + target_len(2) + label_id(4) + weight(4).
/// The 1-byte legacy `edge_type` discriminant has been replaced by a 4-byte
/// dynamic [`LabelId`] resolved through [`LabelRegistry`].
pub const EDGE_HEADER_SIZE: usize = 12;

/// A graph node stored on disk
#[derive(Debug, Clone)]
pub struct StoredNode {
    pub id: String,
    pub label: String,
    /// Canonical category label string (e.g. `"host"`, `"order"`). Resolved
    /// from [`label_id`] at decode time via the legacy seed mapping.
    /// Caller-visible string; the registry-stored [`label_id`] is the
    /// source-of-truth identifier.
    pub node_type: String,
    /// Authoritative label identifier resolved through [`LabelRegistry`].
    pub label_id: LabelId,
    pub flags: u8,
    pub out_edge_count: u32,
    pub in_edge_count: u32,
    /// Page ID where this node is stored
    pub page_id: u32,
    /// Slot index within the page
    pub slot: u16,
    /// Optional reference to a table row (for unified queries)
    pub table_ref: Option<TableRef>,
    /// Optional reference to a vector embedding (collection name, vector_id)
    pub vector_ref: Option<(String, u64)>,
}

impl StoredNode {
    /// Encode node to bytes in v2 format (label_id replaces node_type).
    pub fn encode(&self) -> Vec<u8> {
        let id_bytes = self.id.as_bytes();
        let label_bytes = self.label.as_bytes();
        let has_table_ref = self.table_ref.is_some();
        let has_vector_ref = self.vector_ref.is_some();

        // Compute flags with table_ref and vector_ref indicators
        let mut flags = self.flags & !(NODE_FLAG_HAS_TABLE_REF | NODE_FLAG_HAS_VECTOR_REF);
        if has_table_ref {
            flags |= NODE_FLAG_HAS_TABLE_REF;
        }
        if has_vector_ref {
            flags |= NODE_FLAG_HAS_VECTOR_REF;
        }

        let table_ref_size = if has_table_ref { TABLE_REF_SIZE } else { 0 };
        let vector_ref_size = if let Some((ref coll, _)) = self.vector_ref {
            2 + coll.len() + 8
        } else {
            0
        };
        let mut buf = Vec::with_capacity(
            NODE_HEADER_SIZE
                + id_bytes.len()
                + label_bytes.len()
                + table_ref_size
                + vector_ref_size,
        );

        // V2 header: id_len(2) + label_len(2) + label_id(4) + flags(1) + out_edges(2) + in_edges(2)
        buf.extend_from_slice(&(id_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(&(label_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(&self.label_id.as_u32().to_le_bytes());
        buf.push(flags);
        buf.extend_from_slice(&(self.out_edge_count as u16).to_le_bytes());
        buf.extend_from_slice(&(self.in_edge_count as u16).to_le_bytes());

        buf.extend_from_slice(id_bytes);
        buf.extend_from_slice(label_bytes);

        if let Some(ref tref) = self.table_ref {
            buf.extend_from_slice(&tref.encode());
        }

        if let Some((ref collection, vector_id)) = self.vector_ref {
            let coll_bytes = collection.as_bytes();
            buf.extend_from_slice(&(coll_bytes.len() as u16).to_le_bytes());
            buf.extend_from_slice(coll_bytes);
            buf.extend_from_slice(&vector_id.to_le_bytes());
        }

        buf
    }

    /// Decode node from bytes (v2 format). For v1 records use [`decode_v1`].
    pub fn decode(data: &[u8], page_id: u32, slot: u16) -> Option<Self> {
        if data.len() < NODE_HEADER_SIZE {
            return None;
        }

        let id_len = u16::from_le_bytes([data[0], data[1]]) as usize;
        let label_len = u16::from_le_bytes([data[2], data[3]]) as usize;
        let label_id = LabelId::new(u32::from_le_bytes([data[4], data[5], data[6], data[7]]));
        let flags = data[8];
        let out_edge_count = u16::from_le_bytes([data[9], data[10]]) as u32;
        let in_edge_count = u16::from_le_bytes([data[11], data[12]]) as u32;
        // Derive legacy node_type from label_id for back-compat with callers
        // that still read the field. PR3 removes this field entirely.
        let node_type = label_id_to_node_label(label_id);

        Self::decode_payload(
            data,
            page_id,
            slot,
            NODE_HEADER_SIZE,
            id_len,
            label_len,
            flags,
            out_edge_count,
            in_edge_count,
            node_type,
            label_id,
        )
    }

    /// Decode a v1 (legacy) node record. The caller must supply a
    /// [`LabelRegistry`] seeded via [`LabelRegistry::with_legacy_seed`] so
    /// the legacy `node_type` discriminant maps to the correct reserved
    /// [`LabelId`].
    pub fn decode_v1(data: &[u8], page_id: u32, slot: u16) -> Option<Self> {
        if data.len() < NODE_HEADER_SIZE_V1 {
            return None;
        }
        let id_len = u16::from_le_bytes([data[0], data[1]]) as usize;
        let label_len = u16::from_le_bytes([data[2], data[3]]) as usize;
        // V1 records carry the legacy enum discriminant; reject any byte
        // outside the 9-variant range so we do not silently misinterpret
        // unrelated bytes as a node-type.
        if data[4] > 8 {
            return None;
        }
        let flags = data[5];
        let out_edge_count = u16::from_le_bytes([data[6], data[7]]) as u32;
        let in_edge_count = u16::from_le_bytes([data[8], data[9]]) as u32;
        let label_id = LabelRegistry::legacy_node_label_id(data[4]);
        let node_type = label_id_to_node_label(label_id);
        Self::decode_payload(
            data,
            page_id,
            slot,
            NODE_HEADER_SIZE_V1,
            id_len,
            label_len,
            flags,
            out_edge_count,
            in_edge_count,
            node_type,
            label_id,
        )
    }

    /// Shared payload (id, label, table_ref, vector_ref) decoder for v1/v2.
    #[allow(clippy::too_many_arguments)]
    fn decode_payload(
        data: &[u8],
        page_id: u32,
        slot: u16,
        header_size: usize,
        id_len: usize,
        label_len: usize,
        flags: u8,
        out_edge_count: u32,
        in_edge_count: u32,
        node_type: String,
        label_id: LabelId,
    ) -> Option<Self> {
        let has_table_ref = (flags & NODE_FLAG_HAS_TABLE_REF) != 0;
        let has_vector_ref = (flags & NODE_FLAG_HAS_VECTOR_REF) != 0;
        let table_ref_size = if has_table_ref { TABLE_REF_SIZE } else { 0 };

        let mut offset = header_size + id_len + label_len + table_ref_size;
        if data.len() < offset {
            return None;
        }

        let id = String::from_utf8_lossy(&data[header_size..header_size + id_len]).to_string();
        let label =
            String::from_utf8_lossy(&data[header_size + id_len..header_size + id_len + label_len])
                .to_string();

        let table_ref = if has_table_ref {
            let ref_start = header_size + id_len + label_len;
            TableRef::decode(&data[ref_start..])
        } else {
            None
        };

        let vector_ref = if has_vector_ref {
            if data.len() < offset + 2 {
                return None;
            }
            let coll_len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
            offset += 2;
            if data.len() < offset + coll_len + 8 {
                return None;
            }
            let collection = String::from_utf8_lossy(&data[offset..offset + coll_len]).to_string();
            offset += coll_len;
            let vector_id = u64::from_le_bytes(data[offset..offset + 8].try_into().ok()?);
            Some((collection, vector_id))
        } else {
            None
        };

        Some(Self {
            id,
            label,
            node_type,
            label_id,
            flags,
            out_edge_count,
            in_edge_count,
            page_id,
            slot,
            table_ref,
            vector_ref,
        })
    }

    /// Calculate encoded size
    pub fn encoded_size(&self) -> usize {
        let table_ref_size = if self.table_ref.is_some() {
            TABLE_REF_SIZE
        } else {
            0
        };
        let vector_ref_size = if let Some((ref coll, _)) = self.vector_ref {
            2 + coll.len() + 8
        } else {
            0
        };
        NODE_HEADER_SIZE + self.id.len() + self.label.len() + table_ref_size + vector_ref_size
    }

    /// Link this node to a table row
    pub fn link_to_row(&mut self, table_id: u16, row_id: u64) {
        self.table_ref = Some(TableRef::new(table_id, row_id));
    }

    /// Unlink from table row
    pub fn unlink_from_row(&mut self) {
        self.table_ref = None;
    }

    /// Link this node to a vector embedding
    pub fn link_to_vector(&mut self, collection: String, vector_id: u64) {
        self.vector_ref = Some((collection, vector_id));
    }

    /// Unlink from vector embedding
    pub fn unlink_from_vector(&mut self) {
        self.vector_ref = None;
    }

    /// Check if this node is linked to a table row
    pub fn is_linked(&self) -> bool {
        self.table_ref.is_some()
    }
}

/// A graph edge stored on disk
#[derive(Debug, Clone)]
pub struct StoredEdge {
    pub source_id: String,
    pub target_id: String,
    /// Canonical edge label string. Derived from [`label_id`] at decode time.
    pub edge_type: String,
    /// Authoritative label identifier resolved through [`LabelRegistry`].
    pub label_id: LabelId,
    pub weight: f32,
    /// Page ID where this edge is stored
    pub page_id: u32,
    /// Slot index within the page
    pub slot: u16,
}

impl StoredEdge {
    /// Encode edge to bytes (v2 format).
    pub fn encode(&self) -> Vec<u8> {
        let source_bytes = self.source_id.as_bytes();
        let target_bytes = self.target_id.as_bytes();

        let mut buf =
            Vec::with_capacity(EDGE_HEADER_SIZE + source_bytes.len() + target_bytes.len());

        // V2 header: source_len(2) + target_len(2) + label_id(4) + weight(4)
        buf.extend_from_slice(&(source_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(&(target_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(&self.label_id.as_u32().to_le_bytes());
        buf.extend_from_slice(&self.weight.to_le_bytes());

        buf.extend_from_slice(source_bytes);
        buf.extend_from_slice(target_bytes);

        buf
    }

    /// Decode edge from bytes (v2 format). For v1 records use [`decode_v1`].
    pub fn decode(data: &[u8], page_id: u32, slot: u16) -> Option<Self> {
        if data.len() < EDGE_HEADER_SIZE {
            return None;
        }

        let source_len = u16::from_le_bytes([data[0], data[1]]) as usize;
        let target_len = u16::from_le_bytes([data[2], data[3]]) as usize;
        let label_id = LabelId::new(u32::from_le_bytes([data[4], data[5], data[6], data[7]]));
        let weight = f32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        let edge_type = label_id_to_edge_label(label_id);

        if data.len() < EDGE_HEADER_SIZE + source_len + target_len {
            return None;
        }

        let source_id =
            String::from_utf8_lossy(&data[EDGE_HEADER_SIZE..EDGE_HEADER_SIZE + source_len])
                .to_string();
        let target_id = String::from_utf8_lossy(
            &data[EDGE_HEADER_SIZE + source_len..EDGE_HEADER_SIZE + source_len + target_len],
        )
        .to_string();

        Some(Self {
            source_id,
            target_id,
            edge_type,
            label_id,
            weight,
            page_id,
            slot,
        })
    }

    /// Decode a v1 (legacy) edge record. The 1-byte enum discriminant maps
    /// to the legacy reserved [`LabelId`] range via
    /// [`LabelRegistry::legacy_edge_label_id`].
    pub fn decode_v1(data: &[u8], page_id: u32, slot: u16) -> Option<Self> {
        if data.len() < EDGE_HEADER_SIZE_V1 {
            return None;
        }
        let source_len = u16::from_le_bytes([data[0], data[1]]) as usize;
        let target_len = u16::from_le_bytes([data[2], data[3]]) as usize;
        if data[4] > 9 {
            return None;
        }
        let weight = f32::from_le_bytes([data[5], data[6], data[7], data[8]]);
        let label_id = LabelRegistry::legacy_edge_label_id(data[4]);
        let edge_type = label_id_to_edge_label(label_id);

        if data.len() < EDGE_HEADER_SIZE_V1 + source_len + target_len {
            return None;
        }
        let source_id =
            String::from_utf8_lossy(&data[EDGE_HEADER_SIZE_V1..EDGE_HEADER_SIZE_V1 + source_len])
                .to_string();
        let target_id = String::from_utf8_lossy(
            &data[EDGE_HEADER_SIZE_V1 + source_len..EDGE_HEADER_SIZE_V1 + source_len + target_len],
        )
        .to_string();

        Some(Self {
            source_id,
            target_id,
            edge_type,
            label_id,
            weight,
            page_id,
            slot,
        })
    }

    /// Calculate encoded size (v2)
    pub fn encoded_size(&self) -> usize {
        EDGE_HEADER_SIZE + self.source_id.len() + self.target_id.len()
    }
}

/// Resolve a [`LabelId`] in the legacy reserved range to its canonical
/// category string. For non-legacy IDs (≥ [`FIRST_USER_LABEL_ID`]) returns
/// `format!("label_{}", id)` — a non-crashing placeholder that flags the
/// caller is reading a record without a registry handle. Real callers
/// should resolve through [`LabelRegistry`] when accuracy matters.
fn label_id_to_node_label(id: LabelId) -> String {
    match id.as_u32() {
        1 => "host".to_string(),
        2 => "service".to_string(),
        3 => "credential".to_string(),
        4 => "vulnerability".to_string(),
        5 => "endpoint".to_string(),
        6 => "technology".to_string(),
        7 => "user".to_string(),
        8 => "domain".to_string(),
        9 => "certificate".to_string(),
        n => format!("label_{}", n),
    }
}

/// Resolve a [`LabelId`] in the legacy reserved edge range to its canonical
/// edge label string.
fn label_id_to_edge_label(id: LabelId) -> String {
    match id.as_u32() {
        10 => "has_service".to_string(),
        11 => "has_endpoint".to_string(),
        12 => "uses_tech".to_string(),
        13 => "auth_access".to_string(),
        14 => "affected_by".to_string(),
        15 => "contains".to_string(),
        16 => "connects_to".to_string(),
        17 => "related_to".to_string(),
        18 => "has_user".to_string(),
        19 => "has_cert".to_string(),
        n => format!("label_{}", n),
    }
}

/// Location of a record in the graph store
#[derive(Debug, Clone, Copy)]
pub struct RecordLocation {
    pub page_id: u32,
    pub slot: u16,
}

/// Graph statistics
#[derive(Debug, Clone, Default)]
pub struct GraphStats {
    pub node_count: u64,
    pub edge_count: u64,
    pub node_pages: u32,
    pub edge_pages: u32,
    /// Cardinality per category label (e.g. `"host" → 42`). Replaces the
    /// closed-enum `nodes_by_type: [u64; 9]` from earlier revisions.
    pub nodes_by_label: HashMap<String, u64>,
    /// Cardinality per edge label.
    pub edges_by_label: HashMap<String, u64>,
}

/// Concurrent in-memory index for fast lookups
/// Uses sharded locks for reduced contention
pub struct ShardedIndex<V> {
    shards: Vec<RwLock<HashMap<String, V>>>,
    shard_count: usize,
}

impl<V: Clone> ShardedIndex<V> {
    pub fn new(shard_count: usize) -> Self {
        let shards = (0..shard_count)
            .map(|_| RwLock::new(HashMap::new()))
            .collect();
        Self {
            shards,
            shard_count,
        }
    }

    #[inline]
    fn shard_for(&self, key: &str) -> usize {
        // Simple hash-based sharding
        let hash: u64 = key
            .bytes()
            .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
        (hash as usize) % self.shard_count
    }

    pub fn get(&self, key: &str) -> Option<V> {
        let shard = self.shard_for(key);
        self.shards[shard].read().ok()?.get(key).cloned()
    }

    pub fn insert(&self, key: String, value: V) {
        let shard = self.shard_for(&key);
        if let Ok(mut guard) = self.shards[shard].write() {
            guard.insert(key, value);
        }
    }

    pub fn remove(&self, key: &str) -> Option<V> {
        let shard = self.shard_for(key);
        self.shards[shard].write().ok()?.remove(key)
    }

    pub fn contains(&self, key: &str) -> bool {
        let shard = self.shard_for(key);
        self.shards[shard]
            .read()
            .ok()
            .map(|g| g.contains_key(key))
            .unwrap_or(false)
    }

    pub fn len(&self) -> usize {
        self.shards
            .iter()
            .filter_map(|s| s.read().ok())
            .map(|g| g.len())
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Edge index key: `(source_id, edge_label)` → `Vec<target_id>`.
/// Optimized for adjacency list queries; the edge label is the canonical
/// string form (e.g. `"connects_to"`) — use the registry to resolve back to
/// a [`LabelId`] when needed.
pub struct EdgeIndex {
    /// Forward edges: source → `[(edge_label, target, weight)]`
    forward: ShardedIndex<Vec<(String, String, f32)>>,
    /// Backward edges: target → `[(edge_label, source, weight)]`
    backward: ShardedIndex<Vec<(String, String, f32)>>,
}

impl EdgeIndex {
    pub fn new(shard_count: usize) -> Self {
        Self {
            forward: ShardedIndex::new(shard_count),
            backward: ShardedIndex::new(shard_count),
        }
    }

    pub fn add_edge(&self, source: &str, target: &str, edge_label: &str, weight: f32) {
        let shard = self.forward.shard_for(source);
        if let Ok(mut guard) = self.forward.shards[shard].write() {
            guard
                .entry(source.to_string())
                .or_insert_with(Vec::new)
                .push((edge_label.to_string(), target.to_string(), weight));
        }

        let shard = self.backward.shard_for(target);
        if let Ok(mut guard) = self.backward.shards[shard].write() {
            guard
                .entry(target.to_string())
                .or_insert_with(Vec::new)
                .push((edge_label.to_string(), source.to_string(), weight));
        }
    }

    pub fn remove_edge(&self, source: &str, target: &str, edge_label: &str) {
        let shard = self.forward.shard_for(source);
        if let Ok(mut guard) = self.forward.shards[shard].write() {
            if let Some(edges) = guard.get_mut(source) {
                edges.retain(|(et, t, _)| !(et == edge_label && t == target));
            }
        }

        let shard = self.backward.shard_for(target);
        if let Ok(mut guard) = self.backward.shards[shard].write() {
            if let Some(edges) = guard.get_mut(target) {
                edges.retain(|(et, s, _)| !(et == edge_label && s == source));
            }
        }
    }

    pub fn outgoing(&self, source: &str) -> Vec<(String, String, f32)> {
        self.forward.get(source).unwrap_or_default()
    }

    pub fn incoming(&self, target: &str) -> Vec<(String, String, f32)> {
        self.backward.get(target).unwrap_or_default()
    }

    pub fn outgoing_of_type(&self, source: &str, edge_label: &str) -> Vec<(String, f32)> {
        self.forward
            .get(source)
            .unwrap_or_default()
            .into_iter()
            .filter(|(et, _, _)| et == edge_label)
            .map(|(_, t, w)| (t, w))
            .collect()
    }
}

/// High-performance graph storage engine
///
/// Provides concurrent read access with minimal locking overhead.
/// Writes are serialized through a write lock but reads can proceed in parallel.
pub struct GraphStore {
    /// Node index: node_id -> location
    node_index: ShardedIndex<RecordLocation>,
    /// Edge index: adjacency lists
    edge_index: EdgeIndex,
    /// Secondary inverted indexes on (type, label) for O(1) non-id lookups.
    /// Avoids full node-page scans in `nodes_of_type` / `nodes_by_label`.
    ///
    /// Stored as `Arc` so [`GraphStore::publish_indexes`] can share the
    /// exact live index with an [`crate::storage::index::IndexRegistry`]
    /// instead of handing out a frozen snapshot.
    node_secondary: std::sync::Arc<secondary_index::NodeSecondaryIndex>,
    /// Dynamic label catalog. Resolves user-supplied label strings to
    /// stable [`LabelId`] u32 values used in the v2 page format.
    pub registry: Arc<LabelRegistry>,
    /// Node pages (packed node records)
    node_pages: RwLock<Vec<Page>>,
    /// Edge pages (packed edge records)
    edge_pages: RwLock<Vec<Page>>,
    /// Current node page for inserts
    current_node_page: AtomicU32,
    /// Current edge page for inserts
    current_edge_page: AtomicU32,
    /// Statistics
    stats: GraphStats,
    node_count: AtomicU64,
    edge_count: AtomicU64,
}

#[path = "graph_store/impl.rs"]
mod graph_store_impl;
pub mod label_registry;
pub mod secondary_index;
pub use label_registry::{
    LabelId, LabelRegistry, LabelRegistryError, Namespace, FIRST_USER_LABEL_ID, MAX_LABEL_LEN,
    UNSET_LABEL_ID,
};
pub use secondary_index::NodeSecondaryIndex;
impl Default for GraphStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Iterator over all nodes in the graph
pub struct NodeIterator<'a> {
    store: &'a GraphStore,
    page_idx: usize,
    cell_idx: usize,
}

impl<'a> Iterator for NodeIterator<'a> {
    type Item = StoredNode;

    fn next(&mut self) -> Option<Self::Item> {
        let pages = self.store.node_pages.read().ok()?;

        loop {
            if self.page_idx >= pages.len() {
                return None;
            }

            let page = &pages[self.page_idx];
            let cell_count = page.cell_count() as usize;

            if self.cell_idx >= cell_count {
                self.page_idx += 1;
                self.cell_idx = 0;
                continue;
            }

            if let Ok((_, value)) = page.read_cell(self.cell_idx) {
                self.cell_idx += 1;
                if let Some(node) =
                    StoredNode::decode(&value, self.page_idx as u32, (self.cell_idx - 1) as u16)
                {
                    return Some(node);
                }
            } else {
                self.cell_idx += 1;
            }
        }
    }
}

/// Graph store errors
#[derive(Debug, Clone)]
pub enum GraphStoreError {
    NodeExists(String),
    NodeNotFound(String),
    EdgeNotFound(String, String),
    PageFull,
    LockPoisoned,
    InvalidData(String),
    IoError(String),
}

impl std::fmt::Display for GraphStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NodeExists(id) => write!(f, "Node already exists: {}", id),
            Self::NodeNotFound(id) => write!(f, "Node not found: {}", id),
            Self::EdgeNotFound(s, t) => write!(f, "Edge not found: {} -> {}", s, t),
            Self::PageFull => write!(f, "Page is full"),
            Self::LockPoisoned => write!(f, "Lock poisoned"),
            Self::InvalidData(msg) => write!(f, "Invalid data: {}", msg),
            Self::IoError(msg) => write!(f, "I/O error: {}", msg),
        }
    }
}

impl std::error::Error for GraphStoreError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_graph_store_basic() {
        let store = GraphStore::new();

        // Add nodes
        store
            .add_node_with_label("host:192.168.1.1", "Web Server", "host")
            .unwrap();
        store
            .add_node_with_label("host:192.168.1.2", "Database", "host")
            .unwrap();
        store
            .add_node_with_label("service:192.168.1.1:80:http", "HTTP", "service")
            .unwrap();

        assert_eq!(store.node_count(), 3);

        // Add edges
        store
            .add_edge_with_label(
                "host:192.168.1.1",
                "service:192.168.1.1:80:http",
                "has_service",
                1.0,
            )
            .unwrap();
        store
            .add_edge_with_label("host:192.168.1.1", "host:192.168.1.2", "connects_to", 1.0)
            .unwrap();

        assert_eq!(store.edge_count(), 2);

        // Query
        let node = store.get_node("host:192.168.1.1").unwrap();
        assert_eq!(node.label, "Web Server");

        let out_edges = store.outgoing_edges("host:192.168.1.1");
        assert_eq!(out_edges.len(), 2);
    }

    #[test]
    fn test_graph_store_serialization() {
        let store = GraphStore::new();

        store
            .add_node_with_label("host:10.0.0.1", "Server A", "host")
            .unwrap();
        store
            .add_node_with_label("host:10.0.0.2", "Server B", "host")
            .unwrap();
        store
            .add_edge_with_label("host:10.0.0.1", "host:10.0.0.2", "connects_to", 0.5)
            .unwrap();

        // Serialize
        let bytes = store.serialize();

        // Deserialize
        let restored = GraphStore::deserialize(&bytes).unwrap();

        assert_eq!(restored.node_count(), 2);
        assert_eq!(restored.edge_count(), 1);

        let node = restored.get_node("host:10.0.0.1").unwrap();
        assert_eq!(node.label, "Server A");
    }

    #[test]
    fn test_concurrent_reads() {
        use std::thread;

        let store = Arc::new(GraphStore::new());

        // Add some data
        for i in 0..100 {
            store
                .add_node_with_label(&format!("host:{}", i), &format!("Host {}", i), "host")
                .unwrap();
        }

        // Spawn multiple reader threads
        let mut handles = vec![];
        for _ in 0..4 {
            let store_clone = Arc::clone(&store);
            handles.push(thread::spawn(move || {
                for i in 0..100 {
                    let _ = store_clone.get_node(&format!("host:{}", i));
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(store.node_count(), 100);
    }

    #[test]
    fn test_edge_index_performance() {
        let store = GraphStore::new();

        // Create a graph with many edges
        store
            .add_node_with_label("hub", "Hub Node", "host")
            .unwrap();
        for i in 0..100 {
            store
                .add_node_with_label(&format!("spoke:{}", i), &format!("Spoke {}", i), "host")
                .unwrap();
            store
                .add_edge_with_label("hub", &format!("spoke:{}", i), "connects_to", 1.0)
                .unwrap();
        }

        // Query outgoing edges (should be fast with index)
        let edges = store.outgoing_edges("hub");
        assert_eq!(edges.len(), 100);
    }

    #[test]
    fn test_nodes_of_type_uses_secondary_index() {
        let store = GraphStore::new();
        store
            .add_node_with_label("host:1", "Web Server", "host")
            .unwrap();
        store
            .add_node_with_label("host:2", "DB Server", "host")
            .unwrap();
        store
            .add_node_with_label("svc:1", "HTTP", "service")
            .unwrap();
        store
            .add_node_with_label("vuln:1", "CVE-2024-1", "vulnerability")
            .unwrap();

        let hosts = store.nodes_with_category("host");
        assert_eq!(hosts.len(), 2);
        assert!(hosts.iter().all(|n| n.node_type == "host"));

        let services = store.nodes_with_category("service");
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].id, "svc:1");

        assert_eq!(store.nodes_with_category("user").len(), 0);
    }

    #[test]
    fn test_nodes_by_label_with_bloom_prune() {
        let store = GraphStore::new();
        store
            .add_node_with_label("host:1", "Edge Router", "host")
            .unwrap();
        store
            .add_node_with_label("host:2", "Edge Router", "host")
            .unwrap();
        store
            .add_node_with_label("host:3", "Core Switch", "host")
            .unwrap();

        let routers = store.nodes_by_label("Edge Router");
        assert_eq!(routers.len(), 2);

        let unknown = store.nodes_by_label("Quantum Router 9000");
        assert!(unknown.is_empty());
        // Bloom is allowed to false-positive but must never hide real labels.
        assert!(store.may_contain_label("Edge Router"));
        assert!(store.may_contain_label("Core Switch"));
    }

    #[test]
    fn test_publish_indexes_to_registry() {
        use crate::storage::index::{IndexKind, IndexRegistry, IndexScope};

        let store = GraphStore::new();
        store.add_node_with_label("h:1", "Alpha", "host").unwrap();
        store.add_node_with_label("h:2", "Beta", "host").unwrap();
        store
            .add_node_with_label("svc:1", "HTTP", "service")
            .unwrap();

        let registry = IndexRegistry::new();
        store.publish_indexes(&registry, "infra");

        let shared = registry.get(&IndexScope::graph("infra")).unwrap();
        let stats = shared.stats();
        // Two scopes × each insert = by_type + by_label per node
        // 3 inserts × 2 scopes = 6 entries
        assert_eq!(stats.entries, 6);
        assert_eq!(stats.kind, IndexKind::Inverted);
        assert!(stats.has_bloom);

        // Live updates are visible through the registry since both sides
        // share the same Arc<NodeSecondaryIndex>.
        store.add_node_with_label("h:3", "Gamma", "host").unwrap();
        let updated = registry.get(&IndexScope::graph("infra")).unwrap().stats();
        assert_eq!(updated.entries, 8);
    }

    #[test]
    fn test_secondary_index_rebuilt_after_deserialize() {
        let store = GraphStore::new();
        store
            .add_node_with_label("host:1", "Alpha", "host")
            .unwrap();
        store
            .add_node_with_label("svc:1", "HTTP", "service")
            .unwrap();

        let bytes = store.serialize();
        let restored = GraphStore::deserialize(&bytes).unwrap();

        assert_eq!(restored.nodes_with_category("host").len(), 1);
        assert_eq!(restored.nodes_by_label("HTTP").len(), 1);
        assert!(restored.may_contain_label("Alpha"));
    }

    #[test]
    fn test_node_iteration() {
        let store = GraphStore::new();

        for i in 0..50 {
            store
                .add_node_with_label(&format!("node:{}", i), &format!("Node {}", i), "host")
                .unwrap();
        }

        let nodes: Vec<_> = store.iter_nodes().collect();
        assert_eq!(nodes.len(), 50);
    }

    #[test]
    fn legacy_node_type_interns_into_registry() {
        let store = GraphStore::new();
        store.add_node_with_label("h1", "web", "host").unwrap();
        // Adding via the legacy enum must intern its as_str() name.
        let id = store
            .registry
            .lookup(label_registry::Namespace::Node, "host")
            .expect("legacy enum name should be interned");
        let fetched = store.get_node("h1").unwrap();
        assert_eq!(fetched.label_id, id);
        assert_eq!(fetched.node_type, "host");
    }

    #[test]
    fn v2_round_trip_preserves_user_labels() {
        let store = GraphStore::new();
        // Intern a non-legacy user label and add a node referencing it via
        // the legacy bridge (Host) — exercises the full v2 encode path.
        let user_id = store.intern_node_label("order").unwrap();
        assert!(user_id.as_u32() >= label_registry::FIRST_USER_LABEL_ID);

        store.add_node_with_label("h1", "web-1", "host").unwrap();
        store.add_node_with_label("h2", "web-2", "service").unwrap();
        store
            .add_edge_with_label("h1", "h2", "connects_to", 1.0)
            .unwrap();

        let bytes = store.serialize();
        // V2 magic + version
        assert_eq!(&bytes[0..4], b"RBGR");
        assert_eq!(
            u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            2
        );

        let restored = GraphStore::deserialize(&bytes).unwrap();
        // Registry survived.
        assert_eq!(
            restored
                .registry
                .lookup(label_registry::Namespace::Node, "order"),
            Some(user_id)
        );
        // Records decoded with v2 label_id intact.
        let h1 = restored.get_node("h1").unwrap();
        assert_eq!(h1.node_type, "host");
        assert_eq!(
            h1.label_id,
            restored
                .registry
                .lookup(label_registry::Namespace::Node, "host")
                .unwrap()
        );
        // Edge index rebuilt.
        let outgoing = restored.outgoing_edges("h1");
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].0, "connects_to");
    }

    #[test]
    fn v1_blob_deserializes_via_legacy_path() {
        // Hand-craft a minimal v1 file: header (24 bytes, version=1) + 1
        // node page + 1 edge page. The node page contains one v1 record:
        //   header_v1 = id_len(2) "n1" label_len(2) "L" type(1=Host) flags(0) out(0) in(0)
        //   payload   = "n1" "L"
        // The edge page contains one v1 edge:
        //   header_v1 = src_len(2) "n1" tgt_len(2) "n1" type(0=HasService) weight(1.0)
        //   payload   = "n1" "n1"
        //
        // Page::insert_cell handles the cell layout for us, so we build
        // pages programmatically rather than poking at raw page bytes.
        let mut node_page = Page::new(PageType::GraphNode, 0);
        // Build a v1 node record.
        let mut v1_node = Vec::new();
        v1_node.extend_from_slice(&2u16.to_le_bytes()); // id_len
        v1_node.extend_from_slice(&1u16.to_le_bytes()); // label_len
        v1_node.push(0); // "host" (disc=0)
        v1_node.push(0); // flags
        v1_node.extend_from_slice(&0u16.to_le_bytes()); // out_edge_count
        v1_node.extend_from_slice(&0u16.to_le_bytes()); // in_edge_count
        v1_node.extend_from_slice(b"n1");
        v1_node.extend_from_slice(b"L");
        node_page.insert_cell(b"n1", &v1_node).unwrap();

        let mut edge_page = Page::new(PageType::GraphEdge, 0);
        let mut v1_edge = Vec::new();
        v1_edge.extend_from_slice(&2u16.to_le_bytes()); // source_len
        v1_edge.extend_from_slice(&2u16.to_le_bytes()); // target_len
        v1_edge.push(0); // "has_service" (disc=0)
        v1_edge.extend_from_slice(&1.0f32.to_le_bytes()); // weight
        v1_edge.extend_from_slice(b"n1");
        v1_edge.extend_from_slice(b"n1");
        edge_page.insert_cell(b"n1|0|n1", &v1_edge).unwrap();

        // Assemble v1 file: header + node-page-count + node-pages + edge-page-count + edge-pages.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RBGR");
        bytes.extend_from_slice(&1u32.to_le_bytes()); // version=1
        bytes.extend_from_slice(&1u64.to_le_bytes()); // node_count
        bytes.extend_from_slice(&1u64.to_le_bytes()); // edge_count
        bytes.extend_from_slice(&1u32.to_le_bytes()); // node_page_count
        bytes.extend_from_slice(node_page.as_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes()); // edge_page_count
        bytes.extend_from_slice(edge_page.as_bytes());

        let store = GraphStore::deserialize(&bytes).expect("v1 blob deserializes");
        // Node decoded via legacy path → label_id mapped to reserved ID 1 ("host").
        let node = store.get_node("n1").unwrap();
        assert_eq!(node.node_type, "host");
        assert_eq!(node.label_id, LabelId::new(1));
        // Edge index rebuilt.
        let out = store.outgoing_edges("n1");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "has_service");
    }

    #[test]
    fn deserialize_rejects_unknown_version() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RBGR");
        bytes.extend_from_slice(&999u32.to_le_bytes()); // bogus version
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        match GraphStore::deserialize(&bytes) {
            Err(GraphStoreError::InvalidData(msg)) => assert!(msg.contains("Unsupported")),
            Err(other) => panic!("unexpected error: {:?}", other),
            Ok(_) => panic!("expected error for unknown version"),
        }
    }
}
