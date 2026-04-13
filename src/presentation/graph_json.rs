use crate::application::graph_payload::{
    graph_centrality_algorithm_to_str, graph_community_algorithm_to_str,
    graph_components_mode_to_str, graph_direction_to_str, graph_path_algorithm_to_str,
    graph_traversal_strategy_to_str,
};
use crate::json::{Map, Value as JsonValue};
use crate::runtime::{
    RuntimeGraphCentralityResult, RuntimeGraphClusteringResult, RuntimeGraphCommunityResult,
    RuntimeGraphComponentsResult, RuntimeGraphCyclesResult, RuntimeGraphEdge,
    RuntimeGraphHitsResult, RuntimeGraphNeighborhoodResult, RuntimeGraphNode, RuntimeGraphPath,
    RuntimeGraphPathResult, RuntimeGraphPropertiesResult, RuntimeGraphTopologicalSortResult,
    RuntimeGraphTraversalResult, RuntimeGraphVisit,
};

pub(crate) fn graph_neighborhood_json(result: &RuntimeGraphNeighborhoodResult) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "source".to_string(),
        JsonValue::String(result.source.clone()),
    );
    object.insert(
        "direction".to_string(),
        JsonValue::String(graph_direction_to_str(result.direction).to_string()),
    );
    object.insert(
        "max_depth".to_string(),
        JsonValue::Number(result.max_depth as f64),
    );
    object.insert(
        "nodes".to_string(),
        JsonValue::Array(result.nodes.iter().map(graph_visit_json).collect()),
    );
    object.insert(
        "edges".to_string(),
        JsonValue::Array(result.edges.iter().map(graph_edge_json).collect()),
    );
    JsonValue::Object(object)
}

pub(crate) fn graph_traversal_json(result: &RuntimeGraphTraversalResult) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "source".to_string(),
        JsonValue::String(result.source.clone()),
    );
    object.insert(
        "direction".to_string(),
        JsonValue::String(graph_direction_to_str(result.direction).to_string()),
    );
    object.insert(
        "strategy".to_string(),
        JsonValue::String(graph_traversal_strategy_to_str(result.strategy).to_string()),
    );
    object.insert(
        "max_depth".to_string(),
        JsonValue::Number(result.max_depth as f64),
    );
    object.insert(
        "visits".to_string(),
        JsonValue::Array(result.visits.iter().map(graph_visit_json).collect()),
    );
    object.insert(
        "edges".to_string(),
        JsonValue::Array(result.edges.iter().map(graph_edge_json).collect()),
    );
    JsonValue::Object(object)
}

pub(crate) fn graph_path_result_json(result: &RuntimeGraphPathResult) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "source".to_string(),
        JsonValue::String(result.source.clone()),
    );
    object.insert(
        "target".to_string(),
        JsonValue::String(result.target.clone()),
    );
    object.insert(
        "direction".to_string(),
        JsonValue::String(graph_direction_to_str(result.direction).to_string()),
    );
    object.insert(
        "algorithm".to_string(),
        JsonValue::String(graph_path_algorithm_to_str(result.algorithm).to_string()),
    );
    object.insert(
        "nodes_visited".to_string(),
        JsonValue::Number(result.nodes_visited as f64),
    );
    object.insert(
        "negative_cycle_detected".to_string(),
        result
            .negative_cycle_detected
            .map(JsonValue::Bool)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "path".to_string(),
        match &result.path {
            Some(path) => graph_path_json(path),
            None => JsonValue::Null,
        },
    );
    JsonValue::Object(object)
}

pub(crate) fn graph_components_json(result: &RuntimeGraphComponentsResult) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "mode".to_string(),
        JsonValue::String(graph_components_mode_to_str(result.mode).to_string()),
    );
    object.insert("count".to_string(), JsonValue::Number(result.count as f64));
    object.insert(
        "components".to_string(),
        JsonValue::Array(
            result
                .components
                .iter()
                .map(|component| {
                    let mut item = Map::new();
                    item.insert("id".to_string(), JsonValue::String(component.id.clone()));
                    item.insert("size".to_string(), JsonValue::Number(component.size as f64));
                    item.insert(
                        "nodes".to_string(),
                        JsonValue::Array(
                            component
                                .nodes
                                .iter()
                                .cloned()
                                .map(JsonValue::String)
                                .collect(),
                        ),
                    );
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

pub(crate) fn graph_centrality_json(result: &RuntimeGraphCentralityResult) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "algorithm".to_string(),
        JsonValue::String(graph_centrality_algorithm_to_str(result.algorithm).to_string()),
    );
    object.insert(
        "normalized".to_string(),
        result
            .normalized
            .map(JsonValue::Bool)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "iterations".to_string(),
        result
            .iterations
            .map(|value| JsonValue::Number(value as f64))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "converged".to_string(),
        result
            .converged
            .map(JsonValue::Bool)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "scores".to_string(),
        JsonValue::Array(
            result
                .scores
                .iter()
                .map(|score| {
                    let mut item = Map::new();
                    item.insert("node".to_string(), graph_node_json(&score.node));
                    item.insert("score".to_string(), JsonValue::Number(score.score));
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    object.insert(
        "degree_scores".to_string(),
        JsonValue::Array(
            result
                .degree_scores
                .iter()
                .map(|score| {
                    let mut item = Map::new();
                    item.insert("node".to_string(), graph_node_json(&score.node));
                    item.insert(
                        "in_degree".to_string(),
                        JsonValue::Number(score.in_degree as f64),
                    );
                    item.insert(
                        "out_degree".to_string(),
                        JsonValue::Number(score.out_degree as f64),
                    );
                    item.insert(
                        "total_degree".to_string(),
                        JsonValue::Number(score.total_degree as f64),
                    );
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

pub(crate) fn graph_community_json(result: &RuntimeGraphCommunityResult) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "algorithm".to_string(),
        JsonValue::String(graph_community_algorithm_to_str(result.algorithm).to_string()),
    );
    object.insert("count".to_string(), JsonValue::Number(result.count as f64));
    object.insert(
        "iterations".to_string(),
        result
            .iterations
            .map(|value| JsonValue::Number(value as f64))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "converged".to_string(),
        result
            .converged
            .map(JsonValue::Bool)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "modularity".to_string(),
        result
            .modularity
            .map(JsonValue::Number)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "passes".to_string(),
        result
            .passes
            .map(|value| JsonValue::Number(value as f64))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "communities".to_string(),
        JsonValue::Array(
            result
                .communities
                .iter()
                .map(|community| {
                    let mut item = Map::new();
                    item.insert("id".to_string(), JsonValue::String(community.id.clone()));
                    item.insert("size".to_string(), JsonValue::Number(community.size as f64));
                    item.insert(
                        "nodes".to_string(),
                        JsonValue::Array(
                            community
                                .nodes
                                .iter()
                                .cloned()
                                .map(JsonValue::String)
                                .collect(),
                        ),
                    );
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

pub(crate) fn graph_clustering_json(result: &RuntimeGraphClusteringResult) -> JsonValue {
    let mut object = Map::new();
    object.insert("global".to_string(), JsonValue::Number(result.global));
    object.insert(
        "local".to_string(),
        JsonValue::Array(
            result
                .local
                .iter()
                .map(|score| {
                    let mut item = Map::new();
                    item.insert("node".to_string(), graph_node_json(&score.node));
                    item.insert("score".to_string(), JsonValue::Number(score.score));
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    object.insert(
        "triangle_count".to_string(),
        result
            .triangle_count
            .map(|value| JsonValue::Number(value as f64))
            .unwrap_or(JsonValue::Null),
    );
    JsonValue::Object(object)
}

pub(crate) fn graph_hits_json(result: &RuntimeGraphHitsResult) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "iterations".to_string(),
        JsonValue::Number(result.iterations as f64),
    );
    object.insert("converged".to_string(), JsonValue::Bool(result.converged));
    object.insert(
        "hubs".to_string(),
        JsonValue::Array(
            result
                .hubs
                .iter()
                .map(|score| {
                    let mut item = Map::new();
                    item.insert("node".to_string(), graph_node_json(&score.node));
                    item.insert("score".to_string(), JsonValue::Number(score.score));
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    object.insert(
        "authorities".to_string(),
        JsonValue::Array(
            result
                .authorities
                .iter()
                .map(|score| {
                    let mut item = Map::new();
                    item.insert("node".to_string(), graph_node_json(&score.node));
                    item.insert("score".to_string(), JsonValue::Number(score.score));
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

pub(crate) fn graph_cycles_json(result: &RuntimeGraphCyclesResult) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "limit_reached".to_string(),
        JsonValue::Bool(result.limit_reached),
    );
    object.insert(
        "cycles".to_string(),
        JsonValue::Array(result.cycles.iter().map(graph_path_json).collect()),
    );
    JsonValue::Object(object)
}

pub(crate) fn graph_topological_sort_json(result: &RuntimeGraphTopologicalSortResult) -> JsonValue {
    let mut object = Map::new();
    object.insert("acyclic".to_string(), JsonValue::Bool(result.acyclic));
    object.insert(
        "ordered_nodes".to_string(),
        JsonValue::Array(result.ordered_nodes.iter().map(graph_node_json).collect()),
    );
    JsonValue::Object(object)
}

pub(crate) fn graph_properties_json(result: &RuntimeGraphPropertiesResult) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "node_count".to_string(),
        JsonValue::Number(result.node_count as f64),
    );
    object.insert(
        "edge_count".to_string(),
        JsonValue::Number(result.edge_count as f64),
    );
    object.insert(
        "self_loop_count".to_string(),
        JsonValue::Number(result.self_loop_count as f64),
    );
    object.insert(
        "negative_edge_count".to_string(),
        JsonValue::Number(result.negative_edge_count as f64),
    );
    object.insert(
        "connected_component_count".to_string(),
        JsonValue::Number(result.connected_component_count as f64),
    );
    object.insert(
        "weak_component_count".to_string(),
        JsonValue::Number(result.weak_component_count as f64),
    );
    object.insert(
        "strong_component_count".to_string(),
        JsonValue::Number(result.strong_component_count as f64),
    );
    object.insert("is_empty".to_string(), JsonValue::Bool(result.is_empty));
    object.insert(
        "is_connected".to_string(),
        JsonValue::Bool(result.is_connected),
    );
    object.insert(
        "is_weakly_connected".to_string(),
        JsonValue::Bool(result.is_weakly_connected),
    );
    object.insert(
        "is_strongly_connected".to_string(),
        JsonValue::Bool(result.is_strongly_connected),
    );
    object.insert(
        "is_complete".to_string(),
        JsonValue::Bool(result.is_complete),
    );
    object.insert(
        "is_complete_directed".to_string(),
        JsonValue::Bool(result.is_complete_directed),
    );
    object.insert("is_cyclic".to_string(), JsonValue::Bool(result.is_cyclic));
    object.insert(
        "is_circular".to_string(),
        JsonValue::Bool(result.is_circular),
    );
    object.insert("is_acyclic".to_string(), JsonValue::Bool(result.is_acyclic));
    object.insert("is_tree".to_string(), JsonValue::Bool(result.is_tree));
    object.insert("density".to_string(), JsonValue::Number(result.density));
    object.insert(
        "density_directed".to_string(),
        JsonValue::Number(result.density_directed),
    );
    JsonValue::Object(object)
}

fn graph_visit_json(visit: &RuntimeGraphVisit) -> JsonValue {
    let mut object = Map::new();
    object.insert("depth".to_string(), JsonValue::Number(visit.depth as f64));
    object.insert("node".to_string(), graph_node_json(&visit.node));
    JsonValue::Object(object)
}

fn graph_node_json(node: &RuntimeGraphNode) -> JsonValue {
    let mut object = Map::new();
    object.insert("id".to_string(), JsonValue::String(node.id.clone()));
    object.insert("label".to_string(), JsonValue::String(node.label.clone()));
    object.insert(
        "node_type".to_string(),
        JsonValue::String(node.node_type.clone()),
    );
    object.insert(
        "out_edge_count".to_string(),
        JsonValue::Number(node.out_edge_count as f64),
    );
    object.insert(
        "in_edge_count".to_string(),
        JsonValue::Number(node.in_edge_count as f64),
    );
    JsonValue::Object(object)
}

fn graph_edge_json(edge: &RuntimeGraphEdge) -> JsonValue {
    let mut object = Map::new();
    object.insert("source".to_string(), JsonValue::String(edge.source.clone()));
    object.insert("target".to_string(), JsonValue::String(edge.target.clone()));
    object.insert(
        "edge_type".to_string(),
        JsonValue::String(edge.edge_type.clone()),
    );
    object.insert("weight".to_string(), JsonValue::Number(edge.weight as f64));
    JsonValue::Object(object)
}

fn graph_path_json(path: &RuntimeGraphPath) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "hop_count".to_string(),
        JsonValue::Number(path.hop_count as f64),
    );
    object.insert(
        "total_weight".to_string(),
        JsonValue::Number(path.total_weight),
    );
    object.insert(
        "nodes".to_string(),
        JsonValue::Array(path.nodes.iter().map(graph_node_json).collect()),
    );
    object.insert(
        "edges".to_string(),
        JsonValue::Array(path.edges.iter().map(graph_edge_json).collect()),
    );
    JsonValue::Object(object)
}
