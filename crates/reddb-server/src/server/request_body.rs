use super::*;

pub(crate) fn extract_query(body: &[u8]) -> Result<String, HttpResponse> {
    extract_query_request(body).map(|request| request.query)
}

pub(crate) fn is_trivial_scalar_query(body: &[u8]) -> bool {
    extract_query_request(body).is_ok_and(|request| is_trivial_scalar_sql(&request.query))
}

pub(crate) fn is_trivial_scalar_sql(sql: &str) -> bool {
    let normalized = sql.trim().trim_end_matches(';').trim();
    let tokens: Vec<_> = normalized.split_ascii_whitespace().collect();
    matches!(tokens.as_slice(), [select, one]
        if select.eq_ignore_ascii_case("select") && *one == "1")
        || matches!(tokens.as_slice(), [select, one, as_token, alias]
            if select.eq_ignore_ascii_case("select")
                && *one == "1"
                && as_token.eq_ignore_ascii_case("as")
                && !alias.is_empty())
}

pub(crate) fn extract_query_request(body: &[u8]) -> Result<ParsedQueryRequest, HttpResponse> {
    let text =
        std::str::from_utf8(body).map_err(|_| json_error(400, "request body must be UTF-8"))?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(json_error(400, "missing query body"));
    }

    if trimmed.starts_with('{') {
        let parsed = parse_json(trimmed)
            .map_err(|err| json_error(400, format!("invalid JSON body: {err}")))?;
        let json = JsonValue::from(parsed);
        if let Some(query) = json.get("query").and_then(JsonValue::as_str) {
            if query.trim().is_empty() {
                return Err(json_error(400, "query field cannot be empty"));
            }

            let (entity_types, capabilities) =
                crate::application::query_payload::parse_json_search_selection(&json)
                    .map_err(|err| json_error(400, err.to_string()))?;
            let commit_policy = parse_commit_policy_field(&json)?;
            let params = parse_params_field(&json)?;
            return Ok(ParsedQueryRequest {
                query: query.to_string(),
                entity_types,
                capabilities,
                commit_policy,
                params,
            });
        }
        return Err(json_error(
            400,
            "JSON body must contain a string field named 'query'",
        ));
    }

    Ok(ParsedQueryRequest {
        query: trimmed.to_string(),
        entity_types: None,
        capabilities: None,
        commit_policy: None,
        params: None,
    })
}

fn parse_commit_policy_field(
    json: &JsonValue,
) -> Result<Option<crate::replication::CommitPolicy>, HttpResponse> {
    match json.get("commit_policy") {
        None => Ok(None),
        Some(JsonValue::String(value)) => crate::replication::CommitPolicy::parse_strict(value)
            .map(Some)
            .ok_or_else(|| {
                json_error_code(
                    400,
                    "INVALID_COMMIT_POLICY",
                    format!("invalid commit_policy value '{value}'"),
                )
            }),
        Some(_) => Err(json_error_code(
            400,
            "INVALID_COMMIT_POLICY",
            "'commit_policy' must be a string",
        )),
    }
}

/// Parse the optional `params` JSON array on a query request body.
/// JSON transports decode into the Request module's transport-neutral
/// parameter vocabulary before execution.
fn parse_params_field(
    json: &JsonValue,
) -> Result<Option<Vec<crate::runtime::query_request::ParamValue>>, HttpResponse> {
    match json.get("params") {
        None => Ok(None),
        Some(JsonValue::Array(items)) => items
            .iter()
            .map(crate::runtime::query_request::ParamValue::decode_json)
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
            .map_err(|message| json_error_code(400, "INVALID_PARAMS", message)),
        Some(_) => Err(json_error_code(
            400,
            "INVALID_PARAMS",
            "'params' must be a JSON array",
        )),
    }
}

pub(crate) fn parse_json_body(body: &[u8]) -> Result<JsonValue, HttpResponse> {
    let text =
        std::str::from_utf8(body).map_err(|_| json_error(400, "request body must be UTF-8"))?;
    let parsed =
        parse_json(text).map_err(|err| json_error(400, format!("invalid JSON body: {err}")))?;
    Ok(JsonValue::from(parsed))
}

pub(crate) fn parse_json_body_allow_empty(body: &[u8]) -> Result<JsonValue, HttpResponse> {
    if body.is_empty() {
        return Ok(JsonValue::Object(Map::new()));
    }
    parse_json_body(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_reserve_classifier_accepts_only_trivial_scalar_queries() {
        for sql in ["SELECT 1", " select 1; ", "SELECT 1 AS health"] {
            assert!(is_trivial_scalar_sql(sql), "{sql}");
        }
        for sql in [
            "SELECT 1 FROM users",
            "SELECT pg_sleep(1)",
            "SELECT 1; DELETE FROM users",
            "WITH value AS (SELECT 1) SELECT * FROM value",
        ] {
            assert!(!is_trivial_scalar_sql(sql), "{sql}");
        }
        assert!(is_trivial_scalar_query(br#"{"query":"SELECT 1"}"#));
        assert!(!is_trivial_scalar_query(
            br#"{"query":"SELECT * FROM users"}"#
        ));
    }
}
