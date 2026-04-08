use crate::json::{Map, Value as JsonValue};
use crate::storage::query::unified::UnifiedRecord;
use crate::storage::schema::Value;

pub(crate) fn search_selection_json(
    entity_types: &Option<Vec<String>>,
    capabilities: &Option<Vec<String>>,
) -> JsonValue {
    let entity_types = entity_types.as_ref().filter(|value| !value.is_empty());
    let capabilities = capabilities.as_ref().filter(|value| !value.is_empty());
    let has_filter = entity_types.is_some() || capabilities.is_some();

    let mut object = Map::new();
    object.insert(
        "scope".to_string(),
        if has_filter {
            JsonValue::String("filtered".to_string())
        } else {
            JsonValue::String("any".to_string())
        },
    );
    if let Some(entity_types) = entity_types {
        object.insert(
            "entity_types".to_string(),
            JsonValue::Array(
                entity_types
                    .iter()
                    .cloned()
                    .map(JsonValue::String)
                    .collect(),
            ),
        );
    }
    if let Some(capabilities) = capabilities {
        object.insert(
            "capabilities".to_string(),
            JsonValue::Array(
                capabilities
                    .iter()
                    .cloned()
                    .map(JsonValue::String)
                    .collect(),
            ),
        );
    }
    JsonValue::Object(object)
}

pub(crate) fn filter_query_records(
    records: &[UnifiedRecord],
    entity_types: &Option<Vec<String>>,
    capabilities: &Option<Vec<String>>,
) -> Vec<UnifiedRecord> {
    let entity_types = entity_types
        .as_ref()
        .filter(|values| !values.is_empty())
        .map(|values| {
            values
                .iter()
                .map(|value| value.to_lowercase())
                .collect::<Vec<_>>()
        });
    let capabilities = capabilities
        .as_ref()
        .filter(|values| !values.is_empty())
        .map(|values| {
            values
                .iter()
                .map(|value| value.to_lowercase())
                .collect::<Vec<_>>()
        });

    if entity_types.is_none() && capabilities.is_none() {
        return records.to_vec();
    }

    let entity_types = entity_types.as_deref();
    let capabilities = capabilities.as_deref();

    records
        .iter()
        .filter(|record| {
            let entity_type = record
                .values
                .get("_entity_type")
                .and_then(Value::as_text)
                .unwrap_or_else(|| {
                    record
                        .values
                        .get("_kind")
                        .and_then(Value::as_text)
                        .unwrap_or("")
                })
                .to_lowercase();

            let entity_capabilities = record
                .values
                .get("_capabilities")
                .and_then(Value::as_text)
                .map(record_capability_tokens)
                .unwrap_or_default();

            let entity_type_ok = entity_types.is_none_or(|types| {
                !types.is_empty() && types.iter().any(|expected| expected == &entity_type)
            });
            let capability_ok = capabilities.is_none_or(|required| {
                required
                    .iter()
                    .any(|target| entity_capabilities.iter().any(|value| value == target))
            });

            entity_type_ok && capability_ok
        })
        .cloned()
        .collect()
}

fn record_capability_tokens(text: &str) -> Vec<String> {
    text.split(',')
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}
