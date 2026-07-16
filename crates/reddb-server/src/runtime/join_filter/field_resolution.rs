//! Runtime field and document-path resolution.
use super::*;

pub(in crate::runtime) fn parse_canonical_field_ref(value: &str) -> RedDBResult<FieldRef> {
    if let Some(rest) = value.strip_prefix("table:") {
        let (table, column) = rest.rsplit_once('.').ok_or_else(|| {
            RedDBError::Query(format!("invalid canonical table field ref {value}"))
        })?;
        return Ok(FieldRef::TableColumn {
            table: table.to_string(),
            column: column.to_string(),
        });
    }
    if let Some(rest) = value.strip_prefix("node:") {
        let (alias, property) = rest.rsplit_once('.').ok_or_else(|| {
            RedDBError::Query(format!("invalid canonical node field ref {value}"))
        })?;
        return Ok(FieldRef::NodeProperty {
            alias: alias.to_string(),
            property: property.to_string(),
        });
    }
    if let Some(rest) = value.strip_prefix("edge:") {
        let (alias, property) = rest.rsplit_once('.').ok_or_else(|| {
            RedDBError::Query(format!("invalid canonical edge field ref {value}"))
        })?;
        return Ok(FieldRef::EdgeProperty {
            alias: alias.to_string(),
            property: property.to_string(),
        });
    }
    if let Some(alias) = value.strip_prefix("node_id:") {
        return Ok(FieldRef::NodeId {
            alias: alias.to_string(),
        });
    }
    Err(RedDBError::Query(format!(
        "unsupported canonical field ref {value}"
    )))
}

pub(in crate::runtime) fn legacy_runtime_system_alias(column: &str) -> Option<&'static str> {
    match column {
        "entity_id" => Some("rid"),
        "red_collection" => Some("collection"),
        "red_kind" => Some("kind"),
        _ => None,
    }
}

pub(in crate::runtime) fn resolve_runtime_field(
    record: &UnifiedRecord,
    field: &FieldRef,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Option<Value> {
    match field {
        FieldRef::TableColumn { table, column } => {
            if !table.is_empty() {
                if let Some(value) = record.get(format!("{table}.{column}").as_str()) {
                    return Some(value.clone());
                }

                let matches_context =
                    runtime_table_context_matches(table.as_str(), table_name, table_alias);
                if !matches_context {
                    if table_name.is_none() && table_alias.is_none() {
                        if let Some(value) = record.get(column.as_str()) {
                            return Some(value.clone());
                        }
                    }
                    return resolve_runtime_document_path(record, &format!("{table}.{column}"));
                }
            }

            record
                .get(column.as_str())
                .cloned()
                .or_else(|| {
                    legacy_runtime_system_alias(column)
                        .and_then(|canonical| record.get(canonical).cloned())
                })
                .or_else(|| resolve_runtime_document_path(record, column))
        }
        FieldRef::NodeProperty { alias, property } => {
            if let Some(value) = record.get(format!("{alias}.{property}").as_str()) {
                return Some(value.clone());
            }

            let node = record.nodes.get(alias)?;
            match property.as_str() {
                "id" => Some(Value::NodeRef(node.id.clone())),
                "label" => Some(Value::text(node.label.clone())),
                "type" | "node_type" => Some(Value::text(node.node_label.clone())),
                _ => node.properties.get(property).cloned(),
            }
        }
        FieldRef::EdgeProperty { alias, property } => {
            if let Some(value) = record.get(format!("{alias}.{property}").as_str()) {
                return Some(value.clone());
            }

            let edge = record.edges.get(alias)?;
            match property.as_str() {
                "from" | "source" => Some(Value::NodeRef(edge.from.clone())),
                "to" | "target" => Some(Value::NodeRef(edge.to.clone())),
                "type" | "edge_type" | "label" => Some(Value::text(edge.edge_label.clone())),
                "weight" => Some(Value::Float(edge.weight as f64)),
                _ => None,
            }
        }
        FieldRef::NodeId { alias } => record
            .nodes
            .get(alias)
            .map(|node| Value::NodeRef(node.id.clone()))
            .or_else(|| record.get(format!("{alias}.id").as_str()).cloned()),
    }
}

pub(in crate::runtime) fn runtime_table_context_matches(
    field_table: &str,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> bool {
    if Some(field_table) == table_name || Some(field_table) == table_alias {
        return true;
    }

    if !is_universal_entity_source(field_table) {
        return false;
    }

    table_name.is_some_and(is_universal_entity_source)
        || table_alias.is_some_and(is_universal_entity_source)
}

pub(in crate::runtime) fn resolve_runtime_document_path(
    record: &UnifiedRecord,
    path: &str,
) -> Option<Value> {
    // Phase 2 dotted tenancy: relax the document-capability gate so
    // dotted paths resolve against JSON stored in `TEXT` columns too.
    // The inner resolver returns None on non-JSON scalars, so the
    // behaviour for non-document rows is unchanged — there's just no
    // early exit based on the capability flag.
    let segments = parse_runtime_document_path(path);
    let (root, tail) = segments.split_first()?;
    if let Some(root_value) = record.get(root.as_str()) {
        return resolve_runtime_document_path_from_value(root_value, tail);
    }
    let (flattened_root, flattened_tail) = tail.split_first()?;
    if let Some(value) = record.get(&tail.join(".")) {
        return Some(value.clone());
    }
    let flattened_value = record.get(flattened_root.as_str())?;
    resolve_runtime_document_path_from_value(flattened_value, flattened_tail)
}

pub(in crate::runtime) fn resolve_runtime_document_path_from_value(
    value: &Value,
    path: &[String],
) -> Option<Value> {
    if path.is_empty() {
        return Some(value.clone());
    }

    match value {
        Value::Json(bytes) | Value::Blob(bytes) => {
            // A document body may be the native binary container (PRD-1398);
            // decode it to JSON so dotted `body.field` paths resolve.
            let json = crate::document_body::decode_container_to_json(bytes)
                .or_else(|| crate::json::from_slice::<JsonValue>(bytes).ok())?;
            resolve_runtime_document_json_path(&json, path)
        }
        // Phase 2 dotted tenancy: users commonly declare JSON columns
        // as `TEXT` and stash JSON strings — the FieldRef resolver
        // tries to parse in that case so dotted paths (RLS policies,
        // tenancy filters, JSONPath on text columns) work without
        // forcing a column-type change.
        Value::Text(raw) => {
            let trimmed = raw.trim_start();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                let json = crate::json::from_str::<JsonValue>(raw).ok()?;
                resolve_runtime_document_json_path(&json, path)
            } else {
                None
            }
        }
        _ => None,
    }
}

pub(in crate::runtime) fn resolve_runtime_document_json_path(
    value: &JsonValue,
    path: &[String],
) -> Option<Value> {
    let mut current = value;
    for segment in path {
        current = match current {
            JsonValue::Object(entries) => entries
                .iter()
                .find_map(|(key, value)| (key == segment).then_some(value))
                .or_else(|| {
                    entries
                        .iter()
                        .find_map(|(key, value)| key.eq_ignore_ascii_case(segment).then_some(value))
                })?,
            JsonValue::Array(items) => {
                let index = segment.parse::<usize>().ok()?;
                items.get(index)?
            }
            _ => return None,
        };
    }
    runtime_json_value_to_runtime_value(current)
}

pub(in crate::runtime) fn runtime_json_value_to_runtime_value(value: &JsonValue) -> Option<Value> {
    match value {
        JsonValue::Null => Some(Value::Null),
        JsonValue::Bool(value) => Some(Value::Boolean(*value)),
        JsonValue::Integer(value) => Some(Value::Integer(*value)),
        JsonValue::Decimal(value) => Some(Value::DecimalText(value.clone())),
        JsonValue::Number(value) => Some(Value::Float(*value)),
        JsonValue::String(value) => Some(Value::text(value.clone())),
        JsonValue::Array(values) => Some(Value::Array(
            values
                .iter()
                .map(|value| runtime_json_value_to_runtime_value(value).unwrap_or(Value::Null))
                .collect(),
        )),
        JsonValue::Object(_) => Some(Value::Json(value.to_string_compact().into_bytes())),
    }
}

pub(in crate::runtime) fn parse_runtime_document_path(path: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut chars = path.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '.' => {
                if !current.is_empty() {
                    segments.push(std::mem::take(&mut current));
                }
            }
            '[' => {
                if !current.is_empty() {
                    segments.push(std::mem::take(&mut current));
                }
                let mut index = String::new();
                for next in chars.by_ref() {
                    if next == ']' {
                        break;
                    }
                    index.push(next);
                }
                if !index.is_empty() {
                    segments.push(index);
                }
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        segments.push(current);
    }

    segments
}
