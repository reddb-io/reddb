use std::collections::BTreeMap;

use crate::application::json_input::{json_object_string_map_field, json_string_field};
use crate::json::Value as JsonValue;
use crate::runtime::RuntimeGraphProjection;
use crate::{RedDBError, RedDBResult};

#[derive(Debug, Clone, Default)]
pub(crate) struct AnalyticsJobMutationInput {
    pub kind: String,
    pub projection: Option<String>,
    pub metadata: BTreeMap<String, String>,
}

pub(crate) fn parse_analytics_job_mutation_input(
    payload: &JsonValue,
) -> RedDBResult<AnalyticsJobMutationInput> {
    let Some(kind) = json_string_field(payload, "kind") else {
        return Err(RedDBError::Query(
            "field 'kind' must be a string".to_string(),
        ));
    };

    Ok(AnalyticsJobMutationInput {
        kind,
        projection: json_string_field(payload, "projection"),
        metadata: json_object_string_map_field(payload, "metadata").unwrap_or_default(),
    })
}

#[derive(Debug, Clone)]
pub(crate) struct GraphProjectionUpsertInput {
    pub name: String,
    pub projection: RuntimeGraphProjection,
    pub source: Option<String>,
}

pub(crate) fn graph_projection_from_parts(
    node_labels: Vec<String>,
    node_types: Vec<String>,
    edge_labels: Vec<String>,
) -> Option<RuntimeGraphProjection> {
    let projection = RuntimeGraphProjection {
        node_labels: (!node_labels.is_empty()).then_some(node_labels),
        node_types: (!node_types.is_empty()).then_some(node_types),
        edge_labels: (!edge_labels.is_empty()).then_some(edge_labels),
    };
    if projection.node_labels.is_none()
        && projection.node_types.is_none()
        && projection.edge_labels.is_none()
    {
        None
    } else {
        Some(projection)
    }
}

pub(crate) fn finalize_graph_projection_upsert_input(
    name: String,
    projection: Option<RuntimeGraphProjection>,
    source: Option<String>,
    missing_projection_message: &str,
) -> RedDBResult<GraphProjectionUpsertInput> {
    let trimmed_name = name.trim();
    if trimmed_name.is_empty() {
        return Err(RedDBError::Query(
            "graph projection name cannot be empty".to_string(),
        ));
    }

    let Some(projection) = projection else {
        return Err(RedDBError::Query(missing_projection_message.to_string()));
    };

    Ok(GraphProjectionUpsertInput {
        name: trimmed_name.to_string(),
        projection,
        source: normalize_optional_string(source),
    })
}

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed.to_string())
    })
}
