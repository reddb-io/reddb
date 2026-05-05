//! Bidirectional Graph-Table Index
//!
//! Enables unified queries by maintaining bidirectional mappings:
//! - node_id → (table_id, row_id)
//! - (table_id, row_id) → node_id
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    GraphTableIndex                           │
//! ├─────────────────────────────────────────────────────────────┤
//! │  NodeToRow Index (16 shards)    RowToNode Index (16 shards) │
//! │  ┌────┐┌────┐┌────┐...         ┌────┐┌────┐┌────┐...       │
//! │  │ S0 ││ S1 ││ S2 │            │ S0 ││ S1 ││ S2 │          │
//! │  └────┘└────┘└────┘            └────┘└────┘└────┘          │
//! │      │                              │                       │
//! │      ▼                              ▼                       │
//! │  node_id → TableRef            RowKey → node_id             │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Thread Safety
//!
//! Uses sharded RwLock for concurrent access:
//! - Multiple readers can access different shards simultaneously
//! - Writers only block their specific shard
//! - FNV hashing distributes keys evenly across shards

use std::collections::HashMap;
use std::sync::RwLock;

use super::graph_store::TableRef;

/// Number of shards for concurrent access
const NUM_SHARDS: usize = 16;

/// FNV-1a hash for fast shard selection
fn fnv_hash(data: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET;
    for byte in data {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Composite key for row lookups: (table_id, row_id)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RowKey {
    pub table_id: u16,
    pub row_id: u64,
}

impl RowKey {
    pub fn new(table_id: u16, row_id: u64) -> Self {
        Self { table_id, row_id }
    }

    pub fn from_table_ref(tref: &TableRef) -> Self {
        Self {
            table_id: tref.table_id,
            row_id: tref.row_id,
        }
    }

    /// Convert to bytes for hashing
    fn to_bytes(&self) -> [u8; 10] {
        let mut buf = [0u8; 10];
        buf[0..2].copy_from_slice(&self.table_id.to_le_bytes());
        buf[2..10].copy_from_slice(&self.row_id.to_le_bytes());
        buf
    }
}

/// Sharded index for node_id → TableRef
struct NodeToRowIndex {
    shards: Vec<RwLock<HashMap<String, TableRef>>>,
}

impl NodeToRowIndex {
    fn new() -> Self {
        let mut shards = Vec::with_capacity(NUM_SHARDS);
        for _ in 0..NUM_SHARDS {
            shards.push(RwLock::new(HashMap::new()));
        }
        Self { shards }
    }

    fn shard_for(&self, node_id: &str) -> usize {
        (fnv_hash(node_id.as_bytes()) as usize) % NUM_SHARDS
    }

    fn insert(&self, node_id: String, table_ref: TableRef) {
        let shard = self.shard_for(&node_id);
        if let Ok(mut map) = self.shards[shard].write() {
            map.insert(node_id, table_ref);
        }
    }

    fn get(&self, node_id: &str) -> Option<TableRef> {
        let shard = self.shard_for(node_id);
        if let Ok(map) = self.shards[shard].read() {
            map.get(node_id).copied()
        } else {
            None
        }
    }

    fn remove(&self, node_id: &str) -> Option<TableRef> {
        let shard = self.shard_for(node_id);
        if let Ok(mut map) = self.shards[shard].write() {
            map.remove(node_id)
        } else {
            None
        }
    }

    fn contains(&self, node_id: &str) -> bool {
        let shard = self.shard_for(node_id);
        if let Ok(map) = self.shards[shard].read() {
            map.contains_key(node_id)
        } else {
            false
        }
    }

    fn len(&self) -> usize {
        self.shards
            .iter()
            .filter_map(|s| s.read().ok())
            .map(|m| m.len())
            .sum()
    }
}

/// Sharded index for (table_id, row_id) → node_id
struct RowToNodeIndex {
    shards: Vec<RwLock<HashMap<RowKey, String>>>,
}

impl RowToNodeIndex {
    fn new() -> Self {
        let mut shards = Vec::with_capacity(NUM_SHARDS);
        for _ in 0..NUM_SHARDS {
            shards.push(RwLock::new(HashMap::new()));
        }
        Self { shards }
    }

    fn shard_for(&self, key: &RowKey) -> usize {
        (fnv_hash(&key.to_bytes()) as usize) % NUM_SHARDS
    }

    fn insert(&self, key: RowKey, node_id: String) {
        let shard = self.shard_for(&key);
        if let Ok(mut map) = self.shards[shard].write() {
            map.insert(key, node_id);
        }
    }

    fn get(&self, key: &RowKey) -> Option<String> {
        let shard = self.shard_for(key);
        if let Ok(map) = self.shards[shard].read() {
            map.get(key).cloned()
        } else {
            None
        }
    }

    fn remove(&self, key: &RowKey) -> Option<String> {
        let shard = self.shard_for(key);
        if let Ok(mut map) = self.shards[shard].write() {
            map.remove(key)
        } else {
            None
        }
    }

    fn contains(&self, key: &RowKey) -> bool {
        let shard = self.shard_for(key);
        if let Ok(map) = self.shards[shard].read() {
            map.contains_key(key)
        } else {
            false
        }
    }

    /// Get all nodes for a specific table
    fn nodes_for_table(&self, table_id: u16) -> Vec<(u64, String)> {
        let mut results = Vec::new();
        for shard in &self.shards {
            if let Ok(map) = shard.read() {
                for (key, node_id) in map.iter() {
                    if key.table_id == table_id {
                        results.push((key.row_id, node_id.clone()));
                    }
                }
            }
        }
        results
    }

    fn len(&self) -> usize {
        self.shards
            .iter()
            .filter_map(|s| s.read().ok())
            .map(|m| m.len())
            .sum()
    }
}

/// Bidirectional index for graph-table linkage
///
/// Enables efficient lookups in both directions:
/// - From graph node to table row
/// - From table row to graph node
///
/// Thread-safe with sharded locking for concurrent access.
pub struct GraphTableIndex {
    node_to_row: NodeToRowIndex,
    row_to_node: RowToNodeIndex,
}

impl GraphTableIndex {
    /// Create a new empty index
    pub fn new() -> Self {
        Self {
            node_to_row: NodeToRowIndex::new(),
            row_to_node: RowToNodeIndex::new(),
        }
    }

    /// Link a graph node to a table row
    ///
    /// Creates bidirectional mapping between node_id and (table_id, row_id).
    /// Overwrites existing mappings if present.
    pub fn link(&self, node_id: &str, table_id: u16, row_id: u64) {
        let table_ref = TableRef::new(table_id, row_id);
        let row_key = RowKey::new(table_id, row_id);

        self.node_to_row.insert(node_id.to_string(), table_ref);
        self.row_to_node.insert(row_key, node_id.to_string());
    }

    /// Unlink a graph node from its table row
    ///
    /// Removes both directions of the mapping.
    /// Returns the TableRef if it existed.
    pub fn unlink_node(&self, node_id: &str) -> Option<TableRef> {
        if let Some(table_ref) = self.node_to_row.remove(node_id) {
            let row_key = RowKey::from_table_ref(&table_ref);
            self.row_to_node.remove(&row_key);
            Some(table_ref)
        } else {
            None
        }
    }

    /// Unlink a table row from its graph node
    ///
    /// Removes both directions of the mapping.
    /// Returns the node_id if it existed.
    pub fn unlink_row(&self, table_id: u16, row_id: u64) -> Option<String> {
        let row_key = RowKey::new(table_id, row_id);
        if let Some(node_id) = self.row_to_node.remove(&row_key) {
            self.node_to_row.remove(&node_id);
            Some(node_id)
        } else {
            None
        }
    }

    /// Get the table row for a graph node
    pub fn get_row_for_node(&self, node_id: &str) -> Option<TableRef> {
        self.node_to_row.get(node_id)
    }

    /// Get the graph node for a table row
    pub fn get_node_for_row(&self, table_id: u16, row_id: u64) -> Option<String> {
        let row_key = RowKey::new(table_id, row_id);
        self.row_to_node.get(&row_key)
    }

    /// Check if a node is linked to a table row
    pub fn is_node_linked(&self, node_id: &str) -> bool {
        self.node_to_row.contains(node_id)
    }

    /// Check if a table row is linked to a graph node
    pub fn is_row_linked(&self, table_id: u16, row_id: u64) -> bool {
        let row_key = RowKey::new(table_id, row_id);
        self.row_to_node.contains(&row_key)
    }

    /// Get all nodes linked to a specific table
    ///
    /// Returns pairs of (row_id, node_id) for the given table.
    pub fn nodes_for_table(&self, table_id: u16) -> Vec<(u64, String)> {
        self.row_to_node.nodes_for_table(table_id)
    }

    /// Get statistics about the index
    pub fn stats(&self) -> GraphTableIndexStats {
        GraphTableIndexStats {
            node_to_row_count: self.node_to_row.len(),
            row_to_node_count: self.row_to_node.len(),
            num_shards: NUM_SHARDS,
        }
    }

    /// Clear all mappings
    pub fn clear(&self) {
        for shard in &self.node_to_row.shards {
            if let Ok(mut map) = shard.write() {
                map.clear();
            }
        }
        for shard in &self.row_to_node.shards {
            if let Ok(mut map) = shard.write() {
                map.clear();
            }
        }
    }

    /// Serialize to bytes for persistence
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Collect all mappings
        let mut mappings = Vec::new();
        for shard in &self.node_to_row.shards {
            if let Ok(map) = shard.read() {
                for (node_id, table_ref) in map.iter() {
                    mappings.push((node_id.clone(), *table_ref));
                }
            }
        }

        // Write count
        buf.extend_from_slice(&(mappings.len() as u32).to_le_bytes());

        // Write each mapping: node_id_len(2) + node_id + table_ref(10)
        for (node_id, table_ref) in mappings {
            let id_bytes = node_id.as_bytes();
            buf.extend_from_slice(&(id_bytes.len() as u16).to_le_bytes());
            buf.extend_from_slice(id_bytes);
            buf.extend_from_slice(&table_ref.encode());
        }

        buf
    }

    /// Deserialize from bytes
    pub fn deserialize(data: &[u8]) -> Result<Self, GraphTableIndexError> {
        if data.len() < 4 {
            return Err(GraphTableIndexError::InvalidData("Too short".to_string()));
        }

        let index = Self::new();
        let count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        let mut offset = 4;

        for _ in 0..count {
            if offset + 2 > data.len() {
                return Err(GraphTableIndexError::InvalidData(
                    "Truncated node_id length".to_string(),
                ));
            }

            let id_len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
            offset += 2;

            if offset + id_len + 10 > data.len() {
                return Err(GraphTableIndexError::InvalidData(
                    "Truncated mapping".to_string(),
                ));
            }

            let node_id = String::from_utf8_lossy(&data[offset..offset + id_len]).to_string();
            offset += id_len;

            let table_ref = TableRef::decode(&data[offset..]).ok_or_else(|| {
                GraphTableIndexError::InvalidData("Invalid table ref".to_string())
            })?;
            offset += 10;

            index.link(&node_id, table_ref.table_id, table_ref.row_id);
        }

        Ok(index)
    }
}

impl Default for GraphTableIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics for GraphTableIndex
#[derive(Debug, Clone, Copy)]
pub struct GraphTableIndexStats {
    pub node_to_row_count: usize,
    pub row_to_node_count: usize,
    pub num_shards: usize,
}

/// Error type for GraphTableIndex operations
#[derive(Debug, Clone)]
pub enum GraphTableIndexError {
    InvalidData(String),
}

impl std::fmt::Display for GraphTableIndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidData(msg) => write!(f, "Invalid data: {}", msg),
        }
    }
}

impl std::error::Error for GraphTableIndexError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_link_and_lookup() {
        let index = GraphTableIndex::new();

        index.link("host:192.168.1.1", 1, 100);
        index.link("service:ssh", 2, 200);

        // Forward lookup (node → row)
        let tref = index.get_row_for_node("host:192.168.1.1").unwrap();
        assert_eq!(tref.table_id, 1);
        assert_eq!(tref.row_id, 100);

        // Reverse lookup (row → node)
        let node_id = index.get_node_for_row(2, 200).unwrap();
        assert_eq!(node_id, "service:ssh");

        // Non-existent
        assert!(index.get_row_for_node("unknown").is_none());
        assert!(index.get_node_for_row(99, 999).is_none());
    }

    #[test]
    fn test_unlink() {
        let index = GraphTableIndex::new();

        index.link("node1", 1, 10);
        assert!(index.is_node_linked("node1"));
        assert!(index.is_row_linked(1, 10));

        // Unlink by node
        let tref = index.unlink_node("node1").unwrap();
        assert_eq!(tref.table_id, 1);
        assert_eq!(tref.row_id, 10);

        assert!(!index.is_node_linked("node1"));
        assert!(!index.is_row_linked(1, 10));
    }

    #[test]
    fn test_unlink_by_row() {
        let index = GraphTableIndex::new();

        index.link("node2", 2, 20);

        let node_id = index.unlink_row(2, 20).unwrap();
        assert_eq!(node_id, "node2");

        assert!(!index.is_node_linked("node2"));
        assert!(!index.is_row_linked(2, 20));
    }

    #[test]
    fn test_nodes_for_table() {
        let index = GraphTableIndex::new();

        index.link("host:1", 1, 100);
        index.link("host:2", 1, 101);
        index.link("host:3", 1, 102);
        index.link("service:1", 2, 200);

        let hosts = index.nodes_for_table(1);
        assert_eq!(hosts.len(), 3);

        let services = index.nodes_for_table(2);
        assert_eq!(services.len(), 1);
    }

    #[test]
    fn test_serialization() {
        let index = GraphTableIndex::new();

        index.link("node:a", 1, 100);
        index.link("node:b", 2, 200);
        index.link("node:c", 1, 300);

        let bytes = index.serialize();
        let restored = GraphTableIndex::deserialize(&bytes).unwrap();

        assert_eq!(restored.stats().node_to_row_count, 3);
        assert_eq!(restored.get_row_for_node("node:a").unwrap().row_id, 100);
        assert_eq!(restored.get_node_for_row(2, 200).unwrap(), "node:b");
    }

    #[test]
    fn test_concurrent_access() {
        use std::sync::Arc;
        use std::thread;

        let index = Arc::new(GraphTableIndex::new());
        let mut handles = vec![];

        // Spawn writers
        for i in 0..10 {
            let idx = Arc::clone(&index);
            handles.push(thread::spawn(move || {
                for j in 0..100 {
                    idx.link(&format!("node:{}:{}", i, j), i as u16, j);
                }
            }));
        }

        // Spawn readers
        for _ in 0..5 {
            let idx = Arc::clone(&index);
            handles.push(thread::spawn(move || {
                for i in 0..10 {
                    for j in 0..100 {
                        let _ = idx.get_row_for_node(&format!("node:{}:{}", i, j));
                    }
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(index.stats().node_to_row_count, 1000);
    }
}
