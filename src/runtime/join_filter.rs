use super::*;

pub(super) fn parse_canonical_field_ref(value: &str) -> RedDBResult<FieldRef> {
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

pub(super) fn execute_runtime_nested_loop_join(
    left_query: &TableQuery,
    left_records: &[UnifiedRecord],
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    left_join_field: &FieldRef,
    right_records: &[UnifiedRecord],
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
    right_join_field: &FieldRef,
    join_type: JoinType,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let mut matched_right = vec![false; right_records.len()];
    let mut records = Vec::new();

    for left_record in left_records {
        let mut matched = false;
        for (index, right_record) in right_records.iter().enumerate() {
            if join_condition_matches(
                left_record,
                left_table_name,
                left_table_alias,
                left_join_field,
                right_record,
                right_table_name,
                right_table_alias,
                right_join_field,
            ) {
                matched = true;
                matched_right[index] = true;
                records.push(merge_join_records(
                    Some(left_record),
                    Some(right_record),
                    left_query,
                    right_table_alias.or(right_table_name),
                ));
            }
        }

        if !matched && matches!(join_type, JoinType::LeftOuter) {
            records.push(merge_join_records(
                Some(left_record),
                None,
                left_query,
                None,
            ));
        }
    }

    if matches!(join_type, JoinType::RightOuter) {
        for (matched, right_record) in matched_right.into_iter().zip(right_records.iter()) {
            if !matched {
                records.push(merge_join_records(
                    None,
                    Some(right_record),
                    left_query,
                    right_table_alias.or(right_table_name),
                ));
            }
        }
    }

    Ok(records)
}

pub(super) fn execute_runtime_graph_lookup_join(
    left_query: &TableQuery,
    left_records: &[UnifiedRecord],
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    left_join_field: &FieldRef,
    right_records: &[UnifiedRecord],
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
    right_join_field: &FieldRef,
    join_type: JoinType,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let mut right_index: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, right_record) in right_records.iter().enumerate() {
        let keys = runtime_graph_join_record_keys(
            right_record,
            right_join_field,
            right_table_name,
            right_table_alias,
        );
        for key in keys {
            right_index.entry(key).or_default().push(index);
        }
    }

    let mut matched_right = vec![false; right_records.len()];
    let mut records = Vec::new();

    for left_record in left_records {
        let candidate_indexes = runtime_graph_join_probe_indexes(
            left_record,
            left_join_field,
            left_table_name,
            left_table_alias,
            &right_index,
        );
        let mut matched = false;

        for index in candidate_indexes {
            let right_record = &right_records[index];
            if join_condition_matches(
                left_record,
                left_table_name,
                left_table_alias,
                left_join_field,
                right_record,
                right_table_name,
                right_table_alias,
                right_join_field,
            ) {
                matched = true;
                matched_right[index] = true;
                records.push(merge_join_records(
                    Some(left_record),
                    Some(right_record),
                    left_query,
                    right_table_alias.or(right_table_name),
                ));
            }
        }

        if !matched && matches!(join_type, JoinType::LeftOuter) {
            records.push(merge_join_records(
                Some(left_record),
                None,
                left_query,
                None,
            ));
        }
    }

    if matches!(join_type, JoinType::RightOuter) {
        for (matched, right_record) in matched_right.into_iter().zip(right_records.iter()) {
            if !matched {
                records.push(merge_join_records(
                    None,
                    Some(right_record),
                    left_query,
                    right_table_alias.or(right_table_name),
                ));
            }
        }
    }

    Ok(records)
}

pub(super) fn execute_runtime_indexed_join(
    left_query: &TableQuery,
    left_records: &[UnifiedRecord],
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    left_join_field: &FieldRef,
    right_records: &[UnifiedRecord],
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
    right_join_field: &FieldRef,
    join_type: JoinType,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let mut right_index: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, right_record) in right_records.iter().enumerate() {
        let Some(value) = resolve_runtime_field(
            right_record,
            right_join_field,
            right_table_name,
            right_table_alias,
        ) else {
            continue;
        };
        let Some(key) = runtime_join_lookup_key(&value) else {
            continue;
        };
        right_index.entry(key).or_default().push(index);
    }

    let mut matched_right = vec![false; right_records.len()];
    let mut records = Vec::new();

    for left_record in left_records {
        let left_value = resolve_runtime_field(
            left_record,
            left_join_field,
            left_table_name,
            left_table_alias,
        );
        let candidate_indexes = left_value
            .as_ref()
            .and_then(runtime_join_lookup_key)
            .and_then(|key| right_index.get(&key).cloned())
            .unwrap_or_default();
        let mut matched = false;

        for index in candidate_indexes {
            let right_record = &right_records[index];
            if join_condition_matches(
                left_record,
                left_table_name,
                left_table_alias,
                left_join_field,
                right_record,
                right_table_name,
                right_table_alias,
                right_join_field,
            ) {
                matched = true;
                matched_right[index] = true;
                records.push(merge_join_records(
                    Some(left_record),
                    Some(right_record),
                    left_query,
                    right_table_alias.or(right_table_name),
                ));
            }
        }

        if !matched && matches!(join_type, JoinType::LeftOuter) {
            records.push(merge_join_records(
                Some(left_record),
                None,
                left_query,
                None,
            ));
        }
    }

    if matches!(join_type, JoinType::RightOuter) {
        for (matched, right_record) in matched_right.into_iter().zip(right_records.iter()) {
            if !matched {
                records.push(merge_join_records(
                    None,
                    Some(right_record),
                    left_query,
                    right_table_alias.or(right_table_name),
                ));
            }
        }
    }

    Ok(records)
}

pub(super) fn runtime_join_lookup_key(value: &Value) -> Option<String> {
    if let Some(number) = runtime_value_number(value) {
        return Some(format!("n:{number}"));
    }
    if let Value::Boolean(boolean) = value {
        return Some(format!("b:{boolean}"));
    }
    runtime_value_text(value).map(|value| format!("t:{value}"))
}

pub(super) fn runtime_join_lookup_keys(value: &Value) -> Vec<String> {
    let mut keys = Vec::new();
    if let Some(key) = runtime_join_lookup_key(value) {
        keys.push(key);
    }
    if let Some(identity) = runtime_join_identity_key(value) {
        keys.push(format!("id:{identity}"));
    }
    keys.sort();
    keys.dedup();
    keys
}

pub(super) fn runtime_join_identity_key(value: &Value) -> Option<String> {
    if let Some(number) = runtime_value_number(value) {
        return Some(number.to_string());
    }
    let text = runtime_value_text(value)?;
    if let Some((_, suffix)) = text.rsplit_once(':') {
        return Some(suffix.to_string());
    }
    Some(text)
}

pub(super) fn runtime_graph_join_record_keys(
    record: &UnifiedRecord,
    right_join_field: &FieldRef,
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
) -> Vec<String> {
    let mut keys = Vec::new();

    if let Some(value) = resolve_runtime_field(
        record,
        right_join_field,
        right_table_name,
        right_table_alias,
    ) {
        keys.extend(runtime_join_lookup_keys(&value));
    }

    for hint in ["_source_node", "_source_edge", "_linked_identity"] {
        if let Some(value) = record.values.get(hint) {
            keys.extend(runtime_join_lookup_keys(value));
        }
    }

    for node in record.nodes.values() {
        keys.extend(runtime_join_lookup_keys(&Value::NodeRef(node.id.clone())));
    }

    for edge in record.edges.values() {
        keys.extend(runtime_join_lookup_keys(&Value::NodeRef(edge.from.clone())));
        keys.extend(runtime_join_lookup_keys(&Value::NodeRef(edge.to.clone())));
    }

    keys.sort();
    keys.dedup();
    keys
}

pub(super) fn runtime_graph_join_probe_indexes(
    left_record: &UnifiedRecord,
    left_join_field: &FieldRef,
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    right_index: &HashMap<String, Vec<usize>>,
) -> Vec<usize> {
    let mut candidates = BTreeSet::new();
    if let Some(value) = resolve_runtime_field(
        left_record,
        left_join_field,
        left_table_name,
        left_table_alias,
    ) {
        for key in runtime_join_lookup_keys(&value) {
            if let Some(indexes) = right_index.get(&key) {
                candidates.extend(indexes.iter().copied());
            }
        }
    }
    for hint in ["_source_node", "_source_edge", "_linked_identity"] {
        if let Some(value) = left_record.values.get(hint) {
            for key in runtime_join_lookup_keys(value) {
                if let Some(indexes) = right_index.get(&key) {
                    candidates.extend(indexes.iter().copied());
                }
            }
        }
    }
    candidates.into_iter().collect()
}

pub(super) fn execute_runtime_full_scan_join(
    left_query: &TableQuery,
    left_records: &[UnifiedRecord],
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    left_join_field: &FieldRef,
    right_records: &[UnifiedRecord],
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
    right_join_field: &FieldRef,
    join_type: JoinType,
) -> RedDBResult<Vec<UnifiedRecord>> {
    execute_runtime_nested_loop_join(
        left_query,
        left_records,
        left_table_name,
        left_table_alias,
        left_join_field,
        right_records,
        right_table_name,
        right_table_alias,
        right_join_field,
        join_type,
    )
}

pub(super) fn project_runtime_record(
    source: &UnifiedRecord,
    projections: &[Projection],
    table_name: Option<&str>,
    table_alias: Option<&str>,
    document_projection: bool,
    entity_projection: bool,
) -> UnifiedRecord {
    let select_all = projections.is_empty()
        || projections
            .iter()
            .any(|item| matches!(item, Projection::All));
    let mut record = UnifiedRecord::new();
    record.nodes = source.nodes.clone();
    record.edges = source.edges.clone();
    record.paths = source.paths.clone();
    record.vector_results = source.vector_results.clone();

    if select_all {
        for key in visible_value_keys(source) {
            if let Some(value) = source.values.get(&key) {
                record.values.insert(key, value.clone());
            }
        }
    }

    for projection in projections {
        if matches!(projection, Projection::All) {
            continue;
        }

        let label = projection_name(projection);
        let value = match projection {
            Projection::Column(column) => resolve_runtime_projection_value(
                source,
                column,
                table_name,
                table_alias,
                document_projection,
                entity_projection,
            ),
            Projection::Alias(column, _) => resolve_runtime_projection_value(
                source,
                column,
                table_name,
                table_alias,
                document_projection,
                entity_projection,
            ),
            Projection::Field(field, _) => {
                resolve_runtime_field(source, field, table_name, table_alias)
            }
            Projection::Expression(filter, _) => Some(Value::Boolean(evaluate_runtime_filter(
                source,
                filter,
                table_name,
                table_alias,
            ))),
            Projection::Function(_, _) => Some(Value::Null),
            Projection::All => None,
        };

        record.values.insert(label, value.unwrap_or(Value::Null));
    }

    record
}

pub(super) fn resolve_runtime_projection_value(
    source: &UnifiedRecord,
    column: &str,
    table_name: Option<&str>,
    table_alias: Option<&str>,
    document_projection: bool,
    entity_projection: bool,
) -> Option<Value> {
    source.values.get(column).cloned().or_else(|| {
        if document_projection || entity_projection {
            let field = FieldRef::TableColumn {
                table: table_alias.or(table_name).unwrap_or_default().to_string(),
                column: column.to_string(),
            };
            resolve_runtime_field(source, &field, table_name, table_alias)
        } else {
            None
        }
    })
}

pub(super) fn projected_columns(
    records: &[UnifiedRecord],
    projections: &[Projection],
) -> Vec<String> {
    if projections.is_empty()
        || projections
            .iter()
            .any(|item| matches!(item, Projection::All))
    {
        return collect_visible_columns(records);
    }

    projections
        .iter()
        .filter(|projection| !matches!(projection, Projection::All))
        .map(projection_name)
        .collect()
}

pub(super) fn collect_visible_columns(records: &[UnifiedRecord]) -> Vec<String> {
    let mut columns = BTreeSet::new();
    for record in records {
        for key in visible_value_keys(record) {
            columns.insert(key);
        }
    }
    columns.into_iter().collect()
}

pub(super) fn visible_value_keys(record: &UnifiedRecord) -> Vec<String> {
    let mut keys: Vec<String> = record
        .values
        .keys()
        .filter(|key| !key.starts_with('_'))
        .cloned()
        .collect();
    keys.sort();
    keys
}

pub(super) fn projection_name(projection: &Projection) -> String {
    match projection {
        Projection::All => "*".to_string(),
        Projection::Column(column) => column.clone(),
        Projection::Alias(_, alias) => alias.clone(),
        Projection::Function(name, _) => name.clone(),
        Projection::Expression(_, alias) => alias.clone().unwrap_or_else(|| "expr".to_string()),
        Projection::Field(field, alias) => alias.clone().unwrap_or_else(|| field_ref_name(field)),
    }
}

pub(super) fn field_ref_name(field: &FieldRef) -> String {
    match field {
        FieldRef::TableColumn { table, column } => {
            if table.is_empty() {
                column.clone()
            } else {
                format!("{table}.{column}")
            }
        }
        FieldRef::NodeProperty { alias, property } => format!("{alias}.{property}"),
        FieldRef::EdgeProperty { alias, property } => format!("{alias}.{property}"),
        FieldRef::NodeId { alias } => format!("{alias}.id"),
    }
}

pub(super) fn evaluate_runtime_filter(
    record: &UnifiedRecord,
    filter: &Filter,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> bool {
    match filter {
        Filter::Compare { field, op, value } => {
            resolve_runtime_field(record, field, table_name, table_alias)
                .as_ref()
                .and_then(|candidate| evaluate_metadata_field_compare(field, candidate, *op, value))
                .or_else(|| {
                    resolve_runtime_field(record, field, table_name, table_alias)
                        .as_ref()
                        .map(|candidate| compare_runtime_values(candidate, value, *op))
                })
                .unwrap_or(false)
        }
        Filter::And(left, right) => {
            evaluate_runtime_filter(record, left, table_name, table_alias)
                && evaluate_runtime_filter(record, right, table_name, table_alias)
        }
        Filter::Or(left, right) => {
            evaluate_runtime_filter(record, left, table_name, table_alias)
                || evaluate_runtime_filter(record, right, table_name, table_alias)
        }
        Filter::Not(inner) => !evaluate_runtime_filter(record, inner, table_name, table_alias),
        Filter::IsNull(field) => resolve_runtime_field(record, field, table_name, table_alias)
            .map(|value| value == Value::Null)
            .unwrap_or(true),
        Filter::IsNotNull(field) => resolve_runtime_field(record, field, table_name, table_alias)
            .map(|value| value != Value::Null)
            .unwrap_or(false),
        Filter::In { field, values } => {
            resolve_runtime_field(record, field, table_name, table_alias)
                .as_ref()
                .is_some_and(|candidate| {
                    evaluate_metadata_field_in(field, candidate, values).unwrap_or_else(|| {
                        values
                            .iter()
                            .any(|value| compare_runtime_values(candidate, value, CompareOp::Eq))
                    })
                })
        }
        Filter::Between { field, low, high } => {
            resolve_runtime_field(record, field, table_name, table_alias)
                .as_ref()
                .is_some_and(|candidate| {
                    compare_runtime_values(candidate, low, CompareOp::Ge)
                        && compare_runtime_values(candidate, high, CompareOp::Le)
                })
        }
        Filter::Like { field, pattern } => {
            resolve_runtime_field(record, field, table_name, table_alias)
                .as_ref()
                .and_then(runtime_value_text)
                .is_some_and(|value| like_matches(&value, pattern))
        }
        Filter::StartsWith { field, prefix } => {
            resolve_runtime_field(record, field, table_name, table_alias)
                .as_ref()
                .and_then(runtime_value_text)
                .is_some_and(|value| value.starts_with(prefix))
        }
        Filter::EndsWith { field, suffix } => {
            resolve_runtime_field(record, field, table_name, table_alias)
                .as_ref()
                .and_then(runtime_value_text)
                .is_some_and(|value| value.ends_with(suffix))
        }
        Filter::Contains { field, substring } => {
            resolve_runtime_field(record, field, table_name, table_alias)
                .as_ref()
                .and_then(runtime_value_text)
                .is_some_and(|value| value.contains(substring))
        }
    }
}

pub(super) fn compare_runtime_order(
    left: &UnifiedRecord,
    right: &UnifiedRecord,
    clauses: &[OrderByClause],
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Ordering {
    for clause in clauses {
        let left_value = resolve_runtime_field(left, &clause.field, table_name, table_alias);
        let right_value = resolve_runtime_field(right, &clause.field, table_name, table_alias);
        let ordering = compare_runtime_optional_values(
            left_value.as_ref(),
            right_value.as_ref(),
            clause.nulls_first,
        );

        if ordering != Ordering::Equal {
            return if clause.ascending {
                ordering
            } else {
                ordering.reverse()
            };
        }
    }

    runtime_record_identity_key(left).cmp(&runtime_record_identity_key(right))
}

pub(super) fn compare_runtime_optional_values(
    left: Option<&Value>,
    right: Option<&Value>,
    nulls_first: bool,
) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => {
            if nulls_first {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (Some(_), None) => {
            if nulls_first {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (Some(Value::Null), Some(Value::Null)) => Ordering::Equal,
        (Some(Value::Null), Some(_)) => {
            if nulls_first {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (Some(_), Some(Value::Null)) => {
            if nulls_first {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (Some(left), Some(right)) => runtime_partial_cmp(left, right).unwrap_or(Ordering::Equal),
    }
}

pub(super) fn resolve_runtime_field(
    record: &UnifiedRecord,
    field: &FieldRef,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Option<Value> {
    match field {
        FieldRef::TableColumn { table, column } => {
            if !table.is_empty() {
                if let Some(value) = record.values.get(&format!("{table}.{column}")) {
                    return Some(value.clone());
                }

                let matches_context =
                    runtime_table_context_matches(table.as_str(), table_name, table_alias);
                if !matches_context {
                    return resolve_runtime_document_path(record, &format!("{table}.{column}"));
                }
            }

            record
                .values
                .get(column)
                .cloned()
                .or_else(|| resolve_runtime_document_path(record, column))
        }
        FieldRef::NodeProperty { alias, property } => {
            if let Some(value) = record.values.get(&format!("{alias}.{property}")) {
                return Some(value.clone());
            }

            let node = record.nodes.get(alias)?;
            match property.as_str() {
                "id" => Some(Value::NodeRef(node.id.clone())),
                "label" => Some(Value::Text(node.label.clone())),
                "type" | "node_type" => Some(Value::Text(format!("{:?}", node.node_type))),
                _ => None,
            }
        }
        FieldRef::EdgeProperty { alias, property } => {
            if let Some(value) = record.values.get(&format!("{alias}.{property}")) {
                return Some(value.clone());
            }

            let edge = record.edges.get(alias)?;
            match property.as_str() {
                "from" | "source" => Some(Value::NodeRef(edge.from.clone())),
                "to" | "target" => Some(Value::NodeRef(edge.to.clone())),
                "type" | "edge_type" | "label" => {
                    Some(Value::Text(format!("{:?}", edge.edge_type)))
                }
                "weight" => Some(Value::Float(edge.weight as f64)),
                _ => None,
            }
        }
        FieldRef::NodeId { alias } => record
            .nodes
            .get(alias)
            .map(|node| Value::NodeRef(node.id.clone()))
            .or_else(|| record.values.get(&format!("{alias}.id")).cloned()),
    }
}

pub(super) fn runtime_table_context_matches(
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

pub(super) fn resolve_runtime_document_path(record: &UnifiedRecord, path: &str) -> Option<Value> {
    if !runtime_record_has_document_capability(record) {
        return None;
    }
    let segments = parse_runtime_document_path(path);
    let (root, tail) = segments.split_first()?;
    let root_value = record.values.get(root)?;
    resolve_runtime_document_path_from_value(root_value, tail)
}

pub(super) fn resolve_runtime_document_path_from_value(
    value: &Value,
    path: &[String],
) -> Option<Value> {
    if path.is_empty() {
        return Some(value.clone());
    }

    match value {
        Value::Json(bytes) | Value::Blob(bytes) => {
            let json = crate::json::from_slice::<JsonValue>(bytes).ok()?;
            resolve_runtime_document_json_path(&json, path)
        }
        _ => None,
    }
}

pub(super) fn resolve_runtime_document_json_path(
    value: &JsonValue,
    path: &[String],
) -> Option<Value> {
    let mut current = value;
    for segment in path {
        current = match current {
            JsonValue::Object(entries) => {
                entries.iter().find_map(
                    |(key, value)| {
                        if key == segment {
                            Some(value)
                        } else {
                            None
                        }
                    },
                )?
            }
            JsonValue::Array(items) => {
                let index = segment.parse::<usize>().ok()?;
                items.get(index)?
            }
            _ => return None,
        };
    }
    runtime_json_scalar_to_value(current)
}

pub(super) fn runtime_json_scalar_to_value(value: &JsonValue) -> Option<Value> {
    match value {
        JsonValue::Null => Some(Value::Null),
        JsonValue::Bool(value) => Some(Value::Boolean(*value)),
        JsonValue::Number(value) => Some(Value::Float(*value)),
        JsonValue::String(value) => Some(Value::Text(value.clone())),
        JsonValue::Array(_) | JsonValue::Object(_) => None,
    }
}

pub(super) fn parse_runtime_document_path(path: &str) -> Vec<String> {
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

pub(super) fn compare_runtime_values(left: &Value, right: &Value, op: CompareOp) -> bool {
    match op {
        CompareOp::Eq => runtime_values_equal(left, right),
        CompareOp::Ne => !runtime_values_equal(left, right),
        CompareOp::Lt => runtime_partial_cmp(left, right).is_some_and(|ord| ord == Ordering::Less),
        CompareOp::Le => runtime_partial_cmp(left, right)
            .is_some_and(|ord| matches!(ord, Ordering::Less | Ordering::Equal)),
        CompareOp::Gt => {
            runtime_partial_cmp(left, right).is_some_and(|ord| ord == Ordering::Greater)
        }
        CompareOp::Ge => runtime_partial_cmp(left, right)
            .is_some_and(|ord| matches!(ord, Ordering::Greater | Ordering::Equal)),
    }
}

pub(super) fn runtime_values_equal(left: &Value, right: &Value) -> bool {
    if let (Some(left), Some(right)) = (runtime_value_number(left), runtime_value_number(right)) {
        return left == right;
    }

    if let (Some(left), Some(right)) = (runtime_value_text(left), runtime_value_text(right)) {
        return left == right;
    }

    if let (Value::Boolean(left), Value::Boolean(right)) = (left, right) {
        return left == right;
    }

    left == right
}

pub(super) fn runtime_partial_cmp(left: &Value, right: &Value) -> Option<Ordering> {
    if let (Some(left), Some(right)) = (runtime_value_number(left), runtime_value_number(right)) {
        return left.partial_cmp(&right);
    }

    if let (Some(left), Some(right)) = (runtime_value_text(left), runtime_value_text(right)) {
        return Some(left.cmp(&right));
    }

    match (left, right) {
        (Value::Boolean(left), Value::Boolean(right)) => Some(left.cmp(right)),
        _ => None,
    }
}

pub(super) fn runtime_value_number(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(value) => Some(*value as f64),
        Value::UnsignedInteger(value) => Some(*value as f64),
        Value::Float(value) => Some(*value),
        Value::Timestamp(value) => Some(*value as f64),
        Value::Duration(value) => Some(*value as f64),
        _ => None,
    }
}

pub(super) fn runtime_value_text(value: &Value) -> Option<String> {
    match value {
        Value::Text(value) => Some(value.clone()),
        Value::NodeRef(value) => Some(value.clone()),
        Value::EdgeRef(value) => Some(value.clone()),
        Value::RowRef(table, row_id) => Some(format!("{table}:{row_id}")),
        Value::VectorRef(collection, vector_id) => Some(format!("{collection}:{vector_id}")),
        Value::IpAddr(value) => Some(value.to_string()),
        Value::MacAddr(value) => Some(format!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            value[0], value[1], value[2], value[3], value[4], value[5]
        )),
        Value::Uuid(value) => Some(
            value
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
        ),
        Value::Boolean(value) => Some(value.to_string()),
        Value::Integer(value) => Some(value.to_string()),
        Value::UnsignedInteger(value) => Some(value.to_string()),
        Value::Float(value) => Some(value.to_string()),
        Value::Timestamp(value) => Some(value.to_string()),
        Value::Duration(value) => Some(value.to_string()),
        Value::Null => None,
        Value::Blob(_) | Value::Vector(_) | Value::Json(_) => None,
        Value::Color([r, g, b]) => Some(format!("#{:02X}{:02X}{:02X}", r, g, b)),
        Value::Email(s) => Some(s.clone()),
        Value::Url(s) => Some(s.clone()),
        Value::Phone(n) => Some(format!("+{}", n)),
        Value::Semver(packed) => Some(format!(
            "{}.{}.{}",
            packed / 1_000_000,
            (packed / 1_000) % 1_000,
            packed % 1_000
        )),
        Value::Cidr(ip, prefix) => Some(format!(
            "{}.{}.{}.{}/{}",
            (ip >> 24) & 0xFF,
            (ip >> 16) & 0xFF,
            (ip >> 8) & 0xFF,
            ip & 0xFF,
            prefix
        )),
        Value::Date(days) => Some(days.to_string()),
        Value::Time(ms) => {
            let total_secs = ms / 1000;
            Some(format!(
                "{:02}:{:02}:{:02}",
                total_secs / 3600,
                (total_secs / 60) % 60,
                total_secs % 60
            ))
        }
        Value::Decimal(v) => Some(format!("{:.4}", *v as f64 / 10_000.0)),
        Value::EnumValue(i) => Some(format!("enum({})", i)),
        Value::Array(_) => None,
        Value::TimestampMs(ms) => Some(ms.to_string()),
        Value::Ipv4(ip) => Some(format!(
            "{}.{}.{}.{}",
            (ip >> 24) & 0xFF,
            (ip >> 16) & 0xFF,
            (ip >> 8) & 0xFF,
            ip & 0xFF
        )),
        Value::Ipv6(bytes) => Some(format!("{}", std::net::Ipv6Addr::from(*bytes))),
        Value::Subnet(ip, mask) => {
            let prefix = mask.leading_ones();
            Some(format!(
                "{}.{}.{}.{}/{}",
                (ip >> 24) & 0xFF,
                (ip >> 16) & 0xFF,
                (ip >> 8) & 0xFF,
                ip & 0xFF,
                prefix
            ))
        }
        Value::Port(p) => Some(p.to_string()),
        Value::Latitude(micro) => Some(format!("{:.6}", *micro as f64 / 1_000_000.0)),
        Value::Longitude(micro) => Some(format!("{:.6}", *micro as f64 / 1_000_000.0)),
        Value::GeoPoint(lat, lon) => Some(format!(
            "{:.6},{:.6}",
            *lat as f64 / 1_000_000.0,
            *lon as f64 / 1_000_000.0
        )),
        Value::Country2(c) => Some(String::from_utf8_lossy(c).to_string()),
        Value::Country3(c) => Some(String::from_utf8_lossy(c).to_string()),
        Value::Lang2(c) => Some(String::from_utf8_lossy(c).to_string()),
        Value::Lang5(c) => Some(String::from_utf8_lossy(c).to_string()),
        Value::Currency(c) => Some(String::from_utf8_lossy(c).to_string()),
        Value::ColorAlpha([r, g, b, a]) => Some(format!("#{:02X}{:02X}{:02X}{:02X}", r, g, b, a)),
        Value::BigInt(v) => Some(v.to_string()),
        Value::KeyRef(col, key) => Some(format!("{}:{}", col, key)),
        Value::DocRef(col, id) => Some(format!("{}#{}", col, id)),
        Value::TableRef(name) => Some(name.clone()),
        Value::PageRef(page_id) => Some(format!("page:{}", page_id)),
    }
}

pub(super) fn table_column_name(field: &FieldRef) -> Option<&str> {
    match field {
        FieldRef::TableColumn { column, .. } => Some(column.as_str()),
        _ => None,
    }
}

pub(super) fn evaluate_metadata_field_compare(
    field: &FieldRef,
    candidate: &Value,
    op: CompareOp,
    value: &Value,
) -> Option<bool> {
    let column = table_column_name(field)?;
    if !column.eq_ignore_ascii_case("_capabilities") {
        if column.eq_ignore_ascii_case("_entity_type") {
            let candidate = runtime_value_text(candidate).map(|item| item.to_ascii_lowercase())?;
            let value = runtime_value_text(value).map(|item| item.to_ascii_lowercase())?;
            return Some(match op {
                CompareOp::Eq => candidate == value,
                CompareOp::Ne => candidate != value,
                _ => false,
            });
        }

        return None;
    }

    let capability = runtime_value_text(value)?;
    let capabilities = runtime_value_text(candidate)?;
    let capabilities = capabilities
        .split(',')
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    let target = capability.trim().to_ascii_lowercase();

    match op {
        CompareOp::Eq => Some(capabilities.iter().any(|value| value == &target)),
        CompareOp::Ne => Some(!capabilities.iter().any(|value| value == &target)),
        _ => None,
    }
}

pub(super) fn evaluate_metadata_field_in(
    field: &FieldRef,
    candidate: &Value,
    values: &[Value],
) -> Option<bool> {
    let column = table_column_name(field)?;
    if !column.eq_ignore_ascii_case("_capabilities") {
        if !column.eq_ignore_ascii_case("_entity_type") {
            return None;
        }

        let candidate = runtime_value_text(candidate).map(|item| item.to_ascii_lowercase())?;

        for value in values {
            let Some(value) = runtime_value_text(value) else {
                continue;
            };
            if value.to_ascii_lowercase() == candidate {
                return Some(true);
            }
        }

        return Some(false);
    }

    let capabilities = runtime_value_text(candidate)?
        .split(',')
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();

    if capabilities.is_empty() {
        return Some(false);
    }

    for value in values {
        let Some(value) = runtime_value_text(value) else {
            continue;
        };
        let value = value.trim().to_ascii_lowercase();
        if capabilities.iter().any(|candidate| candidate == &value) {
            return Some(true);
        }
    }
    Some(false)
}

pub(super) fn like_matches(value: &str, pattern: &str) -> bool {
    like_matches_bytes(value.as_bytes(), pattern.as_bytes())
}

pub(super) fn like_matches_bytes(value: &[u8], pattern: &[u8]) -> bool {
    if pattern.is_empty() {
        return value.is_empty();
    }

    match pattern[0] {
        b'%' => {
            like_matches_bytes(value, &pattern[1..])
                || (!value.is_empty() && like_matches_bytes(&value[1..], pattern))
        }
        b'_' => !value.is_empty() && like_matches_bytes(&value[1..], &pattern[1..]),
        byte => {
            !value.is_empty() && value[0] == byte && like_matches_bytes(&value[1..], &pattern[1..])
        }
    }
}

pub(super) fn query_expr_name(expr: &QueryExpr) -> &'static str {
    match expr {
        QueryExpr::Table(_) => "table",
        QueryExpr::Graph(_) => "graph",
        QueryExpr::Join(_) => "join",
        QueryExpr::Path(_) => "path",
        QueryExpr::Vector(_) => "vector",
        QueryExpr::Hybrid(_) => "hybrid",
        QueryExpr::Insert(_) => "insert",
        QueryExpr::Update(_) => "update",
        QueryExpr::Delete(_) => "delete",
        QueryExpr::CreateTable(_) => "create_table",
        QueryExpr::DropTable(_) => "drop_table",
        QueryExpr::AlterTable(_) => "alter_table",
        QueryExpr::GraphCommand(_) => "graph_command",
        QueryExpr::SearchCommand(_) => "search_command",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_evaluate_metadata_field_compare_entity_type_is_case_insensitive() {
        let field = FieldRef::TableColumn {
            table: "any".to_string(),
            column: "_entity_type".to_string(),
        };

        assert_eq!(
            evaluate_metadata_field_compare(
                &field,
                &Value::Text("table".to_string()),
                CompareOp::Eq,
                &Value::Text("TABLE".to_string()),
            ),
            Some(true)
        );

        assert_eq!(
            evaluate_metadata_field_compare(
                &field,
                &Value::Text("graph_node".to_string()),
                CompareOp::Ne,
                &Value::Text("GRAPH_NODE".to_string()),
            ),
            Some(false)
        );
    }

    #[test]
    fn test_evaluate_metadata_field_in_entity_type_is_case_insensitive() {
        let field = FieldRef::TableColumn {
            table: "any".to_string(),
            column: "_entity_type".to_string(),
        };

        assert_eq!(
            evaluate_metadata_field_in(
                &field,
                &Value::Text("vector".to_string()),
                &[
                    Value::Text("TABLE".to_string()),
                    Value::Text("vector".to_string()),
                    Value::Text("graph_node".to_string()),
                ],
            ),
            Some(true)
        );

        assert_eq!(
            evaluate_metadata_field_in(
                &field,
                &Value::Text("document".to_string()),
                &[
                    Value::Text("TABLE".to_string()),
                    Value::Text("GRAPH_NODE".to_string()),
                ],
            ),
            Some(false)
        );
    }

    #[test]
    fn test_evaluate_metadata_field_compare_entity_type_unsupported_op_is_false() {
        let field = FieldRef::TableColumn {
            table: "any".to_string(),
            column: "_entity_type".to_string(),
        };

        assert_eq!(
            evaluate_metadata_field_compare(
                &field,
                &Value::Text("vector".to_string()),
                CompareOp::Gt,
                &Value::Text("vector".to_string()),
            ),
            Some(false)
        );
    }
}
