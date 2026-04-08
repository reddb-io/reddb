use std::collections::BTreeMap;

use crate::application::{
    json_input::{json_bool_field, json_f32_field, json_string_list_field, json_usize_field},
    GraphCentralityInput, GraphClusteringInput, GraphCommunitiesInput, GraphComponentsInput,
    GraphCyclesInput, GraphHitsInput, GraphNeighborhoodInput, GraphPersonalizedPageRankInput,
    GraphShortestPathInput, GraphTopologicalSortInput, GraphTraversalInput,
};
use crate::json::Value as JsonValue;
use crate::runtime::{
    RuntimeGraphCentralityAlgorithm, RuntimeGraphCommunityAlgorithm,
    RuntimeGraphComponentsMode, RuntimeGraphDirection, RuntimeGraphPathAlgorithm,
    RuntimeGraphProjection, RuntimeGraphTraversalStrategy,
};
use crate::{RedDBError, RedDBResult};

pub(crate) fn parse_inline_projection(payload: &JsonValue) -> Option<RuntimeGraphProjection> {
    let projection = RuntimeGraphProjection {
        node_labels: json_string_list_field(payload, "node_labels"),
        node_types: json_string_list_field(payload, "node_types"),
        edge_labels: json_string_list_field(payload, "edge_labels"),
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

pub(crate) fn parse_graph_neighborhood_input(
    payload: &JsonValue,
    projection: Option<RuntimeGraphProjection>,
) -> RedDBResult<GraphNeighborhoodInput> {
    Ok(GraphNeighborhoodInput {
        node: parse_required_string_field(payload, "node")?,
        direction: parse_graph_direction(payload.get("direction").and_then(JsonValue::as_str))
            .unwrap_or(RuntimeGraphDirection::Both),
        max_depth: json_usize_field(payload, "max_depth").unwrap_or(1).max(1),
        edge_labels: json_string_list_field(payload, "edge_labels"),
        projection,
    })
}

pub(crate) fn parse_graph_traversal_input(
    payload: &JsonValue,
    projection: Option<RuntimeGraphProjection>,
) -> RedDBResult<GraphTraversalInput> {
    Ok(GraphTraversalInput {
        source: parse_required_string_field(payload, "source")?,
        direction: parse_graph_direction(payload.get("direction").and_then(JsonValue::as_str))
            .unwrap_or(RuntimeGraphDirection::Outgoing),
        max_depth: json_usize_field(payload, "max_depth").unwrap_or(3).max(1),
        strategy: parse_graph_traversal_strategy(payload.get("strategy").and_then(JsonValue::as_str))
            .unwrap_or(RuntimeGraphTraversalStrategy::Bfs),
        edge_labels: json_string_list_field(payload, "edge_labels"),
        projection,
    })
}

pub(crate) fn parse_graph_shortest_path_input(
    payload: &JsonValue,
    projection: Option<RuntimeGraphProjection>,
) -> RedDBResult<GraphShortestPathInput> {
    Ok(GraphShortestPathInput {
        source: parse_required_string_field(payload, "source")?,
        target: parse_required_string_field(payload, "target")?,
        direction: parse_graph_direction(payload.get("direction").and_then(JsonValue::as_str))
            .unwrap_or(RuntimeGraphDirection::Outgoing),
        algorithm: parse_graph_path_algorithm(payload.get("algorithm").and_then(JsonValue::as_str))
            .unwrap_or(RuntimeGraphPathAlgorithm::Dijkstra),
        edge_labels: json_string_list_field(payload, "edge_labels"),
        projection,
    })
}

pub(crate) fn parse_graph_components_input(
    payload: &JsonValue,
    projection: Option<RuntimeGraphProjection>,
) -> GraphComponentsInput {
    GraphComponentsInput {
        mode: parse_graph_components_mode(payload.get("mode").and_then(JsonValue::as_str))
            .unwrap_or(RuntimeGraphComponentsMode::Connected),
        min_size: json_usize_field(payload, "min_size").unwrap_or(1).max(1),
        projection,
    }
}

pub(crate) fn graph_components_metadata(input: &GraphComponentsInput) -> BTreeMap<String, String> {
    analytics_metadata(vec![
        ("mode", graph_components_mode_to_str(input.mode).to_string()),
        ("min_size", input.min_size.to_string()),
    ])
}

pub(crate) fn parse_graph_centrality_input(
    payload: &JsonValue,
    projection: Option<RuntimeGraphProjection>,
) -> GraphCentralityInput {
    GraphCentralityInput {
        algorithm: parse_graph_centrality_algorithm(payload.get("algorithm").and_then(JsonValue::as_str))
            .unwrap_or(RuntimeGraphCentralityAlgorithm::PageRank),
        top_k: json_usize_field(payload, "top_k").unwrap_or(25).max(1),
        normalize: json_bool_field(payload, "normalize").unwrap_or(true),
        max_iterations: json_usize_field(payload, "max_iterations"),
        epsilon: json_f32_field(payload, "epsilon").map(|value| value as f64),
        alpha: json_f32_field(payload, "alpha").map(|value| value as f64),
        projection,
    }
}

pub(crate) fn graph_centrality_kind(algorithm: RuntimeGraphCentralityAlgorithm) -> String {
    format!(
        "graph.centrality.{}",
        graph_centrality_algorithm_to_str(algorithm)
    )
}

pub(crate) fn graph_centrality_metadata(input: &GraphCentralityInput) -> BTreeMap<String, String> {
    analytics_metadata(vec![
        ("top_k", input.top_k.to_string()),
        (
            "normalize",
            if input.normalize { "true" } else { "false" }.to_string(),
        ),
    ])
}

pub(crate) fn parse_graph_communities_input(
    payload: &JsonValue,
    projection: Option<RuntimeGraphProjection>,
) -> GraphCommunitiesInput {
    GraphCommunitiesInput {
        algorithm: parse_graph_community_algorithm(payload.get("algorithm").and_then(JsonValue::as_str))
            .unwrap_or(RuntimeGraphCommunityAlgorithm::Louvain),
        min_size: json_usize_field(payload, "min_size").unwrap_or(1).max(1),
        max_iterations: json_usize_field(payload, "max_iterations"),
        resolution: json_f32_field(payload, "resolution").map(|value| value as f64),
        projection,
    }
}

pub(crate) fn graph_communities_kind(algorithm: RuntimeGraphCommunityAlgorithm) -> String {
    format!(
        "graph.community.{}",
        graph_community_algorithm_to_str(algorithm)
    )
}

pub(crate) fn graph_communities_metadata(
    input: &GraphCommunitiesInput,
) -> BTreeMap<String, String> {
    analytics_metadata(vec![("min_size", input.min_size.to_string())])
}

pub(crate) fn parse_graph_clustering_input(
    payload: &JsonValue,
    projection: Option<RuntimeGraphProjection>,
) -> GraphClusteringInput {
    GraphClusteringInput {
        top_k: json_usize_field(payload, "top_k").unwrap_or(25).max(1),
        include_triangles: json_bool_field(payload, "include_triangles").unwrap_or(false),
        projection,
    }
}

pub(crate) fn graph_clustering_metadata(input: &GraphClusteringInput) -> BTreeMap<String, String> {
    analytics_metadata(vec![
        ("top_k", input.top_k.to_string()),
        (
            "include_triangles",
            if input.include_triangles { "true" } else { "false" }.to_string(),
        ),
    ])
}

pub(crate) fn parse_graph_personalized_pagerank_input(
    payload: &JsonValue,
    projection: Option<RuntimeGraphProjection>,
) -> RedDBResult<GraphPersonalizedPageRankInput> {
    let Some(seeds) = json_string_list_field(payload, "seeds") else {
        return Err(RedDBError::Query(
            "field 'seeds' must be a non-empty array of strings".to_string(),
        ));
    };

    Ok(GraphPersonalizedPageRankInput {
        seeds,
        top_k: json_usize_field(payload, "top_k").unwrap_or(25).max(1),
        alpha: json_f32_field(payload, "alpha").map(|value| value as f64),
        epsilon: json_f32_field(payload, "epsilon").map(|value| value as f64),
        max_iterations: json_usize_field(payload, "max_iterations"),
        projection,
    })
}

pub(crate) fn graph_personalized_pagerank_metadata(
    input: &GraphPersonalizedPageRankInput,
) -> BTreeMap<String, String> {
    analytics_metadata(vec![("top_k", input.top_k.to_string())])
}

pub(crate) fn parse_graph_hits_input(
    payload: &JsonValue,
    projection: Option<RuntimeGraphProjection>,
) -> GraphHitsInput {
    GraphHitsInput {
        top_k: json_usize_field(payload, "top_k").unwrap_or(25).max(1),
        epsilon: json_f32_field(payload, "epsilon").map(|value| value as f64),
        max_iterations: json_usize_field(payload, "max_iterations"),
        projection,
    }
}

pub(crate) fn graph_hits_metadata(input: &GraphHitsInput) -> BTreeMap<String, String> {
    analytics_metadata(vec![("top_k", input.top_k.to_string())])
}

pub(crate) fn parse_graph_cycles_input(
    payload: &JsonValue,
    projection: Option<RuntimeGraphProjection>,
) -> GraphCyclesInput {
    GraphCyclesInput {
        max_length: json_usize_field(payload, "max_length").unwrap_or(10).max(2),
        max_cycles: json_usize_field(payload, "max_cycles").unwrap_or(100).max(1),
        projection,
    }
}

pub(crate) fn graph_cycles_metadata(input: &GraphCyclesInput) -> BTreeMap<String, String> {
    analytics_metadata(vec![
        ("max_length", input.max_length.to_string()),
        ("max_cycles", input.max_cycles.to_string()),
    ])
}

pub(crate) fn parse_graph_topological_sort_input(
    projection: Option<RuntimeGraphProjection>,
) -> GraphTopologicalSortInput {
    GraphTopologicalSortInput { projection }
}

pub(crate) fn parse_graph_direction(value: Option<&str>) -> Option<RuntimeGraphDirection> {
    match value.map(normalize_graph_token).as_deref() {
        Some("outgoing") | Some("out") => Some(RuntimeGraphDirection::Outgoing),
        Some("incoming") | Some("in") => Some(RuntimeGraphDirection::Incoming),
        Some("both") | Some("any") => Some(RuntimeGraphDirection::Both),
        Some(_) => None,
        None => None,
    }
}

pub(crate) fn parse_graph_traversal_strategy(
    value: Option<&str>,
) -> Option<RuntimeGraphTraversalStrategy> {
    match value.map(normalize_graph_token).as_deref() {
        Some("bfs") | Some("breadthfirst") => Some(RuntimeGraphTraversalStrategy::Bfs),
        Some("dfs") | Some("depthfirst") => Some(RuntimeGraphTraversalStrategy::Dfs),
        Some(_) => None,
        None => None,
    }
}

pub(crate) fn parse_graph_path_algorithm(value: Option<&str>) -> Option<RuntimeGraphPathAlgorithm> {
    match value.map(normalize_graph_token).as_deref() {
        Some("bfs") => Some(RuntimeGraphPathAlgorithm::Bfs),
        Some("dijkstra") | Some("weighted") => Some(RuntimeGraphPathAlgorithm::Dijkstra),
        Some(_) => None,
        None => None,
    }
}

pub(crate) fn parse_graph_components_mode(value: Option<&str>) -> Option<RuntimeGraphComponentsMode> {
    match value.map(normalize_graph_token).as_deref() {
        Some("connected") | Some("undirected") => Some(RuntimeGraphComponentsMode::Connected),
        Some("weak") | Some("wcc") => Some(RuntimeGraphComponentsMode::Weak),
        Some("strong") | Some("scc") => Some(RuntimeGraphComponentsMode::Strong),
        Some(_) => None,
        None => None,
    }
}

pub(crate) fn parse_graph_centrality_algorithm(
    value: Option<&str>,
) -> Option<RuntimeGraphCentralityAlgorithm> {
    match value.map(normalize_graph_token).as_deref() {
        Some("degree") => Some(RuntimeGraphCentralityAlgorithm::Degree),
        Some("closeness") => Some(RuntimeGraphCentralityAlgorithm::Closeness),
        Some("betweenness") => Some(RuntimeGraphCentralityAlgorithm::Betweenness),
        Some("eigenvector") => Some(RuntimeGraphCentralityAlgorithm::Eigenvector),
        Some("pagerank") | Some("page_rank") => Some(RuntimeGraphCentralityAlgorithm::PageRank),
        Some(_) => None,
        None => None,
    }
}

pub(crate) fn parse_graph_community_algorithm(
    value: Option<&str>,
) -> Option<RuntimeGraphCommunityAlgorithm> {
    match value.map(normalize_graph_token).as_deref() {
        Some("labelpropagation") | Some("label") => {
            Some(RuntimeGraphCommunityAlgorithm::LabelPropagation)
        }
        Some("louvain") => Some(RuntimeGraphCommunityAlgorithm::Louvain),
        Some(_) => None,
        None => None,
    }
}

pub(crate) fn graph_direction_to_str(value: RuntimeGraphDirection) -> &'static str {
    match value {
        RuntimeGraphDirection::Outgoing => "outgoing",
        RuntimeGraphDirection::Incoming => "incoming",
        RuntimeGraphDirection::Both => "both",
    }
}

pub(crate) fn graph_traversal_strategy_to_str(
    value: RuntimeGraphTraversalStrategy,
) -> &'static str {
    match value {
        RuntimeGraphTraversalStrategy::Bfs => "bfs",
        RuntimeGraphTraversalStrategy::Dfs => "dfs",
    }
}

pub(crate) fn graph_path_algorithm_to_str(value: RuntimeGraphPathAlgorithm) -> &'static str {
    match value {
        RuntimeGraphPathAlgorithm::Bfs => "bfs",
        RuntimeGraphPathAlgorithm::Dijkstra => "dijkstra",
    }
}

pub(crate) fn graph_components_mode_to_str(value: RuntimeGraphComponentsMode) -> &'static str {
    match value {
        RuntimeGraphComponentsMode::Connected => "connected",
        RuntimeGraphComponentsMode::Weak => "weak",
        RuntimeGraphComponentsMode::Strong => "strong",
    }
}

pub(crate) fn graph_centrality_algorithm_to_str(
    value: RuntimeGraphCentralityAlgorithm,
) -> &'static str {
    match value {
        RuntimeGraphCentralityAlgorithm::Degree => "degree",
        RuntimeGraphCentralityAlgorithm::Closeness => "closeness",
        RuntimeGraphCentralityAlgorithm::Betweenness => "betweenness",
        RuntimeGraphCentralityAlgorithm::Eigenvector => "eigenvector",
        RuntimeGraphCentralityAlgorithm::PageRank => "pagerank",
    }
}

pub(crate) fn graph_community_algorithm_to_str(
    value: RuntimeGraphCommunityAlgorithm,
) -> &'static str {
    match value {
        RuntimeGraphCommunityAlgorithm::LabelPropagation => "label_propagation",
        RuntimeGraphCommunityAlgorithm::Louvain => "louvain",
    }
}

pub(crate) fn analytics_metadata(entries: Vec<(&str, String)>) -> BTreeMap<String, String> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

fn parse_required_string_field(payload: &JsonValue, field: &str) -> RedDBResult<String> {
    let Some(value) = payload.get(field).and_then(JsonValue::as_str) else {
        return Err(RedDBError::Query(format!("field '{field}' must be a string")));
    };
    Ok(value.to_string())
}

fn normalize_graph_token(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(|character| character.to_lowercase())
        .collect()
}
