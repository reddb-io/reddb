use crate::health::{HealthReport, HealthState};
use crate::json::{Map, Value as JsonValue};
use crate::storage::unified::devx::PhysicalAuthorityStatus;
use crate::storage::unified::store::NativeRegistrySummary;

pub(crate) fn health_json(report: &HealthReport) -> JsonValue {
    let issues = report
        .issues
        .iter()
        .map(|issue| {
            let mut object = Map::new();
            object.insert(
                "component".to_string(),
                JsonValue::String(issue.component.clone()),
            );
            object.insert(
                "message".to_string(),
                JsonValue::String(issue.message.clone()),
            );
            JsonValue::Object(object)
        })
        .collect();

    let diagnostics = report
        .diagnostics
        .iter()
        .map(|(key, value)| (key.clone(), JsonValue::String(value.clone())))
        .collect();

    let mut object = Map::new();
    object.insert(
        "state".to_string(),
        JsonValue::String(
            match report.state {
                HealthState::Healthy => "healthy",
                HealthState::Degraded => "degraded",
                HealthState::Unhealthy => "unhealthy",
            }
            .to_string(),
        ),
    );
    object.insert("issues".to_string(), JsonValue::Array(issues));
    object.insert("diagnostics".to_string(), JsonValue::Object(diagnostics));
    object.insert(
        "checked_at_unix_ms".to_string(),
        JsonValue::Number(report.checked_at_unix_ms as f64),
    );
    JsonValue::Object(object)
}

pub(crate) fn catalog_readiness_json(
    query_ready: bool,
    write_ready: bool,
    repair_ready: bool,
    health: &HealthReport,
    authority: &PhysicalAuthorityStatus,
) -> JsonValue {
    let mut object = Map::new();
    object.insert("query_ready".to_string(), JsonValue::Bool(query_ready));
    object.insert("write_ready".to_string(), JsonValue::Bool(write_ready));
    object.insert("repair_ready".to_string(), JsonValue::Bool(repair_ready));
    object.insert("health".to_string(), health_json(health));
    object.insert(
        "authority".to_string(),
        physical_authority_status_json(authority),
    );
    JsonValue::Object(object)
}

pub(crate) fn native_registry_summary_json(summary: &NativeRegistrySummary) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "collection_count".to_string(),
        JsonValue::Number(summary.collection_count as f64),
    );
    object.insert(
        "index_count".to_string(),
        JsonValue::Number(summary.index_count as f64),
    );
    object.insert(
        "graph_projection_count".to_string(),
        JsonValue::Number(summary.graph_projection_count as f64),
    );
    object.insert(
        "analytics_job_count".to_string(),
        JsonValue::Number(summary.analytics_job_count as f64),
    );
    object.insert(
        "vector_artifact_count".to_string(),
        JsonValue::Number(summary.vector_artifact_count as f64),
    );
    object.insert(
        "collections_complete".to_string(),
        JsonValue::Bool(summary.collections_complete),
    );
    object.insert(
        "indexes_complete".to_string(),
        JsonValue::Bool(summary.indexes_complete),
    );
    object.insert(
        "graph_projections_complete".to_string(),
        JsonValue::Bool(summary.graph_projections_complete),
    );
    object.insert(
        "analytics_jobs_complete".to_string(),
        JsonValue::Bool(summary.analytics_jobs_complete),
    );
    object.insert(
        "vector_artifacts_complete".to_string(),
        JsonValue::Bool(summary.vector_artifacts_complete),
    );
    object.insert(
        "omitted_collection_count".to_string(),
        JsonValue::Number(summary.omitted_collection_count as f64),
    );
    object.insert(
        "omitted_index_count".to_string(),
        JsonValue::Number(summary.omitted_index_count as f64),
    );
    object.insert(
        "omitted_graph_projection_count".to_string(),
        JsonValue::Number(summary.omitted_graph_projection_count as f64),
    );
    object.insert(
        "omitted_analytics_job_count".to_string(),
        JsonValue::Number(summary.omitted_analytics_job_count as f64),
    );
    object.insert(
        "omitted_vector_artifact_count".to_string(),
        JsonValue::Number(summary.omitted_vector_artifact_count as f64),
    );
    object.insert(
        "collection_names".to_string(),
        JsonValue::Array(
            summary
                .collection_names
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "indexes".to_string(),
        JsonValue::Array(
            summary
                .indexes
                .iter()
                .map(|index| {
                    let mut item = Map::new();
                    item.insert("name".to_string(), JsonValue::String(index.name.clone()));
                    item.insert("kind".to_string(), JsonValue::String(index.kind.clone()));
                    item.insert(
                        "collection".to_string(),
                        match &index.collection {
                            Some(collection) => JsonValue::String(collection.clone()),
                            None => JsonValue::Null,
                        },
                    );
                    item.insert("enabled".to_string(), JsonValue::Bool(index.enabled));
                    item.insert(
                        "entries".to_string(),
                        JsonValue::String(index.entries.to_string()),
                    );
                    item.insert(
                        "estimated_memory_bytes".to_string(),
                        JsonValue::String(index.estimated_memory_bytes.to_string()),
                    );
                    item.insert(
                        "last_refresh_ms".to_string(),
                        match index.last_refresh_ms {
                            Some(value) => JsonValue::String(value.to_string()),
                            None => JsonValue::Null,
                        },
                    );
                    item.insert(
                        "backend".to_string(),
                        JsonValue::String(index.backend.clone()),
                    );
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    object.insert(
        "graph_projections".to_string(),
        JsonValue::Array(
            summary
                .graph_projections
                .iter()
                .map(|projection| {
                    let mut item = Map::new();
                    item.insert(
                        "name".to_string(),
                        JsonValue::String(projection.name.clone()),
                    );
                    item.insert(
                        "source".to_string(),
                        JsonValue::String(projection.source.clone()),
                    );
                    item.insert(
                        "created_at_unix_ms".to_string(),
                        JsonValue::String(projection.created_at_unix_ms.to_string()),
                    );
                    item.insert(
                        "updated_at_unix_ms".to_string(),
                        JsonValue::String(projection.updated_at_unix_ms.to_string()),
                    );
                    item.insert(
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
                    item.insert(
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
                    item.insert(
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
                    item.insert(
                        "last_materialized_sequence".to_string(),
                        match projection.last_materialized_sequence {
                            Some(value) => JsonValue::String(value.to_string()),
                            None => JsonValue::Null,
                        },
                    );
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    object.insert(
        "analytics_jobs".to_string(),
        JsonValue::Array(
            summary
                .analytics_jobs
                .iter()
                .map(|job| {
                    let mut item = Map::new();
                    item.insert("id".to_string(), JsonValue::String(job.id.clone()));
                    item.insert("kind".to_string(), JsonValue::String(job.kind.clone()));
                    item.insert(
                        "projection".to_string(),
                        match &job.projection {
                            Some(projection) => JsonValue::String(projection.clone()),
                            None => JsonValue::Null,
                        },
                    );
                    item.insert("state".to_string(), JsonValue::String(job.state.clone()));
                    item.insert(
                        "created_at_unix_ms".to_string(),
                        JsonValue::String(job.created_at_unix_ms.to_string()),
                    );
                    item.insert(
                        "updated_at_unix_ms".to_string(),
                        JsonValue::String(job.updated_at_unix_ms.to_string()),
                    );
                    item.insert(
                        "last_run_sequence".to_string(),
                        match job.last_run_sequence {
                            Some(value) => JsonValue::String(value.to_string()),
                            None => JsonValue::Null,
                        },
                    );
                    item.insert(
                        "metadata".to_string(),
                        JsonValue::Object(
                            job.metadata
                                .iter()
                                .map(|(key, value)| (key.clone(), JsonValue::String(value.clone())))
                                .collect(),
                        ),
                    );
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    object.insert(
        "vector_artifacts".to_string(),
        JsonValue::Array(
            summary
                .vector_artifacts
                .iter()
                .map(|artifact| {
                    let mut item = Map::new();
                    item.insert(
                        "collection".to_string(),
                        JsonValue::String(artifact.collection.clone()),
                    );
                    item.insert(
                        "artifact_kind".to_string(),
                        JsonValue::String(artifact.artifact_kind.clone()),
                    );
                    item.insert(
                        "vector_count".to_string(),
                        JsonValue::String(artifact.vector_count.to_string()),
                    );
                    item.insert(
                        "dimension".to_string(),
                        JsonValue::Number(artifact.dimension as f64),
                    );
                    item.insert(
                        "max_layer".to_string(),
                        JsonValue::Number(artifact.max_layer as f64),
                    );
                    item.insert(
                        "serialized_bytes".to_string(),
                        JsonValue::String(artifact.serialized_bytes.to_string()),
                    );
                    item.insert(
                        "checksum".to_string(),
                        JsonValue::String(artifact.checksum.to_string()),
                    );
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

pub(crate) fn physical_authority_status_json(status: &PhysicalAuthorityStatus) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "preference".to_string(),
        JsonValue::String(status.preference.clone()),
    );
    object.insert(
        "sidecar_available".to_string(),
        JsonValue::Bool(status.sidecar_available),
    );
    object.insert(
        "native_state_available".to_string(),
        JsonValue::Bool(status.native_state_available),
    );
    object.insert(
        "native_bootstrap_ready".to_string(),
        JsonValue::Bool(status.native_bootstrap_ready),
    );
    object.insert(
        "native_registry_complete".to_string(),
        match status.native_registry_complete {
            Some(value) => JsonValue::Bool(value),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "native_recovery_complete".to_string(),
        match status.native_recovery_complete {
            Some(value) => JsonValue::Bool(value),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "native_catalog_complete".to_string(),
        match status.native_catalog_complete {
            Some(value) => JsonValue::Bool(value),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "sidecar_loaded_from".to_string(),
        match &status.sidecar_loaded_from {
            Some(value) => JsonValue::String(value.clone()),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "native_header_repair_policy".to_string(),
        match &status.native_header_repair_policy {
            Some(value) => JsonValue::String(value.clone()),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "metadata_sequence".to_string(),
        match status.metadata_sequence {
            Some(value) => JsonValue::String(value.to_string()),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "native_sequence".to_string(),
        match status.native_sequence {
            Some(value) => JsonValue::String(value.to_string()),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "native_metadata_last_loaded_from".to_string(),
        match &status.native_metadata_last_loaded_from {
            Some(value) => JsonValue::String(value.clone()),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "native_metadata_generated_at_unix_ms".to_string(),
        match status.native_metadata_generated_at_unix_ms {
            Some(value) => JsonValue::String(value.to_string()),
            None => JsonValue::Null,
        },
    );
    JsonValue::Object(object)
}
