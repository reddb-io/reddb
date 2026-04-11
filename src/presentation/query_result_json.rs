use crate::json::{Map, Value as JsonValue};
use crate::runtime::{RuntimeQueryResult, RuntimeStats};
use crate::storage::query::modes::QueryMode;
use crate::storage::query::unified::{
    GraphPath, MatchedEdge, MatchedNode, QueryStats, UnifiedRecord, UnifiedResult,
    VectorSearchResult,
};
use crate::storage::query::{is_universal_entity_source as is_universal_query_source, QueryExpr};

pub(crate) fn query_mode_name(mode: QueryMode) -> &'static str {
    match mode {
        QueryMode::Sql => "sql",
        QueryMode::Gremlin => "gremlin",
        QueryMode::Cypher => "cypher",
        QueryMode::Sparql => "sparql",
        QueryMode::Path => "path",
        QueryMode::Natural => "natural",
        QueryMode::Unknown => "unknown",
    }
}

pub(crate) fn query_mode_capability(mode: QueryMode) -> &'static str {
    match mode {
        QueryMode::Sql => "table",
        QueryMode::Gremlin | QueryMode::Cypher | QueryMode::Sparql | QueryMode::Path => "graph",
        QueryMode::Natural => "multi",
        QueryMode::Unknown => "unknown",
    }
}

pub(crate) fn runtime_query_json(
    result: &RuntimeQueryResult,
    entity_types: &Option<Vec<String>>,
    capabilities: &Option<Vec<String>>,
) -> JsonValue {
    let records = crate::presentation::query_view::filter_query_records(
        &result.result.records,
        entity_types,
        capabilities,
    );

    let mut object = Map::new();
    object.insert("ok".to_string(), JsonValue::Bool(true));
    object.insert("query".to_string(), JsonValue::String(result.query.clone()));
    object.insert(
        "mode".to_string(),
        JsonValue::String(query_mode_name(result.mode).to_string()),
    );
    object.insert(
        "capability".to_string(),
        JsonValue::String(query_mode_capability_from_runtime_result(result).to_string()),
    );
    object.insert(
        "statement".to_string(),
        JsonValue::String(result.statement.to_string()),
    );
    object.insert(
        "engine".to_string(),
        JsonValue::String(result.engine.to_string()),
    );
    object.insert(
        "record_count".to_string(),
        JsonValue::Number(records.len() as f64),
    );
    if result.affected_rows > 0 {
        object.insert(
            "affected_rows".to_string(),
            JsonValue::Number(result.affected_rows as f64),
        );
    }
    if result.statement_type != "select" {
        object.insert(
            "statement_type".to_string(),
            JsonValue::String(result.statement_type.to_string()),
        );
    }
    object.insert(
        "result".to_string(),
        unified_result_json_with_records(&result.result, &records),
    );
    object.insert(
        "selection".to_string(),
        crate::presentation::query_view::search_selection_json(entity_types, capabilities),
    );
    JsonValue::Object(object)
}

pub(crate) fn unified_result_json_with_records(
    result: &UnifiedResult,
    records: &[UnifiedRecord],
) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "columns".to_string(),
        JsonValue::Array(
            result
                .columns
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "records".to_string(),
        JsonValue::Array(records.iter().map(unified_record_json).collect()),
    );
    object.insert("stats".to_string(), query_stats_json(&result.stats));
    JsonValue::Object(object)
}

pub(crate) fn query_stats_json(stats: &QueryStats) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "nodes_scanned".to_string(),
        JsonValue::Number(stats.nodes_scanned as f64),
    );
    object.insert(
        "edges_scanned".to_string(),
        JsonValue::Number(stats.edges_scanned as f64),
    );
    object.insert(
        "rows_scanned".to_string(),
        JsonValue::Number(stats.rows_scanned as f64),
    );
    object.insert(
        "exec_time_us".to_string(),
        JsonValue::Number(stats.exec_time_us as f64),
    );
    JsonValue::Object(object)
}

pub(crate) fn runtime_stats_json(stats: &RuntimeStats) -> JsonValue {
    let mut store = Map::new();
    store.insert(
        "collection_count".to_string(),
        JsonValue::Number(stats.store.collection_count as f64),
    );
    store.insert(
        "total_entities".to_string(),
        JsonValue::Number(stats.store.total_entities as f64),
    );
    store.insert(
        "total_memory_bytes".to_string(),
        JsonValue::Number(stats.store.total_memory_bytes as f64),
    );
    store.insert(
        "cross_ref_count".to_string(),
        JsonValue::Number(stats.store.cross_ref_count as f64),
    );

    let mut object = Map::new();
    object.insert(
        "active_connections".to_string(),
        JsonValue::Number(stats.active_connections as f64),
    );
    object.insert(
        "idle_connections".to_string(),
        JsonValue::Number(stats.idle_connections as f64),
    );
    object.insert(
        "total_checkouts".to_string(),
        JsonValue::Number(stats.total_checkouts as f64),
    );
    object.insert("paged_mode".to_string(), JsonValue::Bool(stats.paged_mode));
    object.insert(
        "started_at_unix_ms".to_string(),
        JsonValue::Number(stats.started_at_unix_ms as f64),
    );
    object.insert("store".to_string(), JsonValue::Object(store));

    let mut system = Map::new();
    system.insert(
        "pid".to_string(),
        JsonValue::Number(stats.system.pid as f64),
    );
    system.insert(
        "cpu_cores".to_string(),
        JsonValue::Number(stats.system.cpu_cores as f64),
    );
    system.insert(
        "total_memory_bytes".to_string(),
        JsonValue::Number(stats.system.total_memory_bytes as f64),
    );
    system.insert(
        "available_memory_bytes".to_string(),
        JsonValue::Number(stats.system.available_memory_bytes as f64),
    );
    system.insert("os".to_string(), JsonValue::String(stats.system.os.clone()));
    system.insert(
        "arch".to_string(),
        JsonValue::String(stats.system.arch.clone()),
    );
    system.insert(
        "hostname".to_string(),
        JsonValue::String(stats.system.hostname.clone()),
    );
    object.insert("system".to_string(), JsonValue::Object(system));

    JsonValue::Object(object)
}

fn query_mode_capability_from_runtime_result(result: &RuntimeQueryResult) -> &'static str {
    match result.mode {
        QueryMode::Sql => {
            if is_any_table_query(&result.query) {
                "multi"
            } else {
                query_mode_capability(result.mode)
            }
        }
        mode => query_mode_capability(mode),
    }
}

fn is_any_table_query(query: &str) -> bool {
    let Ok(expr) = crate::storage::query::modes::parse_multi(query) else {
        return false;
    };

    match expr {
        QueryExpr::Table(table) => is_universal_table_source(&table.table),
        QueryExpr::Join(_) => true,
        _ => false,
    }
}

fn is_universal_table_source(table: &str) -> bool {
    is_universal_query_source(table)
}

fn unified_record_json(record: &UnifiedRecord) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "values".to_string(),
        JsonValue::Object(
            record
                .values
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        crate::presentation::entity_json::storage_value_to_json(value),
                    )
                })
                .collect(),
        ),
    );
    object.insert(
        "nodes".to_string(),
        JsonValue::Object(
            record
                .nodes
                .iter()
                .map(|(key, value)| (key.clone(), matched_node_json(value)))
                .collect(),
        ),
    );
    object.insert(
        "edges".to_string(),
        JsonValue::Object(
            record
                .edges
                .iter()
                .map(|(key, value)| (key.clone(), matched_edge_json(value)))
                .collect(),
        ),
    );
    object.insert(
        "paths".to_string(),
        JsonValue::Array(record.paths.iter().map(graph_path_json).collect()),
    );
    object.insert(
        "vector_results".to_string(),
        JsonValue::Array(
            record
                .vector_results
                .iter()
                .map(vector_search_result_json)
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

fn matched_node_json(node: &MatchedNode) -> JsonValue {
    let mut object = Map::new();
    object.insert("id".to_string(), JsonValue::String(node.id.clone()));
    object.insert("label".to_string(), JsonValue::String(node.label.clone()));
    object.insert(
        "node_type".to_string(),
        JsonValue::String(node.node_type.as_str().to_string()),
    );
    JsonValue::Object(object)
}

fn matched_edge_json(edge: &MatchedEdge) -> JsonValue {
    let mut object = Map::new();
    object.insert("from".to_string(), JsonValue::String(edge.from.clone()));
    object.insert("to".to_string(), JsonValue::String(edge.to.clone()));
    object.insert(
        "edge_type".to_string(),
        JsonValue::String(edge.edge_type.as_str().to_string()),
    );
    object.insert("weight".to_string(), JsonValue::Number(edge.weight as f64));
    JsonValue::Object(object)
}

fn graph_path_json(path: &GraphPath) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "nodes".to_string(),
        JsonValue::Array(path.nodes.iter().cloned().map(JsonValue::String).collect()),
    );
    object.insert(
        "edges".to_string(),
        JsonValue::Array(path.edges.iter().map(matched_edge_json).collect()),
    );
    object.insert(
        "total_weight".to_string(),
        JsonValue::Number(path.total_weight as f64),
    );
    JsonValue::Object(object)
}

fn vector_search_result_json(result: &VectorSearchResult) -> JsonValue {
    let score = 1.0 / (1.0 + result.distance);
    let mut object = Map::new();
    object.insert("id".to_string(), JsonValue::Number(result.id as f64));
    object.insert("entity_id".to_string(), JsonValue::Number(result.id as f64));
    object.insert(
        "_entity_id".to_string(),
        JsonValue::Number(result.id as f64),
    );
    object.insert(
        "collection".to_string(),
        JsonValue::String(result.collection.clone()),
    );
    object.insert(
        "_collection".to_string(),
        JsonValue::String(result.collection.clone()),
    );
    object.insert("_kind".to_string(), JsonValue::String("vector".to_string()));
    object.insert(
        "_entity_type".to_string(),
        JsonValue::String("vector".to_string()),
    );
    object.insert(
        "_capabilities".to_string(),
        JsonValue::String("vector,similarity,embedding".to_string()),
    );
    object.insert("_score".to_string(), JsonValue::Number(score as f64));
    object.insert("final_score".to_string(), JsonValue::Number(score as f64));
    object.insert(
        "distance".to_string(),
        JsonValue::Number(result.distance as f64),
    );
    object.insert(
        "_distance".to_string(),
        JsonValue::Number(result.distance as f64),
    );
    object.insert(
        "vector_distance".to_string(),
        JsonValue::Number(result.distance as f64),
    );
    object.insert(
        "vector".to_string(),
        match &result.vector {
            Some(vector) => JsonValue::Array(
                vector
                    .iter()
                    .map(|value| JsonValue::Number(*value as f64))
                    .collect(),
            ),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "metadata".to_string(),
        match &result.metadata {
            Some(metadata) => JsonValue::Object(
                metadata
                    .iter()
                    .map(|(key, value)| {
                        (
                            key.clone(),
                            crate::presentation::entity_json::storage_value_to_json(value),
                        )
                    })
                    .collect(),
            ),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "linked_node".to_string(),
        match &result.linked_node {
            Some(value) => JsonValue::String(value.clone()),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "linked_row".to_string(),
        match &result.linked_row {
            Some((table, row_id)) => {
                let mut linked = Map::new();
                linked.insert("table".to_string(), JsonValue::String(table.clone()));
                linked.insert("row_id".to_string(), JsonValue::Number(*row_id as f64));
                JsonValue::Object(linked)
            }
            None => JsonValue::Null,
        },
    );
    JsonValue::Object(object)
}
