//! Reference Types for Cross-Entity Linking
//!
//! TableRef, NodeRef, VectorRef, and AnyRef for metadata linking.

use std::collections::HashMap;

use super::super::{EntityId, MetadataValue};

/// Reference to a table row (for metadata linking)
#[derive(Debug, Clone)]
pub struct TableRef {
    pub table: String,
    pub row_id: u64,
}

impl TableRef {
    pub fn new(table: impl Into<String>, row_id: u64) -> Self {
        Self {
            table: table.into(),
            row_id,
        }
    }

    /// Convert to MetadataValue for storage
    pub fn to_metadata(&self) -> MetadataValue {
        MetadataValue::Object(HashMap::from([
            (
                "_type".to_string(),
                MetadataValue::String("table_ref".to_string()),
            ),
            (
                "table".to_string(),
                MetadataValue::String(self.table.clone()),
            ),
            ("row_id".to_string(), MetadataValue::Int(self.row_id as i64)),
        ]))
    }
}

/// Reference to a graph node (for metadata linking)
#[derive(Debug, Clone)]
pub struct NodeRef {
    pub collection: String,
    pub node_id: EntityId,
}

impl NodeRef {
    pub fn new(collection: impl Into<String>, node_id: EntityId) -> Self {
        Self {
            collection: collection.into(),
            node_id,
        }
    }

    pub fn to_metadata(&self) -> MetadataValue {
        MetadataValue::Object(HashMap::from([
            (
                "_type".to_string(),
                MetadataValue::String("node_ref".to_string()),
            ),
            (
                "collection".to_string(),
                MetadataValue::String(self.collection.clone()),
            ),
            (
                "node_id".to_string(),
                MetadataValue::Int(self.node_id.0 as i64),
            ),
        ]))
    }
}

/// Reference to a vector (for metadata linking)
#[derive(Debug, Clone)]
pub struct VectorRef {
    pub collection: String,
    pub vector_id: EntityId,
}

impl VectorRef {
    pub fn new(collection: impl Into<String>, vector_id: EntityId) -> Self {
        Self {
            collection: collection.into(),
            vector_id,
        }
    }

    pub fn to_metadata(&self) -> MetadataValue {
        MetadataValue::Object(HashMap::from([
            (
                "_type".to_string(),
                MetadataValue::String("vector_ref".to_string()),
            ),
            (
                "collection".to_string(),
                MetadataValue::String(self.collection.clone()),
            ),
            (
                "vector_id".to_string(),
                MetadataValue::Int(self.vector_id.0 as i64),
            ),
        ]))
    }
}

/// Universal reference enum - can point to anything
#[derive(Debug, Clone)]
pub enum AnyRef {
    Table(TableRef),
    Node(NodeRef),
    Vector(VectorRef),
    Edge(EntityId),
}

impl AnyRef {
    pub fn to_metadata(&self) -> MetadataValue {
        match self {
            Self::Table(r) => r.to_metadata(),
            Self::Node(r) => r.to_metadata(),
            Self::Vector(r) => r.to_metadata(),
            Self::Edge(id) => MetadataValue::Object(HashMap::from([
                (
                    "_type".to_string(),
                    MetadataValue::String("edge_ref".to_string()),
                ),
                ("edge_id".to_string(), MetadataValue::Int(id.0 as i64)),
            ])),
        }
    }
}
