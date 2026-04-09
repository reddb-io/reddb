use crate::catalog::{
    CatalogAnalyticsJobStatus, CatalogAttentionSummary, CatalogConsistencyReport,
    CatalogGraphProjectionStatus, CatalogIndexStatus, CatalogModelSnapshot, CollectionDescriptor,
    CollectionModel, SchemaMode,
};
use crate::json::{Map, Value as JsonValue};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn readiness_json(kind: &str, ready: bool) -> JsonValue {
    let mut object = Map::new();
    object.insert("kind".to_string(), JsonValue::String(kind.to_string()));
    object.insert("ready".to_string(), JsonValue::Bool(ready));
    JsonValue::Object(object)
}

pub(crate) fn catalog_model_snapshot_json(snapshot: &CatalogModelSnapshot) -> JsonValue {
    let mut summary_stats = Map::new();
    for (name, stats) in &snapshot.summary.stats_by_collection {
        let mut object = Map::new();
        object.insert(
            "entities".to_string(),
            JsonValue::Number(stats.entities as f64),
        );
        object.insert(
            "cross_refs".to_string(),
            JsonValue::Number(stats.cross_refs as f64),
        );
        object.insert(
            "segments".to_string(),
            JsonValue::Number(stats.segments as f64),
        );
        summary_stats.insert(name.clone(), JsonValue::Object(object));
    }

    let collections = snapshot
        .collections
        .iter()
        .map(collection_descriptor_json)
        .collect();

    let mut summary = Map::new();
    summary.insert(
        "name".to_string(),
        JsonValue::String(snapshot.summary.name.clone()),
    );
    summary.insert(
        "total_entities".to_string(),
        JsonValue::Number(snapshot.summary.total_entities as f64),
    );
    summary.insert(
        "total_collections".to_string(),
        JsonValue::Number(snapshot.summary.total_collections as f64),
    );
    summary.insert(
        "updated_at_unix_ms".to_string(),
        JsonValue::Number(unix_ms(snapshot.summary.updated_at) as f64),
    );
    summary.insert(
        "stats_by_collection".to_string(),
        JsonValue::Object(summary_stats),
    );
    summary.insert(
        "queryable_index_count".to_string(),
        JsonValue::Number(snapshot.queryable_index_count as f64),
    );
    summary.insert(
        "indexes_requiring_rebuild_count".to_string(),
        JsonValue::Number(snapshot.indexes_requiring_rebuild_count as f64),
    );
    summary.insert(
        "queryable_graph_projection_count".to_string(),
        JsonValue::Number(snapshot.queryable_graph_projection_count as f64),
    );
    summary.insert(
        "graph_projections_requiring_rematerialization_count".to_string(),
        JsonValue::Number(snapshot.graph_projections_requiring_rematerialization_count as f64),
    );
    summary.insert(
        "executable_analytics_job_count".to_string(),
        JsonValue::Number(snapshot.executable_analytics_job_count as f64),
    );
    summary.insert(
        "analytics_jobs_requiring_rerun_count".to_string(),
        JsonValue::Number(snapshot.analytics_jobs_requiring_rerun_count as f64),
    );

    let mut object = Map::new();
    object.insert("summary".to_string(), JsonValue::Object(summary));
    object.insert("collections".to_string(), JsonValue::Array(collections));
    object.insert(
        "declared_indexes".to_string(),
        crate::presentation::admin_json::indexes_json(&snapshot.declared_indexes),
    );
    object.insert(
        "declared_graph_projections".to_string(),
        crate::presentation::admin_json::graph_projections_json(&snapshot.declared_graph_projections),
    );
    object.insert(
        "declared_analytics_jobs".to_string(),
        crate::presentation::admin_json::analytics_jobs_json(&snapshot.declared_analytics_jobs),
    );
    object.insert(
        "operational_indexes".to_string(),
        crate::presentation::admin_json::indexes_json(&snapshot.operational_indexes),
    );
    object.insert(
        "operational_graph_projections".to_string(),
        crate::presentation::admin_json::graph_projections_json(
            &snapshot.operational_graph_projections,
        ),
    );
    object.insert(
        "operational_analytics_jobs".to_string(),
        crate::presentation::admin_json::analytics_jobs_json(&snapshot.operational_analytics_jobs),
    );
    object.insert(
        "index_statuses".to_string(),
        JsonValue::Array(snapshot.index_statuses.iter().map(index_status_json).collect()),
    );
    object.insert(
        "graph_projection_statuses".to_string(),
        JsonValue::Array(
            snapshot
                .graph_projection_statuses
                .iter()
                .map(graph_projection_status_json)
                .collect(),
        ),
    );
    object.insert(
        "analytics_job_statuses".to_string(),
        JsonValue::Array(
            snapshot
                .analytics_job_statuses
                .iter()
                .map(analytics_job_status_json)
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

pub(crate) fn catalog_model_snapshot_with_readiness_json(
    snapshot: &CatalogModelSnapshot,
    readiness: JsonValue,
) -> JsonValue {
    let mut object = match catalog_model_snapshot_json(snapshot) {
        JsonValue::Object(object) => object,
        _ => Map::new(),
    };
    object.insert("readiness".to_string(), readiness);
    JsonValue::Object(object)
}

pub(crate) fn catalog_attention_summary_json(summary: &CatalogAttentionSummary) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "collections_requiring_attention".to_string(),
        JsonValue::Number(summary.collections_requiring_attention as f64),
    );
    object.insert(
        "indexes_requiring_attention".to_string(),
        JsonValue::Number(summary.indexes_requiring_attention as f64),
    );
    object.insert(
        "graph_projections_requiring_attention".to_string(),
        JsonValue::Number(summary.graph_projections_requiring_attention as f64),
    );
    object.insert(
        "analytics_jobs_requiring_attention".to_string(),
        JsonValue::Number(summary.analytics_jobs_requiring_attention as f64),
    );
    object.insert(
        "top_collection".to_string(),
        summary
            .top_collection
            .as_ref()
            .map(collection_readiness_json)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "top_index".to_string(),
        summary
            .top_index
            .as_ref()
            .map(index_status_json)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "top_graph_projection".to_string(),
        summary
            .top_graph_projection
            .as_ref()
            .map(graph_projection_status_json)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "top_analytics_job".to_string(),
        summary
            .top_analytics_job
            .as_ref()
            .map(analytics_job_status_json)
            .unwrap_or(JsonValue::Null),
    );
    JsonValue::Object(object)
}

pub(crate) fn index_status_json(status: &CatalogIndexStatus) -> JsonValue {
    let mut object = Map::new();
    object.insert("name".to_string(), JsonValue::String(status.name.clone()));
    object.insert(
        "collection".to_string(),
        status
            .collection
            .clone()
            .map(JsonValue::String)
            .unwrap_or(JsonValue::Null),
    );
    object.insert("kind".to_string(), JsonValue::String(status.kind.clone()));
    object.insert("declared".to_string(), JsonValue::Bool(status.declared));
    object.insert(
        "operational".to_string(),
        JsonValue::Bool(status.operational),
    );
    object.insert("enabled".to_string(), JsonValue::Bool(status.enabled));
    object.insert(
        "build_state".to_string(),
        status
            .build_state
            .clone()
            .map(JsonValue::String)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "artifact_state".to_string(),
        JsonValue::String(status.artifact_state.as_str().to_string()),
    );
    object.insert(
        "queryable".to_string(),
        JsonValue::Bool(status.queryable),
    );
    object.insert(
        "requires_rebuild".to_string(),
        JsonValue::Bool(status.requires_rebuild),
    );
    object.insert(
        "attention_score".to_string(),
        JsonValue::Number(status.attention_score as f64),
    );
    object.insert(
        "attention_reasons".to_string(),
        JsonValue::Array(
            status
                .attention_reasons
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert("in_sync".to_string(), JsonValue::Bool(status.in_sync));
    JsonValue::Object(object)
}

pub(crate) fn catalog_index_statuses_json(statuses: &[CatalogIndexStatus]) -> JsonValue {
    JsonValue::Array(statuses.iter().map(index_status_json).collect())
}

pub(crate) fn catalog_index_attention_json(statuses: &[CatalogIndexStatus]) -> JsonValue {
    JsonValue::Array(
        statuses
            .iter()
            .filter(|status| status.attention_score > 0)
            .map(index_status_json)
            .collect(),
    )
}

pub(crate) fn graph_projection_status_json(status: &CatalogGraphProjectionStatus) -> JsonValue {
    let mut object = Map::new();
    object.insert("name".to_string(), JsonValue::String(status.name.clone()));
    object.insert(
        "source".to_string(),
        status
            .source
            .clone()
            .map(JsonValue::String)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "lifecycle_state".to_string(),
        JsonValue::String(status.lifecycle_state.clone()),
    );
    object.insert("declared".to_string(), JsonValue::Bool(status.declared));
    object.insert(
        "operational".to_string(),
        JsonValue::Bool(status.operational),
    );
    object.insert("in_sync".to_string(), JsonValue::Bool(status.in_sync));
    object.insert(
        "queryable".to_string(),
        JsonValue::Bool(status.queryable),
    );
    object.insert(
        "requires_rematerialization".to_string(),
        JsonValue::Bool(status.requires_rematerialization),
    );
    object.insert(
        "last_materialized_sequence".to_string(),
        status
            .last_materialized_sequence
            .map(|value| JsonValue::Number(value as f64))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "dependent_job_count".to_string(),
        JsonValue::Number(status.dependent_job_count as f64),
    );
    object.insert(
        "active_dependent_job_count".to_string(),
        JsonValue::Number(status.active_dependent_job_count as f64),
    );
    object.insert(
        "stale_dependent_job_count".to_string(),
        JsonValue::Number(status.stale_dependent_job_count as f64),
    );
    object.insert(
        "dependent_jobs_in_sync".to_string(),
        JsonValue::Bool(status.dependent_jobs_in_sync),
    );
    object.insert(
        "rerun_required".to_string(),
        JsonValue::Bool(status.rerun_required),
    );
    object.insert(
        "attention_score".to_string(),
        JsonValue::Number(status.attention_score as f64),
    );
    object.insert(
        "attention_reasons".to_string(),
        JsonValue::Array(
            status
                .attention_reasons
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

pub(crate) fn catalog_graph_projection_statuses_json(
    statuses: &[CatalogGraphProjectionStatus],
) -> JsonValue {
    JsonValue::Array(statuses.iter().map(graph_projection_status_json).collect())
}

pub(crate) fn catalog_graph_projection_attention_json(
    statuses: &[CatalogGraphProjectionStatus],
) -> JsonValue {
    JsonValue::Array(
        statuses
            .iter()
            .filter(|status| status.attention_score > 0)
            .map(graph_projection_status_json)
            .collect(),
    )
}

pub(crate) fn analytics_job_status_json(status: &CatalogAnalyticsJobStatus) -> JsonValue {
    let mut object = Map::new();
    object.insert("id".to_string(), JsonValue::String(status.id.clone()));
    object.insert("kind".to_string(), JsonValue::String(status.kind.clone()));
    object.insert(
        "projection".to_string(),
        status
            .projection
            .clone()
            .map(JsonValue::String)
            .unwrap_or(JsonValue::Null),
    );
    object.insert("state".to_string(), JsonValue::String(status.state.clone()));
    object.insert(
        "lifecycle_state".to_string(),
        JsonValue::String(status.lifecycle_state.clone()),
    );
    object.insert("declared".to_string(), JsonValue::Bool(status.declared));
    object.insert(
        "operational".to_string(),
        JsonValue::Bool(status.operational),
    );
    object.insert("in_sync".to_string(), JsonValue::Bool(status.in_sync));
    object.insert(
        "last_run_sequence".to_string(),
        status
            .last_run_sequence
            .map(|value| JsonValue::Number(value as f64))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "projection_declared".to_string(),
        status
            .projection_declared
            .map(JsonValue::Bool)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "projection_operational".to_string(),
        status
            .projection_operational
            .map(JsonValue::Bool)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "projection_lifecycle_state".to_string(),
        status
            .projection_lifecycle_state
            .clone()
            .map(JsonValue::String)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "dependency_in_sync".to_string(),
        status
            .dependency_in_sync
            .map(JsonValue::Bool)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "executable".to_string(),
        JsonValue::Bool(status.executable),
    );
    object.insert(
        "requires_rerun".to_string(),
        JsonValue::Bool(status.requires_rerun),
    );
    object.insert(
        "attention_score".to_string(),
        JsonValue::Number(status.attention_score as f64),
    );
    object.insert(
        "attention_reasons".to_string(),
        JsonValue::Array(
            status
                .attention_reasons
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

pub(crate) fn catalog_analytics_job_statuses_json(
    statuses: &[CatalogAnalyticsJobStatus],
) -> JsonValue {
    JsonValue::Array(statuses.iter().map(analytics_job_status_json).collect())
}

pub(crate) fn catalog_analytics_job_attention_json(
    statuses: &[CatalogAnalyticsJobStatus],
) -> JsonValue {
    JsonValue::Array(
        statuses
            .iter()
            .filter(|status| status.attention_score > 0)
            .map(analytics_job_status_json)
            .collect(),
    )
}

pub(crate) fn catalog_consistency_json(report: &CatalogConsistencyReport) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "declared_index_count".to_string(),
        JsonValue::Number(report.declared_index_count as f64),
    );
    object.insert(
        "operational_index_count".to_string(),
        JsonValue::Number(report.operational_index_count as f64),
    );
    object.insert(
        "declared_graph_projection_count".to_string(),
        JsonValue::Number(report.declared_graph_projection_count as f64),
    );
    object.insert(
        "operational_graph_projection_count".to_string(),
        JsonValue::Number(report.operational_graph_projection_count as f64),
    );
    object.insert(
        "declared_analytics_job_count".to_string(),
        JsonValue::Number(report.declared_analytics_job_count as f64),
    );
    object.insert(
        "operational_analytics_job_count".to_string(),
        JsonValue::Number(report.operational_analytics_job_count as f64),
    );
    let string_array = |values: &[String]| {
        JsonValue::Array(values.iter().cloned().map(JsonValue::String).collect())
    };
    object.insert(
        "missing_operational_indexes".to_string(),
        string_array(&report.missing_operational_indexes),
    );
    object.insert(
        "undeclared_operational_indexes".to_string(),
        string_array(&report.undeclared_operational_indexes),
    );
    object.insert(
        "missing_operational_graph_projections".to_string(),
        string_array(&report.missing_operational_graph_projections),
    );
    object.insert(
        "undeclared_operational_graph_projections".to_string(),
        string_array(&report.undeclared_operational_graph_projections),
    );
    object.insert(
        "missing_operational_analytics_jobs".to_string(),
        string_array(&report.missing_operational_analytics_jobs),
    );
    object.insert(
        "undeclared_operational_analytics_jobs".to_string(),
        string_array(&report.undeclared_operational_analytics_jobs),
    );
    JsonValue::Object(object)
}

pub(crate) fn collection_readiness_json(descriptor: &CollectionDescriptor) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "name".to_string(),
        JsonValue::String(descriptor.name.clone()),
    );
    object.insert(
        "model".to_string(),
        JsonValue::String(collection_model_str(descriptor.model).to_string()),
    );
    object.insert(
        "resources_in_sync".to_string(),
        JsonValue::Bool(descriptor.resources_in_sync),
    );
    object.insert(
        "attention_required".to_string(),
        JsonValue::Bool(descriptor.attention_required),
    );
    object.insert(
        "attention_score".to_string(),
        JsonValue::Number(descriptor.attention_score as f64),
    );
    object.insert(
        "attention_reasons".to_string(),
        JsonValue::Array(
            descriptor
                .attention_reasons
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "queryable_index_count".to_string(),
        JsonValue::Number(descriptor.queryable_index_count as f64),
    );
    object.insert(
        "indexes_requiring_rebuild_count".to_string(),
        JsonValue::Number(descriptor.indexes_requiring_rebuild_count as f64),
    );
    object.insert(
        "queryable_graph_projection_count".to_string(),
        JsonValue::Number(descriptor.queryable_graph_projection_count as f64),
    );
    object.insert(
        "graph_projections_requiring_rematerialization_count".to_string(),
        JsonValue::Number(descriptor.graph_projections_requiring_rematerialization_count as f64),
    );
    object.insert(
        "executable_analytics_job_count".to_string(),
        JsonValue::Number(descriptor.executable_analytics_job_count as f64),
    );
    object.insert(
        "analytics_jobs_requiring_rerun_count".to_string(),
        JsonValue::Number(descriptor.analytics_jobs_requiring_rerun_count as f64),
    );
    object.insert(
        "indexes_in_sync".to_string(),
        JsonValue::Bool(descriptor.indexes_in_sync),
    );
    JsonValue::Object(object)
}

pub(crate) fn catalog_collection_readiness_json(
    collections: &[CollectionDescriptor],
) -> JsonValue {
    JsonValue::Array(collections.iter().map(collection_readiness_json).collect())
}

pub(crate) fn catalog_collection_attention_json(
    collections: &[CollectionDescriptor],
) -> JsonValue {
    JsonValue::Array(
        collections
            .iter()
            .filter(|collection| collection.attention_required)
            .map(collection_readiness_json)
            .collect(),
    )
}

pub(crate) fn collection_descriptor_json(descriptor: &CollectionDescriptor) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "name".to_string(),
        JsonValue::String(descriptor.name.clone()),
    );
    object.insert(
        "model".to_string(),
        JsonValue::String(collection_model_str(descriptor.model).to_string()),
    );
    object.insert(
        "schema_mode".to_string(),
        JsonValue::String(schema_mode_str(descriptor.schema_mode).to_string()),
    );
    object.insert(
        "entities".to_string(),
        JsonValue::Number(descriptor.entities as f64),
    );
    object.insert(
        "cross_refs".to_string(),
        JsonValue::Number(descriptor.cross_refs as f64),
    );
    object.insert(
        "segments".to_string(),
        JsonValue::Number(descriptor.segments as f64),
    );
    object.insert(
        "indices".to_string(),
        JsonValue::Array(
            descriptor
                .indices
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "declared_indices".to_string(),
        JsonValue::Array(
            descriptor
                .declared_indices
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "operational_indices".to_string(),
        JsonValue::Array(
            descriptor
                .operational_indices
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "indexes_in_sync".to_string(),
        JsonValue::Bool(descriptor.indexes_in_sync),
    );
    object.insert(
        "missing_operational_indices".to_string(),
        JsonValue::Array(
            descriptor
                .missing_operational_indices
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "undeclared_operational_indices".to_string(),
        JsonValue::Array(
            descriptor
                .undeclared_operational_indices
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "queryable_index_count".to_string(),
        JsonValue::Number(descriptor.queryable_index_count as f64),
    );
    object.insert(
        "indexes_requiring_rebuild_count".to_string(),
        JsonValue::Number(descriptor.indexes_requiring_rebuild_count as f64),
    );
    object.insert(
        "queryable_graph_projection_count".to_string(),
        JsonValue::Number(descriptor.queryable_graph_projection_count as f64),
    );
    object.insert(
        "graph_projections_requiring_rematerialization_count".to_string(),
        JsonValue::Number(descriptor.graph_projections_requiring_rematerialization_count as f64),
    );
    object.insert(
        "executable_analytics_job_count".to_string(),
        JsonValue::Number(descriptor.executable_analytics_job_count as f64),
    );
    object.insert(
        "analytics_jobs_requiring_rerun_count".to_string(),
        JsonValue::Number(descriptor.analytics_jobs_requiring_rerun_count as f64),
    );
    object.insert(
        "resources_in_sync".to_string(),
        JsonValue::Bool(descriptor.resources_in_sync),
    );
    object.insert(
        "attention_required".to_string(),
        JsonValue::Bool(descriptor.attention_required),
    );
    JsonValue::Object(object)
}

fn collection_model_str(model: CollectionModel) -> &'static str {
    match model {
        CollectionModel::Table => "table",
        CollectionModel::Document => "document",
        CollectionModel::Graph => "graph",
        CollectionModel::Vector => "vector",
        CollectionModel::Mixed => "mixed",
    }
}

fn schema_mode_str(mode: SchemaMode) -> &'static str {
    match mode {
        SchemaMode::Strict => "strict",
        SchemaMode::SemiStructured => "semi_structured",
        SchemaMode::Dynamic => "dynamic",
    }
}

fn unix_ms(value: SystemTime) -> u128 {
    value
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
