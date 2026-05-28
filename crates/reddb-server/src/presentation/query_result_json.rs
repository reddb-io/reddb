use crate::json::{Map, Value as JsonValue};
use crate::runtime::{RuntimeQueryResult, RuntimeStats};
use crate::storage::query::modes::QueryMode;
use crate::storage::query::unified::{
    GraphPath, MatchedEdge, MatchedNode, QueryStats, UnifiedRecord, UnifiedResult,
    VectorSearchResult,
};
use crate::storage::query::{is_universal_entity_source as is_universal_query_source, QueryExpr};
use crate::storage::schema::types::Value as StorageValue;

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
        "descriptor".to_string(),
        descriptor_json(&result.result, &records),
    );
    object.insert(
        "selection".to_string(),
        crate::presentation::query_view::search_selection_json(entity_types, capabilities),
    );
    JsonValue::Object(object)
}

/// #737 — additive query-result descriptor metadata.
///
/// Lets Red UI (and any other client) pick a stable renderer for the
/// payload without probing rows. Strictly additive — every other
/// field of the response is unchanged, so legacy clients keep working
/// unmodified.
pub(crate) fn descriptor_json(result: &UnifiedResult, records: &[UnifiedRecord]) -> JsonValue {
    let mut rows = 0usize;
    let mut nodes = 0usize;
    let mut edges = 0usize;
    let mut paths = 0usize;
    let mut vector_matches = 0usize;

    for record in records {
        if record.iter_fields().next().is_some() {
            rows += 1;
        }
        nodes += record.nodes.len();
        edges += record.edges.len();
        paths += record.paths.len();
        vector_matches += record.vector_results.len();
    }

    let has_table = rows > 0 || !result.columns.is_empty();
    let has_graph = nodes > 0 || edges > 0 || paths > 0;
    let has_vector = vector_matches > 0;
    // The query endpoint doesn't return queue / timeseries / metrics
    // result shapes today. Acceptance criterion #3 requires honest
    // false flags here rather than "unknown" — the engine *does* know
    // these shapes are absent because the result envelope has no
    // carrier for them.
    let has_queue = false;
    let has_timeseries = false;
    let has_metrics = false;

    let mut models_present: Vec<&'static str> = Vec::new();
    if has_table {
        models_present.push("table");
    }
    if has_graph {
        models_present.push("graph");
    }
    if has_vector {
        models_present.push("vector");
    }

    let result_kind = match (has_table, has_graph, has_vector) {
        (false, false, false) => "empty",
        (true, false, false) => "table",
        (false, true, false) => "graph",
        (false, false, true) => "vector",
        _ => "mixed",
    };

    // Renderer hints — first entry is the primary renderer, the
    // remainder are fallback renderers the UI can offer as tabs.
    let renderer_hints: Vec<&'static str> = match result_kind {
        "empty" => vec!["empty"],
        "table" => vec!["table"],
        "graph" => vec!["graph"],
        "vector" => vec!["vector"],
        _ => {
            let mut hints = Vec::new();
            if has_graph {
                hints.push("graph");
            }
            if has_vector {
                hints.push("vector");
            }
            if has_table {
                hints.push("table");
            }
            hints
        }
    };

    let mut counts = Map::new();
    counts.insert("rows".to_string(), JsonValue::Number(rows as f64));
    counts.insert("nodes".to_string(), JsonValue::Number(nodes as f64));
    counts.insert("edges".to_string(), JsonValue::Number(edges as f64));
    counts.insert("paths".to_string(), JsonValue::Number(paths as f64));
    counts.insert(
        "vector_matches".to_string(),
        JsonValue::Number(vector_matches as f64),
    );

    let mut object = Map::new();
    object.insert(
        "result_kind".to_string(),
        JsonValue::String(result_kind.to_string()),
    );
    object.insert(
        "models_present".to_string(),
        JsonValue::Array(
            models_present
                .iter()
                .map(|name| JsonValue::String((*name).to_string()))
                .collect(),
        ),
    );
    object.insert(
        "renderer_hints".to_string(),
        JsonValue::Array(
            renderer_hints
                .iter()
                .map(|name| JsonValue::String((*name).to_string()))
                .collect(),
        ),
    );
    object.insert("counts_by_kind".to_string(), JsonValue::Object(counts));
    object.insert("has_table".to_string(), JsonValue::Bool(has_table));
    object.insert("has_graph".to_string(), JsonValue::Bool(has_graph));
    object.insert("has_vector".to_string(), JsonValue::Bool(has_vector));
    object.insert("has_queue".to_string(), JsonValue::Bool(has_queue));
    object.insert(
        "has_timeseries".to_string(),
        JsonValue::Bool(has_timeseries),
    );
    object.insert("has_metrics".to_string(), JsonValue::Bool(has_metrics));
    object.insert(
        "columns".to_string(),
        JsonValue::Array(descriptor_columns_json(&result.columns, records)),
    );

    JsonValue::Object(object)
}

/// Issue #805 / #750 — the descriptor frame emitted FIRST on the
/// `/query/stream` transport. Builds on the additive #737
/// [`descriptor_json`] (so the column/type/renderer metadata is
/// identical to the non-streaming `/query` response a UI already
/// knows) and adds a `schema_fingerprint`: a stable digest over the
/// ordered `(name,type)` column pairs. The fingerprint lets a client
/// detect when a resumed or re-issued stream's column shape diverges
/// from what it initialised its renderer against, without diffing the
/// whole column list. Computed from the already-inferred descriptor
/// columns so it never re-scans records.
pub(crate) fn stream_query_descriptor_json(
    result: &UnifiedResult,
    records: &[UnifiedRecord],
) -> JsonValue {
    let mut descriptor = descriptor_json(result, records);
    if let JsonValue::Object(map) = &mut descriptor {
        let fingerprint = schema_fingerprint(map.get("columns"));
        map.insert(
            "schema_fingerprint".to_string(),
            JsonValue::String(fingerprint),
        );
    }
    descriptor
}

/// Stable digest over the descriptor's ordered `(name,type)` column
/// pairs. Order-sensitive on purpose — two queries projecting the same
/// columns in a different order are different shapes for a column-bound
/// renderer. Returns a 16-byte (32 hex char) SHA-256 prefix.
fn schema_fingerprint(columns: Option<&JsonValue>) -> String {
    let mut material = String::new();
    if let Some(JsonValue::Array(entries)) = columns {
        for entry in entries {
            let name = entry.get("name").and_then(JsonValue::as_str).unwrap_or("");
            let ty = entry.get("type").and_then(JsonValue::as_str).unwrap_or("");
            material.push_str(name);
            material.push('\u{1f}');
            material.push_str(ty);
            material.push('\u{1e}');
        }
    }
    let digest = crate::crypto::sha256::sha256(material.as_bytes());
    crate::utils::to_hex_prefix(&digest, 16)
}

fn descriptor_columns_json(columns: &[String], records: &[UnifiedRecord]) -> Vec<JsonValue> {
    columns
        .iter()
        .map(|name| {
            let mut entry = Map::new();
            entry.insert("name".to_string(), JsonValue::String(name.clone()));
            let (ty, nullable) = infer_column_type(name, records);
            entry.insert("type".to_string(), JsonValue::String(ty.to_string()));
            entry.insert("nullable".to_string(), JsonValue::Bool(nullable));
            JsonValue::Object(entry)
        })
        .collect()
}

/// First non-null value of `column` determines its descriptor type.
/// `nullable=true` when at least one record has the column missing or
/// `Value::Null`. If no record ever carries a non-null value for the
/// column we return `("unknown", true)` — acceptance criterion #3
/// requires a safe unknown rather than a guess.
fn infer_column_type(column: &str, records: &[UnifiedRecord]) -> (&'static str, bool) {
    let mut concrete: Option<&'static str> = None;
    let mut nullable = false;
    for record in records {
        match record.get(column) {
            None => nullable = true,
            Some(StorageValue::Null) => nullable = true,
            Some(value) => {
                if concrete.is_none() {
                    concrete = Some(coarse_type_for(value));
                }
            }
        }
    }
    match concrete {
        Some(ty) => (ty, nullable),
        None => ("unknown", true),
    }
}

/// Map a storage `Value` variant to a coarse JSON-renderer-friendly
/// type tag. Stays intentionally small — UI rendering cares about
/// "is this a number / string / object / vector / reference", not
/// every internal subtype.
fn coarse_type_for(value: &StorageValue) -> &'static str {
    match value {
        StorageValue::Null => "null",
        StorageValue::Boolean(_) => "boolean",
        StorageValue::Integer(_)
        | StorageValue::UnsignedInteger(_)
        | StorageValue::Float(_)
        | StorageValue::Decimal(_)
        | StorageValue::BigInt(_)
        | StorageValue::Port(_)
        | StorageValue::Latitude(_)
        | StorageValue::Longitude(_)
        | StorageValue::EnumValue(_) => "number",
        StorageValue::Timestamp(_) | StorageValue::TimestampMs(_) => "timestamp",
        StorageValue::Duration(_) => "duration",
        StorageValue::Date(_) | StorageValue::Time(_) => "string",
        StorageValue::Text(_)
        | StorageValue::Email(_)
        | StorageValue::Url(_)
        | StorageValue::Password(_)
        | StorageValue::AssetCode(_) => "string",
        StorageValue::Uuid(_)
        | StorageValue::IpAddr(_)
        | StorageValue::Ipv4(_)
        | StorageValue::Ipv6(_)
        | StorageValue::MacAddr(_)
        | StorageValue::Cidr(_, _)
        | StorageValue::Subnet(_, _)
        | StorageValue::Country2(_)
        | StorageValue::Country3(_)
        | StorageValue::Lang2(_)
        | StorageValue::Lang5(_)
        | StorageValue::Currency(_)
        | StorageValue::Color(_)
        | StorageValue::ColorAlpha(_)
        | StorageValue::Phone(_)
        | StorageValue::Semver(_) => "string",
        StorageValue::Blob(_) | StorageValue::Secret(_) => "binary",
        StorageValue::Array(_) => "array",
        StorageValue::Json(_) | StorageValue::Money { .. } => "object",
        StorageValue::Vector(_) => "vector",
        StorageValue::NodeRef(_)
        | StorageValue::EdgeRef(_)
        | StorageValue::VectorRef(_, _)
        | StorageValue::RowRef(_, _)
        | StorageValue::KeyRef(_, _)
        | StorageValue::DocRef(_, _)
        | StorageValue::TableRef(_)
        | StorageValue::PageRef(_) => "reference",
        StorageValue::GeoPoint(_, _) => "object",
    }
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
        JsonValue::Array(
            records
                .iter()
                .map(|record| unified_record_json(record, &result.columns))
                .collect(),
        ),
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

    let blob = stats.result_blob_cache;
    let mut result_blob_cache = Map::new();
    result_blob_cache.insert("hits".to_string(), JsonValue::Number(blob.hits() as f64));
    result_blob_cache.insert(
        "misses".to_string(),
        JsonValue::Number(blob.misses() as f64),
    );
    result_blob_cache.insert(
        "expirations".to_string(),
        JsonValue::Number(blob.expirations() as f64),
    );
    result_blob_cache.insert(
        "evictions".to_string(),
        JsonValue::Number(blob.evictions() as f64),
    );
    result_blob_cache.insert(
        "invalidations".to_string(),
        JsonValue::Number(blob.invalidations() as f64),
    );
    result_blob_cache.insert(
        "entries".to_string(),
        JsonValue::Number(blob.entries() as f64),
    );
    result_blob_cache.insert(
        "memory_bytes".to_string(),
        JsonValue::Number(blob.bytes_in_use() as f64),
    );
    result_blob_cache.insert(
        "l2_memory_bytes".to_string(),
        JsonValue::Number(blob.l2_bytes_in_use() as f64),
    );
    result_blob_cache.insert(
        "l2_full_rejections".to_string(),
        JsonValue::Number(blob.l2_full_rejections() as f64),
    );
    object.insert(
        "result_blob_cache".to_string(),
        JsonValue::Object(result_blob_cache),
    );

    let kv = stats.kv;
    let mut kv_object = Map::new();
    kv_object.insert("puts".to_string(), JsonValue::Number(kv.puts as f64));
    kv_object.insert("gets".to_string(), JsonValue::Number(kv.gets as f64));
    kv_object.insert("deletes".to_string(), JsonValue::Number(kv.deletes as f64));
    kv_object.insert("incrs".to_string(), JsonValue::Number(kv.incrs as f64));
    kv_object.insert(
        "cas_success".to_string(),
        JsonValue::Number(kv.cas_success as f64),
    );
    kv_object.insert(
        "cas_conflict".to_string(),
        JsonValue::Number(kv.cas_conflict as f64),
    );
    kv_object.insert(
        "watch_streams_active".to_string(),
        JsonValue::Number(kv.watch_streams_active as f64),
    );
    kv_object.insert(
        "watch_events_emitted".to_string(),
        JsonValue::Number(kv.watch_events_emitted as f64),
    );
    kv_object.insert(
        "watch_drops".to_string(),
        JsonValue::Number(kv.watch_drops as f64),
    );
    object.insert("kv".to_string(), JsonValue::Object(kv_object));

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

pub(crate) fn unified_record_json(record: &UnifiedRecord, columns: &[String]) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "values".to_string(),
        JsonValue::Object(projected_values_json(record, columns)),
    );
    let meta = record_metadata_json(record, columns);
    if !meta.is_empty() {
        object.insert("meta".to_string(), JsonValue::Object(meta));
    }
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

fn projected_values_json(record: &UnifiedRecord, columns: &[String]) -> Map<String, JsonValue> {
    if columns.is_empty() {
        return record
            .iter_fields()
            .map(|(key, value)| {
                (
                    key.to_string(),
                    crate::presentation::entity_json::storage_value_to_json(value),
                )
            })
            .collect();
    }

    columns
        .iter()
        .filter_map(|column| {
            record.get(column).map(|value| {
                (
                    column.clone(),
                    crate::presentation::entity_json::storage_value_to_json(value),
                )
            })
        })
        .collect()
}

fn record_metadata_json(record: &UnifiedRecord, columns: &[String]) -> Map<String, JsonValue> {
    record
        .iter_fields()
        .filter_map(|(key, value)| {
            let key = key.as_ref();
            if columns.iter().any(|column| column == key) || !is_record_metadata_key(key) {
                return None;
            }
            Some((
                key.to_string(),
                crate::presentation::entity_json::storage_value_to_json(value),
            ))
        })
        .collect()
}

fn is_record_metadata_key(key: &str) -> bool {
    matches!(
        key,
        "rid"
            | "red_entity_id"
            | "collection"
            | "red_collection"
            | "kind"
            | "red_kind"
            | "tenant"
            | "created_at"
            | "updated_at"
            | "row_id"
            | "red_sequence_id"
            | "red_entity_type"
            | "red_capabilities"
    )
}

fn matched_node_json(node: &MatchedNode) -> JsonValue {
    let mut object = Map::new();
    object.insert("id".to_string(), JsonValue::String(node.id.clone()));
    object.insert("label".to_string(), JsonValue::String(node.label.clone()));
    object.insert(
        "node_type".to_string(),
        JsonValue::String(node.node_label.clone()),
    );
    JsonValue::Object(object)
}

fn matched_edge_json(edge: &MatchedEdge) -> JsonValue {
    let mut object = Map::new();
    object.insert("from".to_string(), JsonValue::String(edge.from.clone()));
    object.insert("to".to_string(), JsonValue::String(edge.to.clone()));
    object.insert(
        "edge_type".to_string(),
        JsonValue::String(edge.edge_label.clone()),
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
        "red_entity_id".to_string(),
        JsonValue::Number(result.id as f64),
    );
    object.insert(
        "collection".to_string(),
        JsonValue::String(result.collection.clone()),
    );
    object.insert(
        "red_collection".to_string(),
        JsonValue::String(result.collection.clone()),
    );
    object.insert(
        "red_kind".to_string(),
        JsonValue::String("vector".to_string()),
    );
    object.insert(
        "red_entity_type".to_string(),
        JsonValue::String("vector".to_string()),
    );
    object.insert(
        "red_capabilities".to_string(),
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

#[cfg(test)]
mod descriptor_tests {
    //! #737 — contract tests for the additive query-result descriptor.
    //!
    //! These exercise `descriptor_json` directly against
    //! `UnifiedResult` fixtures so the contract is pinned independent
    //! of the SQL/Gremlin executor stack. The seven shapes the brief
    //! calls out (table-only, document/JSON, graph, vector, queue,
    //! timeseries-or-metrics, mixed multimodel) each get one test.
    use super::*;
    use crate::storage::query::unified::{
        GraphPath, MatchedEdge, MatchedNode, UnifiedRecord, UnifiedResult, VectorSearchResult,
    };
    use crate::storage::schema::types::Value as StorageValue;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn arc_schema(cols: &[&str]) -> Arc<Vec<Arc<str>>> {
        Arc::new(cols.iter().map(|c| Arc::from(*c)).collect())
    }

    fn descriptor(result: &UnifiedResult) -> JsonValue {
        descriptor_json(result, &result.records)
    }

    fn as_kind(d: &JsonValue) -> &str {
        d.get("result_kind").and_then(JsonValue::as_str).unwrap()
    }

    fn as_models(d: &JsonValue) -> Vec<String> {
        d.get("models_present")
            .and_then(JsonValue::as_array)
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    }

    fn as_count(d: &JsonValue, key: &str) -> f64 {
        d.get("counts_by_kind")
            .and_then(|c| c.get(key))
            .and_then(JsonValue::as_f64)
            .unwrap()
    }

    fn as_bool(d: &JsonValue, key: &str) -> bool {
        d.get(key).and_then(JsonValue::as_bool).unwrap()
    }

    #[test]
    fn descriptor_table_only_classifies_table_with_typed_columns() {
        let schema = arc_schema(&["id", "name", "active"]);
        let rec = UnifiedRecord::with_schema(
            schema,
            vec![
                StorageValue::Integer(1),
                StorageValue::Text(Arc::from("alice")),
                StorageValue::Boolean(true),
            ],
        );
        let mut result =
            UnifiedResult::with_columns(vec!["id".into(), "name".into(), "active".into()]);
        result.push(rec);

        let d = descriptor(&result);
        assert_eq!(as_kind(&d), "table");
        assert_eq!(as_models(&d), vec!["table"]);
        assert_eq!(as_count(&d, "rows"), 1.0);
        assert_eq!(as_count(&d, "nodes"), 0.0);
        assert_eq!(as_count(&d, "vector_matches"), 0.0);
        assert!(as_bool(&d, "has_table"));
        assert!(!as_bool(&d, "has_graph"));
        assert!(!as_bool(&d, "has_vector"));
        assert!(!as_bool(&d, "has_queue"));
        assert!(!as_bool(&d, "has_timeseries"));
        assert!(!as_bool(&d, "has_metrics"));

        let cols = d.get("columns").and_then(JsonValue::as_array).unwrap();
        let types: Vec<(&str, &str, bool)> = cols
            .iter()
            .map(|c| {
                (
                    c.get("name").and_then(JsonValue::as_str).unwrap(),
                    c.get("type").and_then(JsonValue::as_str).unwrap(),
                    c.get("nullable").and_then(JsonValue::as_bool).unwrap(),
                )
            })
            .collect();
        assert_eq!(
            types,
            vec![
                ("id", "number", false),
                ("name", "string", false),
                ("active", "boolean", false),
            ]
        );
    }

    #[test]
    fn descriptor_document_json_record_is_a_table_with_object_column() {
        // Document/JSON-shaped responses ride the same UnifiedResult
        // envelope as table responses (one record per document, with
        // a `Json` payload column). The descriptor must still resolve
        // them as `table` rather than guess at a "json" kind.
        let schema = arc_schema(&["id", "payload"]);
        let rec = UnifiedRecord::with_schema(
            schema,
            vec![
                StorageValue::Integer(7),
                StorageValue::Json(b"{\"k\":1}".to_vec()),
            ],
        );
        let mut result = UnifiedResult::with_columns(vec!["id".into(), "payload".into()]);
        result.push(rec);

        let d = descriptor(&result);
        assert_eq!(as_kind(&d), "table");
        let cols = d.get("columns").and_then(JsonValue::as_array).unwrap();
        let payload = cols
            .iter()
            .find(|c| c.get("name").and_then(JsonValue::as_str) == Some("payload"))
            .unwrap();
        assert_eq!(
            payload.get("type").and_then(JsonValue::as_str).unwrap(),
            "object"
        );
    }

    #[test]
    fn descriptor_graph_only_classifies_graph_and_counts_topology() {
        let mut rec = UnifiedRecord::new();
        rec.set_node(
            "n",
            MatchedNode {
                id: "n1".into(),
                label: "n1".into(),
                node_label: "person".into(),
                properties: HashMap::new(),
            },
        );
        rec.set_edge("e", MatchedEdge::from_tuple("n1", "knows", "n2", 1.0));
        rec.paths.push(
            GraphPath::start("n1").extend(MatchedEdge::from_tuple("n1", "knows", "n2", 1.0), "n2"),
        );

        let mut result = UnifiedResult::empty();
        result.push(rec);

        let d = descriptor(&result);
        assert_eq!(as_kind(&d), "graph");
        assert_eq!(as_models(&d), vec!["graph"]);
        assert_eq!(as_count(&d, "nodes"), 1.0);
        assert_eq!(as_count(&d, "edges"), 1.0);
        assert_eq!(as_count(&d, "paths"), 1.0);
        assert_eq!(as_count(&d, "rows"), 0.0);
        assert!(as_bool(&d, "has_graph"));
        assert!(!as_bool(&d, "has_table"));
        assert!(!as_bool(&d, "has_vector"));

        let hints = d
            .get("renderer_hints")
            .and_then(JsonValue::as_array)
            .unwrap();
        assert_eq!(hints[0].as_str(), Some("graph"));
    }

    #[test]
    fn descriptor_vector_only_classifies_vector_and_counts_matches() {
        let mut rec = UnifiedRecord::new();
        rec.vector_results
            .push(VectorSearchResult::new(1, "embeddings", 0.1));
        rec.vector_results
            .push(VectorSearchResult::new(2, "embeddings", 0.2));
        let mut result = UnifiedResult::empty();
        result.push(rec);

        let d = descriptor(&result);
        assert_eq!(as_kind(&d), "vector");
        assert_eq!(as_models(&d), vec!["vector"]);
        assert_eq!(as_count(&d, "vector_matches"), 2.0);
        assert!(as_bool(&d, "has_vector"));
        assert!(!as_bool(&d, "has_graph"));
        assert!(!as_bool(&d, "has_table"));
    }

    #[test]
    fn descriptor_queue_shape_is_absent_from_query_envelope() {
        // The `/query` endpoint can't carry queue payloads — the
        // engine has no `queue_results` slot on UnifiedResult. The
        // descriptor must report this as a known false rather than
        // an unknown, so the UI can confidently hide queue-only
        // renderers when reading query responses.
        let result = UnifiedResult::empty();
        let d = descriptor(&result);
        assert!(!as_bool(&d, "has_queue"));
        assert_eq!(as_kind(&d), "empty");
    }

    #[test]
    fn descriptor_timeseries_metrics_shape_is_absent_from_query_envelope() {
        // Same contract as the queue test: timeseries / metrics
        // shapes have no query-envelope carrier, so they're known
        // absent (not unknown).
        let schema = arc_schema(&["ts", "value"]);
        let rec = UnifiedRecord::with_schema(
            schema,
            vec![
                StorageValue::TimestampMs(1_700_000_000_000),
                StorageValue::Float(42.5),
            ],
        );
        let mut result = UnifiedResult::with_columns(vec!["ts".into(), "value".into()]);
        result.push(rec);

        let d = descriptor(&result);
        // Even though the payload *looks like* timeseries, the
        // query envelope's known-shape flag stays false: we
        // didn't add a typed carrier for it in this slice.
        assert!(!as_bool(&d, "has_timeseries"));
        assert!(!as_bool(&d, "has_metrics"));
        assert_eq!(as_kind(&d), "table");
        let cols = d.get("columns").and_then(JsonValue::as_array).unwrap();
        let ts = cols
            .iter()
            .find(|c| c.get("name").and_then(JsonValue::as_str) == Some("ts"))
            .unwrap();
        assert_eq!(
            ts.get("type").and_then(JsonValue::as_str).unwrap(),
            "timestamp"
        );
    }

    #[test]
    fn descriptor_mixed_multimodel_lists_each_present_model() {
        let schema = arc_schema(&["id"]);
        let mut rec = UnifiedRecord::with_schema(schema, vec![StorageValue::Integer(9)]);
        rec.set_node(
            "n",
            MatchedNode {
                id: "n1".into(),
                label: "n1".into(),
                node_label: "doc".into(),
                properties: HashMap::new(),
            },
        );
        rec.vector_results
            .push(VectorSearchResult::new(11, "embeds", 0.05));

        let mut result = UnifiedResult::with_columns(vec!["id".into()]);
        result.push(rec);

        let d = descriptor(&result);
        assert_eq!(as_kind(&d), "mixed");
        let models = as_models(&d);
        assert!(models.contains(&"table".to_string()));
        assert!(models.contains(&"graph".to_string()));
        assert!(models.contains(&"vector".to_string()));
        assert!(as_bool(&d, "has_table"));
        assert!(as_bool(&d, "has_graph"));
        assert!(as_bool(&d, "has_vector"));

        let hints = d
            .get("renderer_hints")
            .and_then(JsonValue::as_array)
            .unwrap();
        // mixed → at least 2 renderer hints offered to the UI
        assert!(hints.len() >= 2, "expected multiple hints, got {hints:?}");
    }

    #[test]
    fn descriptor_column_with_only_nulls_is_unknown_nullable() {
        // Acceptance criterion #3: safe unknown when the engine
        // can't determine the column type. We never observed a
        // non-null value, so `type=unknown, nullable=true`.
        let schema = arc_schema(&["mystery"]);
        let rec = UnifiedRecord::with_schema(schema, vec![StorageValue::Null]);
        let mut result = UnifiedResult::with_columns(vec!["mystery".into()]);
        result.push(rec);

        let d = descriptor(&result);
        let cols = d.get("columns").and_then(JsonValue::as_array).unwrap();
        let entry = &cols[0];
        assert_eq!(
            entry.get("type").and_then(JsonValue::as_str).unwrap(),
            "unknown"
        );
        assert!(entry.get("nullable").and_then(JsonValue::as_bool).unwrap());
    }

    #[test]
    fn descriptor_preserves_existing_query_response_shape() {
        // Acceptance criterion #2: additive. The base envelope keys
        // that pre-#737 clients rely on must all still be present,
        // and `descriptor` is the only new top-level key.
        use crate::runtime::RuntimeQueryResult;
        use crate::storage::query::modes::QueryMode;

        let mut unified = UnifiedResult::with_columns(vec!["x".into()]);
        unified.push(UnifiedRecord::with_schema(
            arc_schema(&["x"]),
            vec![StorageValue::Integer(1)],
        ));

        let runtime_result = RuntimeQueryResult {
            query: "SELECT x FROM t".to_string(),
            mode: QueryMode::Sql,
            statement: "select",
            engine: "test",
            result: unified,
            affected_rows: 0,
            statement_type: "select",
        };

        let json = runtime_query_json(&runtime_result, &None, &None);
        let obj = json.as_object().unwrap();
        for key in [
            "ok",
            "query",
            "mode",
            "capability",
            "statement",
            "engine",
            "record_count",
            "result",
            "selection",
            "descriptor",
        ] {
            assert!(obj.contains_key(key), "missing key {key} in {obj:?}");
        }
    }
}
