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
        crate::presentation::admin_json::graph_projections_json(
            &snapshot.declared_graph_projections,
        ),
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
        JsonValue::Array(
            snapshot
                .index_statuses
                .iter()
                .map(index_status_json)
                .collect(),
        ),
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
    object.insert("queryable".to_string(), JsonValue::Bool(status.queryable));
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
    object.insert("queryable".to_string(), JsonValue::Bool(status.queryable));
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
    object.insert("executable".to_string(), JsonValue::Bool(status.executable));
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
        "contract_present".to_string(),
        JsonValue::Bool(descriptor.contract_present),
    );
    object.insert(
        "contract_origin".to_string(),
        descriptor
            .contract_origin
            .map(|origin| JsonValue::String(contract_origin_str(origin).to_string()))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "declared_model".to_string(),
        descriptor
            .declared_model
            .map(|model| JsonValue::String(collection_model_str(model).to_string()))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "observed_model".to_string(),
        JsonValue::String(collection_model_str(descriptor.observed_model).to_string()),
    );
    object.insert(
        "queue_mode".to_string(),
        descriptor
            .queue_mode
            .map(|mode| JsonValue::String(mode.as_str().to_string()))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "declared_schema_mode".to_string(),
        descriptor
            .declared_schema_mode
            .map(|mode| JsonValue::String(schema_mode_str(mode).to_string()))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "observed_schema_mode".to_string(),
        JsonValue::String(schema_mode_str(descriptor.observed_schema_mode).to_string()),
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

pub(crate) fn catalog_collection_readiness_json(collections: &[CollectionDescriptor]) -> JsonValue {
    JsonValue::Array(collections.iter().map(collection_readiness_json).collect())
}

pub(crate) fn catalog_collection_attention_json(collections: &[CollectionDescriptor]) -> JsonValue {
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
        "contract_present".to_string(),
        JsonValue::Bool(descriptor.contract_present),
    );
    object.insert(
        "contract_origin".to_string(),
        descriptor
            .contract_origin
            .map(|origin| JsonValue::String(contract_origin_str(origin).to_string()))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "declared_model".to_string(),
        descriptor
            .declared_model
            .map(|model| JsonValue::String(collection_model_str(model).to_string()))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "observed_model".to_string(),
        JsonValue::String(collection_model_str(descriptor.observed_model).to_string()),
    );
    object.insert(
        "vector_dimension".to_string(),
        descriptor
            .vector_dimension
            .map(|dimension| JsonValue::Number(dimension as f64))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "vector_metric".to_string(),
        descriptor
            .vector_metric
            .map(|metric| JsonValue::String(distance_metric_str(metric).to_string()))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "declared_schema_mode".to_string(),
        descriptor
            .declared_schema_mode
            .map(|mode| JsonValue::String(schema_mode_str(mode).to_string()))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "observed_schema_mode".to_string(),
        JsonValue::String(schema_mode_str(descriptor.observed_schema_mode).to_string()),
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

/// Capability tags advertised to the Red UI for a collection. The
/// "primary" tag mirrors the model kind so the UI never has to map an
/// enum twice; the rest enumerate the action surfaces the toolbar may
/// expose for that model. Action *authorization* is reported separately
/// in the `actions` object — these tags only say which actions are
/// shaped by the model.
fn ui_capabilities_for_model(model: CollectionModel) -> Vec<&'static str> {
    match model {
        CollectionModel::Table => vec![
            "select",
            "insert",
            "update",
            "delete",
            "describe",
            "create_index",
            "drop_collection",
        ],
        CollectionModel::Document => vec![
            "select",
            "insert",
            "update",
            "delete",
            "describe",
            "json_path",
            "drop_collection",
        ],
        CollectionModel::Queue => vec![
            "push",
            "peek",
            "read",
            "ack",
            "nack",
            "dlq_move",
            "purge",
            "describe",
            "drop_collection",
        ],
        CollectionModel::Vector => vec!["upsert", "search", "describe", "drop_collection"],
        CollectionModel::Graph => vec![
            "nodes",
            "edges",
            "traverse",
            "subgraph",
            "describe",
            "drop_collection",
        ],
        CollectionModel::Kv => vec![
            "get",
            "put",
            "delete",
            "list_by_prefix",
            "increment",
            "compare_and_set",
            "describe",
            "drop_collection",
        ],
        CollectionModel::TimeSeries => vec![
            "insert",
            "range_query",
            "downsample",
            "describe",
            "drop_collection",
        ],
        CollectionModel::Metrics => vec!["query", "describe", "drop_collection"],
        CollectionModel::Hll
        | CollectionModel::Sketch
        | CollectionModel::Filter
        | CollectionModel::Config
        | CollectionModel::Vault
        | CollectionModel::Mixed => vec!["describe", "drop_collection"],
    }
}

/// Tri-state action decision rendered as a JSON object.
///
/// `Allowed(true|false)` is authoritative; the backend can prove the
/// answer with the auth state at hand. `Unknown` is reserved for
/// actions whose authorization depends on policy slices that are not
/// yet implemented (#740/#741) — the UI must treat these as
/// "indeterminate", not "permitted". The accompanying `reason`
/// explains either the denial or the deferral.
fn ui_action_decision(allowed: Option<bool>, reason: Option<&str>) -> JsonValue {
    let mut object = Map::new();
    match allowed {
        Some(value) => object.insert("allowed".to_string(), JsonValue::Bool(value)),
        None => object.insert(
            "allowed".to_string(),
            JsonValue::String("unknown".to_string()),
        ),
    };
    object.insert(
        "reason".to_string(),
        reason
            .map(|r| JsonValue::String(r.to_string()))
            .unwrap_or(JsonValue::Null),
    );
    JsonValue::Object(object)
}

/// Red UI collection metadata contract (issue #736, PRD #735).
///
/// One stable payload that lets the UI specialize empty and non-empty
/// collections without probing data rows: model kind, primary +
/// secondary capabilities, schema + system-column summary, indexes,
/// retention/TTL, tenant scope, entity count, and current-principal
/// action affordances. Per the thread-discussion decision on #736, the
/// actions envelope is partial: actions backed by today's coarse
/// `Role` get a boolean answer; finer-grained actions return
/// `"allowed":"unknown"` with a reason pointing at the deferred
/// security slices (#740/#741) — never a fake `true`/`false`.
pub(crate) fn collection_ui_metadata_json(
    descriptor: &CollectionDescriptor,
    contract: Option<&crate::physical::CollectionContract>,
    default_ttl_ms: Option<u64>,
    role: Option<crate::auth::Role>,
    auth_enabled: bool,
) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "name".to_string(),
        JsonValue::String(descriptor.name.clone()),
    );
    let model = descriptor.model;
    let model_str = collection_model_str(model);
    object.insert(
        "model".to_string(),
        JsonValue::String(model_str.to_string()),
    );
    object.insert(
        "primary_capability".to_string(),
        JsonValue::String(model_str.to_string()),
    );
    object.insert(
        "capabilities".to_string(),
        JsonValue::Array(
            ui_capabilities_for_model(model)
                .into_iter()
                .map(|s| JsonValue::String(s.to_string()))
                .collect(),
        ),
    );

    // Schema + system-column summary. Strict tables get the typed
    // column list; non-table / dynamic collections expose only the
    // mode + system columns so the UI can render a useful header
    // without probing rows.
    let mut schema = Map::new();
    schema.insert(
        "mode".to_string(),
        JsonValue::String(schema_mode_str(descriptor.schema_mode).to_string()),
    );
    schema.insert(
        "declared_mode".to_string(),
        descriptor
            .declared_schema_mode
            .map(|m| JsonValue::String(schema_mode_str(m).to_string()))
            .unwrap_or(JsonValue::Null),
    );
    schema.insert(
        "observed_mode".to_string(),
        JsonValue::String(schema_mode_str(descriptor.observed_schema_mode).to_string()),
    );
    schema.insert(
        "columns".to_string(),
        contract
            .map(|c| JsonValue::Array(contract_columns_json(c)))
            .unwrap_or(JsonValue::Null),
    );
    schema.insert(
        "system_columns".to_string(),
        JsonValue::Array(
            ["__id", "__created_at", "__updated_at"]
                .into_iter()
                .map(|s| JsonValue::String(s.to_string()))
                .collect(),
        ),
    );
    schema.insert(
        "timestamps_enabled".to_string(),
        JsonValue::Bool(contract.map(|c| c.timestamps_enabled).unwrap_or(false)),
    );
    schema.insert(
        "append_only".to_string(),
        JsonValue::Bool(contract.map(|c| c.append_only).unwrap_or(false)),
    );
    object.insert("schema".to_string(), JsonValue::Object(schema));

    let mut indexes = Map::new();
    indexes.insert(
        "declared".to_string(),
        JsonValue::Array(
            descriptor
                .declared_indices
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    indexes.insert(
        "operational".to_string(),
        JsonValue::Array(
            descriptor
                .operational_indices
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    indexes.insert(
        "effective".to_string(),
        JsonValue::Array(
            descriptor
                .indices
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    indexes.insert(
        "in_sync".to_string(),
        JsonValue::Bool(descriptor.indexes_in_sync),
    );
    object.insert("indexes".to_string(), JsonValue::Object(indexes));

    let mut retention = Map::new();
    retention.insert(
        "default_ttl_ms".to_string(),
        default_ttl_ms
            .map(|ms| JsonValue::Number(ms as f64))
            .unwrap_or(JsonValue::Null),
    );
    retention.insert(
        "retention_duration_ms".to_string(),
        descriptor
            .retention_duration_ms
            .map(|ms| JsonValue::Number(ms as f64))
            .unwrap_or(JsonValue::Null),
    );
    object.insert("retention".to_string(), JsonValue::Object(retention));

    // Tenant scope is not yet a per-collection fact in the catalog.
    // The UI gets an explicit "unknown" so it doesn't infer a global
    // scope from silence. #740/#741 add tenant-aware authorization.
    let mut tenant_scope = Map::new();
    tenant_scope.insert("kind".to_string(), JsonValue::String("unknown".to_string()));
    tenant_scope.insert(
        "reason".to_string(),
        JsonValue::String("tenant-aware collection ownership deferred to #740/#741".to_string()),
    );
    object.insert("tenant_scope".to_string(), JsonValue::Object(tenant_scope));

    object.insert(
        "entity_count".to_string(),
        JsonValue::Number(descriptor.entities as f64),
    );

    // Model-specific knobs so the UI can build the right toolbar
    // without re-decoding the catalog snapshot.
    let mut model_specific = Map::new();
    if matches!(model, CollectionModel::Queue) {
        model_specific.insert(
            "queue_mode".to_string(),
            descriptor
                .queue_mode
                .map(|m| JsonValue::String(m.as_str().to_string()))
                .unwrap_or(JsonValue::Null),
        );
        model_specific.insert(
            "max_attempts".to_string(),
            descriptor
                .queue_max_attempts
                .map(|v| JsonValue::Number(v as f64))
                .unwrap_or(JsonValue::Null),
        );
        model_specific.insert(
            "lock_deadline_ms".to_string(),
            descriptor
                .queue_lock_deadline_ms
                .map(|v| JsonValue::Number(v as f64))
                .unwrap_or(JsonValue::Null),
        );
        model_specific.insert(
            "in_flight_cap_per_group".to_string(),
            descriptor
                .queue_in_flight_cap_per_group
                .map(|v| JsonValue::Number(v as f64))
                .unwrap_or(JsonValue::Null),
        );
        model_specific.insert(
            "dlq_target".to_string(),
            descriptor
                .queue_dlq_target
                .as_ref()
                .map(|s| JsonValue::String(s.clone()))
                .unwrap_or(JsonValue::Null),
        );
    }
    if matches!(model, CollectionModel::Vector) {
        model_specific.insert(
            "dimension".to_string(),
            descriptor
                .vector_dimension
                .map(|v| JsonValue::Number(v as f64))
                .unwrap_or(JsonValue::Null),
        );
        model_specific.insert(
            "metric".to_string(),
            descriptor
                .vector_metric
                .map(|m| JsonValue::String(distance_metric_str(m).to_string()))
                .unwrap_or(JsonValue::Null),
        );
    }
    if matches!(model, CollectionModel::TimeSeries) {
        model_specific.insert(
            "session_key".to_string(),
            descriptor
                .session_key
                .as_ref()
                .map(|s| JsonValue::String(s.clone()))
                .unwrap_or(JsonValue::Null),
        );
        model_specific.insert(
            "session_gap_ms".to_string(),
            descriptor
                .session_gap_ms
                .map(|v| JsonValue::Number(v as f64))
                .unwrap_or(JsonValue::Null),
        );
    }
    object.insert(
        "model_specific".to_string(),
        JsonValue::Object(model_specific),
    );

    // Action affordances. Two-tier: coarse role-derived decisions get
    // a boolean; fine-grained operations the backend cannot prove
    // today get "unknown" with a pointer at #740/#741 (the thread
    // discussion on #736 makes this contract explicit).
    let can_read = role.map(|r| r.can_read()).unwrap_or(!auth_enabled);
    let can_write = role.map(|r| r.can_write()).unwrap_or(!auth_enabled);
    let can_admin = role.map(|r| r.can_admin()).unwrap_or(!auth_enabled);
    let deny_read_reason = if !can_read {
        Some("principal lacks read role")
    } else {
        None
    };
    let deny_write_reason = if !can_write {
        Some("principal lacks write role")
    } else {
        None
    };
    let deny_admin_reason = if !can_admin {
        Some("principal lacks admin role")
    } else {
        None
    };

    let mut actions = Map::new();
    actions.insert(
        "read".to_string(),
        ui_action_decision(Some(can_read), deny_read_reason),
    );
    actions.insert(
        "write".to_string(),
        ui_action_decision(Some(can_write), deny_write_reason),
    );
    actions.insert(
        "delete".to_string(),
        ui_action_decision(Some(can_write), deny_write_reason),
    );
    actions.insert(
        "create_index".to_string(),
        ui_action_decision(Some(can_write), deny_write_reason),
    );
    actions.insert(
        "drop_collection".to_string(),
        ui_action_decision(Some(can_admin), deny_admin_reason),
    );
    actions.insert(
        "alter_collection".to_string(),
        ui_action_decision(
            None,
            Some("tenant- and resource-scoped policy deferred to #740/#741"),
        ),
    );

    // Model-specific actions. Each one is keyed by the model so the
    // UI can render the correct toolbar without parsing capability
    // tags. Authorization on these is intentionally `unknown` until
    // the policy slices land — the coarse Role gate cannot prove
    // queue-level or vector-level decisions on its own.
    match model {
        CollectionModel::Queue => {
            for action in ["push", "peek", "ack", "nack", "purge", "dlq_move"] {
                let coarse = if action == "purge" || action == "dlq_move" {
                    Some(can_admin)
                } else {
                    Some(can_write)
                };
                let reason = match action {
                    "purge" | "dlq_move" => deny_admin_reason,
                    _ => deny_write_reason,
                };
                actions.insert(action.to_string(), ui_action_decision(coarse, reason));
            }
        }
        CollectionModel::Vector => {
            actions.insert(
                "search".to_string(),
                ui_action_decision(Some(can_read), deny_read_reason),
            );
            actions.insert(
                "upsert".to_string(),
                ui_action_decision(Some(can_write), deny_write_reason),
            );
        }
        CollectionModel::Graph => {
            actions.insert(
                "traverse".to_string(),
                ui_action_decision(Some(can_read), deny_read_reason),
            );
            actions.insert(
                "subgraph".to_string(),
                ui_action_decision(Some(can_read), deny_read_reason),
            );
        }
        CollectionModel::Kv => {
            actions.insert(
                "increment".to_string(),
                ui_action_decision(Some(can_write), deny_write_reason),
            );
            actions.insert(
                "compare_and_set".to_string(),
                ui_action_decision(Some(can_write), deny_write_reason),
            );
            actions.insert(
                "list_by_prefix".to_string(),
                ui_action_decision(Some(can_read), deny_read_reason),
            );
        }
        _ => {}
    }
    object.insert("actions".to_string(), JsonValue::Object(actions));

    object.insert(
        "attention_required".to_string(),
        JsonValue::Bool(descriptor.attention_required),
    );

    JsonValue::Object(object)
}

pub(crate) fn collection_contract_json(
    contract: &crate::physical::CollectionContract,
) -> JsonValue {
    let mut object = Map::new();
    object.insert("name".to_string(), JsonValue::String(contract.name.clone()));
    object.insert(
        "origin".to_string(),
        JsonValue::String(contract_origin_str(contract.origin).to_string()),
    );
    object.insert(
        "declared_model".to_string(),
        JsonValue::String(collection_model_str(contract.declared_model).to_string()),
    );
    object.insert(
        "schema_mode".to_string(),
        JsonValue::String(schema_mode_str(contract.schema_mode).to_string()),
    );
    object.insert(
        "version".to_string(),
        JsonValue::Number(contract.version as f64),
    );
    object.insert(
        "created_at_unix_ms".to_string(),
        JsonValue::Number(contract.created_at_unix_ms as f64),
    );
    object.insert(
        "updated_at_unix_ms".to_string(),
        JsonValue::Number(contract.updated_at_unix_ms as f64),
    );
    object.insert(
        "default_ttl_ms".to_string(),
        contract
            .default_ttl_ms
            .map(|ttl_ms| JsonValue::Number(ttl_ms as f64))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "context_index_fields".to_string(),
        JsonValue::Array(
            contract
                .context_index_fields
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "metrics_raw_retention_ms".to_string(),
        contract
            .metrics_raw_retention_ms
            .map(|ttl_ms| JsonValue::Number(ttl_ms as f64))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "metrics_tenant_identity".to_string(),
        contract
            .metrics_tenant_identity
            .as_ref()
            .map(|identity| JsonValue::String(identity.clone()))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "metrics_namespace".to_string(),
        contract
            .metrics_namespace
            .as_ref()
            .map(|namespace| JsonValue::String(namespace.clone()))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "columns".to_string(),
        JsonValue::Array(contract_columns_json(contract)),
    );
    object.insert(
        "table_def".to_string(),
        contract
            .table_def
            .as_ref()
            .map(table_def_json)
            .unwrap_or(JsonValue::Null),
    );
    JsonValue::Object(object)
}

fn contract_columns_json(contract: &crate::physical::CollectionContract) -> Vec<JsonValue> {
    if let Some(table_def) = &contract.table_def {
        return table_def
            .columns
            .iter()
            .map(|column| {
                let mut object = Map::new();
                object.insert("name".to_string(), JsonValue::String(column.name.clone()));
                object.insert(
                    "data_type".to_string(),
                    JsonValue::String(
                        column
                            .metadata
                            .get("ddl_data_type")
                            .cloned()
                            .unwrap_or_else(|| schema_data_type_str(column.data_type).to_string()),
                    ),
                );
                object.insert("not_null".to_string(), JsonValue::Bool(!column.nullable));
                object.insert(
                    "default".to_string(),
                    column
                        .default
                        .as_ref()
                        .map(|default| {
                            JsonValue::String(String::from_utf8_lossy(default).to_string())
                        })
                        .unwrap_or(JsonValue::Null),
                );
                object.insert("compress".to_string(), JsonValue::Bool(column.compress));
                object.insert(
                    "unique".to_string(),
                    JsonValue::Bool(
                        column
                            .metadata
                            .get("unique")
                            .map(|value| value == "true")
                            .unwrap_or(false),
                    ),
                );
                object.insert(
                    "primary_key".to_string(),
                    JsonValue::Bool(
                        column
                            .metadata
                            .get("primary_key")
                            .map(|value| value == "true")
                            .unwrap_or_else(|| {
                                table_def.primary_key.iter().any(|key| key == &column.name)
                            }),
                    ),
                );
                object.insert(
                    "enum_variants".to_string(),
                    JsonValue::Array(
                        column
                            .enum_variants
                            .iter()
                            .cloned()
                            .map(JsonValue::String)
                            .collect(),
                    ),
                );
                object.insert(
                    "decimal_precision".to_string(),
                    JsonValue::Number(column.decimal_precision as f64),
                );
                object.insert(
                    "array_element".to_string(),
                    column
                        .element_type
                        .map(|data_type| {
                            JsonValue::String(schema_data_type_str(data_type).to_string())
                        })
                        .unwrap_or(JsonValue::Null),
                );
                JsonValue::Object(object)
            })
            .collect();
    }

    contract
        .declared_columns
        .iter()
        .map(|column| {
            let mut object = Map::new();
            object.insert("name".to_string(), JsonValue::String(column.name.clone()));
            object.insert(
                "data_type".to_string(),
                JsonValue::String(column.data_type.clone()),
            );
            object.insert("not_null".to_string(), JsonValue::Bool(column.not_null));
            object.insert(
                "default".to_string(),
                column
                    .default
                    .clone()
                    .map(JsonValue::String)
                    .unwrap_or(JsonValue::Null),
            );
            object.insert(
                "compress".to_string(),
                column
                    .compress
                    .map(|value| JsonValue::Number(value as f64))
                    .unwrap_or(JsonValue::Null),
            );
            object.insert("unique".to_string(), JsonValue::Bool(column.unique));
            object.insert(
                "primary_key".to_string(),
                JsonValue::Bool(column.primary_key),
            );
            object.insert(
                "enum_variants".to_string(),
                JsonValue::Array(
                    column
                        .enum_variants
                        .iter()
                        .cloned()
                        .map(JsonValue::String)
                        .collect(),
                ),
            );
            object.insert(
                "decimal_precision".to_string(),
                column
                    .decimal_precision
                    .map(|value| JsonValue::Number(value as f64))
                    .unwrap_or(JsonValue::Null),
            );
            object.insert(
                "array_element".to_string(),
                column
                    .array_element
                    .clone()
                    .map(JsonValue::String)
                    .unwrap_or(JsonValue::Null),
            );
            JsonValue::Object(object)
        })
        .collect()
}

fn table_def_json(table_def: &crate::storage::schema::TableDef) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "name".to_string(),
        JsonValue::String(table_def.name.clone()),
    );
    object.insert(
        "primary_key".to_string(),
        JsonValue::Array(
            table_def
                .primary_key
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "column_count".to_string(),
        JsonValue::Number(table_def.columns.len() as f64),
    );
    object.insert(
        "index_count".to_string(),
        JsonValue::Number(table_def.indexes.len() as f64),
    );
    object.insert(
        "constraint_count".to_string(),
        JsonValue::Number(table_def.constraints.len() as f64),
    );
    JsonValue::Object(object)
}

fn collection_model_str(model: CollectionModel) -> &'static str {
    match model {
        CollectionModel::Table => "table",
        CollectionModel::Document => "document",
        CollectionModel::Graph => "graph",
        CollectionModel::Vector => "vector",
        CollectionModel::Hll => "hll",
        CollectionModel::Sketch => "sketch",
        CollectionModel::Filter => "filter",
        CollectionModel::Kv => "kv",
        CollectionModel::Config => "config",
        CollectionModel::Vault => "vault",
        CollectionModel::Mixed => "mixed",
        CollectionModel::TimeSeries => "timeseries",
        CollectionModel::Queue => "queue",
        CollectionModel::Metrics => "metrics",
    }
}

fn distance_metric_str(metric: crate::storage::engine::distance::DistanceMetric) -> &'static str {
    match metric {
        crate::storage::engine::distance::DistanceMetric::L2 => "l2",
        crate::storage::engine::distance::DistanceMetric::Cosine => "cosine",
        crate::storage::engine::distance::DistanceMetric::InnerProduct => "inner_product",
    }
}

fn schema_mode_str(mode: SchemaMode) -> &'static str {
    match mode {
        SchemaMode::Strict => "strict",
        SchemaMode::SemiStructured => "semi_structured",
        SchemaMode::Dynamic => "dynamic",
    }
}

fn contract_origin_str(origin: crate::physical::ContractOrigin) -> &'static str {
    match origin {
        crate::physical::ContractOrigin::Explicit => "explicit",
        crate::physical::ContractOrigin::Implicit => "implicit",
        crate::physical::ContractOrigin::Migrated => "migrated",
    }
}

fn schema_data_type_str(data_type: crate::storage::schema::DataType) -> &'static str {
    match data_type {
        crate::storage::schema::DataType::Integer => "integer",
        crate::storage::schema::DataType::UnsignedInteger => "unsigned_integer",
        crate::storage::schema::DataType::Float => "float",
        crate::storage::schema::DataType::Text => "text",
        crate::storage::schema::DataType::Blob => "blob",
        crate::storage::schema::DataType::Boolean => "boolean",
        crate::storage::schema::DataType::Timestamp => "timestamp",
        crate::storage::schema::DataType::Duration => "duration",
        crate::storage::schema::DataType::IpAddr => "ipaddr",
        crate::storage::schema::DataType::MacAddr => "macaddr",
        crate::storage::schema::DataType::Vector => "vector",
        crate::storage::schema::DataType::Nullable => "nullable",
        crate::storage::schema::DataType::Unknown => "unknown",
        crate::storage::schema::DataType::Json => "json",
        crate::storage::schema::DataType::Uuid => "uuid",
        crate::storage::schema::DataType::NodeRef => "noderef",
        crate::storage::schema::DataType::EdgeRef => "edgeref",
        crate::storage::schema::DataType::VectorRef => "vectorref",
        crate::storage::schema::DataType::RowRef => "rowref",
        crate::storage::schema::DataType::Color => "color",
        crate::storage::schema::DataType::Email => "email",
        crate::storage::schema::DataType::Url => "url",
        crate::storage::schema::DataType::Phone => "phone",
        crate::storage::schema::DataType::Semver => "semver",
        crate::storage::schema::DataType::Cidr => "cidr",
        crate::storage::schema::DataType::Date => "date",
        crate::storage::schema::DataType::Time => "time",
        crate::storage::schema::DataType::Decimal => "decimal",
        crate::storage::schema::DataType::Enum => "enum",
        crate::storage::schema::DataType::Array => "array",
        crate::storage::schema::DataType::TimestampMs => "timestamp_ms",
        crate::storage::schema::DataType::Ipv4 => "ipv4",
        crate::storage::schema::DataType::Ipv6 => "ipv6",
        crate::storage::schema::DataType::Subnet => "subnet",
        crate::storage::schema::DataType::Port => "port",
        crate::storage::schema::DataType::Latitude => "latitude",
        crate::storage::schema::DataType::Longitude => "longitude",
        crate::storage::schema::DataType::GeoPoint => "geopoint",
        crate::storage::schema::DataType::Country2 => "country2",
        crate::storage::schema::DataType::Country3 => "country3",
        crate::storage::schema::DataType::Lang2 => "lang2",
        crate::storage::schema::DataType::Lang5 => "lang5",
        crate::storage::schema::DataType::Currency => "currency",
        crate::storage::schema::DataType::AssetCode => "asset_code",
        crate::storage::schema::DataType::Money => "money",
        crate::storage::schema::DataType::ColorAlpha => "color_alpha",
        crate::storage::schema::DataType::BigInt => "bigint",
        crate::storage::schema::DataType::KeyRef => "keyref",
        crate::storage::schema::DataType::DocRef => "docref",
        crate::storage::schema::DataType::TableRef => "tableref",
        crate::storage::schema::DataType::PageRef => "pageref",
        crate::storage::schema::DataType::Secret => "secret",
        crate::storage::schema::DataType::Password => "password",
        crate::storage::schema::DataType::TextZstd => "text",
        crate::storage::schema::DataType::BlobZstd => "blob",
    }
}

fn unix_ms(value: SystemTime) -> u128 {
    value
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
