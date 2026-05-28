// JSON Patch / JSON Pointer helpers shared by document and KV patch surfaces
// (issue #751). Builds a `ParsedPatchPayload` from the request body and emits
// structured, Red UI-friendly error envelopes with a JSON Pointer location
// for the failing operation so the editor can highlight the right field.
//
// Wire shape consumed:
//   { "operations": [ { "op": "set" | "replace" | "unset",
//                        "path": "/body/meta/ip",
//                        "value": <any-json> } ],
//     "dry_run": true | false }
//
// Error envelope emitted on validation failure:
//   { "ok": false,
//     "code": "PATCH_PATH_INVALID" | "PATCH_OP_INVALID" | ...,
//     "error": "<message>",
//     "message": "<message>",
//     "op_index": 0,
//     "pointer": "/body/meta/ip" }

use super::*;

pub(crate) struct ParsedPatchPayload {
    pub operations: Vec<PatchOperation>,
    pub dry_run: bool,
}

pub(crate) struct PatchValidationError {
    pub op_index: Option<usize>,
    pub pointer: Option<String>,
    pub code: &'static str,
    pub message: String,
}

impl PatchValidationError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            op_index: None,
            pointer: None,
            code,
            message: message.into(),
        }
    }

    pub(crate) fn at(mut self, op_index: usize) -> Self {
        self.op_index = Some(op_index);
        self
    }

    pub(crate) fn with_pointer(mut self, pointer: String) -> Self {
        self.pointer = Some(pointer);
        self
    }
}

/// Render a path segment list back into a JSON Pointer string with the
/// RFC 6901 escapes (`~` → `~0`, `/` → `~1`).
pub(crate) fn pointer_string(path: &[String]) -> String {
    if path.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for segment in path {
        out.push('/');
        for ch in segment.chars() {
            match ch {
                '~' => out.push_str("~0"),
                '/' => out.push_str("~1"),
                _ => out.push(ch),
            }
        }
    }
    out
}

pub(crate) fn parse_patch_payload(
    payload: &JsonValue,
) -> Result<ParsedPatchPayload, PatchValidationError> {
    let dry_run = match payload.get("dry_run") {
        Some(JsonValue::Bool(value)) => *value,
        None | Some(JsonValue::Null) => false,
        _ => {
            return Err(PatchValidationError::new(
                "PATCH_BODY_INVALID",
                "field 'dry_run' must be a boolean when present",
            ));
        }
    };

    let operations = parse_patch_operations_inner(payload)?;
    Ok(ParsedPatchPayload {
        operations,
        dry_run,
    })
}

/// Legacy shim kept so the existing document patch handler can keep its
/// signature while the rest of the helper is migrated. Returns the same
/// operations the original helper returned, but using the structured
/// validation error type underneath.
pub(crate) fn parse_patch_operations(
    payload: &JsonValue,
) -> Result<Vec<PatchOperation>, HttpResponse> {
    parse_patch_operations_inner(payload).map_err(|err| patch_error_response(400, &err))
}

fn parse_patch_operations_inner(
    payload: &JsonValue,
) -> Result<Vec<PatchOperation>, PatchValidationError> {
    let Some(value) = payload.get("operations") else {
        return Ok(Vec::new());
    };

    let operations = match value {
        JsonValue::Null => return Ok(Vec::new()),
        JsonValue::Array(operations) => operations,
        _ => {
            return Err(PatchValidationError::new(
                "PATCH_BODY_INVALID",
                "field 'operations' must be an array, null, or omitted",
            ));
        }
    };

    if operations.is_empty() {
        return Ok(Vec::new());
    }

    let mut parsed = Vec::with_capacity(operations.len());
    for (index, operation) in operations.iter().enumerate() {
        let JsonValue::Object(operation) = operation else {
            return Err(PatchValidationError::new(
                "PATCH_OP_INVALID",
                "each patch operation must be an object",
            )
            .at(index));
        };

        let op_raw = operation
            .get("op")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| {
                PatchValidationError::new(
                    "PATCH_OP_INVALID",
                    "patch operations require an 'op' field",
                )
                .at(index)
            })?;
        let op = parse_patch_operation_type(op_raw).map_err(|err| err.at(index))?;

        let path_raw = operation
            .get("path")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| {
                PatchValidationError::new(
                    "PATCH_PATH_INVALID",
                    "patch operations require a 'path' field",
                )
                .at(index)
            })?;
        let path = parse_patch_path(path_raw).map_err(|err| err.at(index))?;
        let pointer = pointer_string(&path);

        match op {
            PatchOperationType::Set | PatchOperationType::Replace => {
                let value = operation.get("value").cloned().ok_or_else(|| {
                    PatchValidationError::new(
                        "PATCH_OP_INVALID",
                        "set/replace operations require a 'value' field",
                    )
                    .at(index)
                    .with_pointer(pointer.clone())
                })?;
                parsed.push(PatchOperation {
                    op,
                    path,
                    value: Some(value),
                });
            }
            PatchOperationType::Unset => {
                if operation.contains_key("value") {
                    return Err(PatchValidationError::new(
                        "PATCH_OP_INVALID",
                        "unset operations must not include a 'value' field",
                    )
                    .at(index)
                    .with_pointer(pointer));
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

fn parse_patch_operation_type(raw: &str) -> Result<PatchOperationType, PatchValidationError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "set" | "add" => Ok(PatchOperationType::Set),
        "replace" => Ok(PatchOperationType::Replace),
        "unset" | "remove" | "delete" | "deleted" => Ok(PatchOperationType::Unset),
        other => Err(PatchValidationError::new(
            "PATCH_OP_INVALID",
            format!("unsupported patch operation '{other}'. expected set, replace, or unset"),
        )),
    }
}

fn parse_patch_path(path: &str) -> Result<Vec<String>, PatchValidationError> {
    let value = path.trim();
    if value.is_empty() {
        return Err(
            PatchValidationError::new("PATCH_PATH_INVALID", "patch path cannot be empty")
                .with_pointer(String::new()),
        );
    }
    let normalized = value.strip_prefix('/').unwrap_or(value);
    if normalized.is_empty() {
        return Err(
            PatchValidationError::new("PATCH_PATH_INVALID", "patch path cannot be empty")
                .with_pointer(String::from("/")),
        );
    }
    let mut out = Vec::new();
    for raw_segment in normalized.split('/') {
        if raw_segment.is_empty() {
            return Err(PatchValidationError::new(
                "PATCH_PATH_INVALID",
                "patch path contains empty segment",
            )
            .with_pointer(value.to_string()));
        }
        out.push(raw_segment.replace("~1", "/").replace("~0", "~"));
    }
    Ok(out)
}

/// Build the structured HTTP response for a patch validation error.
pub(crate) fn patch_error_response(status: u16, err: &PatchValidationError) -> HttpResponse {
    let mut object = Map::new();
    object.insert("ok".to_string(), JsonValue::Bool(false));
    object.insert("code".to_string(), JsonValue::String(err.code.to_string()));
    object.insert(
        "error".to_string(),
        crate::json_field::SerializedJsonField::tainted(&err.message),
    );
    object.insert(
        "message".to_string(),
        crate::json_field::SerializedJsonField::tainted(&err.message),
    );
    if let Some(index) = err.op_index {
        object.insert("op_index".to_string(), JsonValue::Number(index as f64));
    }
    if let Some(pointer) = &err.pointer {
        object.insert("pointer".to_string(), JsonValue::String(pointer.clone()));
    }
    json_response(status, JsonValue::Object(object))
}

/// Build the dry-run success envelope — same shape across document and KV
/// surfaces so Red UI can reuse one decoder.
pub(crate) fn patch_dry_run_response(operations: usize) -> HttpResponse {
    let mut object = Map::new();
    object.insert("ok".to_string(), JsonValue::Bool(true));
    object.insert("dry_run".to_string(), JsonValue::Bool(true));
    object.insert(
        "operations".to_string(),
        JsonValue::Number(operations as f64),
    );
    json_response(200, JsonValue::Object(object))
}

/// Convert a stored KV `Value` into a `JsonValue` for nested patching.
/// Returns a structured `KV_VALUE_NOT_JSON` error when the stored value is
/// not a JSON-compatible scalar / object / array. Text values that round-trip
/// through `parse_json` are accepted because driver code commonly serializes
/// JSON payloads through the `Value::Text` path before they reach storage.
pub(crate) fn kv_value_to_json_value(
    value: &crate::storage::schema::Value,
) -> Result<JsonValue, PatchValidationError> {
    use crate::storage::schema::Value as V;
    let parse = |text: &str| -> Result<JsonValue, PatchValidationError> {
        let raw = crate::utils::json::parse_json(text).map_err(|err| {
            PatchValidationError::new(
                "KV_VALUE_NOT_JSON",
                format!("stored KV value is not valid JSON: {err}"),
            )
        })?;
        Ok(JsonValue::from(raw))
    };
    match value {
        V::Json(bytes) => {
            let text = std::str::from_utf8(bytes).map_err(|err| {
                PatchValidationError::new(
                    "KV_VALUE_NOT_JSON",
                    format!("stored KV value is not valid UTF-8 JSON: {err}"),
                )
            })?;
            parse(text)
        }
        V::Text(s) => parse(s.as_ref()).map_err(|_| {
            PatchValidationError::new(
                "KV_VALUE_NOT_JSON",
                "stored KV value is a scalar text; nested patch requires a JSON object or array",
            )
        }),
        V::Null => Err(PatchValidationError::new(
            "KV_VALUE_NOT_JSON",
            "stored KV value is null; nested patch requires a JSON object or array",
        )),
        _ => Err(PatchValidationError::new(
            "KV_VALUE_NOT_JSON",
            "stored KV value is a non-JSON scalar; nested patch requires a JSON object or array",
        )),
    }
}

/// Wrap an apply-phase error (from `apply_patch_operations_to_json`) into the
/// structured patch error envelope, attaching the pointer of the operation
/// that failed when we can identify it from the message.
pub(crate) fn patch_apply_error_response(
    status: u16,
    operations: &[PatchOperation],
    message: String,
) -> HttpResponse {
    // The application layer reports errors against the whole patch payload
    // without an explicit op_index. For the common single-op case we can
    // still surface the pointer.
    let pointer = if operations.len() == 1 {
        Some(pointer_string(&operations[0].path))
    } else {
        None
    };
    let mut err = PatchValidationError::new("PATCH_APPLY_FAILED", message);
    if let Some(pointer) = pointer {
        err = err.with_pointer(pointer);
    }
    patch_error_response(status, &err)
}
