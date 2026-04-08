use crate::json::{Map, Value as JsonValue};
use crate::runtime::RuntimeIvfSearchResult;
use crate::storage::query::unified::dsl::QueryResult as DslQueryResult;
use crate::storage::unified::devx::SimilarResult;
use crate::storage::{MatchComponents, ScoredMatch, UnifiedEntity};

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
                    item.insert(
                        "entity_id".to_string(),
                        JsonValue::Number(result.entity_id.raw() as f64),
                    );
                    item.insert("score".to_string(), JsonValue::Number(result.score as f64));
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
                    entry.insert(
                        "entity_id".to_string(),
                        JsonValue::Number(item.entity_id as f64),
                    );
                    entry.insert(
                        "distance".to_string(),
                        JsonValue::Number(item.distance as f64),
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
    let mut object = Map::new();
    object.insert(
        "matches".to_string(),
        JsonValue::Array(result.matches.iter().map(scored_match_to_json).collect()),
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
    let mut object = Map::new();
    object.insert("entity".to_string(), entity_to_json(&item.entity));
    object.insert("score".to_string(), JsonValue::Number(item.score as f64));
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
