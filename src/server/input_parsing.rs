use super::*;

pub(crate) fn json_vector_field(payload: &JsonValue, field: &str) -> Result<Vec<f32>, HttpResponse> {
    let values = payload
        .get(field)
        .and_then(JsonValue::as_array)
        .ok_or_else(|| {
            json_error(
                400,
                format!("JSON body must contain an array field named '{field}'"),
            )
        })?;
    if values.is_empty() {
        return Err(json_error(400, format!("field '{field}' cannot be empty")));
    }

    let mut vector = Vec::with_capacity(values.len());
    for value in values {
        let number = value
            .as_f64()
            .ok_or_else(|| json_error(400, format!("field '{field}' must contain only numbers")))?;
        vector.push(number as f32);
    }
    Ok(vector)
}

pub(crate) fn json_vector_from_value(
    value: Option<&JsonValue>,
    field: &str,
) -> Result<Vec<f32>, HttpResponse> {
    let Some(JsonValue::Array(values)) = value else {
        return Err(json_error(
            400,
            format!("JSON body must contain an array field named '{field}'"),
        ));
    };
    if values.is_empty() {
        return Err(json_error(400, format!("field '{field}' cannot be empty")));
    }

    let mut vector = Vec::with_capacity(values.len());
    for value in values {
        let number = value
            .as_f64()
            .ok_or_else(|| json_error(400, format!("field '{field}' must contain only numbers")))?;
        vector.push(number as f32);
    }
    Ok(vector)
}

pub(crate) fn optional_json_vector_field(
    payload: &JsonValue,
    field: &str,
) -> Result<Option<Vec<f32>>, HttpResponse> {
    match payload.get(field) {
        Some(JsonValue::Null) | None => Ok(None),
        Some(_) => json_vector_field(payload, field).map(Some),
    }
}

pub(crate) fn authorization_bearer_token<'a>(headers: &'a BTreeMap<String, String>) -> Option<&'a str> {
    headers.get("authorization")?.strip_prefix("Bearer ")
}

pub(crate) fn json_collection_entity_ref(
    value: &JsonValue,
    kind: &str,
) -> Result<(String, u64), HttpResponse> {
    let Some(object) = value.as_object() else {
        return Err(json_error(400, format!("{kind} link must be an object")));
    };
    let Some(collection) = object.get("collection").and_then(JsonValue::as_str) else {
        return Err(json_error(
            400,
            format!("{kind} link requires 'collection'"),
        ));
    };
    let Some(id) = object.get("id").and_then(JsonValue::as_i64) else {
        return Err(json_error(
            400,
            format!("{kind} link requires numeric 'id'"),
        ));
    };
    Ok((collection.to_string(), id as u64))
}

pub(crate) fn json_to_storage_value(value: &JsonValue) -> Result<Value, HttpResponse> {
    crate::application::entity::json_to_storage_value(value)
        .map_err(|err| json_error(400, err.to_string()))
}

pub(crate) fn json_to_metadata_value(value: &JsonValue) -> Result<MetadataValue, HttpResponse> {
    crate::application::entity::json_to_metadata_value(value)
        .map_err(|err| json_error(400, err.to_string()))
}
