//! Disk-Backed Graph Backend
//!
//! Bridges the high-level GraphSegment API with the low-level GraphStore engine.
//! Provides transparent disk-backed storage while maintaining the same interface
//! the intelligence modules expect.

use std::sync::Arc;

use crate::storage::engine::graph_store::{GraphEdgeType, GraphNodeType, GraphStore};
use crate::storage::segments::graph::{EdgeRef, EdgeType, GraphNode, NodeType};

/// Convert NodeType to GraphNodeType
impl From<NodeType> for GraphNodeType {
    fn from(nt: NodeType) -> Self {
        match nt {
            NodeType::Host => GraphNodeType::Host,
            NodeType::Service => GraphNodeType::Service,
            NodeType::Credential => GraphNodeType::Credential,
            NodeType::Vulnerability => GraphNodeType::Vulnerability,
            NodeType::Endpoint => GraphNodeType::Endpoint,
            NodeType::Technology => GraphNodeType::Technology,
            NodeType::Network => GraphNodeType::Host, // Map network to host
            NodeType::Domain => GraphNodeType::Domain,
            NodeType::AttackChain => GraphNodeType::Host, // Map attack chain to host
        }
    }
}

/// Convert GraphNodeType to NodeType
impl From<GraphNodeType> for NodeType {
    fn from(snt: GraphNodeType) -> Self {
        match snt {
            GraphNodeType::Host => NodeType::Host,
            GraphNodeType::Service => NodeType::Service,
            GraphNodeType::Credential => NodeType::Credential,
            GraphNodeType::Vulnerability => NodeType::Vulnerability,
            GraphNodeType::Endpoint => NodeType::Endpoint,
            GraphNodeType::Technology => NodeType::Technology,
            GraphNodeType::User => NodeType::Credential, // Map user to credential
            GraphNodeType::Domain => NodeType::Host,     // Map domain to host
            GraphNodeType::Certificate => NodeType::Technology, // Map cert to tech
        }
    }
}

/// Convert EdgeType to GraphEdgeType
impl From<EdgeType> for GraphEdgeType {
    fn from(et: EdgeType) -> Self {
        match et {
            EdgeType::HasService => GraphEdgeType::HasService,
            EdgeType::HasEndpoint => GraphEdgeType::HasEndpoint,
            EdgeType::UsesTech => GraphEdgeType::UsesTech,
            EdgeType::AuthAccess => GraphEdgeType::AuthAccess,
            EdgeType::AffectedBy => GraphEdgeType::AffectedBy,
            EdgeType::Contains => GraphEdgeType::Contains,
            EdgeType::ConnectsTo => GraphEdgeType::ConnectsTo,
            EdgeType::RelatedTo => GraphEdgeType::RelatedTo,
            EdgeType::AttackPath => GraphEdgeType::ConnectsTo, // Map attack path to connects
        }
    }
}

/// Convert GraphEdgeType to EdgeType
impl From<GraphEdgeType> for EdgeType {
    fn from(get: GraphEdgeType) -> Self {
        match get {
            GraphEdgeType::HasService => EdgeType::HasService,
            GraphEdgeType::HasEndpoint => EdgeType::HasEndpoint,
            GraphEdgeType::UsesTech => EdgeType::UsesTech,
            GraphEdgeType::AuthAccess => EdgeType::AuthAccess,
            GraphEdgeType::AffectedBy => EdgeType::AffectedBy,
            GraphEdgeType::Contains => EdgeType::Contains,
            GraphEdgeType::ConnectsTo => EdgeType::ConnectsTo,
            GraphEdgeType::RelatedTo => EdgeType::RelatedTo,
            GraphEdgeType::HasUser => EdgeType::Contains, // Map to contains
            GraphEdgeType::HasCert => EdgeType::UsesTech, // Map to uses_tech
        }
    }
}

/// Disk-backed graph storage adapter
///
/// Wraps GraphStore to provide a GraphSegment-compatible interface.
/// All operations are automatically persisted to disk.
pub struct DiskBackedGraph {
    store: GraphStore,
}

impl DiskBackedGraph {
    /// Create a new disk-backed graph
    pub fn new() -> Self {
        Self {
            store: GraphStore::new(),
        }
    }

    /// Load from serialized bytes
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        GraphStore::deserialize(bytes)
            .ok()
            .map(|store| Self { store })
    }

    /// Serialize to bytes for persistence
    pub fn to_bytes(&self) -> Vec<u8> {
        self.store.serialize()
    }

    /// Add a node (converts GraphNode to StoredNode)
    pub fn add_node(&self, node: &GraphNode) -> bool {
        self.store
            .add_node(&node.id, &node.label, node.node_type.into())
            .is_ok()
    }

    /// Add a node by its components
    pub fn add_node_simple(&self, id: &str, label: &str, node_type: NodeType) -> bool {
        self.store.add_node(id, label, node_type.into()).is_ok()
    }

    /// Add an edge
    pub fn add_edge(&self, source: &str, target: &str, edge_type: EdgeType, weight: f32) -> bool {
        self.store
            .add_edge(source, target, edge_type.into(), weight)
            .is_ok()
    }

    /// Ensure a node exists (add if missing)
    pub fn ensure_node(&self, id: &str, label: &str, node_type: NodeType) {
        if !self.store.has_node(id) {
            let _ = self.store.add_node(id, label, node_type.into());
        }
    }

    /// Get a node by ID (converts StoredNode to GraphNode)
    pub fn get_node(&self, id: &str) -> Option<GraphNode> {
        let stored = self.store.get_node(id)?;

        // Get edges for this node
        let out_edges: Vec<EdgeRef> = self
            .store
            .outgoing_edges(id)
            .iter()
            .map(|(et, target, weight)| EdgeRef {
                target_id: target.clone(),
                edge_type: (*et).into(),
                weight: *weight,
            })
            .collect();

        let in_edges: Vec<EdgeRef> = self
            .store
            .incoming_edges(id)
            .iter()
            .map(|(et, source, weight)| EdgeRef {
                target_id: source.clone(),
                edge_type: (*et).into(),
                weight: *weight,
            })
            .collect();

        Some(GraphNode {
            id: stored.id,
            node_type: stored.node_type.into(),
            label: stored.label,
            metadata: Vec::new(), // StoredNode doesn't have metadata currently
            in_edges,
            out_edges,
            cache_generation: 0,
            cache_value: 0.0,
            depth: 0,
        })
    }

    /// Check if node exists
    pub fn has_node(&self, id: &str) -> bool {
        self.store.has_node(id)
    }

    /// Get node count
    pub fn node_count(&self) -> usize {
        self.store.node_count() as usize
    }

    /// Get edge count
    pub fn edge_count(&self) -> usize {
        self.store.edge_count() as usize
    }

    /// Get all nodes of a specific type
    pub fn nodes_of_type(&self, node_type: NodeType) -> Vec<GraphNode> {
        let stored_type: GraphNodeType = node_type.into();
        self.store
            .nodes_of_type(stored_type)
            .iter()
            .filter_map(|stored| self.get_node(&stored.id))
            .collect()
    }

    /// Get all node IDs
    pub fn all_node_ids(&self) -> Vec<String> {
        self.store.iter_nodes().map(|n| n.id).collect()
    }

    /// Iterate over all nodes (streaming for large graphs)
    pub fn iter_nodes(&self) -> impl Iterator<Item = GraphNode> + '_ {
        self.store.iter_nodes().filter_map(|stored| {
            let out_edges: Vec<EdgeRef> = self
                .store
                .outgoing_edges(&stored.id)
                .iter()
                .map(|(et, target, weight)| EdgeRef {
                    target_id: target.clone(),
                    edge_type: (*et).into(),
                    weight: *weight,
                })
                .collect();

            let in_edges: Vec<EdgeRef> = self
                .store
                .incoming_edges(&stored.id)
                .iter()
                .map(|(et, source, weight)| EdgeRef {
                    target_id: source.clone(),
                    edge_type: (*et).into(),
                    weight: *weight,
                })
                .collect();

            Some(GraphNode {
                id: stored.id.clone(),
                node_type: stored.node_type.into(),
                label: stored.label.clone(),
                metadata: Vec::new(),
                in_edges,
                out_edges,
                cache_generation: 0,
                cache_value: 0.0,
                depth: 0,
            })
        })
    }

    /// Get outgoing edges from a node
    pub fn outgoing_edges(&self, node_id: &str) -> Vec<EdgeRef> {
        self.store
            .outgoing_edges(node_id)
            .iter()
            .map(|(et, target, weight)| EdgeRef {
                target_id: target.clone(),
                edge_type: (*et).into(),
                weight: *weight,
            })
            .collect()
    }

    /// Get incoming edges to a node
    pub fn incoming_edges(&self, node_id: &str) -> Vec<EdgeRef> {
        self.store
            .incoming_edges(node_id)
            .iter()
            .map(|(et, source, weight)| EdgeRef {
                target_id: source.clone(),
                edge_type: (*et).into(),
                weight: *weight,
            })
            .collect()
    }

    /// Get statistics
    pub fn stats(&self) -> GraphBackendStats {
        let store_stats = self.store.stats();
        GraphBackendStats {
            node_count: store_stats.node_count as usize,
            edge_count: store_stats.edge_count as usize,
            node_pages: store_stats.node_pages as usize,
            edge_pages: store_stats.edge_pages as usize,
        }
    }

    /// Access the underlying store (for advanced operations)
    pub fn store(&self) -> &GraphStore {
        &self.store
    }
}

impl Default for DiskBackedGraph {
    fn default() -> Self {
        Self::new()
    }
}

/// Graph backend statistics
#[derive(Debug, Clone, Copy)]
pub struct GraphBackendStats {
    pub node_count: usize,
    pub edge_count: usize,
    pub node_pages: usize,
    pub edge_pages: usize,
}

/// Thread-safe shared graph for concurrent access
pub type SharedGraph = Arc<DiskBackedGraph>;

/// Create a new shared graph
pub fn shared_graph() -> SharedGraph {
    Arc::new(DiskBackedGraph::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_disk_backed_graph_basic() {
        let graph = DiskBackedGraph::new();

        // Add nodes
        let node = GraphNode::new("host:192.168.1.1", NodeType::Host, "192.168.1.1");
        assert!(graph.add_node(&node));

        let node2 = GraphNode::new("service:192.168.1.1:22:ssh", NodeType::Service, "SSH");
        assert!(graph.add_node(&node2));

        // Add edge
        assert!(graph.add_edge(
            "host:192.168.1.1",
            "service:192.168.1.1:22:ssh",
            EdgeType::HasService,
            2.0
        ));

        assert_eq!(graph.node_count(), 2);
        assert_eq!(graph.edge_count(), 1);

        // Retrieve node with edges
        let retrieved = graph.get_node("host:192.168.1.1").unwrap();
        assert_eq!(retrieved.out_edges.len(), 1);
        assert_eq!(retrieved.out_edges[0].edge_type, EdgeType::HasService);
    }

    #[test]
    fn test_disk_backed_graph_serialization() {
        let graph = DiskBackedGraph::new();

        let node = GraphNode::new("host:10.0.0.1", NodeType::Host, "10.0.0.1");
        graph.add_node(&node);

        // Serialize
        let bytes = graph.to_bytes();

        // Deserialize
        let loaded = DiskBackedGraph::from_bytes(&bytes).unwrap();
        assert_eq!(loaded.node_count(), 1);

        let retrieved = loaded.get_node("host:10.0.0.1").unwrap();
        assert_eq!(retrieved.node_type, NodeType::Host);
    }

    #[test]
    fn test_nodes_of_type() {
        let graph = DiskBackedGraph::new();

        graph.add_node_simple("host:1", "Host 1", NodeType::Host);
        graph.add_node_simple("host:2", "Host 2", NodeType::Host);
        graph.add_node_simple("cred:admin", "Admin", NodeType::Credential);

        let hosts = graph.nodes_of_type(NodeType::Host);
        assert_eq!(hosts.len(), 2);

        let creds = graph.nodes_of_type(NodeType::Credential);
        assert_eq!(creds.len(), 1);
    }

    #[test]
    fn test_shared_graph_concurrent() {
        use std::thread;

        let graph = shared_graph();
        let mut handles = vec![];

        // Spawn multiple writers
        for i in 0..10 {
            let g = Arc::clone(&graph);
            handles.push(thread::spawn(move || {
                g.add_node_simple(
                    &format!("host:192.168.1.{}", i),
                    &format!("Host {}", i),
                    NodeType::Host,
                );
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(graph.node_count(), 10);
    }
}
