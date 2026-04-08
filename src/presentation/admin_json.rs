use crate::json::{Map, Value as JsonValue};

pub(crate) fn indexes_json(indexes: &[crate::PhysicalIndexState]) -> JsonValue {
    JsonValue::Array(indexes.iter().map(index_json).collect())
}

pub(crate) fn index_json(index: &crate::PhysicalIndexState) -> JsonValue {
    let mut object = Map::new();
    object.insert("name".to_string(), JsonValue::String(index.name.clone()));
    object.insert(
        "kind".to_string(),
        JsonValue::String(index.kind.as_str().to_string()),
    );
    object.insert(
        "collection".to_string(),
        match &index.collection {
            Some(collection) => JsonValue::String(collection.clone()),
            None => JsonValue::Null,
        },
    );
    object.insert("enabled".to_string(), JsonValue::Bool(index.enabled));
    object.insert(
        "entries".to_string(),
        JsonValue::Number(index.entries as f64),
    );
    object.insert(
        "estimated_memory_bytes".to_string(),
        JsonValue::String(index.estimated_memory_bytes.to_string()),
    );
    object.insert(
        "last_refresh_ms".to_string(),
        match index.last_refresh_ms {
            Some(value) => JsonValue::String(value.to_string()),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "backend".to_string(),
        JsonValue::String(index.backend.clone()),
    );
    object.insert(
        "artifact_kind".to_string(),
        match &index.artifact_kind {
            Some(value) => JsonValue::String(value.clone()),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "artifact_root_page".to_string(),
        match index.artifact_root_page {
            Some(value) => JsonValue::Number(value as f64),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "artifact_checksum".to_string(),
        match index.artifact_checksum {
            Some(value) => JsonValue::String(value.to_string()),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "build_state".to_string(),
        JsonValue::String(index.build_state.clone()),
    );
    JsonValue::Object(object)
}

pub(crate) fn graph_projections_json(projections: &[crate::PhysicalGraphProjection]) -> JsonValue {
    JsonValue::Array(projections.iter().map(graph_projection_json).collect())
}

pub(crate) fn graph_projection_json(projection: &crate::PhysicalGraphProjection) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "name".to_string(),
        JsonValue::String(projection.name.clone()),
    );
    object.insert(
        "created_at_unix_ms".to_string(),
        JsonValue::String(projection.created_at_unix_ms.to_string()),
    );
    object.insert(
        "updated_at_unix_ms".to_string(),
        JsonValue::String(projection.updated_at_unix_ms.to_string()),
    );
    object.insert(
        "state".to_string(),
        JsonValue::String(projection.state.clone()),
    );
    object.insert(
        "source".to_string(),
        JsonValue::String(projection.source.clone()),
    );
    object.insert(
        "node_labels".to_string(),
        JsonValue::Array(
            projection
                .node_labels
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "node_types".to_string(),
        JsonValue::Array(
            projection
                .node_types
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "edge_labels".to_string(),
        JsonValue::Array(
            projection
                .edge_labels
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "last_materialized_sequence".to_string(),
        projection
            .last_materialized_sequence
            .map(|value| JsonValue::String(value.to_string()))
            .unwrap_or(JsonValue::Null),
    );
    JsonValue::Object(object)
}

pub(crate) fn analytics_jobs_json(jobs: &[crate::PhysicalAnalyticsJob]) -> JsonValue {
    JsonValue::Array(jobs.iter().map(analytics_job_json).collect())
}

pub(crate) fn analytics_job_json(job: &crate::PhysicalAnalyticsJob) -> JsonValue {
    let mut object = Map::new();
    object.insert("id".to_string(), JsonValue::String(job.id.clone()));
    object.insert("kind".to_string(), JsonValue::String(job.kind.clone()));
    object.insert("state".to_string(), JsonValue::String(job.state.clone()));
    object.insert(
        "projection".to_string(),
        match &job.projection {
            Some(projection) => JsonValue::String(projection.clone()),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "created_at_unix_ms".to_string(),
        JsonValue::String(job.created_at_unix_ms.to_string()),
    );
    object.insert(
        "updated_at_unix_ms".to_string(),
        JsonValue::String(job.updated_at_unix_ms.to_string()),
    );
    object.insert(
        "last_run_sequence".to_string(),
        job.last_run_sequence
            .map(|value| JsonValue::String(value.to_string()))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "metadata".to_string(),
        JsonValue::Object(
            job.metadata
                .iter()
                .map(|(key, value)| (key.clone(), JsonValue::String(value.clone())))
                .collect(),
        ),
    );
    JsonValue::Object(object)
}
