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
use std::sync::RwLock;

use super::page::{Page, PageType, PAGE_SIZE};

/// Maximum key size for node/edge IDs
pub const MAX_ID_SIZE: usize = 256;

/// Maximum label size
pub const MAX_LABEL_SIZE: usize = 512;

/// Node record header size: id_len(2) + label_len(2) + type(1) + flags(1) + edge_count(4)
pub const NODE_HEADER_SIZE: usize = 10;

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

/// Edge record header size: source_len(2) + target_len(2) + type(1) + weight(4)
pub const EDGE_HEADER_SIZE: usize = 9;

/// Graph node types (matches intelligence layer)
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GraphNodeType {
    Host = 0,
    Service = 1,
    Credential = 2,
    Vulnerability = 3,
    Endpoint = 4,
    Technology = 5,
    User = 6,
    Domain = 7,
    Certificate = 8,
}

impl GraphNodeType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Host),
            1 => Some(Self::Service),
            2 => Some(Self::Credential),
            3 => Some(Self::Vulnerability),
            4 => Some(Self::Endpoint),
            5 => Some(Self::Technology),
            6 => Some(Self::User),
            7 => Some(Self::Domain),
            8 => Some(Self::Certificate),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Service => "service",
            Self::Credential => "credential",
            Self::Vulnerability => "vulnerability",
            Self::Endpoint => "endpoint",
            Self::Technology => "technology",
            Self::User => "user",
            Self::Domain => "domain",
            Self::Certificate => "certificate",
        }
    }
}

/// Graph edge types (matches intelligence layer)
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GraphEdgeType {
    HasService = 0,
    HasEndpoint = 1,
    UsesTech = 2,
    AuthAccess = 3,
    AffectedBy = 4,
    Contains = 5,
    ConnectsTo = 6,
    RelatedTo = 7,
    HasUser = 8,
    HasCert = 9,
}

impl GraphEdgeType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::HasService),
            1 => Some(Self::HasEndpoint),
            2 => Some(Self::UsesTech),
            3 => Some(Self::AuthAccess),
            4 => Some(Self::AffectedBy),
            5 => Some(Self::Contains),
            6 => Some(Self::ConnectsTo),
            7 => Some(Self::RelatedTo),
            8 => Some(Self::HasUser),
            9 => Some(Self::HasCert),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::HasService => "has_service",
            Self::HasEndpoint => "has_endpoint",
            Self::UsesTech => "uses_tech",
            Self::AuthAccess => "auth_access",
            Self::AffectedBy => "affected_by",
            Self::Contains => "contains",
            Self::ConnectsTo => "connects_to",
            Self::RelatedTo => "related_to",
            Self::HasUser => "has_user",
            Self::HasCert => "has_cert",
        }
    }
}

/// A graph node stored on disk
#[derive(Debug, Clone)]
pub struct StoredNode {
    pub id: String,
    pub label: String,
    pub node_type: GraphNodeType,
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
    /// Encode node to bytes for storage
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
            2 + coll.len() + 8 // collection_len(2) + collection + vector_id(8)
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

        // Header: id_len(2) + label_len(2) + type(1) + flags(1) + out_edges(2) + in_edges(2)
        buf.extend_from_slice(&(id_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(&(label_bytes.len() as u16).to_le_bytes());
        buf.push(self.node_type as u8);
        buf.push(flags);
        buf.extend_from_slice(&(self.out_edge_count as u16).to_le_bytes());
        buf.extend_from_slice(&(self.in_edge_count as u16).to_le_bytes());

        // Data
        buf.extend_from_slice(id_bytes);
        buf.extend_from_slice(label_bytes);

        // Optional table reference (10 bytes)
        if let Some(ref tref) = self.table_ref {
            buf.extend_from_slice(&tref.encode());
        }

        // Optional vector reference (variable size: 2 + collection_len + 8)
        if let Some((ref collection, vector_id)) = self.vector_ref {
            let coll_bytes = collection.as_bytes();
            buf.extend_from_slice(&(coll_bytes.len() as u16).to_le_bytes());
            buf.extend_from_slice(coll_bytes);
            buf.extend_from_slice(&vector_id.to_le_bytes());
        }

        buf
    }

    /// Decode node from bytes
    pub fn decode(data: &[u8], page_id: u32, slot: u16) -> Option<Self> {
        if data.len() < NODE_HEADER_SIZE {
            return None;
        }

        let id_len = u16::from_le_bytes([data[0], data[1]]) as usize;
        let label_len = u16::from_le_bytes([data[2], data[3]]) as usize;
        let node_type = GraphNodeType::from_u8(data[4])?;
        let flags = data[5];
        let out_edge_count = u16::from_le_bytes([data[6], data[7]]) as u32;
        let in_edge_count = u16::from_le_bytes([data[8], data[9]]) as u32;

        let has_table_ref = (flags & NODE_FLAG_HAS_TABLE_REF) != 0;
        let has_vector_ref = (flags & NODE_FLAG_HAS_VECTOR_REF) != 0;
        let table_ref_size = if has_table_ref { TABLE_REF_SIZE } else { 0 };

        // We need to calculate vector_ref_size dynamically based on collection name length
        let mut offset = NODE_HEADER_SIZE + id_len + label_len + table_ref_size;

        // Preliminary bounds check (without vector_ref)
        if data.len() < offset {
            return None;
        }

        let id =
            String::from_utf8_lossy(&data[NODE_HEADER_SIZE..NODE_HEADER_SIZE + id_len]).to_string();
        let label = String::from_utf8_lossy(
            &data[NODE_HEADER_SIZE + id_len..NODE_HEADER_SIZE + id_len + label_len],
        )
        .to_string();

        // Decode optional table reference
        let table_ref = if has_table_ref {
            let ref_start = NODE_HEADER_SIZE + id_len + label_len;
            TableRef::decode(&data[ref_start..])
        } else {
            None
        };

        // Decode optional vector reference
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
    pub edge_type: GraphEdgeType,
    pub weight: f32,
    /// Page ID where this edge is stored
    pub page_id: u32,
    /// Slot index within the page
    pub slot: u16,
}

impl StoredEdge {
    /// Encode edge to bytes for storage
    pub fn encode(&self) -> Vec<u8> {
        let source_bytes = self.source_id.as_bytes();
        let target_bytes = self.target_id.as_bytes();

        let mut buf =
            Vec::with_capacity(EDGE_HEADER_SIZE + source_bytes.len() + target_bytes.len());

        // Header: source_len(2) + target_len(2) + type(1) + weight(4)
        buf.extend_from_slice(&(source_bytes.len() as u16).to_le_bytes());
        buf.extend_from_slice(&(target_bytes.len() as u16).to_le_bytes());
        buf.push(self.edge_type as u8);
        buf.extend_from_slice(&self.weight.to_le_bytes());

        // Data
        buf.extend_from_slice(source_bytes);
        buf.extend_from_slice(target_bytes);

        buf
    }

    /// Decode edge from bytes
    pub fn decode(data: &[u8], page_id: u32, slot: u16) -> Option<Self> {
        if data.len() < EDGE_HEADER_SIZE {
            return None;
        }

        let source_len = u16::from_le_bytes([data[0], data[1]]) as usize;
        let target_len = u16::from_le_bytes([data[2], data[3]]) as usize;
        let edge_type = GraphEdgeType::from_u8(data[4])?;
        let weight = f32::from_le_bytes([data[5], data[6], data[7], data[8]]);

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
            weight,
            page_id,
            slot,
        })
    }

    /// Calculate encoded size
    pub fn encoded_size(&self) -> usize {
        EDGE_HEADER_SIZE + self.source_id.len() + self.target_id.len()
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
    pub nodes_by_type: [u64; 9],
    pub edges_by_type: [u64; 10],
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

/// Edge index key: (source_id, edge_type) -> Vec<target_id>
/// Optimized for adjacency list queries
pub struct EdgeIndex {
    /// Forward edges: source -> [(edge_type, target)]
    forward: ShardedIndex<Vec<(GraphEdgeType, String, f32)>>,
    /// Backward edges: target -> [(edge_type, source)]
    backward: ShardedIndex<Vec<(GraphEdgeType, String, f32)>>,
}

impl EdgeIndex {
    pub fn new(shard_count: usize) -> Self {
        Self {
            forward: ShardedIndex::new(shard_count),
            backward: ShardedIndex::new(shard_count),
        }
    }

    pub fn add_edge(&self, source: &str, target: &str, edge_type: GraphEdgeType, weight: f32) {
        // Add to forward index
        let shard = self.forward.shard_for(source);
        if let Ok(mut guard) = self.forward.shards[shard].write() {
            guard
                .entry(source.to_string())
                .or_insert_with(Vec::new)
                .push((edge_type, target.to_string(), weight));
        }

        // Add to backward index
        let shard = self.backward.shard_for(target);
        if let Ok(mut guard) = self.backward.shards[shard].write() {
            guard
                .entry(target.to_string())
                .or_insert_with(Vec::new)
                .push((edge_type, source.to_string(), weight));
        }
    }

    pub fn remove_edge(&self, source: &str, target: &str, edge_type: GraphEdgeType) {
        // Remove from forward index
        let shard = self.forward.shard_for(source);
        if let Ok(mut guard) = self.forward.shards[shard].write() {
            if let Some(edges) = guard.get_mut(source) {
                edges.retain(|(et, t, _)| !(*et == edge_type && t == target));
            }
        }

        // Remove from backward index
        let shard = self.backward.shard_for(target);
        if let Ok(mut guard) = self.backward.shards[shard].write() {
            if let Some(edges) = guard.get_mut(target) {
                edges.retain(|(et, s, _)| !(*et == edge_type && s == source));
            }
        }
    }

    pub fn outgoing(&self, source: &str) -> Vec<(GraphEdgeType, String, f32)> {
        self.forward.get(source).unwrap_or_default()
    }

    pub fn incoming(&self, target: &str) -> Vec<(GraphEdgeType, String, f32)> {
        self.backward.get(target).unwrap_or_default()
    }

    pub fn outgoing_of_type(&self, source: &str, edge_type: GraphEdgeType) -> Vec<(String, f32)> {
        self.forward
            .get(source)
            .unwrap_or_default()
            .into_iter()
            .filter(|(et, _, _)| *et == edge_type)
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
    node_secondary: secondary_index::NodeSecondaryIndex,
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
pub mod secondary_index;
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
            .add_node("host:192.168.1.1", "Web Server", GraphNodeType::Host)
            .unwrap();
        store
            .add_node("host:192.168.1.2", "Database", GraphNodeType::Host)
            .unwrap();
        store
            .add_node(
                "service:192.168.1.1:80:http",
                "HTTP",
                GraphNodeType::Service,
            )
            .unwrap();

        assert_eq!(store.node_count(), 3);

        // Add edges
        store
            .add_edge(
                "host:192.168.1.1",
                "service:192.168.1.1:80:http",
                GraphEdgeType::HasService,
                1.0,
            )
            .unwrap();
        store
            .add_edge(
                "host:192.168.1.1",
                "host:192.168.1.2",
                GraphEdgeType::ConnectsTo,
                1.0,
            )
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
            .add_node("host:10.0.0.1", "Server A", GraphNodeType::Host)
            .unwrap();
        store
            .add_node("host:10.0.0.2", "Server B", GraphNodeType::Host)
            .unwrap();
        store
            .add_edge(
                "host:10.0.0.1",
                "host:10.0.0.2",
                GraphEdgeType::ConnectsTo,
                0.5,
            )
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
                .add_node(
                    &format!("host:{}", i),
                    &format!("Host {}", i),
                    GraphNodeType::Host,
                )
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
            .add_node("hub", "Hub Node", GraphNodeType::Host)
            .unwrap();
        for i in 0..100 {
            store
                .add_node(
                    &format!("spoke:{}", i),
                    &format!("Spoke {}", i),
                    GraphNodeType::Host,
                )
                .unwrap();
            store
                .add_edge(
                    "hub",
                    &format!("spoke:{}", i),
                    GraphEdgeType::ConnectsTo,
                    1.0,
                )
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
            .add_node("host:1", "Web Server", GraphNodeType::Host)
            .unwrap();
        store
            .add_node("host:2", "DB Server", GraphNodeType::Host)
            .unwrap();
        store
            .add_node("svc:1", "HTTP", GraphNodeType::Service)
            .unwrap();
        store
            .add_node("vuln:1", "CVE-2024-1", GraphNodeType::Vulnerability)
            .unwrap();

        let hosts = store.nodes_of_type(GraphNodeType::Host);
        assert_eq!(hosts.len(), 2);
        assert!(hosts
            .iter()
            .all(|n| matches!(n.node_type, GraphNodeType::Host)));

        let services = store.nodes_of_type(GraphNodeType::Service);
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].id, "svc:1");

        assert_eq!(store.nodes_of_type(GraphNodeType::User).len(), 0);
    }

    #[test]
    fn test_nodes_by_label_with_bloom_prune() {
        let store = GraphStore::new();
        store
            .add_node("host:1", "Edge Router", GraphNodeType::Host)
            .unwrap();
        store
            .add_node("host:2", "Edge Router", GraphNodeType::Host)
            .unwrap();
        store
            .add_node("host:3", "Core Switch", GraphNodeType::Host)
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
    fn test_secondary_index_rebuilt_after_deserialize() {
        let store = GraphStore::new();
        store
            .add_node("host:1", "Alpha", GraphNodeType::Host)
            .unwrap();
        store
            .add_node("svc:1", "HTTP", GraphNodeType::Service)
            .unwrap();

        let bytes = store.serialize();
        let restored = GraphStore::deserialize(&bytes).unwrap();

        assert_eq!(restored.nodes_of_type(GraphNodeType::Host).len(), 1);
        assert_eq!(restored.nodes_by_label("HTTP").len(), 1);
        assert!(restored.may_contain_label("Alpha"));
    }

    #[test]
    fn test_node_iteration() {
        let store = GraphStore::new();

        for i in 0..50 {
            store
                .add_node(
                    &format!("node:{}", i),
                    &format!("Node {}", i),
                    GraphNodeType::Host,
                )
                .unwrap();
        }

        let nodes: Vec<_> = store.iter_nodes().collect();
        assert_eq!(nodes.len(), 50);
    }
}
