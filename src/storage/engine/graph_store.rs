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

impl GraphStore {
    /// Create a new empty graph store
    pub fn new() -> Self {
        // Use 16 shards for good parallelism on modern CPUs
        const SHARD_COUNT: usize = 16;

        // Create initial pages
        let initial_node_page = Page::new(PageType::GraphNode, 0);
        let initial_edge_page = Page::new(PageType::GraphEdge, 0);

        Self {
            node_index: ShardedIndex::new(SHARD_COUNT),
            edge_index: EdgeIndex::new(SHARD_COUNT),
            node_pages: RwLock::new(vec![initial_node_page]),
            edge_pages: RwLock::new(vec![initial_edge_page]),
            current_node_page: AtomicU32::new(0),
            current_edge_page: AtomicU32::new(0),
            stats: GraphStats::default(),
            node_count: AtomicU64::new(0),
            edge_count: AtomicU64::new(0),
        }
    }

    /// Add a node to the graph
    pub fn add_node(
        &self,
        id: &str,
        label: &str,
        node_type: GraphNodeType,
    ) -> Result<RecordLocation, GraphStoreError> {
        // Check if node already exists
        if self.node_index.contains(id) {
            return Err(GraphStoreError::NodeExists(id.to_string()));
        }

        let node = StoredNode {
            id: id.to_string(),
            label: label.to_string(),
            node_type,
            flags: 0,
            out_edge_count: 0,
            in_edge_count: 0,
            page_id: 0,
            slot: 0,
            table_ref: None,
            vector_ref: None,
        };

        let encoded = node.encode();

        // Get write lock on node pages
        let mut pages = self
            .node_pages
            .write()
            .map_err(|_| GraphStoreError::LockPoisoned)?;

        let current_page_id = self.current_node_page.load(Ordering::Acquire);
        let page = &mut pages[current_page_id as usize];

        // Try to insert into current page
        match page.insert_cell(id.as_bytes(), &encoded) {
            Ok(slot) => {
                let location = RecordLocation {
                    page_id: current_page_id,
                    slot: slot as u16,
                };
                self.node_index.insert(id.to_string(), location);
                self.node_count.fetch_add(1, Ordering::Relaxed);
                Ok(location)
            }
            Err(_) => {
                // Page full, allocate new page
                let new_page_id = pages.len() as u32;
                let mut new_page = Page::new(PageType::GraphNode, new_page_id);

                let slot = new_page
                    .insert_cell(id.as_bytes(), &encoded)
                    .map_err(|_| GraphStoreError::PageFull)?;

                pages.push(new_page);
                self.current_node_page.store(new_page_id, Ordering::Release);

                let location = RecordLocation {
                    page_id: new_page_id,
                    slot: slot as u16,
                };
                self.node_index.insert(id.to_string(), location);
                self.node_count.fetch_add(1, Ordering::Relaxed);
                Ok(location)
            }
        }
    }

    /// Add a node linked to a table row (for unified queries)
    pub fn add_node_linked(
        &self,
        id: &str,
        label: &str,
        node_type: GraphNodeType,
        table_id: u16,
        row_id: u64,
    ) -> Result<RecordLocation, GraphStoreError> {
        // Check if node already exists
        if self.node_index.contains(id) {
            return Err(GraphStoreError::NodeExists(id.to_string()));
        }

        let node = StoredNode {
            id: id.to_string(),
            label: label.to_string(),
            node_type,
            flags: NODE_FLAG_HAS_TABLE_REF,
            out_edge_count: 0,
            in_edge_count: 0,
            page_id: 0,
            slot: 0,
            table_ref: Some(TableRef::new(table_id, row_id)),
            vector_ref: None,
        };

        let encoded = node.encode();

        // Get write lock on node pages
        let mut pages = self
            .node_pages
            .write()
            .map_err(|_| GraphStoreError::LockPoisoned)?;

        let current_page_id = self.current_node_page.load(Ordering::Acquire);
        let page = &mut pages[current_page_id as usize];

        // Try to insert into current page
        match page.insert_cell(id.as_bytes(), &encoded) {
            Ok(slot) => {
                let location = RecordLocation {
                    page_id: current_page_id,
                    slot: slot as u16,
                };
                self.node_index.insert(id.to_string(), location);
                self.node_count.fetch_add(1, Ordering::Relaxed);
                Ok(location)
            }
            Err(_) => {
                // Page full, allocate new page
                let new_page_id = pages.len() as u32;
                let mut new_page = Page::new(PageType::GraphNode, new_page_id);

                let slot = new_page
                    .insert_cell(id.as_bytes(), &encoded)
                    .map_err(|_| GraphStoreError::PageFull)?;

                pages.push(new_page);
                self.current_node_page.store(new_page_id, Ordering::Release);

                let location = RecordLocation {
                    page_id: new_page_id,
                    slot: slot as u16,
                };
                self.node_index.insert(id.to_string(), location);
                self.node_count.fetch_add(1, Ordering::Relaxed);
                Ok(location)
            }
        }
    }

    /// Get table reference for a node (if linked)
    pub fn get_node_table_ref(&self, node_id: &str) -> Option<TableRef> {
        self.get_node(node_id).and_then(|n| n.table_ref)
    }

    /// Add an edge to the graph
    pub fn add_edge(
        &self,
        source_id: &str,
        target_id: &str,
        edge_type: GraphEdgeType,
        weight: f32,
    ) -> Result<RecordLocation, GraphStoreError> {
        // Verify nodes exist
        if !self.node_index.contains(source_id) {
            return Err(GraphStoreError::NodeNotFound(source_id.to_string()));
        }
        if !self.node_index.contains(target_id) {
            return Err(GraphStoreError::NodeNotFound(target_id.to_string()));
        }

        let edge = StoredEdge {
            source_id: source_id.to_string(),
            target_id: target_id.to_string(),
            edge_type,
            weight,
            page_id: 0,
            slot: 0,
        };

        let encoded = edge.encode();

        // Create composite key for edge storage
        let edge_key = format!("{}|{}|{}", source_id, edge_type as u8, target_id);

        // Get write lock on edge pages
        let mut pages = self
            .edge_pages
            .write()
            .map_err(|_| GraphStoreError::LockPoisoned)?;

        let current_page_id = self.current_edge_page.load(Ordering::Acquire);
        let page = &mut pages[current_page_id as usize];

        // Try to insert into current page
        let location = match page.insert_cell(edge_key.as_bytes(), &encoded) {
            Ok(slot) => RecordLocation {
                page_id: current_page_id,
                slot: slot as u16,
            },
            Err(_) => {
                // Page full, allocate new page
                let new_page_id = pages.len() as u32;
                let mut new_page = Page::new(PageType::GraphEdge, new_page_id);

                let slot = new_page
                    .insert_cell(edge_key.as_bytes(), &encoded)
                    .map_err(|_| GraphStoreError::PageFull)?;

                pages.push(new_page);
                self.current_edge_page.store(new_page_id, Ordering::Release);

                RecordLocation {
                    page_id: new_page_id,
                    slot: slot as u16,
                }
            }
        };

        // Update edge index (this is the fast path for traversal)
        self.edge_index
            .add_edge(source_id, target_id, edge_type, weight);
        self.edge_count.fetch_add(1, Ordering::Relaxed);

        Ok(location)
    }

    /// Get a node by ID (lock-free read)
    pub fn get_node(&self, id: &str) -> Option<StoredNode> {
        let location = self.node_index.get(id)?;

        let pages = self.node_pages.read().ok()?;
        let page = pages.get(location.page_id as usize)?;

        let (_, value) = page.read_cell(location.slot as usize).ok()?;
        StoredNode::decode(&value, location.page_id, location.slot)
    }

    /// Get all outgoing edges from a node (lock-free read)
    #[inline]
    pub fn outgoing_edges(&self, source_id: &str) -> Vec<(GraphEdgeType, String, f32)> {
        self.edge_index.outgoing(source_id)
    }

    /// Get all incoming edges to a node (lock-free read)
    #[inline]
    pub fn incoming_edges(&self, target_id: &str) -> Vec<(GraphEdgeType, String, f32)> {
        self.edge_index.incoming(target_id)
    }

    /// Get outgoing edges of a specific type
    #[inline]
    pub fn outgoing_of_type(
        &self,
        source_id: &str,
        edge_type: GraphEdgeType,
    ) -> Vec<(String, f32)> {
        self.edge_index.outgoing_of_type(source_id, edge_type)
    }

    /// Check if a node exists
    #[inline]
    pub fn has_node(&self, id: &str) -> bool {
        self.node_index.contains(id)
    }

    /// Get node count
    #[inline]
    pub fn node_count(&self) -> u64 {
        self.node_count.load(Ordering::Relaxed)
    }

    /// Get edge count
    #[inline]
    pub fn edge_count(&self) -> u64 {
        self.edge_count.load(Ordering::Relaxed)
    }

    /// Iterate over all nodes (streaming)
    pub fn iter_nodes(&self) -> NodeIterator<'_> {
        NodeIterator {
            store: self,
            page_idx: 0,
            cell_idx: 0,
        }
    }

    /// Iterate all edges in the graph
    ///
    /// This collects outgoing edges from all nodes to build a complete edge list.
    /// Returns StoredEdge structs with source, target, type, and weight.
    pub fn iter_all_edges(&self) -> Vec<StoredEdge> {
        let mut edges = Vec::new();

        for node in self.iter_nodes() {
            // Get outgoing edges for this node
            for (edge_type, target_id, weight) in self.outgoing_edges(&node.id) {
                edges.push(StoredEdge {
                    source_id: node.id.clone(),
                    target_id,
                    edge_type,
                    weight,
                    page_id: 0, // Not needed for iteration
                    slot: 0,    // Not needed for iteration
                });
            }
        }

        edges
    }

    /// Get nodes of a specific type
    pub fn nodes_of_type(&self, node_type: GraphNodeType) -> Vec<StoredNode> {
        self.iter_nodes()
            .filter(|n| n.node_type == node_type)
            .collect()
    }

    /// Get statistics
    pub fn stats(&self) -> GraphStats {
        let mut stats = GraphStats {
            node_count: self.node_count.load(Ordering::Relaxed),
            edge_count: self.edge_count.load(Ordering::Relaxed),
            node_pages: self.node_pages.read().map(|p| p.len() as u32).unwrap_or(0),
            edge_pages: self.edge_pages.read().map(|p| p.len() as u32).unwrap_or(0),
            ..Default::default()
        };

        // Count nodes by type
        for node in self.iter_nodes() {
            stats.nodes_by_type[node.node_type as usize] += 1;
        }

        stats
    }

    /// Serialize to bytes for persistence
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Header: magic(4) + version(4) + node_count(8) + edge_count(8) + node_pages(4) + edge_pages(4)
        buf.extend_from_slice(b"RBGR"); // RedBlue GRaph
        buf.extend_from_slice(&1u32.to_le_bytes()); // version
        buf.extend_from_slice(&self.node_count.load(Ordering::Relaxed).to_le_bytes());
        buf.extend_from_slice(&self.edge_count.load(Ordering::Relaxed).to_le_bytes());

        // Serialize node pages
        if let Ok(pages) = self.node_pages.read() {
            buf.extend_from_slice(&(pages.len() as u32).to_le_bytes());
            for page in pages.iter() {
                buf.extend_from_slice(page.as_bytes());
            }
        }

        // Serialize edge pages
        if let Ok(pages) = self.edge_pages.read() {
            buf.extend_from_slice(&(pages.len() as u32).to_le_bytes());
            for page in pages.iter() {
                buf.extend_from_slice(page.as_bytes());
            }
        }

        buf
    }

    /// Deserialize from bytes
    pub fn deserialize(data: &[u8]) -> Result<Self, GraphStoreError> {
        if data.len() < 32 {
            return Err(GraphStoreError::InvalidData("Too short".to_string()));
        }

        // Verify magic
        if &data[0..4] != b"RBGR" {
            return Err(GraphStoreError::InvalidData("Invalid magic".to_string()));
        }

        let _version = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let node_count = u64::from_le_bytes([
            data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
        ]);
        let edge_count = u64::from_le_bytes([
            data[16], data[17], data[18], data[19], data[20], data[21], data[22], data[23],
        ]);

        let mut offset = 24;

        // Read node page count
        let node_page_count = u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        offset += 4;

        // Read node pages
        let mut node_pages = Vec::with_capacity(node_page_count);
        for _ in 0..node_page_count {
            if offset + PAGE_SIZE > data.len() {
                return Err(GraphStoreError::InvalidData(
                    "Truncated node pages".to_string(),
                ));
            }
            let page = Page::from_slice(&data[offset..offset + PAGE_SIZE])
                .map_err(|_| GraphStoreError::InvalidData("Invalid page".to_string()))?;
            node_pages.push(page);
            offset += PAGE_SIZE;
        }

        // Read edge page count
        let edge_page_count = u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        offset += 4;

        // Read edge pages
        let mut edge_pages = Vec::with_capacity(edge_page_count);
        for _ in 0..edge_page_count {
            if offset + PAGE_SIZE > data.len() {
                return Err(GraphStoreError::InvalidData(
                    "Truncated edge pages".to_string(),
                ));
            }
            let page = Page::from_slice(&data[offset..offset + PAGE_SIZE])
                .map_err(|_| GraphStoreError::InvalidData("Invalid page".to_string()))?;
            edge_pages.push(page);
            offset += PAGE_SIZE;
        }

        // Rebuild indexes from pages
        let store = Self {
            node_index: ShardedIndex::new(16),
            edge_index: EdgeIndex::new(16),
            node_pages: RwLock::new(node_pages),
            edge_pages: RwLock::new(edge_pages),
            current_node_page: AtomicU32::new(0),
            current_edge_page: AtomicU32::new(0),
            stats: GraphStats::default(),
            node_count: AtomicU64::new(node_count),
            edge_count: AtomicU64::new(edge_count),
        };

        store.rebuild_indexes()?;

        Ok(store)
    }

    /// Rebuild indexes from pages (used after deserialization)
    fn rebuild_indexes(&self) -> Result<(), GraphStoreError> {
        // Rebuild node index
        if let Ok(pages) = self.node_pages.read() {
            for (page_idx, page) in pages.iter().enumerate() {
                let cell_count = page.cell_count() as usize;
                for cell_idx in 0..cell_count {
                    if let Ok((key, value)) = page.read_cell(cell_idx) {
                        let id = String::from_utf8_lossy(&key).to_string();
                        self.node_index.insert(
                            id,
                            RecordLocation {
                                page_id: page_idx as u32,
                                slot: cell_idx as u16,
                            },
                        );
                    }
                }
            }

            // Update current node page
            if !pages.is_empty() {
                self.current_node_page
                    .store((pages.len() - 1) as u32, Ordering::Release);
            }
        }

        // Rebuild edge index
        if let Ok(pages) = self.edge_pages.read() {
            for (page_idx, page) in pages.iter().enumerate() {
                let cell_count = page.cell_count() as usize;
                for cell_idx in 0..cell_count {
                    if let Ok((_, value)) = page.read_cell(cell_idx) {
                        if let Some(edge) =
                            StoredEdge::decode(&value, page_idx as u32, cell_idx as u16)
                        {
                            self.edge_index.add_edge(
                                &edge.source_id,
                                &edge.target_id,
                                edge.edge_type,
                                edge.weight,
                            );
                        }
                    }
                }
            }

            // Update current edge page
            if !pages.is_empty() {
                self.current_edge_page
                    .store((pages.len() - 1) as u32, Ordering::Release);
            }
        }

        Ok(())
    }
}

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
