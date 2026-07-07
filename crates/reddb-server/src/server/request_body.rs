use super::*;

pub(crate) fn extract_query(body: &[u8]) -> Result<String, HttpResponse> {
    extract_query_request(body).map(|request| request.query)
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
/// Reuses the same JSON→`Value` mapping as the embedded stdio binder
/// so HTTP and stdio share one type-coercion contract.
fn parse_params_field(json: &JsonValue) -> Result<Option<Vec<Value>>, HttpResponse> {
    match json.get("params") {
        None => Ok(None),
        Some(JsonValue::Array(items)) => Ok(Some(
            items
                .iter()
                .map(crate::rpc_stdio::json_value_to_schema_value)
                .collect(),
        )),
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
