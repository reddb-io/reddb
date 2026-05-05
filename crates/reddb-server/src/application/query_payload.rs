use crate::application::{
    json_input::{json_bool_field, json_f32_field, json_string_list_field, json_usize_field},
    SearchContextInput, SearchHybridInput, SearchIndexInput, SearchIvfInput, SearchMultimodalInput,
    SearchSimilarInput, SearchTextInput,
};
use crate::json::Value as JsonValue;
use crate::runtime::{RuntimeFilter, RuntimeFilterValue, RuntimeGraphPattern, RuntimeQueryWeights};
use crate::{RedDBError, RedDBResult};

pub(crate) enum UnifiedSearchInput {
    Hybrid(SearchHybridInput),
    Multimodal(SearchMultimodalInput),
    Index(SearchIndexInput),
}

pub(crate) fn parse_text_search_input(payload: &JsonValue) -> RedDBResult<SearchTextInput> {
    let query = parse_required_query(payload)?;
    let (entity_types, capabilities) = parse_search_filters(payload)?;

    Ok(SearchTextInput {
        query,
        collections: json_string_list_field(payload, "collections"),
        entity_types,
        capabilities,
        fields: json_string_list_field(payload, "fields"),
        limit: json_usize_field(payload, "limit"),
        fuzzy: json_bool_field(payload, "fuzzy").unwrap_or(false),
    })
}

pub(crate) fn parse_multimodal_search_input(
    payload: &JsonValue,
) -> RedDBResult<SearchMultimodalInput> {
    let query = parse_required_query_or_key(payload)?;
    let (entity_types, capabilities) = parse_search_filters(payload)?;

    Ok(SearchMultimodalInput {
        query,
        collections: json_string_list_field(payload, "collections"),
        entity_types,
        capabilities,
        limit: json_usize_field(payload, "limit"),
    })
}

pub(crate) fn parse_unified_search_input(payload: &JsonValue) -> RedDBResult<UnifiedSearchInput> {
    if payload_requests_index(payload) {
        return parse_index_search_input(payload).map(UnifiedSearchInput::Index);
    }

    match parse_search_mode(payload)? {
        SearchMode::Index => parse_index_search_input(payload).map(UnifiedSearchInput::Index),
        SearchMode::Hybrid => {
            parse_hybrid_search_input(payload, "unified search").map(UnifiedSearchInput::Hybrid)
        }
        SearchMode::Multimodal => {
            parse_multimodal_search_input(payload).map(UnifiedSearchInput::Multimodal)
        }
        SearchMode::Auto => {
            if payload_requests_hybrid(payload) {
                parse_hybrid_search_input(payload, "unified search").map(UnifiedSearchInput::Hybrid)
            } else {
                parse_multimodal_search_input(payload).map(UnifiedSearchInput::Multimodal)
            }
        }
    }
}

pub(crate) fn parse_index_search_input(payload: &JsonValue) -> RedDBResult<SearchIndexInput> {
    let (entity_types, capabilities) = parse_search_filters(payload)?;
    let lookup = parse_lookup_object(payload)?;

    let index = lookup_or_payload_string(payload, lookup, "index")?;
    let value = lookup_or_payload_string(payload, lookup, "value")?;
    let exact = lookup_or_payload_bool(payload, lookup, "exact")?.unwrap_or(true);

    Ok(SearchIndexInput {
        index,
        value,
        exact,
        collections: json_string_list_field(payload, "collections"),
        entity_types,
        capabilities,
        limit: json_usize_field(payload, "limit"),
    })
}

pub(crate) fn parse_hybrid_search_input(
    payload: &JsonValue,
    search_kind: &str,
) -> RedDBResult<SearchHybridInput> {
    let query = parse_optional_query(payload)?;
    let vector = optional_json_vector_field(payload, "vector")?;
    if vector.is_none() && query.is_none() {
        return Err(RedDBError::Query(format!(
            "field 'query' or 'vector' is required for {search_kind}"
        )));
    }
    let (entity_types, capabilities) = parse_search_filters(payload)?;

    Ok(SearchHybridInput {
        vector,
        query,
        k: json_usize_field(payload, "k"),
        collections: json_string_list_field(payload, "collections"),
        entity_types,
        capabilities,
        graph_pattern: json_graph_pattern(payload)?,
        filters: json_filters(payload)?,
        weights: json_weights(payload),
        min_score: json_f32_field(payload, "min_score"),
        limit: json_usize_field(payload, "limit"),
    })
}

pub(crate) fn parse_similar_search_input(
    collection: String,
    payload: &JsonValue,
) -> RedDBResult<SearchSimilarInput> {
    if collection.trim().is_empty() {
        return Err(RedDBError::Query("collection cannot be empty".to_string()));
    }

    let text = payload
        .get("text")
        .and_then(JsonValue::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let provider = payload
        .get("provider")
        .and_then(JsonValue::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    // Either vector or text must be provided
    let vector = if text.is_some() {
        Vec::new() // will be generated from text at runtime
    } else {
        json_vector_field(payload, "vector")?
    };

    Ok(SearchSimilarInput {
        collection,
        vector,
        k: json_usize_field(payload, "k").unwrap_or(10).max(1),
        min_score: json_f32_field(payload, "min_score").unwrap_or(0.0),
        text,
        provider,
    })
}

pub(crate) fn parse_ivf_search_input(
    collection: String,
    payload: &JsonValue,
) -> RedDBResult<SearchIvfInput> {
    if collection.trim().is_empty() {
        return Err(RedDBError::Query("collection cannot be empty".to_string()));
    }
    Ok(SearchIvfInput {
        collection,
        vector: json_vector_field(payload, "vector")?,
        k: json_usize_field(payload, "k").unwrap_or(10).max(1),
        n_lists: json_usize_field(payload, "n_lists").unwrap_or(32).max(1),
        n_probes: json_usize_field(payload, "n_probes"),
    })
}

pub(crate) fn normalize_search_selection(
    entity_type_values: &[String],
    capability_values: &[String],
) -> RedDBResult<(Option<Vec<String>>, Option<Vec<String>>)> {
    let mut entity_types = Vec::new();
    let mut capabilities = Vec::new();

    for raw in entity_type_values {
        for value in split_filter_values(raw) {
            if !apply_entity_type_alias(&value, &mut entity_types, &mut capabilities)? {
                return Ok((None, None));
            }
        }
    }

    for raw in capability_values {
        for value in split_filter_values(raw) {
            if !normalize_capability_filter(&value, &mut capabilities)? {
                return Ok((None, None));
            }
        }
    }

    Ok((
        normalize_search_filter_list(entity_types),
        normalize_search_filter_list(capabilities),
    ))
}

pub(crate) fn parse_json_search_selection(
    payload: &JsonValue,
) -> RedDBResult<(Option<Vec<String>>, Option<Vec<String>>)> {
    parse_search_filters(payload)
}

fn parse_search_filters(
    payload: &JsonValue,
) -> RedDBResult<(Option<Vec<String>>, Option<Vec<String>>)> {
    let mut entity_types = Vec::new();
    let mut capabilities = Vec::new();

    if let Some(values) = parse_string_array_or_scalar(payload, "entity_types")? {
        for value in values {
            if !apply_entity_type_alias(&value, &mut entity_types, &mut capabilities)? {
                return Ok((None, None));
            }
        }
    }

    if let Some(values) = parse_string_array_or_scalar(payload, "capabilities")? {
        for value in values {
            if !normalize_capability_filter(&value, &mut capabilities)? {
                return Ok((None, None));
            }
        }
    }

    Ok((
        normalize_search_filter_list(entity_types),
        normalize_search_filter_list(capabilities),
    ))
}

fn parse_string_array_or_scalar(
    payload: &JsonValue,
    field: &str,
) -> RedDBResult<Option<Vec<String>>> {
    match payload.get(field) {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::String(value)) => {
            let values = split_filter_values(value);
            if values.is_empty() {
                Ok(None)
            } else {
                Ok(Some(values))
            }
        }
        Some(JsonValue::Array(values)) => {
            let mut out = Vec::with_capacity(values.len());
            for value in values {
                let Some(text) = value.as_str() else {
                    return Err(RedDBError::Query(format!(
                        "field '{field}' must be a string or array of strings"
                    )));
                };
                out.extend(split_filter_values(text));
            }
            Ok((!out.is_empty()).then_some(out))
        }
        Some(_) => Err(RedDBError::Query(format!(
            "field '{field}' must be a string, array of strings, or omitted"
        ))),
    }
}

fn split_filter_values(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn apply_entity_type_alias(
    value: &str,
    entity_types: &mut Vec<String>,
    capabilities: &mut Vec<String>,
) -> RedDBResult<bool> {
    match normalize_search_token(value).as_str() {
        "all" | "any" | "*" | "entity" | "entities" => Ok(false),
        "table" | "row" | "rows" | "structured" => {
            entity_types.push("table".to_string());
            Ok(true)
        }
        "kv" | "kvs" | "keyvalue" | "keyvalues" => {
            entity_types.push("kv".to_string());
            Ok(true)
        }
        "document" | "doc" | "documents" => {
            capabilities.push("document".to_string());
            Ok(true)
        }
        "vector" | "vectors" | "embedding" | "embeddings" | "similarity" | "similarities" => {
            entity_types.push("vector".to_string());
            Ok(true)
        }
        "graph" | "graphs" | "node" | "nodes" => {
            entity_types.push("graph_node".to_string());
            entity_types.push("graph_edge".to_string());
            Ok(true)
        }
        "graph_node" | "graphnode" | "graph_node_type" => {
            entity_types.push("graph_node".to_string());
            Ok(true)
        }
        "graph_edge" | "graphedge" | "graph_edge_type" => {
            entity_types.push("graph_edge".to_string());
            Ok(true)
        }
        other => Err(RedDBError::Query(format!(
            "invalid entity_types value '{other}'"
        ))),
    }
}

fn normalize_capability_filter(value: &str, capabilities: &mut Vec<String>) -> RedDBResult<bool> {
    match normalize_search_token(value).as_str() {
        "all" | "any" | "*" => Ok(false),
        "table" => {
            capabilities.push("table".to_string());
            Ok(true)
        }
        "kv" | "kvs" | "keyvalue" | "keyvalues" => {
            capabilities.push("kv".to_string());
            Ok(true)
        }
        "structured" => {
            capabilities.push("structured".to_string());
            Ok(true)
        }
        "document" | "doc" | "documents" => {
            capabilities.push("document".to_string());
            Ok(true)
        }
        "vector" | "vectors" => {
            capabilities.push("vector".to_string());
            Ok(true)
        }
        "similarity" | "similarities" => {
            capabilities.push("similarity".to_string());
            Ok(true)
        }
        "embedding" | "embeddings" => {
            capabilities.push("embedding".to_string());
            Ok(true)
        }
        "graph" => {
            capabilities.push("graph".to_string());
            Ok(true)
        }
        "graph_node" | "graphnode" => {
            capabilities.push("graph_node".to_string());
            Ok(true)
        }
        "graph_edge" | "graphedge" => {
            capabilities.push("graph_edge".to_string());
            Ok(true)
        }
        other => Err(RedDBError::Query(format!(
            "invalid capabilities value '{other}'"
        ))),
    }
}

fn normalize_search_filter_list(mut values: Vec<String>) -> Option<Vec<String>> {
    values.sort();
    values.dedup();
    (!values.is_empty()).then_some(values)
}

fn normalize_search_token(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(|character| character.to_lowercase())
        .collect()
}

fn parse_required_query(payload: &JsonValue) -> RedDBResult<String> {
    let Some(query) = payload.get("query").and_then(JsonValue::as_str) else {
        return Err(RedDBError::Query(
            "field 'query' must be a string".to_string(),
        ));
    };
    let query = query.trim();
    if query.is_empty() {
        return Err(RedDBError::Query(
            "field 'query' cannot be empty".to_string(),
        ));
    }
    Ok(query.to_string())
}

#[derive(Debug, Clone, Copy)]
enum SearchMode {
    Auto,
    Hybrid,
    Multimodal,
    Index,
}

fn parse_search_mode(payload: &JsonValue) -> RedDBResult<SearchMode> {
    let mode = payload
        .get("mode")
        .or_else(|| payload.get("search_mode"))
        .and_then(JsonValue::as_str);
    let Some(mode) = mode else {
        return Ok(SearchMode::Auto);
    };

    let normalized = mode.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "auto" => Ok(SearchMode::Auto),
        "hybrid" | "semantic" | "universal" => Ok(SearchMode::Hybrid),
        "multimodal" | "lookup" | "global" => Ok(SearchMode::Multimodal),
        "index" | "indexed" => Ok(SearchMode::Index),
        other => Err(RedDBError::Query(format!(
            "invalid search mode '{other}'. expected auto, hybrid, multimodal, or index"
        ))),
    }
}

fn payload_requests_hybrid(payload: &JsonValue) -> bool {
    payload.get("vector").is_some()
        || payload.get("graph").is_some()
        || payload.get("filters").is_some()
        || payload.get("weights").is_some()
        || payload.get("min_score").is_some()
        || payload.get("k").is_some()
}

fn payload_requests_index(payload: &JsonValue) -> bool {
    payload.get("lookup").is_some()
        || payload.get("index").is_some()
        || payload.get("value").is_some()
}

fn parse_lookup_object(
    payload: &JsonValue,
) -> RedDBResult<Option<&crate::serde_json::Map<String, JsonValue>>> {
    match payload.get("lookup") {
        None | Some(JsonValue::Null) => Ok(None),
        Some(value) => value.as_object().map(Some).ok_or_else(|| {
            RedDBError::Query("field 'lookup' must be an object when present".to_string())
        }),
    }
}

fn lookup_or_payload_string(
    payload: &JsonValue,
    lookup: Option<&crate::serde_json::Map<String, JsonValue>>,
    field: &str,
) -> RedDBResult<String> {
    let value = lookup
        .and_then(|item| item.get(field))
        .or_else(|| payload.get(field))
        .ok_or_else(|| RedDBError::Query(format!("field '{field}' must be a string")))?;

    let Some(value) = value.as_str() else {
        return Err(RedDBError::Query(format!(
            "field '{field}' must be a string"
        )));
    };

    let value = value.trim();
    if value.is_empty() {
        return Err(RedDBError::Query(format!(
            "field '{field}' cannot be empty"
        )));
    }
    Ok(value.to_string())
}

fn lookup_or_payload_bool(
    payload: &JsonValue,
    lookup: Option<&crate::serde_json::Map<String, JsonValue>>,
    field: &str,
) -> RedDBResult<Option<bool>> {
    let value = lookup
        .and_then(|item| item.get(field))
        .or_else(|| payload.get(field));

    match value {
        None | Some(JsonValue::Null) => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| RedDBError::Query(format!("field '{field}' must be a boolean"))),
    }
}

fn parse_required_query_or_key(payload: &JsonValue) -> RedDBResult<String> {
    if let Some(query) = payload.get("query").and_then(JsonValue::as_str) {
        let query = query.trim();
        if query.is_empty() {
            return Err(RedDBError::Query(
                "field 'query' cannot be empty".to_string(),
            ));
        }
        return Ok(query.to_string());
    }

    if let Some(key) = payload.get("key").and_then(JsonValue::as_str) {
        let key = key.trim();
        if key.is_empty() {
            return Err(RedDBError::Query("field 'key' cannot be empty".to_string()));
        }
        return Ok(key.to_string());
    }

    Err(RedDBError::Query(
        "field 'query' or 'key' must be a string".to_string(),
    ))
}

fn parse_optional_query(payload: &JsonValue) -> RedDBResult<Option<String>> {
    match payload.get("query") {
        None | Some(JsonValue::Null) => Ok(None),
        Some(value) => {
            let Some(query) = value.as_str() else {
                return Err(RedDBError::Query(
                    "field 'query' must be a string".to_string(),
                ));
            };
            let query = query.trim();
            if query.is_empty() {
                return Err(RedDBError::Query(
                    "field 'query' cannot be empty".to_string(),
                ));
            }
            Ok(Some(query.to_string()))
        }
    }
}

pub(crate) fn parse_context_search_input(payload: &JsonValue) -> RedDBResult<SearchContextInput> {
    let query = parse_required_query(payload)?;

    let field = payload
        .get("field")
        .and_then(JsonValue::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    Ok(SearchContextInput {
        query,
        field,
        vector: optional_json_vector_field(payload, "vector")?,
        collections: json_string_list_field(payload, "collections"),
        graph_depth: json_usize_field(payload, "graph_depth"),
        graph_max_edges: json_usize_field(payload, "graph_max_edges"),
        max_cross_refs: json_usize_field(payload, "max_cross_refs"),
        follow_cross_refs: json_bool_field(payload, "follow_cross_refs"),
        expand_graph: json_bool_field(payload, "expand_graph"),
        global_scan: json_bool_field(payload, "global_scan"),
        reindex: json_bool_field(payload, "reindex"),
        limit: json_usize_field(payload, "limit"),
        min_score: json_f32_field(payload, "min_score"),
    })
}

fn json_vector_field(payload: &JsonValue, field: &str) -> RedDBResult<Vec<f32>> {
    let values = payload
        .get(field)
        .and_then(JsonValue::as_array)
        .ok_or_else(|| RedDBError::Query(format!("field '{field}' must be an array")))?;
    if values.is_empty() {
        return Err(RedDBError::Query(format!(
            "field '{field}' cannot be empty"
        )));
    }
    values
        .iter()
        .map(|value| {
            value.as_f64().map(|value| value as f32).ok_or_else(|| {
                RedDBError::Query(format!("field '{field}' must contain only numbers"))
            })
        })
        .collect()
}

fn optional_json_vector_field(payload: &JsonValue, field: &str) -> RedDBResult<Option<Vec<f32>>> {
    match payload.get(field) {
        Some(JsonValue::Null) | None => Ok(None),
        Some(_) => json_vector_field(payload, field).map(Some),
    }
}

fn json_graph_pattern(payload: &JsonValue) -> RedDBResult<Option<RuntimeGraphPattern>> {
    let Some(graph) = payload.get("graph") else {
        return Ok(None);
    };
    let Some(graph) = graph.as_object() else {
        return Err(RedDBError::Query(
            "field 'graph' must be an object".to_string(),
        ));
    };

    Ok(Some(RuntimeGraphPattern {
        node_label: graph
            .get("label")
            .and_then(JsonValue::as_str)
            .map(|value| value.to_string()),
        node_type: graph
            .get("node_type")
            .and_then(JsonValue::as_str)
            .map(|value| value.to_string()),
        edge_labels: graph
            .get("edge_labels")
            .and_then(JsonValue::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(JsonValue::as_str)
                    .map(|value| value.to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    }))
}

fn json_weights(payload: &JsonValue) -> Option<RuntimeQueryWeights> {
    let weights = payload.get("weights")?.as_object()?;
    Some(RuntimeQueryWeights {
        vector: weights
            .get("vector")
            .and_then(JsonValue::as_f64)
            .unwrap_or(0.5) as f32,
        graph: weights
            .get("graph")
            .and_then(JsonValue::as_f64)
            .unwrap_or(0.3) as f32,
        filter: weights
            .get("filter")
            .and_then(JsonValue::as_f64)
            .unwrap_or(0.2) as f32,
    })
}

fn json_filters(payload: &JsonValue) -> RedDBResult<Vec<RuntimeFilter>> {
    let Some(values) = payload.get("filters") else {
        return Ok(Vec::new());
    };
    let Some(values) = values.as_array() else {
        return Err(RedDBError::Query(
            "field 'filters' must be an array".to_string(),
        ));
    };

    let mut filters = Vec::with_capacity(values.len());
    for value in values {
        let Some(object) = value.as_object() else {
            return Err(RedDBError::Query(
                "every filter must be an object".to_string(),
            ));
        };
        let Some(field) = object.get("field").and_then(JsonValue::as_str) else {
            return Err(RedDBError::Query(
                "every filter must contain a string field named 'field'".to_string(),
            ));
        };
        let Some(op) = object.get("op").and_then(JsonValue::as_str) else {
            return Err(RedDBError::Query(
                "every filter must contain a string field named 'op'".to_string(),
            ));
        };
        let parsed_value = object.get("value").map(json_filter_value).transpose()?;

        filters.push(RuntimeFilter {
            field: field.to_string(),
            op: op.to_string(),
            value: parsed_value,
        });
    }

    Ok(filters)
}

fn json_filter_value(value: &JsonValue) -> RedDBResult<RuntimeFilterValue> {
    Ok(match value {
        JsonValue::Null => RuntimeFilterValue::Null,
        JsonValue::Bool(value) => RuntimeFilterValue::Bool(*value),
        JsonValue::Number(value) => {
            if value.fract().abs() < f64::EPSILON {
                RuntimeFilterValue::Int(*value as i64)
            } else {
                RuntimeFilterValue::Float(*value)
            }
        }
        JsonValue::String(value) => RuntimeFilterValue::String(value.clone()),
        JsonValue::Array(values) => RuntimeFilterValue::List(
            values
                .iter()
                .map(json_filter_value)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        JsonValue::Object(object) => {
            if let (Some(start), Some(end)) = (object.get("start"), object.get("end")) {
                RuntimeFilterValue::Range(
                    Box::new(json_filter_value(start)?),
                    Box::new(json_filter_value(end)?),
                )
            } else {
                return Err(RedDBError::Query(
                    "filter object values must be either scalars, arrays, or {start,end} ranges"
                        .to_string(),
                ));
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_entity_type_alias_accepts_kv_shortcuts() {
        let mut entity_types = Vec::new();
        let mut capabilities = Vec::new();

        assert!(apply_entity_type_alias("kv", &mut entity_types, &mut capabilities).is_ok());
        assert_eq!(entity_types, vec!["kv".to_string()]);
        assert!(capabilities.is_empty());
    }

    #[test]
    fn parse_capability_alias_accepts_kv_shortcuts() {
        let mut capabilities = Vec::new();
        assert!(normalize_capability_filter("key-value", &mut capabilities).is_ok());
        assert_eq!(capabilities, vec!["kv".to_string()]);
    }

    #[test]
    fn parse_unified_prefers_lookup_payload() {
        let payload: JsonValue = crate::json::from_str(
            r#"{"lookup":{"index":"cpf","value":"000.000.000-00"},"mode":"hybrid"}"#,
        )
        .expect("valid json");
        let input = parse_unified_search_input(&payload).expect("lookup should parse");
        assert!(matches!(input, UnifiedSearchInput::Index(_)));
    }

    #[test]
    fn parse_index_payload_supports_top_level_fields() {
        let payload: JsonValue = crate::json::from_str(
            r#"{"mode":"index","index":"cpf","value":"000.000.000-00","exact":false}"#,
        )
        .expect("valid json");
        let input = parse_unified_search_input(&payload).expect("index payload should parse");
        let UnifiedSearchInput::Index(input) = input else {
            panic!("expected index input");
        };
        assert_eq!(input.index, "cpf");
        assert_eq!(input.value, "000.000.000-00");
        assert!(!input.exact);
    }
}
