use super::*;

fn parse_patch_operations(payload: &JsonValue) -> Result<Vec<PatchOperation>, HttpResponse> {
    let Some(value) = payload.get("operations") else {
        return Ok(Vec::new());
    };

    let operations = match value {
        JsonValue::Null => return Ok(Vec::new()),
        JsonValue::Array(operations) => operations,
        _ => {
            return Err(json_error(
                400,
                "field 'operations' must be an array, null, or omitted",
            ));
        }
    };

    if operations.is_empty() {
        return Ok(Vec::new());
    }

    let mut parsed = Vec::with_capacity(operations.len());
    for operation in operations {
        let JsonValue::Object(operation) = operation else {
            return Err(json_error(400, "each patch operation must be an object"));
        };

        let op = parse_patch_operation_type(
            operation
                .get("op")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| json_error(400, "patch operations require an 'op' field"))?,
        )?;
        let path = parse_patch_path(
            operation
                .get("path")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| json_error(400, "patch operations require a 'path' field"))?,
        )?;
        if path.is_empty() {
            return Err(json_error(400, "patch path cannot be empty"));
        }

        match op {
            PatchOperationType::Set | PatchOperationType::Replace => {
                let value = operation.get("value").cloned().ok_or_else(|| {
                    json_error(400, "set/replace operations require a 'value' field")
                })?;
                parsed.push(PatchOperation {
                    op,
                    path,
                    value: Some(value),
                });
            }
            PatchOperationType::Unset => {
                if operation.contains_key("value") {
                    return Err(json_error(
                        400,
                        "unset operations must not include a 'value' field",
                    ));
                }
                parsed.push(PatchOperation {
                    op,
                    path,
                    value: None,
                });
            }
        }
    }

    Ok(parsed)
}

fn parse_patch_operation_type(raw: &str) -> Result<PatchOperationType, HttpResponse> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "set" | "add" => Ok(PatchOperationType::Set),
        "replace" => Ok(PatchOperationType::Replace),
        "unset" | "remove" | "delete" | "deleted" => Ok(PatchOperationType::Unset),
        _ => Err(json_error(
            400,
            format!("unsupported patch operation '{raw}'. expected set, replace, or unset"),
        )),
    }
}

fn parse_patch_path(path: &str) -> Result<Vec<String>, HttpResponse> {
    let value = path.trim();
    if value.is_empty() {
        return Err(json_error(400, "patch path cannot be empty"));
    }
    let normalized = value.strip_prefix('/').unwrap_or(value);
    if normalized.is_empty() {
        return Err(json_error(400, "patch path cannot be empty"));
    }
    let mut out = Vec::new();
    for raw_segment in normalized.split('/') {
        if raw_segment.is_empty() {
            return Err(json_error(400, "patch path contains empty segment"));
        }
        out.push(raw_segment.replace("~1", "/").replace("~0", "~"));
    }
    Ok(out)
}
