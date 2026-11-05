//! Native compatibility paths for the shared query result representation.
use crate::storage::engine::graph_store::StoredNode;
pub use reddb_engine::query::unified::*;

impl From<&StoredNode> for MatchedNode {
    fn from(node: &StoredNode) -> Self {
        Self {
            id: node.id.clone(),
            label: node.label.clone(),
            node_label: node.node_type.as_str().to_string(),
            properties: std::collections::HashMap::new(),
        }
    }
}
