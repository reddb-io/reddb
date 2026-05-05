use crate::application::ServerlessWarmupPlan;
use crate::health::HealthReport;
use crate::json::{Map, Value as JsonValue};
use crate::storage::unified::devx::PhysicalAuthorityStatus;

pub(crate) fn serverless_readiness_summary_json<F, G>(
    query_ready: bool,
    write_ready: bool,
    repair_ready: bool,
    health: &HealthReport,
    authority: &PhysicalAuthorityStatus,
    health_to_json: F,
    authority_to_json: G,
) -> JsonValue
where
    F: Fn(&HealthReport) -> JsonValue,
    G: Fn(&PhysicalAuthorityStatus) -> JsonValue,
{
    let mut object = Map::new();
    object.insert("query_ready".to_string(), JsonValue::Bool(query_ready));
    object.insert("write_ready".to_string(), JsonValue::Bool(write_ready));
    object.insert("repair_ready".to_string(), JsonValue::Bool(repair_ready));
    object.insert("health".to_string(), health_to_json(health));
    object.insert("authority".to_string(), authority_to_json(authority));
    JsonValue::Object(object)
}

pub(crate) fn serverless_warmup_plan_json(plan: &ServerlessWarmupPlan) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "indexes".to_string(),
        JsonValue::Array(
            plan.indexes
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "graph_projections".to_string(),
        JsonValue::Array(
            plan.graph_projections
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "analytics_jobs".to_string(),
        JsonValue::Array(
            plan.analytics_jobs
                .iter()
                .map(|job| {
                    let mut object = Map::new();
                    object.insert("kind".to_string(), JsonValue::String(job.kind.clone()));
                    object.insert(
                        "projection".to_string(),
                        job.projection
                            .as_ref()
                            .map(|projection| JsonValue::String(projection.clone()))
                            .unwrap_or(JsonValue::Null),
                    );
                    JsonValue::Object(object)
                })
                .collect(),
        ),
    );
    object.insert(
        "includes_native_artifacts".to_string(),
        JsonValue::Bool(plan.includes_native_artifacts),
    );
    JsonValue::Object(object)
}

pub(crate) fn serverless_attach_json(
    required: &[String],
    missing: &[String],
    query_ready: bool,
    write_ready: bool,
    repair_ready: bool,
    readiness: JsonValue,
) -> JsonValue {
    let mut object = Map::new();
    object.insert("ready".to_string(), JsonValue::Bool(missing.is_empty()));
    object.insert("query_ready".to_string(), JsonValue::Bool(query_ready));
    object.insert("write_ready".to_string(), JsonValue::Bool(write_ready));
    object.insert("repair_ready".to_string(), JsonValue::Bool(repair_ready));
    object.insert(
        "required".to_string(),
        JsonValue::Array(required.iter().cloned().map(JsonValue::String).collect()),
    );
    object.insert(
        "missing".to_string(),
        JsonValue::Array(missing.iter().cloned().map(JsonValue::String).collect()),
    );
    object.insert("readiness".to_string(), readiness);
    if !missing.is_empty() {
        object.insert(
            "error".to_string(),
            JsonValue::String(format!(
                "required readiness state not met: {}",
                missing.join(", ")
            )),
        );
    }
    JsonValue::Object(object)
}
