use crate::json::Value as JsonValue;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServerlessWarmupScopeToken {
    Indexes,
    GraphProjections,
    AnalyticsJobs,
    NativeArtifacts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeploymentProfileToken {
    Embedded,
    Server,
    Serverless,
}

pub(crate) fn parse_serverless_readiness_requirements(
    payload: &JsonValue,
) -> Result<Vec<String>, String> {
    let value = payload
        .get("require")
        .or_else(|| payload.get("require_readiness"));

    let mut requirements = match value {
        None => Vec::new(),
        Some(JsonValue::Null) => Vec::new(),
        Some(JsonValue::String(value)) => vec![value.to_string()],
        Some(JsonValue::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| "field 'require' must be an array of strings".to_string())
                    .map(str::to_string)
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err("field 'require' must be an array, a string, or omitted".to_string()),
    };

    if requirements.is_empty() {
        return Ok(requirements);
    }

    let mut normalized = Vec::new();
    for requirement in requirements.drain(..) {
        match normalize_serverless_readiness_requirement(&requirement) {
            Some(value) => {
                if value == "all" {
                    normalized.push("query".to_string());
                    normalized.push("write".to_string());
                    normalized.push("repair".to_string());
                } else {
                    normalized.push(value.to_string());
                }
            }
            None => return Err(format!("invalid readiness requirement '{requirement}'")),
        }
    }

    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

pub(crate) fn parse_serverless_reclaim_operations(
    payload: &JsonValue,
) -> Result<Vec<String>, String> {
    let value = payload.get("operations");
    let mut operations = match value {
        None => vec![
            "maintenance".to_string(),
            "retention".to_string(),
            "checkpoint".to_string(),
        ],
        Some(JsonValue::Null) => Vec::new(),
        Some(JsonValue::String(value)) => vec![value.to_string()],
        Some(JsonValue::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| "field 'operations' must be an array of strings".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => {
            return Err("field 'operations' must be an array, a string, or omitted".to_string())
        }
    };

    if operations.is_empty() {
        return Ok(operations);
    }

    let mut normalized = Vec::new();
    for operation in operations.drain(..) {
        match normalize_serverless_reclaim_operation(&operation)? {
            Some(value) => {
                if value == "all" {
                    return Ok(vec![
                        "maintenance".to_string(),
                        "retention".to_string(),
                        "checkpoint".to_string(),
                    ]);
                }
                normalized.push(value);
            }
            None => return Err(format!("invalid reclaim operation '{operation}'")),
        }
    }

    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

pub(crate) fn parse_serverless_warmup_scopes(
    payload: &JsonValue,
) -> Result<Vec<ServerlessWarmupScopeToken>, String> {
    let value = payload.get("scopes").or_else(|| payload.get("scope"));
    let mut scopes = match value {
        None => Vec::new(),
        Some(JsonValue::Null) => Vec::new(),
        Some(JsonValue::String(value)) => vec![value.to_string()],
        Some(JsonValue::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| "field 'scopes' must be an array of strings".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err("field 'scopes' must be an array, a string, or omitted".to_string()),
    };

    if scopes.is_empty() {
        return Ok(vec![
            ServerlessWarmupScopeToken::Indexes,
            ServerlessWarmupScopeToken::GraphProjections,
            ServerlessWarmupScopeToken::AnalyticsJobs,
            ServerlessWarmupScopeToken::NativeArtifacts,
        ]);
    }

    let mut normalized = Vec::new();
    let mut includes_all = false;
    for scope in scopes.drain(..) {
        match normalize_serverless_scope(&scope)? {
            Some(scopes) => {
                if scopes.is_empty() {
                    includes_all = true;
                    continue;
                }
                for normalized_scope in scopes {
                    if !normalized.contains(&normalized_scope) {
                        normalized.push(normalized_scope);
                    }
                }
            }
            None => return Err(format!("invalid warmup scope '{scope}'")),
        }
    }

    if includes_all {
        return Ok(vec![
            ServerlessWarmupScopeToken::Indexes,
            ServerlessWarmupScopeToken::GraphProjections,
            ServerlessWarmupScopeToken::AnalyticsJobs,
            ServerlessWarmupScopeToken::NativeArtifacts,
        ]);
    }

    Ok(normalized)
}

pub(crate) fn deployment_profile_from_token(value: &str) -> Option<DeploymentProfileToken> {
    match normalize_serverless_token(value).as_str() {
        "embedded" => Some(DeploymentProfileToken::Embedded),
        "server" | "serverful" => Some(DeploymentProfileToken::Server),
        "serverless" => Some(DeploymentProfileToken::Serverless),
        _ => None,
    }
}

pub(crate) fn missing_serverless_readiness(
    required: &[String],
    query_ready: bool,
    write_ready: bool,
    repair_ready: bool,
) -> Vec<String> {
    let mut missing = Vec::new();
    for requirement in required {
        match requirement.as_str() {
            "query" if !query_ready => missing.push(requirement.clone()),
            "write" if !write_ready => missing.push(requirement.clone()),
            "repair" if !repair_ready => missing.push(requirement.clone()),
            _ => {}
        }
    }
    missing
}

pub(crate) fn missing_serverless_warmup_preconditions(
    dry_run: bool,
    query_ready: bool,
    write_ready: bool,
    repair_ready: bool,
) -> Vec<String> {
    let mut missing = Vec::new();
    if !query_ready {
        missing.push("query".to_string());
    }
    if !dry_run {
        if !write_ready {
            missing.push("write".to_string());
        }
        if !repair_ready {
            missing.push("repair".to_string());
        }
    }
    missing
}

fn normalize_serverless_token(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(|character| character.to_lowercase())
        .collect()
}

fn normalize_serverless_readiness_requirement(value: &str) -> Option<&'static str> {
    match normalize_serverless_token(value).as_str() {
        "query" => Some("query"),
        "write" => Some("write"),
        "repair" => Some("repair"),
        "all" => Some("all"),
        _ => None,
    }
}

fn normalize_serverless_reclaim_operation(value: &str) -> Result<Option<String>, String> {
    if normalize_serverless_token(value).as_str().is_empty() {
        return Err("invalid reclaim operation".to_string());
    }

    match normalize_serverless_token(value).as_str() {
        "maintenance" => Ok(Some("maintenance".to_string())),
        "maintain" | "maintenance_run" | "runmaintenance" => Ok(Some("maintenance".to_string())),
        "retention" | "retentionpolicy" => Ok(Some("retention".to_string())),
        "checkpoint" => Ok(Some("checkpoint".to_string())),
        "all" => Ok(Some("all".to_string())),
        _ => Ok(None),
    }
}

fn normalize_serverless_scope(
    value: &str,
) -> Result<Option<Vec<ServerlessWarmupScopeToken>>, String> {
    if normalize_serverless_token(value).as_str().is_empty() {
        return Err("invalid serverless scope".to_string());
    }

    match normalize_serverless_token(value).as_str() {
        "index" | "indexes" => Ok(Some(vec![ServerlessWarmupScopeToken::Indexes])),
        "graph" | "graphprojection" | "graphprojections" | "projection" | "projections" => {
            Ok(Some(vec![ServerlessWarmupScopeToken::GraphProjections]))
        }
        "analytics" | "analyticsjob" | "analyticsjobs" | "job" | "jobs" => {
            Ok(Some(vec![ServerlessWarmupScopeToken::AnalyticsJobs]))
        }
        "native" | "nativeartifacts" | "artifacts" | "artifact" => {
            Ok(Some(vec![ServerlessWarmupScopeToken::NativeArtifacts]))
        }
        "all" => Ok(Some(Vec::new())),
        _ => Ok(None),
    }
}
