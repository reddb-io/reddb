use super::*;

pub(crate) fn parse_serverless_readiness_requirements(payload: &JsonValue) -> Result<Vec<String>, String> {
    crate::application::serverless_payload::parse_serverless_readiness_requirements(payload)
}

pub(crate) fn parse_serverless_reclaim_operations(payload: &JsonValue) -> Result<Vec<String>, String> {
    crate::application::serverless_payload::parse_serverless_reclaim_operations(payload)
}

pub(crate) fn parse_serverless_warmup_scopes(
    payload: &JsonValue,
) -> Result<Vec<ServerlessWarmupScope>, String> {
    crate::application::serverless_payload::parse_serverless_warmup_scopes(payload).map(
        |scopes| {
            scopes
                .into_iter()
                .map(|scope| match scope {
                    crate::application::serverless_payload::ServerlessWarmupScopeToken::Indexes => {
                        ServerlessWarmupScope::Indexes
                    }
                    crate::application::serverless_payload::ServerlessWarmupScopeToken::GraphProjections => {
                        ServerlessWarmupScope::GraphProjections
                    }
                    crate::application::serverless_payload::ServerlessWarmupScopeToken::AnalyticsJobs => {
                        ServerlessWarmupScope::AnalyticsJobs
                    }
                    crate::application::serverless_payload::ServerlessWarmupScopeToken::NativeArtifacts => {
                        ServerlessWarmupScope::NativeArtifacts
                    }
                })
                .collect()
        },
    )
}

pub(crate) fn serverless_readiness_summary_to_json(
    query_ready: bool,
    write_ready: bool,
    repair_ready: bool,
    health: &crate::health::HealthReport,
    authority: &crate::storage::unified::devx::PhysicalAuthorityStatus,
) -> JsonValue {
    crate::presentation::serverless_json::serverless_readiness_summary_json(
        query_ready,
        write_ready,
        repair_ready,
        health,
        authority,
        crate::presentation::ops_json::health_json,
        crate::presentation::ops_json::physical_authority_status_json,
    )
}

pub(crate) fn deployment_profile_from_token(value: &str) -> Option<DeploymentProfile> {
    crate::application::serverless_payload::deployment_profile_from_token(value).map(
        |profile| match profile {
            crate::application::serverless_payload::DeploymentProfileToken::Embedded => {
                DeploymentProfile::Embedded
            }
            crate::application::serverless_payload::DeploymentProfileToken::Server => {
                DeploymentProfile::Server
            }
            crate::application::serverless_payload::DeploymentProfileToken::Serverless => {
                DeploymentProfile::Serverless
            }
        },
    )
}
