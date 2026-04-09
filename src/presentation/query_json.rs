use crate::json::{Map, Value as JsonValue};
use crate::runtime::RuntimeIvfSearchResult;
use crate::storage::unified::devx::SimilarResult;
use crate::storage::unified::dsl::QueryResult as DslQueryResult;
use crate::storage::{MatchComponents, ScoredMatch, UnifiedEntity};
use std::cmp::Ordering;

pub(crate) fn similar_results_json<F>(
    collection: &str,
    k: usize,
    min_score: f32,
    results: &[SimilarResult],
    entity_to_json: F,
) -> JsonValue
where
    F: Fn(&UnifiedEntity) -> JsonValue,
{
    let mut object = Map::new();
    object.insert(
        "collection".to_string(),
        JsonValue::String(collection.to_string()),
    );
    object.insert("k".to_string(), JsonValue::Number(k as f64));
    object.insert("min_score".to_string(), JsonValue::Number(min_score as f64));
    object.insert(
        "results".to_string(),
        JsonValue::Array(
            results
                .iter()
                .map(|result| {
                    let mut item = Map::new();
                    let (entity_type, capabilities) = entity_capability_profile(&result.entity);
                    item.insert(
                        "entity_id".to_string(),
                        JsonValue::Number(result.entity_id.raw() as f64),
                    );
                    item.insert(
                        "_entity_id".to_string(),
                        JsonValue::Number(result.entity_id.raw() as f64),
                    );
                    item.insert("score".to_string(), JsonValue::Number(result.score as f64));
                    item.insert("_score".to_string(), JsonValue::Number(result.score as f64));
                    item.insert(
                        "final_score".to_string(),
                        JsonValue::Number(result.score as f64),
                    );
                    item.insert(
                        "distance".to_string(),
                        JsonValue::Number(result.distance as f64),
                    );
                    item.insert(
                        "_distance".to_string(),
                        JsonValue::Number(result.distance as f64),
                    );
                    item.insert(
                        "vector_distance".to_string(),
                        JsonValue::Number(result.distance as f64),
                    );
                    item.insert(
                        "_collection".to_string(),
                        JsonValue::String(collection.to_string()),
                    );
                    item.insert("_kind".to_string(), JsonValue::String("vector".to_string()));
                    item.insert("_entity_type".to_string(), JsonValue::String(entity_type));
                    item.insert("_capabilities".to_string(), JsonValue::String(capabilities));
                    item.insert("entity".to_string(), entity_to_json(&result.entity));
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

pub(crate) fn runtime_ivf_json<F>(result: &RuntimeIvfSearchResult, entity_to_json: F) -> JsonValue
where
    F: Fn(&UnifiedEntity) -> JsonValue,
{
    let mut stats = Map::new();
    stats.insert(
        "total_vectors".to_string(),
        JsonValue::Number(result.stats.total_vectors as f64),
    );
    stats.insert(
        "n_lists".to_string(),
        JsonValue::Number(result.stats.n_lists as f64),
    );
    stats.insert(
        "non_empty_lists".to_string(),
        JsonValue::Number(result.stats.non_empty_lists as f64),
    );
    stats.insert(
        "avg_list_size".to_string(),
        JsonValue::Number(result.stats.avg_list_size),
    );
    stats.insert(
        "max_list_size".to_string(),
        JsonValue::Number(result.stats.max_list_size as f64),
    );
    stats.insert(
        "min_list_size".to_string(),
        JsonValue::Number(result.stats.min_list_size as f64),
    );
    stats.insert(
        "dimension".to_string(),
        JsonValue::Number(result.stats.dimension as f64),
    );
    stats.insert("trained".to_string(), JsonValue::Bool(result.stats.trained));

    let mut object = Map::new();
    object.insert(
        "collection".to_string(),
        JsonValue::String(result.collection.clone()),
    );
    object.insert("k".to_string(), JsonValue::Number(result.k as f64));
    object.insert(
        "n_lists".to_string(),
        JsonValue::Number(result.n_lists as f64),
    );
    object.insert(
        "n_probes".to_string(),
        JsonValue::Number(result.n_probes as f64),
    );
    object.insert("stats".to_string(), JsonValue::Object(stats));
    object.insert(
        "matches".to_string(),
        JsonValue::Array(
            result
                .matches
                .iter()
                .map(|item| {
                    let mut entry = Map::new();
                    let score = 1.0 / (1.0 + item.distance);
                    entry.insert(
                        "entity_id".to_string(),
                        JsonValue::Number(item.entity_id as f64),
                    );
                    entry.insert(
                        "_entity_id".to_string(),
                        JsonValue::Number(item.entity_id as f64),
                    );
                    entry.insert(
                        "distance".to_string(),
                        JsonValue::Number(item.distance as f64),
                    );
                    entry.insert(
                        "_distance".to_string(),
                        JsonValue::Number(item.distance as f64),
                    );
                    entry.insert(
                        "vector_distance".to_string(),
                        JsonValue::Number(item.distance as f64),
                    );
                    entry.insert("_score".to_string(), JsonValue::Number(score as f64));
                    entry.insert("score".to_string(), JsonValue::Number(score as f64));
                    entry.insert("final_score".to_string(), JsonValue::Number(score as f64));
                    entry.insert(
                        "_collection".to_string(),
                        JsonValue::String(result.collection.clone()),
                    );
                    entry.insert("_kind".to_string(), JsonValue::String("vector".to_string()));
                    entry.insert(
                        "_entity_type".to_string(),
                        JsonValue::String("vector".to_string()),
                    );
                    entry.insert(
                        "_capabilities".to_string(),
                        JsonValue::String("vector,similarity,embedding".to_string()),
                    );
                    entry.insert(
                        "entity".to_string(),
                        match &item.entity {
                            Some(entity) => entity_to_json(entity),
                            None => JsonValue::Null,
                        },
                    );
                    JsonValue::Object(entry)
                })
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

pub(crate) fn dsl_query_result_json<F>(
    result: &DslQueryResult,
    selection: JsonValue,
    scored_match_to_json: F,
) -> JsonValue
where
    F: Fn(&ScoredMatch) -> JsonValue,
{
    let mut matches: Vec<&ScoredMatch> = result.matches.iter().collect();
    matches.sort_by(|left, right| {
        let left_score = left
            .components
            .final_score
            .filter(|value| value.is_finite())
            .unwrap_or(left.score);
        let right_score = right
            .components
            .final_score
            .filter(|value| value.is_finite())
            .unwrap_or(right.score);
        right_score
            .partial_cmp(&left_score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.entity.id.raw().cmp(&right.entity.id.raw()))
    });

    let mut object = Map::new();
    object.insert(
        "matches".to_string(),
        JsonValue::Array(matches.into_iter().map(scored_match_to_json).collect()),
    );
    object.insert(
        "scanned".to_string(),
        JsonValue::Number(result.scanned as f64),
    );
    object.insert(
        "execution_time_us".to_string(),
        JsonValue::Number(result.execution_time_us as f64),
    );
    object.insert(
        "explanation".to_string(),
        JsonValue::String(result.explanation.clone()),
    );
    object.insert("selection".to_string(), selection);
    JsonValue::Object(object)
}

pub(crate) fn scored_match_json<F>(item: &ScoredMatch, entity_to_json: F) -> JsonValue
where
    F: Fn(&UnifiedEntity) -> JsonValue,
{
    let score = item
        .components
        .final_score
        .filter(|value| value.is_finite())
        .unwrap_or(item.score);
    let (entity_type, capabilities) = entity_capability_profile(&item.entity);
    let distance = item
        .components
        .vector_similarity
        .and_then(|value| (value.is_finite()).then_some((1.0 - value).max(0.0)));

    let mut object = Map::new();
    object.insert("entity".to_string(), entity_to_json(&item.entity));
    object.insert("score".to_string(), JsonValue::Number(item.score as f64));
    object.insert("final_score".to_string(), JsonValue::Number(score as f64));
    object.insert("_score".to_string(), JsonValue::Number(score as f64));
    object.insert(
        "entity_id".to_string(),
        JsonValue::Number(item.entity.id.raw() as f64),
    );
    object.insert(
        "_entity_id".to_string(),
        JsonValue::Number(item.entity.id.raw() as f64),
    );
    object.insert(
        "_collection".to_string(),
        JsonValue::String(item.entity.kind.collection().to_string()),
    );
    object.insert(
        "_kind".to_string(),
        JsonValue::String(item.entity.kind.storage_type().to_string()),
    );
    object.insert("_entity_type".to_string(), JsonValue::String(entity_type));
    object.insert("_capabilities".to_string(), JsonValue::String(capabilities));
    object.insert(
        "_created_at".to_string(),
        JsonValue::Number(item.entity.created_at as f64),
    );
    object.insert(
        "_updated_at".to_string(),
        JsonValue::Number(item.entity.updated_at as f64),
    );
    object.insert(
        "_sequence_id".to_string(),
        JsonValue::Number(item.entity.sequence_id as f64),
    );
    object.insert(
        "distance".to_string(),
        distance
            .map(|value| JsonValue::Number(value as f64))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "_distance".to_string(),
        distance
            .map(|value| JsonValue::Number(value as f64))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "vector_distance".to_string(),
        distance
            .map(|value| JsonValue::Number(value as f64))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "components".to_string(),
        match_components_json(&item.components),
    );
    object.insert(
        "path".to_string(),
        match &item.path {
            Some(path) => JsonValue::Array(
                path.iter()
                    .map(|id| JsonValue::Number(id.raw() as f64))
                    .collect(),
            ),
            None => JsonValue::Null,
        },
    );
    JsonValue::Object(object)
}

fn entity_capability_profile(entity: &UnifiedEntity) -> (String, String) {
    match (&entity.kind, &entity.data) {
        (crate::storage::EntityKind::TableRow { .. }, crate::storage::EntityData::Row(row)) => {
            let entity_type = if row_is_kv(row) { "kv" } else { "table" };
            let mut capabilities = vec!["table".to_string(), "structured".to_string()];

            if row_is_kv(row) {
                capabilities.push("kv".to_string());
            }
            if row_has_document_capability(row) {
                capabilities.push("document".to_string());
            }

            (entity_type.to_string(), capabilities.join(","))
        }
        (crate::storage::EntityKind::GraphNode { .. }, crate::storage::EntityData::Node(_)) => {
            ("graph_node".to_string(), "graph,graph_node".to_string())
        }
        (crate::storage::EntityKind::GraphEdge { .. }, crate::storage::EntityData::Edge(_)) => {
            ("graph_edge".to_string(), "graph,graph_edge".to_string())
        }
        (crate::storage::EntityKind::Vector { .. }, crate::storage::EntityData::Vector(_)) => (
            "vector".to_string(),
            "vector,similarity,embedding".to_string(),
        ),
        _ => ("unknown".to_string(), "unknown".to_string()),
    }
}

fn row_is_kv(row: &crate::storage::RowData) -> bool {
    let Some(named) = row.named.as_ref() else {
        return false;
    };

    if named.len() == 2 {
        named.contains_key("key") && named.contains_key("value")
    } else if named.len() == 1 {
        named.contains_key("key") || named.contains_key("value")
    } else {
        false
    }
}

fn row_has_document_capability(row: &crate::storage::RowData) -> bool {
    row.named
        .as_ref()
        .map(|named| named.values().any(value_is_document_like))
        .unwrap_or(false)
        || row.columns.iter().any(value_is_document_like)
}

fn value_is_document_like(value: &crate::storage::schema::Value) -> bool {
    matches!(
        value,
        crate::storage::schema::Value::Json(_) | crate::storage::schema::Value::Blob(_)
    )
}

pub(crate) fn match_components_json(components: &MatchComponents) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "vector_similarity".to_string(),
        match components.vector_similarity {
            Some(value) => JsonValue::Number(value as f64),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "text_relevance".to_string(),
        match components.text_relevance {
            Some(value) => JsonValue::Number(value as f64),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "graph_match".to_string(),
        match components.graph_match {
            Some(value) => JsonValue::Number(value as f64),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "structured_match".to_string(),
        match components.structured_match {
            Some(value) => JsonValue::Number(value as f64),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "filter_match".to_string(),
        JsonValue::Bool(components.filter_match),
    );
    object.insert(
        "hop_distance".to_string(),
        match components.hop_distance {
            Some(value) => JsonValue::Number(value as f64),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "final_score".to_string(),
        match components.final_score {
            Some(value) => JsonValue::Number(value as f64),
            None => JsonValue::Null,
        },
    );
    JsonValue::Object(object)
}
