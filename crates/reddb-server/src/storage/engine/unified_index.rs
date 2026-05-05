//! Unified Cross-Storage Index
//!
//! Provides bidirectional lookups between different storage types:
//! - Graph nodes ↔ Table rows
//! - Graph nodes ↔ Vector embeddings
//! - Table rows ↔ Vector embeddings
//!
//! This enables the unified query model where a single RQL query can
//! seamlessly traverse tables, graphs, and vectors.

use std::collections::HashMap;
use std::sync::RwLock;

/// Unique identifier for a vector in a collection
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct VectorKey {
    pub collection: String,
    pub vector_id: u64,
}

impl VectorKey {
    pub fn new(collection: impl Into<String>, vector_id: u64) -> Self {
        Self {
            collection: collection.into(),
            vector_id,
        }
    }
}

/// Unique identifier for a row in a table
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct RowKey {
    pub table: String,
    pub row_id: u64,
}

impl RowKey {
    pub fn new(table: impl Into<String>, row_id: u64) -> Self {
        Self {
            table: table.into(),
            row_id,
        }
    }
}

/// A reference to any storage element
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub enum StorageRef {
    /// Reference to a graph node by ID
    Node(String),
    /// Reference to a graph edge by ID
    Edge(String),
    /// Reference to a vector in a collection
    Vector(VectorKey),
    /// Reference to a table row
    Row(RowKey),
}

impl StorageRef {
    pub fn node(id: impl Into<String>) -> Self {
        StorageRef::Node(id.into())
    }

    pub fn edge(id: impl Into<String>) -> Self {
        StorageRef::Edge(id.into())
    }

    pub fn vector(collection: impl Into<String>, vector_id: u64) -> Self {
        StorageRef::Vector(VectorKey::new(collection, vector_id))
    }

    pub fn row(table: impl Into<String>, row_id: u64) -> Self {
        StorageRef::Row(RowKey::new(table, row_id))
    }
}

/// Cross-reference between two storage elements
#[derive(Debug, Clone)]
pub struct CrossRef {
    pub source: StorageRef,
    pub target: StorageRef,
    /// Optional metadata about the relationship
    pub metadata: Option<HashMap<String, String>>,
}

impl CrossRef {
    pub fn new(source: StorageRef, target: StorageRef) -> Self {
        Self {
            source,
            target,
            metadata: None,
        }
    }

    pub fn with_metadata(mut self, key: &str, value: &str) -> Self {
        self.metadata
            .get_or_insert_with(HashMap::new)
            .insert(key.to_string(), value.to_string());
        self
    }
}

/// Statistics about the unified index
#[derive(Debug, Clone, Default)]
pub struct UnifiedIndexStats {
    pub node_to_vector_count: usize,
    pub node_to_row_count: usize,
    pub vector_to_row_count: usize,
    pub total_refs: usize,
}

/// Unified index for cross-storage lookups
///
/// Maintains bidirectional mappings between all storage types:
/// - Nodes ↔ Vectors
/// - Nodes ↔ Rows
/// - Vectors ↔ Rows (via nodes or direct)
pub struct UnifiedIndex {
    /// Node ID → Vector keys (one node can have multiple embeddings)
    node_to_vectors: RwLock<HashMap<String, Vec<VectorKey>>>,
    /// Vector key → Node ID (one vector belongs to one node)
    vector_to_node: RwLock<HashMap<VectorKey, String>>,

    /// Node ID → Row keys (one node can link to multiple rows)
    node_to_rows: RwLock<HashMap<String, Vec<RowKey>>>,
    /// Row key → Node ID (one row can be represented by one node)
    row_to_node: RwLock<HashMap<RowKey, String>>,

    /// Edge ID → Node IDs (source, target)
    edge_to_nodes: RwLock<HashMap<String, (String, String)>>,

    /// Vector key → Row key (direct vector-to-row mapping)
    vector_to_row: RwLock<HashMap<VectorKey, RowKey>>,
    /// Row key → Vector keys (one row can have multiple embeddings)
    row_to_vectors: RwLock<HashMap<RowKey, Vec<VectorKey>>>,
}

impl UnifiedIndex {
    /// Create a new empty unified index
    pub fn new() -> Self {
        Self {
            node_to_vectors: RwLock::new(HashMap::new()),
            vector_to_node: RwLock::new(HashMap::new()),
            node_to_rows: RwLock::new(HashMap::new()),
            row_to_node: RwLock::new(HashMap::new()),
            edge_to_nodes: RwLock::new(HashMap::new()),
            vector_to_row: RwLock::new(HashMap::new()),
            row_to_vectors: RwLock::new(HashMap::new()),
        }
    }

    // =========================================================================
    // Node ↔ Vector mappings
    // =========================================================================

    /// Link a node to a vector embedding
    pub fn link_node_to_vector(&self, node_id: &str, collection: &str, vector_id: u64) {
        let key = VectorKey::new(collection, vector_id);

        // Forward: node → vector
        if let Ok(mut map) = self.node_to_vectors.write() {
            map.entry(node_id.to_string())
                .or_insert_with(Vec::new)
                .push(key.clone());
        }

        // Reverse: vector → node
        if let Ok(mut map) = self.vector_to_node.write() {
            map.insert(key, node_id.to_string());
        }
    }

    /// Get all vectors linked to a node
    pub fn get_node_vectors(&self, node_id: &str) -> Vec<VectorKey> {
        self.node_to_vectors
            .read()
            .ok()
            .and_then(|map| map.get(node_id).cloned())
            .unwrap_or_default()
    }

    /// Get the node linked to a vector
    pub fn get_vector_node(&self, collection: &str, vector_id: u64) -> Option<String> {
        let key = VectorKey::new(collection, vector_id);
        self.vector_to_node
            .read()
            .ok()
            .and_then(|map| map.get(&key).cloned())
    }

    /// Unlink a node from a vector
    pub fn unlink_node_from_vector(&self, node_id: &str, collection: &str, vector_id: u64) {
        let key = VectorKey::new(collection, vector_id);

        if let Ok(mut map) = self.node_to_vectors.write() {
            if let Some(vectors) = map.get_mut(node_id) {
                vectors.retain(|v| v != &key);
                if vectors.is_empty() {
                    map.remove(node_id);
                }
            }
        }

        if let Ok(mut map) = self.vector_to_node.write() {
            map.remove(&key);
        }
    }

    // =========================================================================
    // Node ↔ Row mappings
    // =========================================================================

    /// Link a node to a table row
    pub fn link_node_to_row(&self, node_id: &str, table: &str, row_id: u64) {
        let key = RowKey::new(table, row_id);

        // Forward: node → row
        if let Ok(mut map) = self.node_to_rows.write() {
            map.entry(node_id.to_string())
                .or_insert_with(Vec::new)
                .push(key.clone());
        }

        // Reverse: row → node
        if let Ok(mut map) = self.row_to_node.write() {
            map.insert(key, node_id.to_string());
        }
    }

    /// Get all rows linked to a node
    pub fn get_node_rows(&self, node_id: &str) -> Vec<RowKey> {
        self.node_to_rows
            .read()
            .ok()
            .and_then(|map| map.get(node_id).cloned())
            .unwrap_or_default()
    }

    /// Get the node linked to a row
    pub fn get_row_node(&self, table: &str, row_id: u64) -> Option<String> {
        let key = RowKey::new(table, row_id);
        self.row_to_node
            .read()
            .ok()
            .and_then(|map| map.get(&key).cloned())
    }

    /// Unlink a node from a row
    pub fn unlink_node_from_row(&self, node_id: &str, table: &str, row_id: u64) {
        let key = RowKey::new(table, row_id);

        if let Ok(mut map) = self.node_to_rows.write() {
            if let Some(rows) = map.get_mut(node_id) {
                rows.retain(|r| r != &key);
                if rows.is_empty() {
                    map.remove(node_id);
                }
            }
        }

        if let Ok(mut map) = self.row_to_node.write() {
            map.remove(&key);
        }
    }

    // =========================================================================
    // Edge tracking
    // =========================================================================

    /// Register an edge with its source and target nodes
    pub fn register_edge(&self, edge_id: &str, source_node: &str, target_node: &str) {
        if let Ok(mut map) = self.edge_to_nodes.write() {
            map.insert(
                edge_id.to_string(),
                (source_node.to_string(), target_node.to_string()),
            );
        }
    }

    /// Get the nodes connected by an edge
    pub fn get_edge_nodes(&self, edge_id: &str) -> Option<(String, String)> {
        self.edge_to_nodes
            .read()
            .ok()
            .and_then(|map| map.get(edge_id).cloned())
    }

    /// Unregister an edge
    pub fn unregister_edge(&self, edge_id: &str) {
        if let Ok(mut map) = self.edge_to_nodes.write() {
            map.remove(edge_id);
        }
    }

    // =========================================================================
    // Vector ↔ Row mappings (direct, bypassing nodes)
    // =========================================================================

    /// Link a vector directly to a table row
    pub fn link_vector_to_row(&self, collection: &str, vector_id: u64, table: &str, row_id: u64) {
        let vkey = VectorKey::new(collection, vector_id);
        let rkey = RowKey::new(table, row_id);

        // Forward: vector → row
        if let Ok(mut map) = self.vector_to_row.write() {
            map.insert(vkey.clone(), rkey.clone());
        }

        // Reverse: row → vectors
        if let Ok(mut map) = self.row_to_vectors.write() {
            map.entry(rkey).or_insert_with(Vec::new).push(vkey);
        }
    }

    /// Get the row linked to a vector
    pub fn get_vector_row(&self, collection: &str, vector_id: u64) -> Option<RowKey> {
        let key = VectorKey::new(collection, vector_id);
        self.vector_to_row
            .read()
            .ok()
            .and_then(|map| map.get(&key).cloned())
    }

    /// Get all vectors linked to a row
    pub fn get_row_vectors(&self, table: &str, row_id: u64) -> Vec<VectorKey> {
        let key = RowKey::new(table, row_id);
        self.row_to_vectors
            .read()
            .ok()
            .and_then(|map| map.get(&key).cloned())
            .unwrap_or_default()
    }

    // =========================================================================
    // Cross-storage resolution
    // =========================================================================

    /// Resolve a storage reference to all related references
    ///
    /// This performs transitive lookups:
    /// - Given a node, returns linked vectors and rows
    /// - Given a vector, returns linked node and row
    /// - Given a row, returns linked node and vectors
    pub fn resolve(&self, source: &StorageRef) -> Vec<StorageRef> {
        let mut results = Vec::new();

        match source {
            StorageRef::Node(node_id) => {
                // Get linked vectors
                for vkey in self.get_node_vectors(node_id) {
                    results.push(StorageRef::Vector(vkey));
                }
                // Get linked rows
                for rkey in self.get_node_rows(node_id) {
                    results.push(StorageRef::Row(rkey));
                }
            }
            StorageRef::Vector(vkey) => {
                // Get linked node
                if let Some(node_id) = self.get_vector_node(&vkey.collection, vkey.vector_id) {
                    results.push(StorageRef::Node(node_id));
                }
                // Get linked row (direct)
                if let Some(rkey) = self.get_vector_row(&vkey.collection, vkey.vector_id) {
                    results.push(StorageRef::Row(rkey));
                }
            }
            StorageRef::Row(rkey) => {
                // Get linked node
                if let Some(node_id) = self.get_row_node(&rkey.table, rkey.row_id) {
                    results.push(StorageRef::Node(node_id));
                }
                // Get linked vectors (direct)
                for vkey in self.get_row_vectors(&rkey.table, rkey.row_id) {
                    results.push(StorageRef::Vector(vkey));
                }
            }
            StorageRef::Edge(edge_id) => {
                // Get connected nodes
                if let Some((src, tgt)) = self.get_edge_nodes(edge_id) {
                    results.push(StorageRef::Node(src));
                    results.push(StorageRef::Node(tgt));
                }
            }
        }

        results
    }

    /// Resolve with transitive closure (up to max_depth)
    ///
    /// Follows references recursively to find all related elements.
    pub fn resolve_transitive(&self, source: &StorageRef, max_depth: usize) -> Vec<StorageRef> {
        let mut visited = std::collections::HashSet::new();
        let mut results = Vec::new();
        let mut frontier = vec![source.clone()];

        for _ in 0..max_depth {
            let mut next_frontier = Vec::new();
            for current in frontier {
                if !visited.insert(current.clone()) {
                    continue;
                }
                for related in self.resolve(&current) {
                    if !visited.contains(&related) {
                        results.push(related.clone());
                        next_frontier.push(related);
                    }
                }
            }
            if next_frontier.is_empty() {
                break;
            }
            frontier = next_frontier;
        }

        results
    }

    // =========================================================================
    // Bulk operations
    // =========================================================================

    /// Remove all references to a node
    pub fn remove_node(&self, node_id: &str) {
        // Remove vector links
        if let Ok(mut nv) = self.node_to_vectors.write() {
            if let Some(vectors) = nv.remove(node_id) {
                if let Ok(mut vn) = self.vector_to_node.write() {
                    for v in vectors {
                        vn.remove(&v);
                    }
                }
            }
        }

        // Remove row links
        if let Ok(mut nr) = self.node_to_rows.write() {
            if let Some(rows) = nr.remove(node_id) {
                if let Ok(mut rn) = self.row_to_node.write() {
                    for r in rows {
                        rn.remove(&r);
                    }
                }
            }
        }
    }

    /// Remove all references to a vector
    pub fn remove_vector(&self, collection: &str, vector_id: u64) {
        let key = VectorKey::new(collection, vector_id);

        // Remove node link
        if let Ok(mut vn) = self.vector_to_node.write() {
            if let Some(node_id) = vn.remove(&key) {
                if let Ok(mut nv) = self.node_to_vectors.write() {
                    if let Some(vectors) = nv.get_mut(&node_id) {
                        vectors.retain(|v| v != &key);
                        if vectors.is_empty() {
                            nv.remove(&node_id);
                        }
                    }
                }
            }
        }

        // Remove row link
        if let Ok(mut vr) = self.vector_to_row.write() {
            if let Some(rkey) = vr.remove(&key) {
                if let Ok(mut rv) = self.row_to_vectors.write() {
                    if let Some(vectors) = rv.get_mut(&rkey) {
                        vectors.retain(|v| v != &key);
                        if vectors.is_empty() {
                            rv.remove(&rkey);
                        }
                    }
                }
            }
        }
    }

    /// Get statistics about the index
    pub fn stats(&self) -> UnifiedIndexStats {
        let node_to_vector_count = self
            .node_to_vectors
            .read()
            .map(|m| m.values().map(|v| v.len()).sum())
            .unwrap_or(0);
        let node_to_row_count = self
            .node_to_rows
            .read()
            .map(|m| m.values().map(|v| v.len()).sum())
            .unwrap_or(0);
        let vector_to_row_count = self.vector_to_row.read().map(|m| m.len()).unwrap_or(0);

        UnifiedIndexStats {
            node_to_vector_count,
            node_to_row_count,
            vector_to_row_count,
            total_refs: node_to_vector_count + node_to_row_count + vector_to_row_count,
        }
    }

    /// Clear all entries from the index
    pub fn clear(&self) {
        if let Ok(mut m) = self.node_to_vectors.write() {
            m.clear();
        }
        if let Ok(mut m) = self.vector_to_node.write() {
            m.clear();
        }
        if let Ok(mut m) = self.node_to_rows.write() {
            m.clear();
        }
        if let Ok(mut m) = self.row_to_node.write() {
            m.clear();
        }
        if let Ok(mut m) = self.edge_to_nodes.write() {
            m.clear();
        }
        if let Ok(mut m) = self.vector_to_row.write() {
            m.clear();
        }
        if let Ok(mut m) = self.row_to_vectors.write() {
            m.clear();
        }
    }
}

impl Default for UnifiedIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_vector_linking() {
        let idx = UnifiedIndex::new();

        // Link node to vector
        idx.link_node_to_vector("host:1", "embeddings", 42);

        // Check forward lookup
        let vectors = idx.get_node_vectors("host:1");
        assert_eq!(vectors.len(), 1);
        assert_eq!(vectors[0].collection, "embeddings");
        assert_eq!(vectors[0].vector_id, 42);

        // Check reverse lookup
        let node = idx.get_vector_node("embeddings", 42);
        assert_eq!(node, Some("host:1".to_string()));
    }

    #[test]
    fn test_node_row_linking() {
        let idx = UnifiedIndex::new();

        // Link node to row
        idx.link_node_to_row("host:1", "hosts", 100);

        // Check forward lookup
        let rows = idx.get_node_rows("host:1");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].table, "hosts");
        assert_eq!(rows[0].row_id, 100);

        // Check reverse lookup
        let node = idx.get_row_node("hosts", 100);
        assert_eq!(node, Some("host:1".to_string()));
    }

    #[test]
    fn test_resolve() {
        let idx = UnifiedIndex::new();

        // Set up relationships
        idx.link_node_to_vector("host:1", "embeddings", 42);
        idx.link_node_to_row("host:1", "hosts", 100);

        // Resolve from node
        let refs = idx.resolve(&StorageRef::node("host:1"));
        assert_eq!(refs.len(), 2);

        // Resolve from vector
        let refs = idx.resolve(&StorageRef::vector("embeddings", 42));
        assert_eq!(refs.len(), 1);
        assert!(matches!(&refs[0], StorageRef::Node(id) if id == "host:1"));
    }

    #[test]
    fn test_transitive_resolve() {
        let idx = UnifiedIndex::new();

        // Chain: row -> node -> vector
        idx.link_node_to_row("host:1", "hosts", 100);
        idx.link_node_to_vector("host:1", "embeddings", 42);

        // Start from row, find vector through node
        let refs = idx.resolve_transitive(&StorageRef::row("hosts", 100), 2);

        // Should find: node:host:1, vector:embeddings:42
        assert!(refs
            .iter()
            .any(|r| matches!(r, StorageRef::Node(id) if id == "host:1")));
        assert!(refs.iter().any(
            |r| matches!(r, StorageRef::Vector(vk) if vk.collection == "embeddings" && vk.vector_id == 42)
        ));
    }

    #[test]
    fn test_multiple_vectors_per_node() {
        let idx = UnifiedIndex::new();

        // Link node to multiple vectors (different collections)
        idx.link_node_to_vector("host:1", "embeddings", 1);
        idx.link_node_to_vector("host:1", "embeddings", 2);
        idx.link_node_to_vector("host:1", "descriptions", 1);

        let vectors = idx.get_node_vectors("host:1");
        assert_eq!(vectors.len(), 3);
    }

    #[test]
    fn test_unlink() {
        let idx = UnifiedIndex::new();

        idx.link_node_to_vector("host:1", "embeddings", 42);
        assert!(idx.get_vector_node("embeddings", 42).is_some());

        idx.unlink_node_from_vector("host:1", "embeddings", 42);
        assert!(idx.get_vector_node("embeddings", 42).is_none());
        assert!(idx.get_node_vectors("host:1").is_empty());
    }

    #[test]
    fn test_stats() {
        let idx = UnifiedIndex::new();

        idx.link_node_to_vector("host:1", "embeddings", 1);
        idx.link_node_to_vector("host:1", "embeddings", 2);
        idx.link_node_to_row("host:1", "hosts", 100);
        idx.link_vector_to_row("embeddings", 3, "hosts", 200);

        let stats = idx.stats();
        assert_eq!(stats.node_to_vector_count, 2);
        assert_eq!(stats.node_to_row_count, 1);
        assert_eq!(stats.vector_to_row_count, 1);
        assert_eq!(stats.total_refs, 4);
    }
}
