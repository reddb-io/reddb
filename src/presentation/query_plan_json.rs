use crate::json::{Map, Value as JsonValue};
use crate::runtime::RuntimeQueryExplain;
use crate::storage::query::planner::CanonicalLogicalNode;

pub(crate) fn query_explain_json(
    result: &RuntimeQueryExplain,
    mode: &str,
    capability: Option<&str>,
    include_ok: bool,
    universal_mode: bool,
) -> JsonValue {
    let mut object = Map::new();

    if include_ok {
        object.insert("ok".to_string(), JsonValue::Bool(true));
    }

    object.insert("query".to_string(), JsonValue::String(result.query.clone()));
    object.insert("mode".to_string(), JsonValue::String(mode.to_string()));

    if let Some(capability) = capability {
        object.insert(
            "capability".to_string(),
            JsonValue::String(capability.to_string()),
        );
    }

    object.insert(
        "statement".to_string(),
        JsonValue::String(result.statement.to_string()),
    );
    object.insert(
        "is_universal".to_string(),
        JsonValue::Bool(result.is_universal),
    );

    let mut cost = Map::new();
    cost.insert("cpu".to_string(), JsonValue::Number(result.plan_cost.cpu));
    cost.insert("io".to_string(), JsonValue::Number(result.plan_cost.io));
    cost.insert(
        "network".to_string(),
        JsonValue::Number(result.plan_cost.network),
    );
    cost.insert(
        "memory".to_string(),
        JsonValue::Number(result.plan_cost.memory),
    );
    cost.insert(
        "total".to_string(),
        JsonValue::Number(result.plan_cost.total),
    );
    object.insert("cost".to_string(), JsonValue::Object(cost));
    object.insert(
        "estimated_rows".to_string(),
        JsonValue::Number(result.estimated_rows),
    );
    object.insert(
        "estimated_selectivity".to_string(),
        JsonValue::Number(result.estimated_selectivity),
    );
    object.insert(
        "estimated_confidence".to_string(),
        JsonValue::Number(result.estimated_confidence),
    );
    object.insert(
        "passes_applied".to_string(),
        JsonValue::Array(
            result
                .passes_applied
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "universal_mode".to_string(),
        JsonValue::Bool(universal_mode),
    );
    object.insert(
        "logical_plan".to_string(),
        logical_plan_json(&result.logical_plan.root),
    );
    JsonValue::Object(object)
}

pub(crate) fn logical_plan_json(node: &CanonicalLogicalNode) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "operator".to_string(),
        JsonValue::String(node.operator.clone()),
    );
    object.insert(
        "source".to_string(),
        node.source
            .clone()
            .map(JsonValue::String)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "details".to_string(),
        JsonValue::Object(
            node.details
                .iter()
                .map(|(key, value)| (key.clone(), JsonValue::String(value.clone())))
                .collect(),
        ),
    );
    object.insert(
        "estimated_rows".to_string(),
        JsonValue::Number(node.estimated_rows),
    );
    object.insert(
        "estimated_selectivity".to_string(),
        JsonValue::Number(node.estimated_selectivity),
    );
    object.insert(
        "estimated_confidence".to_string(),
        JsonValue::Number(node.estimated_confidence),
    );
    object.insert(
        "operator_cost".to_string(),
        JsonValue::Number(node.operator_cost),
    );
    object.insert(
        "children".to_string(),
        JsonValue::Array(node.children.iter().map(logical_plan_json).collect()),
    );
    JsonValue::Object(object)
}

pub(crate) fn logical_plan_uses_universal_mode(node: &CanonicalLogicalNode) -> bool {
    node.operator == "entity_scan"
        || node
            .details
            .get("universal")
            .is_some_and(|value| value == "true")
        || node.children.iter().any(logical_plan_uses_universal_mode)
}
