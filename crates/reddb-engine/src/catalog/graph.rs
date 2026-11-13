//! Optional graph admission. Each vertex has exactly one concrete type.
use super::enforcement::{value_matches_declared_type, ContractError};
use crate::storage::entity::{EntityData, EntityKind, UnifiedEntity};
use reddb_types::Value;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphContract {
    pub vertex_types: BTreeMap<String, GraphVertexContract>,
    pub edge_types: BTreeMap<String, GraphEdgeContract>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphVertexContract {
    pub properties: BTreeMap<String, GraphPropertyContract>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphEdgeContract {
    pub from_type: String,
    pub to_type: String,
    pub properties: BTreeMap<String, GraphPropertyContract>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphPropertyContract {
    /// A RedDB scalar type name (the same names accepted in table contracts).
    pub data_type: String,
    pub required: bool,
}

fn invalid(message: impl Into<String>) -> ContractError {
    ContractError::InvalidOperation(format!("graph contract: {}", message.into()))
}

impl GraphContract {
    pub fn from_json(json: &str) -> Result<Self, ContractError> {
        let persisted = reddb_file::decode_physical_graph_contract_json(json)
            .map_err(|error| invalid(error.to_string()))?;
        from_persisted(persisted)
    }

    pub fn to_json(&self) -> Result<String, ContractError> {
        reddb_file::encode_physical_graph_contract_json(&to_persisted(self))
            .map_err(|error| invalid(error.to_string()))
    }

    pub fn validate(&self) -> Result<(), ContractError> {
        if self.vertex_types.is_empty() || self.vertex_types.len() + self.edge_types.len() > 1024 {
            return Err(invalid(
                "declare at least one vertex type and at most 1024 total types",
            ));
        }
        let mut property_count = 0usize;
        for (name, properties) in self
            .vertex_types
            .iter()
            .map(|(name, vertex)| (name, &vertex.properties))
            .chain(
                self.edge_types
                    .iter()
                    .map(|(name, edge)| (name, &edge.properties)),
            )
        {
            if name.trim().is_empty() || name.len() > 256 {
                return Err(invalid("type names must contain 1 to 256 bytes"));
            }
            property_count += properties.len();
            if property_count > 16384 {
                return Err(invalid("at most 16384 properties may be declared"));
            }
            for (property, rule) in properties {
                if property.trim().is_empty() || property.len() > 256 || rule.data_type.len() > 256
                {
                    return Err(invalid(
                        "property names and type declarations must fit in 256 bytes",
                    ));
                }
                reddb_rql::analyzer::resolve_declared_data_type(&rule.data_type)
                    .map_err(|error| invalid(format!("{name}.{property}: {error}")))?;
            }
        }
        for (name, edge) in &self.edge_types {
            if !self.vertex_types.contains_key(&edge.from_type)
                || !self.vertex_types.contains_key(&edge.to_type)
            {
                return Err(invalid(format!(
                    "edge '{name}' references an undeclared vertex type"
                )));
            }
        }
        Ok(())
    }

    /// Validate a projected graph, after applying every change in a statement
    /// or merge. Additional properties are allowed; declared properties have
    /// exact native types and required properties cannot be absent or NULL.
    pub fn validate_entities(&self, entities: &[&UnifiedEntity]) -> Result<(), ContractError> {
        self.validate()?;
        let mut vertices: HashMap<String, Vec<&str>> = HashMap::new();
        for entity in entities {
            if let EntityKind::GraphNode(node) = &entity.kind {
                let rule = self.vertex_types.get(&node.node_type).ok_or_else(|| {
                    invalid(format!("undeclared vertex type '{}'", node.node_type))
                })?;
                let EntityData::Node(data) = &entity.data else {
                    return Err(invalid("vertex requires node data"));
                };
                validate_properties(&node.node_type, &rule.properties, &data.properties)?;
                for alias in [
                    node.label.clone(),
                    entity.id.raw().to_string(),
                    entity.logical_id().raw().to_string(),
                ] {
                    vertices.entry(alias).or_default().push(&node.node_type);
                }
            }
        }
        for entity in entities {
            if let EntityKind::GraphEdge(edge) = &entity.kind {
                let rule = self
                    .edge_types
                    .get(&edge.label)
                    .ok_or_else(|| invalid(format!("undeclared edge type '{}'", edge.label)))?;
                let EntityData::Edge(data) = &entity.data else {
                    return Err(invalid("edge requires edge data"));
                };
                validate_properties(&edge.label, &rule.properties, &data.properties)?;
                for (endpoint, expected) in [
                    (&edge.from_node, &rule.from_type),
                    (&edge.to_node, &rule.to_type),
                ] {
                    let types = vertices.get(endpoint).ok_or_else(|| {
                        invalid(format!(
                            "edge '{}' has missing endpoint '{endpoint}'",
                            edge.label
                        ))
                    })?;
                    if types.iter().any(|actual| *actual != expected) {
                        return Err(invalid(format!(
                            "edge '{}' requires endpoint '{endpoint}' to have type '{expected}'",
                            edge.label
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}

fn validate_properties(
    owner: &str,
    rules: &BTreeMap<String, GraphPropertyContract>,
    properties: &HashMap<String, Value>,
) -> Result<(), ContractError> {
    for (name, rule) in rules {
        let value = properties.get(name);
        if rule.required && value.is_none_or(|value| matches!(value, Value::Null)) {
            return Err(invalid(format!("{owner}.{name} is required")));
        }
        if let Some(value) = value {
            let data_type = reddb_rql::analyzer::resolve_declared_data_type(&rule.data_type)
                .map_err(|error| invalid(error.to_string()))?;
            if !value_matches_declared_type(value, data_type) {
                return Err(invalid(format!(
                    "{owner}.{name} requires {}",
                    rule.data_type
                )));
            }
        }
    }
    Ok(())
}

pub fn to_persisted(contract: &GraphContract) -> reddb_file::PhysicalGraphContract {
    let properties = |properties: &BTreeMap<String, GraphPropertyContract>| {
        properties
            .iter()
            .map(|(name, property)| {
                (
                    name.clone(),
                    reddb_file::PhysicalGraphPropertyContract {
                        data_type: property.data_type.clone(),
                        required: property.required,
                    },
                )
            })
            .collect()
    };
    reddb_file::PhysicalGraphContract {
        vertex_types: contract
            .vertex_types
            .iter()
            .map(|(name, vertex)| {
                (
                    name.clone(),
                    reddb_file::PhysicalGraphVertexContract {
                        properties: properties(&vertex.properties),
                    },
                )
            })
            .collect(),
        edge_types: contract
            .edge_types
            .iter()
            .map(|(name, edge)| {
                (
                    name.clone(),
                    reddb_file::PhysicalGraphEdgeContract {
                        from_type: edge.from_type.clone(),
                        to_type: edge.to_type.clone(),
                        properties: properties(&edge.properties),
                    },
                )
            })
            .collect(),
    }
}

pub fn from_persisted(
    contract: reddb_file::PhysicalGraphContract,
) -> Result<GraphContract, ContractError> {
    let properties = |properties: BTreeMap<String, reddb_file::PhysicalGraphPropertyContract>| {
        properties
            .into_iter()
            .map(|(name, property)| {
                (
                    name,
                    GraphPropertyContract {
                        data_type: property.data_type,
                        required: property.required,
                    },
                )
            })
            .collect()
    };
    let contract = GraphContract {
        vertex_types: contract
            .vertex_types
            .into_iter()
            .map(|(name, vertex)| {
                (
                    name,
                    GraphVertexContract {
                        properties: properties(vertex.properties),
                    },
                )
            })
            .collect(),
        edge_types: contract
            .edge_types
            .into_iter()
            .map(|(name, edge)| {
                (
                    name,
                    GraphEdgeContract {
                        from_type: edge.from_type,
                        to_type: edge.to_type,
                        properties: properties(edge.properties),
                    },
                )
            })
            .collect(),
    };
    contract.validate()?;
    Ok(contract)
}
