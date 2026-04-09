use crate::json::{Map, Value as JsonValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeploymentProfileView {
    Embedded,
    Server,
    Serverless,
}

pub(crate) fn deployment_profile_json(profile: DeploymentProfileView) -> JsonValue {
    let name = deployment_profile_name(profile);
    let (label, description, allowed, forbidden, opaque, mandatory, warmup, recommended) =
        deployment_profile_contract(profile);

    let mut object = Map::new();
    object.insert("name".to_string(), JsonValue::String(name.to_string()));
    object.insert("label".to_string(), JsonValue::String(label.to_string()));
    object.insert(
        "description".to_string(),
        JsonValue::String(description.to_string()),
    );
    object.insert("allowed_endpoints".to_string(), string_array_json(allowed));
    object.insert(
        "forbidden_endpoints".to_string(),
        string_array_json(forbidden),
    );
    object.insert(
        "host_opaque_endpoints".to_string(),
        string_array_json(opaque),
    );
    object.insert(
        "mandatory_bootstrap_apis".to_string(),
        string_array_json(mandatory),
    );
    object.insert(
        "mandatory_warmup_scopes".to_string(),
        string_array_json(warmup),
    );
    object.insert(
        "recommended_before_traffic".to_string(),
        string_array_json(recommended),
    );
    JsonValue::Object(object)
}

pub(crate) fn deployment_profiles_catalog_json(
    profiles: &[DeploymentProfileView],
    note: &str,
) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "profiles".to_string(),
        JsonValue::Array(
            profiles
                .iter()
                .copied()
                .map(deployment_profile_json)
                .collect(),
        ),
    );
    object.insert("note".to_string(), JsonValue::String(note.to_string()));
    JsonValue::Object(object)
}

fn deployment_profile_name(profile: DeploymentProfileView) -> &'static str {
    match profile {
        DeploymentProfileView::Embedded => "embedded",
        DeploymentProfileView::Server => "server",
        DeploymentProfileView::Serverless => "serverless",
    }
}

fn string_array_json(values: &[&str]) -> JsonValue {
    JsonValue::Array(
        values
            .iter()
            .map(|value| JsonValue::String((*value).to_string()))
            .collect(),
    )
}

fn deployment_profile_contract(
    profile: DeploymentProfileView,
) -> (
    &'static str,
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
    &'static [&'static str],
    &'static [&'static str],
    &'static [&'static str],
    &'static [&'static str],
) {
    match profile {
        DeploymentProfileView::Embedded => (
            "In-process with direct control",
            "Single-process lifecycle, long-lived runtime without host orchestration.",
            &[
                "/health",
                "/ready",
                "/ready/query",
                "/ready/write",
                "/ready/repair",
                "/query",
                "/search",
                "/text/search",
                "/hybrid/search",
                "/similar/<collection>",
                "/scan?collection",
                "/rows/<collection>",
                "/nodes/<collection>",
                "/edges/<collection>",
                "/vectors/<collection>",
                "/rows/<collection>/bulk",
                "/indexes",
                "/indexes/rebuild",
                "/index/<name>/enable|disable|warmup|building|ready|fail|stale",
                "/physical/native-vector-artifacts/warmup",
                "/maintenance",
                "/tick",
                "/retention/apply",
                "/checkpoint",
            ],
            &[
                "/serverless/attach",
                "/serverless/warmup",
                "/serverless/reclaim",
            ],
            &["/checkpoint", "/maintenance", "/retention/apply"],
            &["/checkpoint"],
            &[],
            &["/maintenance"],
        ),
        DeploymentProfileView::Server => (
            "Always-on network service",
            "Long-lived service receiving traffic directly.",
            &[
                "/health",
                "/ready",
                "/ready/query",
                "/ready/write",
                "/ready/repair",
                "/catalog",
                "/catalog/readiness",
                "/query",
                "/search",
                "/text/search",
                "/hybrid/search",
                "/rows/<collection>",
                "/nodes/<collection>",
                "/edges/<collection>",
                "/vectors/<collection>",
                "/indexes",
                "/graph/jobs",
                "/graph/projections",
                "/maintenance",
                "/tick",
                "/retention/apply",
                "/checkpoint",
            ],
            &[],
            &[
                "/serverless/attach",
                "/serverless/warmup",
                "/serverless/reclaim",
            ],
            &["/checkpoint", "/maintenance", "/retention/apply"],
            &["/ready"],
            &["/catalog/readiness"],
        ),
        DeploymentProfileView::Serverless => (
            "Ephemeral host attached to durable state",
            "Attach, optional warmup, and reclaim operations are coordinated with control plane.",
            &[
                "/health",
                "/ready",
                "/ready/query",
                "/ready/write",
                "/ready/repair",
                "/catalog",
                "/catalog/readiness",
                "/serverless/attach",
                "/serverless/warmup",
                "/serverless/reclaim",
                "/tick",
                "/query",
                "/search",
                "/text/search",
                "/hybrid/search",
                "/similar/<collection>",
                "/rows/<collection>",
                "/nodes/<collection>",
                "/edges/<collection>",
                "/vectors/<collection>",
            ],
            &[],
            &[
                "/checkpoint",
                "/maintenance",
                "/retention/apply",
                "/physical/native-state/repair",
                "/physical/metadata/rebuild",
            ],
            &[
                "/serverless/attach",
                "/serverless/warmup",
                "/serverless/reclaim",
            ],
            &[
                "indexes",
                "graph_projections",
                "analytics_jobs",
                "native_artifacts",
            ],
            &["/catalog/readiness", "/serverless/attach"],
        ),
    }
}
