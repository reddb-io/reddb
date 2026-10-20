use std::borrow::Cow;

use crate::json::{Map, Value as JsonValue};
use crate::storage::query::unified::UnifiedRecord;
use reddb_types::Value;

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

pub(crate) fn filter_query_records<'a>(
    records: &'a [UnifiedRecord],
    entity_types: &Option<Vec<String>>,
    capabilities: &Option<Vec<String>>,
) -> Cow<'a, [UnifiedRecord]> {
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
        return Cow::Borrowed(records);
    }

    let entity_types = entity_types.as_deref();
    let capabilities = capabilities.as_deref();

    Cow::Owned(
        records
            .iter()
            .filter(|record| {
                let entity_type = record
                    .get("red_entity_type")
                    .and_then(Value::as_text)
                    .unwrap_or_else(|| {
                        record
                            .get("red_kind")
                            .and_then(Value::as_text)
                            .unwrap_or("")
                    })
                    .to_lowercase();

                let entity_capabilities = record
                    .get("red_capabilities")
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
            .collect(),
    )
}

fn record_capability_tokens(text: &str) -> Vec<String> {
    text.split(',')
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::no_malloc_hot_paths::measure_allocations;

    fn record(id: i64, kind: &str, capabilities: &str) -> UnifiedRecord {
        let mut record = UnifiedRecord::new();
        record.set("id", Value::Integer(id));
        record.set("red_kind", Value::text(kind));
        record.set("red_capabilities", Value::text(capabilities));
        record.set("nested", Value::Array(vec![Value::Integer(id)]));
        record
    }

    #[test]
    fn unfiltered_selection_borrows_records_including_empty_filters() {
        let records = vec![record(1, "table", "kv")];
        for selection in [None, Some(Vec::new())] {
            let selected = filter_query_records(&records, &selection, &selection);
            assert!(matches!(selected, Cow::Borrowed(_)));
            assert_eq!(selected.as_ptr(), records.as_ptr());
        }
    }

    #[test]
    fn filtered_selection_preserves_case_fallback_order_and_nested_values() {
        let records = vec![
            record(1, "Table", "graph, KV"),
            record(2, "document", "kv"),
            record(3, "table", "kv"),
        ];
        let selected = filter_query_records(
            &records,
            &Some(vec!["TABLE".into()]),
            &Some(vec!["Kv".into()]),
        );
        assert!(matches!(selected, Cow::Owned(_)));
        assert_eq!(
            selected
                .iter()
                .map(|row| row.get("id").cloned())
                .collect::<Vec<_>>(),
            vec![Some(Value::Integer(1)), Some(Value::Integer(3))]
        );
        assert_eq!(
            selected[1].get("nested"),
            Some(&Value::Array(vec![Value::Integer(3)]))
        );
        assert_eq!(
            filter_query_records(&records, &Some(vec!["missing".into()]), &None).len(),
            0
        );
    }

    #[test]
    fn resumable_sort_owns_only_when_mutating_and_keeps_source_order() {
        let records = vec![record(2, "table", "kv"), record(1, "table", "kv")];
        let mut selected = filter_query_records(&records, &None, &None);
        selected.to_mut().sort_by_key(|row| {
            row.get("id").and_then(|value| match value {
                Value::Integer(id) => Some(*id),
                _ => None,
            })
        });
        assert_eq!(selected[0].get("id"), Some(&Value::Integer(1)));
        assert_eq!(records[0].get("id"), Some(&Value::Integer(2)));
    }

    #[test]
    fn unfiltered_query_selection_allocates_nothing() {
        use crate::presentation::query_view::filter_query_records;
        use crate::storage::query::unified::UnifiedRecord;
        let mut record = UnifiedRecord::new();
        record.set(
            "nested",
            reddb_types::Value::Array(vec![reddb_types::Value::Integer(42)]),
        );
        let records = vec![record; 1000];
        let (selected, count) =
            measure_allocations(|| filter_query_records(&records, &None, &None));
        assert_eq!(selected.len(), 1000);
        assert_eq!(
            count.allocs, 0,
            "response selection must borrow unfiltered rows"
        );
    }

    #[test]
    #[ignore = "manual RQL/SQL allocation diagnostic; run with result cache disabled"]
    fn rql_projection_allocation_diagnostic() {
        use crate::presentation::query_result_json::runtime_query_json;
        use crate::presentation::query_view::filter_query_records;
        use crate::storage::query::unified::UnifiedRecord;
        use reddb_types::Value;

        let runtime = crate::RedDBRuntime::in_memory().expect("runtime");
        runtime
            .execute_query("KV PUT projected.item = 'payload'")
            .expect("seed");
        let collection = runtime
            .db()
            .store()
            .get_collection("projected")
            .expect("collection");
        let original = collection.query_all(|_| true).pop().expect("row");
        for extra_values in [0, 128] {
            let mut entity = original.clone();
            let crate::storage::EntityData::Row(row) = &mut entity.data else {
                panic!("KV rows are rows");
            };
            row.named.as_mut().expect("named row").insert(
                "not_returned".into(),
                Value::Array(
                    (0..extra_values)
                        .map(|_| Value::Array(vec![Value::Integer(1)]))
                        .collect(),
                ),
            );
            collection.update(entity).expect("extra unprojected data");
            for _ in 0..5 {
                runtime
                    .execute_query("KV GET projected.item")
                    .expect("warmup");
            }
            let (result, execution) =
                measure_allocations(|| runtime.execute_query("KV GET projected.item"));
            let result = result.expect("GET");
            let (json, presentation) =
                measure_allocations(|| runtime_query_json(&result, &None, &None));
            assert_eq!(
                result.result.records[0].get("value"),
                Some(&Value::text("payload"))
            );
            assert!(json.get("result").is_some());
            eprintln!(
                "RQL_PROJECTION extra_values={extra_values} execution_allocs={} presentation_allocs={}",
                execution.allocs, presentation.allocs
            );
        }
        for row_count in [1, 100, 1000] {
            let records: Vec<_> = (0..row_count)
                .map(|id| {
                    let mut record = UnifiedRecord::new();
                    record.set("id", Value::Integer(id));
                    record.set("value", Value::Array(vec![Value::Integer(42)]));
                    record
                })
                .collect();
            let (selected, count) =
                measure_allocations(|| filter_query_records(&records, &None, &None));
            assert_eq!(selected.len(), records.len());
            eprintln!(
                "SQL_SELECTION rows={row_count} allocations={}",
                count.allocs
            );
        }
    }
}
