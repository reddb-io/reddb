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

/// Emit the Cartesian product of two record sets as a flat Vec of
/// merged records. Shared by every runtime join loop — CROSS JOIN
/// has no predicate, so the loop contents are identical regardless
/// of which dispatcher was chosen (nested / hash / graph / indexed).
fn cross_join_records(
    left_records: &[UnifiedRecord],
    right_records: &[UnifiedRecord],
    left_query: &TableQuery,
    right_alias_or_name: Option<&str>,
) -> Vec<UnifiedRecord> {
    let mut records = Vec::with_capacity(left_records.len() * right_records.len());
    for left_record in left_records {
        for right_record in right_records {
            records.push(merge_join_records(
                Some(left_record),
                Some(right_record),
                left_query,
                right_alias_or_name,
            ));
        }
    }
    records
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
    if matches!(join_type, JoinType::Cross) {
        return Ok(cross_join_records(
            left_records,
            right_records,
            left_query,
            right_table_alias.or(right_table_name),
        ));
    }

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

        if !matched && matches!(join_type, JoinType::LeftOuter | JoinType::FullOuter) {
            records.push(merge_join_records(
                Some(left_record),
                None,
                left_query,
                None,
            ));
        }
    }

    if matches!(join_type, JoinType::RightOuter | JoinType::FullOuter) {
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

/// Hash join — O(n+m) instead of O(n*m) for large record sets.
/// Builds a hash table on the right side, probes with the left side.
pub(super) fn execute_runtime_hash_join(
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
    if matches!(join_type, JoinType::Cross) {
        return Ok(cross_join_records(
            left_records,
            right_records,
            left_query,
            right_table_alias.or(right_table_name),
        ));
    }
    // Build hash table on right side
    let mut hash_table: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, right_record) in right_records.iter().enumerate() {
        let key = resolve_runtime_field(
            right_record,
            right_join_field,
            right_table_name,
            right_table_alias,
        )
        .map(|v| v.to_string())
        .unwrap_or_default();
        hash_table.entry(key).or_default().push(idx);
    }

    let mut matched_right = vec![false; right_records.len()];
    let mut records = Vec::new();

    // Probe with left side — O(1) lookup per left record
    for left_record in left_records {
        let key = resolve_runtime_field(
            left_record,
            left_join_field,
            left_table_name,
            left_table_alias,
        )
        .map(|v| v.to_string())
        .unwrap_or_default();

        let mut matched = false;
        if let Some(indices) = hash_table.get(&key) {
            for &idx in indices {
                matched = true;
                matched_right[idx] = true;
                records.push(merge_join_records(
                    Some(left_record),
                    Some(&right_records[idx]),
                    left_query,
                    right_table_alias.or(right_table_name),
                ));
            }
        }

        if !matched && matches!(join_type, JoinType::LeftOuter | JoinType::FullOuter) {
            records.push(merge_join_records(
                Some(left_record),
                None,
                left_query,
                None,
            ));
        }
    }

    if matches!(join_type, JoinType::RightOuter | JoinType::FullOuter) {
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
    if matches!(join_type, JoinType::Cross) {
        return Ok(cross_join_records(
            left_records,
            right_records,
            left_query,
            right_table_alias.or(right_table_name),
        ));
    }
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

        if !matched && matches!(join_type, JoinType::LeftOuter | JoinType::FullOuter) {
            records.push(merge_join_records(
                Some(left_record),
                None,
                left_query,
                None,
            ));
        }
    }

    if matches!(join_type, JoinType::RightOuter | JoinType::FullOuter) {
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
    if matches!(join_type, JoinType::Cross) {
        return Ok(cross_join_records(
            left_records,
            right_records,
            left_query,
            right_table_alias.or(right_table_name),
        ));
    }
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

        if !matched && matches!(join_type, JoinType::LeftOuter | JoinType::FullOuter) {
            records.push(merge_join_records(
                Some(left_record),
                None,
                left_query,
                None,
            ));
        }
    }

    if matches!(join_type, JoinType::RightOuter | JoinType::FullOuter) {
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
    project_runtime_record_with_db(
        None,
        source,
        projections,
        table_name,
        table_alias,
        document_projection,
        entity_projection,
    )
}

pub(super) fn project_runtime_record_with_db(
    db: Option<&RedDB>,
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
            if let Some(value) = source.values.get(key.as_str()) {
                record
                    .values
                    .insert(std::sync::Arc::from(key), value.clone());
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
            Projection::Expression(filter, _) => Some(Value::Boolean(
                evaluate_runtime_filter_with_db(db, source, filter, table_name, table_alias),
            )),
            Projection::Function(ref name, ref args) => {
                evaluate_scalar_function_with_db(db, name, args, source)
            }
            Projection::All => None,
        };

        record
            .values
            .insert(std::sync::Arc::from(label), value.unwrap_or(Value::Null));
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
    // Uniform-schema fast path: for table-row result sets every record
    // carries the same key set. Sample the first record, then spot-check
    // a few more. If everything matches, return the sampled keys — we
    // skip iterating the remaining ~thousands of records and their
    // HashSet churn. This was the dominant cost on SELECT * workloads
    // (~32% of query time after the cross-index fix).
    if let Some(first) = records.first() {
        let mut seen: std::collections::HashSet<&str> =
            std::collections::HashSet::with_capacity(first.values.len());
        let mut keys: Vec<String> = Vec::with_capacity(first.values.len());
        for key in first.values.keys() {
            let k: &str = key;
            if !k.starts_with('_') && seen.insert(k) {
                keys.push(k.to_string());
            }
        }

        let n = records.len();
        let step = (n / 8).max(1);
        let mut uniform = true;
        let mut idx = step;
        while idx < n {
            let rec = &records[idx];
            for key in rec.values.keys() {
                let k: &str = key;
                if k.starts_with('_') {
                    continue;
                }
                if !seen.contains(k) {
                    uniform = false;
                    break;
                }
            }
            if !uniform {
                break;
            }
            idx += step;
        }

        if uniform {
            keys.sort();
            return keys;
        }
    }

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for record in records {
        for key in record.values.keys() {
            let k: &str = key;
            if !k.starts_with('_') && !seen.contains(k) {
                seen.insert(k.to_string());
            }
        }
    }
    let mut columns: Vec<String> = seen.into_iter().collect();
    columns.sort();
    columns
}

pub(super) fn visible_value_keys(record: &UnifiedRecord) -> Vec<String> {
    let mut keys: Vec<String> = record
        .values
        .keys()
        .filter_map(|key| {
            let k: &str = key;
            if k.starts_with('_') {
                None
            } else {
                Some(k.to_string())
            }
        })
        .collect();
    keys.sort();
    keys
}

pub(super) fn projection_name(projection: &Projection) -> String {
    match projection {
        Projection::All => "*".to_string(),
        Projection::Column(column) => column.clone(),
        Projection::Alias(_, alias) => alias.clone(),
        // Scalar function projections encode `FUNC(...) AS alias`
        // as `"FUNC:alias"` in the first tuple field. If an alias
        // is present, expose it as the output column name; otherwise
        // fall back to the function name.
        Projection::Function(name, _) => {
            if let Some((_, alias)) = name.split_once(':') {
                alias.to_string()
            } else {
                name.clone()
            }
        }
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
    evaluate_runtime_filter_with_db(None, record, filter, table_name, table_alias)
}

pub(super) fn evaluate_runtime_filter_with_db(
    db: Option<&RedDB>,
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
        Filter::CompareFields { left, op, right } => {
            let left_value = resolve_runtime_field(record, left, table_name, table_alias);
            let right_value = resolve_runtime_field(record, right, table_name, table_alias);
            match (left_value, right_value) {
                (Some(l), Some(r)) => compare_runtime_values(&l, &r, *op),
                _ => false,
            }
        }
        Filter::CompareExpr { lhs, op, rhs } => {
            // Evaluate both sides as Expr trees and compare the
            // resulting Values. Missing / null operands collapse to
            // false so the predicate acts like SQL three-valued
            // logic's UNKNOWN → not-matched.
            let l = super::expr_eval::evaluate_runtime_expr_with_db(
                db,
                lhs,
                record,
                table_name,
                table_alias,
            );
            let r = super::expr_eval::evaluate_runtime_expr_with_db(
                db,
                rhs,
                record,
                table_name,
                table_alias,
            );
            match (l, r) {
                (Some(lv), Some(rv)) => compare_runtime_values(&lv, &rv, *op),
                _ => false,
            }
        }
        Filter::And(left, right) => {
            evaluate_runtime_filter_with_db(db, record, left, table_name, table_alias)
                && evaluate_runtime_filter_with_db(db, record, right, table_name, table_alias)
        }
        Filter::Or(left, right) => {
            evaluate_runtime_filter_with_db(db, record, left, table_name, table_alias)
                || evaluate_runtime_filter_with_db(db, record, right, table_name, table_alias)
        }
        Filter::Not(inner) => {
            !evaluate_runtime_filter_with_db(db, record, inner, table_name, table_alias)
        }
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
    compare_runtime_order_with_db(None, left, right, clauses, table_name, table_alias)
}

pub(super) fn compare_runtime_order_with_db(
    db: Option<&RedDB>,
    left: &UnifiedRecord,
    right: &UnifiedRecord,
    clauses: &[OrderByClause],
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Ordering {
    for clause in clauses {
        // Fase 1.6: when the ORDER BY item is an expression (CAST,
        // arithmetic, CASE, etc.), evaluate it against each record
        // and compare the resulting Values. Bare-column clauses fall
        // back to the direct field resolver which is cheaper for the
        // common case.
        let (left_value, right_value) = if let Some(ref expr) = clause.expr {
            (
                super::expr_eval::evaluate_runtime_expr_with_db(
                    db,
                    expr,
                    left,
                    table_name,
                    table_alias,
                ),
                super::expr_eval::evaluate_runtime_expr_with_db(
                    db,
                    expr,
                    right,
                    table_name,
                    table_alias,
                ),
            )
        } else {
            (
                resolve_runtime_field(left, &clause.field, table_name, table_alias),
                resolve_runtime_field(right, &clause.field, table_name, table_alias),
            )
        };
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

/// Sort `records` by `order_by` using the Schwartzian transform:
/// extract sort keys once per record (O(n)), sort by the extracted keys
/// (O(n log n) value comparisons, no HashMap lookups), then reorder.
///
/// For a naive `sort_by(compare_runtime_order)`, the sort calls
/// `resolve_runtime_field` O(n log n) times — once per comparison.
/// With pre-extraction, field resolution is O(n) regardless of sort depth.
/// A single sort key, carrying the full `Value` plus an optional `u64` abbreviated key
/// for `Text` values so the comparator can skip full string comparisons in the common case.
struct SortKey {
    value: Option<Value>,
    abbrev: Option<u64>,
}

impl SortKey {
    fn new(value: Option<Value>) -> Self {
        let abbrev = match &value {
            Some(Value::Text(s)) => Some(text_abbrev_key(s)),
            _ => None,
        };
        SortKey { value, abbrev }
    }
}

pub(super) fn sort_records_by_order_by(
    records: &mut Vec<UnifiedRecord>,
    order_by: &[OrderByClause],
    table_name: Option<&str>,
    table_alias: Option<&str>,
) {
    sort_records_by_order_by_with_db(None, records, order_by, table_name, table_alias)
}

pub(super) fn sort_records_by_order_by_with_db(
    db: Option<&RedDB>,
    records: &mut Vec<UnifiedRecord>,
    order_by: &[OrderByClause],
    table_name: Option<&str>,
    table_alias: Option<&str>,
) {
    if order_by.is_empty() || records.len() < 2 {
        return;
    }

    // Extract sort keys once per record — O(n × k) where k = ORDER BY clauses.
    // Text columns also get a u64 abbreviated key (first 8 bytes big-endian) so the
    // comparator short-circuits without touching the heap string in the common case.
    let mut keyed: Vec<(usize, Vec<SortKey>)> = records
        .iter()
        .enumerate()
        .map(|(i, rec)| {
            let keys: Vec<SortKey> = order_by
                .iter()
                .map(|clause| {
                    let v = if let Some(ref expr) = clause.expr {
                        super::expr_eval::evaluate_runtime_expr_with_db(
                            db,
                            expr,
                            rec,
                            table_name,
                            table_alias,
                        )
                    } else {
                        resolve_runtime_field(rec, &clause.field, table_name, table_alias)
                    };
                    SortKey::new(v)
                })
                .collect();
            (i, keys)
        })
        .collect();

    // Sort by extracted keys — O(n log n).
    // Text: compare abbreviated u64 key first; only fall through to full str::cmp on tie.
    // Non-text: delegate to the existing value comparator as before.
    keyed.sort_by(|(_, lkeys), (_, rkeys)| {
        for (clause, (lk, rk)) in order_by.iter().zip(lkeys.iter().zip(rkeys.iter())) {
            let ord = match (&lk.abbrev, &rk.abbrev, &lk.value, &rk.value) {
                // Both have abbreviated keys: fast u64 compare first
                (Some(la), Some(ra), Some(Value::Text(ls)), Some(Value::Text(rs))) => {
                    match la.cmp(ra) {
                        Ordering::Equal => ls.as_str().cmp(rs.as_str()),
                        other => other,
                    }
                }
                // Fallback: full value compare (handles Null, non-text, mixed)
                _ => compare_runtime_optional_values(
                    lk.value.as_ref(),
                    rk.value.as_ref(),
                    clause.nulls_first,
                ),
            };
            if ord != Ordering::Equal {
                return if clause.ascending { ord } else { ord.reverse() };
            }
        }
        Ordering::Equal
    });

    // Reorder records in-place using the sorted index permutation
    let orig: Vec<_> = std::mem::take(records);
    *records = keyed.into_iter().map(|(i, _)| orig[i].clone()).collect();
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
                if let Some(value) = record.values.get(format!("{table}.{column}").as_str()) {
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
                .get(column.as_str())
                .cloned()
                .or_else(|| resolve_runtime_document_path(record, column))
        }
        FieldRef::NodeProperty { alias, property } => {
            if let Some(value) = record.values.get(format!("{alias}.{property}").as_str()) {
                return Some(value.clone());
            }

            let node = record.nodes.get(alias)?;
            match property.as_str() {
                "id" => Some(Value::NodeRef(node.id.clone())),
                "label" => Some(Value::Text(node.label.clone())),
                "type" | "node_type" => Some(Value::Text(format!("{:?}", node.node_type))),
                _ => node.properties.get(property).cloned(),
            }
        }
        FieldRef::EdgeProperty { alias, property } => {
            if let Some(value) = record.values.get(format!("{alias}.{property}").as_str()) {
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
            .or_else(|| record.values.get(format!("{alias}.id").as_str()).cloned()),
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
    // Phase 2 dotted tenancy: relax the document-capability gate so
    // dotted paths resolve against JSON stored in `TEXT` columns too.
    // The inner resolver returns None on non-JSON scalars, so the
    // behaviour for non-document rows is unchanged — there's just no
    // early exit based on the capability flag.
    let segments = parse_runtime_document_path(path);
    let (root, tail) = segments.split_first()?;
    let root_value = record.values.get(root.as_str())?;
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
    if let Some(ordering) = runtime_exact_integer_cmp(left, right) {
        return ordering == Ordering::Equal;
    }

    if let (Some(left), Some(right)) = (runtime_value_number(left), runtime_value_number(right)) {
        return left == right;
    }

    // Equality: prefer borrow path to avoid two String clones.
    if let (Some(ls), Some(rs)) = (runtime_value_text_str(left), runtime_value_text_str(right)) {
        return ls == rs;
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
    if let Some(ordering) = runtime_exact_integer_cmp(left, right) {
        return Some(ordering);
    }

    if let (Some(left), Some(right)) = (runtime_value_number(left), runtime_value_number(right)) {
        return left.partial_cmp(&right);
    }

    // Fast text path: borrow the string slice when possible (avoids two
    // String clones), then compare abbreviated 8-byte keys first — full
    // str::cmp only if the first 8 bytes are equal.
    if let (Some(ls), Some(rs)) = (runtime_value_text_str(left), runtime_value_text_str(right)) {
        let l_abbrev = text_abbrev_key(ls);
        let r_abbrev = text_abbrev_key(rs);
        return Some(match l_abbrev.cmp(&r_abbrev) {
            Ordering::Equal => ls.cmp(rs),
            other => other,
        });
    }
    // Slower path for non-String text variants (RowRef, VectorRef, formatted values).
    if let (Some(left), Some(right)) = (runtime_value_text(left), runtime_value_text(right)) {
        return Some(left.as_str().cmp(right.as_str()));
    }

    match (left, right) {
        (Value::Boolean(left), Value::Boolean(right)) => Some(left.cmp(right)),
        _ => None,
    }
}

fn runtime_exact_integer_cmp(left: &Value, right: &Value) -> Option<Ordering> {
    match (left, right) {
        (Value::Integer(left), Value::Integer(right)) => Some(left.cmp(right)),
        (Value::UnsignedInteger(left), Value::UnsignedInteger(right)) => Some(left.cmp(right)),
        (Value::Integer(left), Value::UnsignedInteger(right)) => Some(if *left < 0 {
            Ordering::Less
        } else {
            (*left as u64).cmp(right)
        }),
        (Value::UnsignedInteger(left), Value::Integer(right)) => Some(if *right < 0 {
            Ordering::Greater
        } else {
            left.cmp(&(*right as u64))
        }),
        _ => None,
    }
}

pub(super) fn runtime_value_number(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(value) => Some(*value as f64),
        Value::UnsignedInteger(value) => Some(*value as f64),
        Value::BigInt(value) => Some(*value as f64),
        Value::Float(value) => Some(*value),
        Value::Timestamp(value) => Some(*value as f64),
        Value::Duration(value) => Some(*value as f64),
        _ => None,
    }
}

fn value_as_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Integer(value) | Value::BigInt(value) => Some(*value),
        Value::UnsignedInteger(value) => i64::try_from(*value).ok(),
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
        Value::Decimal(v) => Some(Value::Decimal(*v).display_string()),
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
        Value::AssetCode(code) => Some(code.clone()),
        Value::Money { .. } => Some(value.display_string()),
        Value::ColorAlpha([r, g, b, a]) => Some(format!("#{:02X}{:02X}{:02X}{:02X}", r, g, b, a)),
        Value::BigInt(v) => Some(v.to_string()),
        Value::KeyRef(col, key) => Some(format!("{}:{}", col, key)),
        Value::DocRef(col, id) => Some(format!("{}#{}", col, id)),
        Value::TableRef(name) => Some(name.clone()),
        Value::PageRef(page_id) => Some(format!("page:{}", page_id)),
        Value::Secret(_) | Value::Password(_) => Some("***".to_string()),
    }
}

/// Borrow-only text view — only covers variants whose value is already
/// a `String` field (no allocations). Used by `runtime_partial_cmp` to
/// avoid cloning text values when comparing.
pub(super) fn runtime_value_text_str(value: &Value) -> Option<&str> {
    match value {
        Value::Text(s) => Some(s.as_str()),
        Value::NodeRef(s) | Value::EdgeRef(s) | Value::TableRef(s) => Some(s.as_str()),
        Value::Email(s) | Value::Url(s) => Some(s.as_str()),
        _ => None,
    }
}

/// Like `runtime_value_text` but returns `Cow::Borrowed` when the value
/// already holds a `String` (Text, Email, Url, ref types) — zero alloc for
/// the common case in Like/StartsWith/EndsWith/Contains hot-path filters.
pub(super) fn runtime_value_text_cow(value: &Value) -> Option<std::borrow::Cow<'_, str>> {
    if let Some(s) = runtime_value_text_str(value) {
        return Some(std::borrow::Cow::Borrowed(s));
    }
    runtime_value_text(value).map(std::borrow::Cow::Owned)
}

/// Abbreviated sort key for a text slice: first 8 bytes in big-endian as a
/// `u64`. Shorter strings are zero-padded. Comparing this key first avoids
/// a full `str::cmp` in the typical case where the first 8 bytes differ —
/// mirrors PostgreSQL varlena abbreviated key optimisation (varlena.c:98-130).
#[inline]
pub(super) fn text_abbrev_key(s: &str) -> u64 {
    let bytes = s.as_bytes();
    let len = bytes.len().min(8);
    let mut key = [0u8; 8];
    key[..len].copy_from_slice(&bytes[..len]);
    u64::from_be_bytes(key)
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
    if !column.eq_ignore_ascii_case("red_capabilities") {
        if column.eq_ignore_ascii_case("red_entity_type") {
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
    if !column.eq_ignore_ascii_case("red_capabilities") {
        if !column.eq_ignore_ascii_case("red_entity_type") {
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

/// O(m × n) iterative LIKE matching — mirrors the Wildcards/Leetcode-44 DP
/// approach but without heap allocation. Replaces the recursive version which
/// was exponential on patterns with many `%` wildcards.
///
/// `%` matches any sequence of zero or more characters.
/// `_` matches exactly one character.
/// All other bytes are literal.
pub(super) fn like_matches_bytes(value: &[u8], pattern: &[u8]) -> bool {
    let (mut vi, mut pi) = (0usize, 0usize);
    // `star_vi` / `star_pi`: position after the last `%` wildcard seen.
    let (mut star_vi, mut star_pi) = (usize::MAX, usize::MAX);

    while vi < value.len() {
        if pi < pattern.len() && (pattern[pi] == b'_' || pattern[pi] == value[vi]) {
            vi += 1;
            pi += 1;
        } else if pi < pattern.len() && pattern[pi] == b'%' {
            // Record position right after `%`; the `%` matches empty for now.
            star_vi = vi;
            star_pi = pi;
            pi += 1;
        } else if star_pi != usize::MAX {
            // Backtrack: the `%` consumes one more value character.
            star_vi += 1;
            vi = star_vi;
            pi = star_pi + 1;
        } else {
            return false;
        }
    }

    // Consume trailing `%` wildcards in pattern.
    while pi < pattern.len() && pattern[pi] == b'%' {
        pi += 1;
    }

    pi == pattern.len()
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
        QueryExpr::CreateIndex(_) => "create_index",
        QueryExpr::DropIndex(_) => "drop_index",
        QueryExpr::ProbabilisticCommand(_) => "probabilistic_command",
        QueryExpr::Ask(_) => "ask",
        QueryExpr::SetConfig { .. } => "set_config",
        QueryExpr::ShowConfig { .. } => "show_config",
        QueryExpr::SetTenant(_) => "set_tenant",
        QueryExpr::ShowTenant => "show_tenant",
        QueryExpr::CreateTimeSeries(_) => "create_timeseries",
        QueryExpr::DropTimeSeries(_) => "drop_timeseries",
        QueryExpr::CreateQueue(_) => "create_queue",
        QueryExpr::DropQueue(_) => "drop_queue",
        QueryExpr::QueueCommand(_) => "queue_command",
        QueryExpr::CreateTree(_) => "create_tree",
        QueryExpr::DropTree(_) => "drop_tree",
        QueryExpr::TreeCommand(_) => "tree_command",
        QueryExpr::ExplainAlter(_) => "explain_alter",
        QueryExpr::TransactionControl(_) => "transaction_control",
        QueryExpr::MaintenanceCommand(_) => "maintenance_command",
        QueryExpr::CreateSchema(_) => "create_schema",
        QueryExpr::DropSchema(_) => "drop_schema",
        QueryExpr::CreateSequence(_) => "create_sequence",
        QueryExpr::DropSequence(_) => "drop_sequence",
        QueryExpr::CopyFrom(_) => "copy_from",
        QueryExpr::CreateView(_) => "create_view",
        QueryExpr::DropView(_) => "drop_view",
        QueryExpr::RefreshMaterializedView(_) => "refresh_materialized_view",
        QueryExpr::CreatePolicy(_) => "create_policy",
        QueryExpr::DropPolicy(_) => "drop_policy",
        QueryExpr::CreateServer(_) => "create_server",
        QueryExpr::DropServer(_) => "drop_server",
        QueryExpr::CreateForeignTable(_) => "create_foreign_table",
        QueryExpr::DropForeignTable(_) => "drop_foreign_table",
    }
}

/// Evaluate a scalar function on a record's values.
fn evaluate_scalar_function(
    name: &str,
    args: &[Projection],
    source: &UnifiedRecord,
) -> Option<Value> {
    evaluate_scalar_function_with_db(None, name, args, source)
}

pub(super) fn evaluate_scalar_function_with_db(
    db: Option<&RedDB>,
    name: &str,
    args: &[Projection],
    source: &UnifiedRecord,
) -> Option<Value> {
    let func_name = name.split(':').next().unwrap_or(name);
    if func_name.eq_ignore_ascii_case("CONFIG") {
        return evaluate_projection_config_function(db, args, source);
    }
    if func_name.eq_ignore_ascii_case("KV") {
        return evaluate_projection_kv_function(db, args, source);
    }
    evaluate_scalar_function_legacy(name, args, source)
}

fn evaluate_scalar_function_legacy(
    name: &str,
    args: &[Projection],
    source: &UnifiedRecord,
) -> Option<Value> {
    // Strip alias suffix if present (e.g. "GEO_DISTANCE:dist_km" → "GEO_DISTANCE")
    let func_name = name.split(':').next().unwrap_or(name);

    match func_name {
        "ADD" | "SUB" | "MUL" | "DIV" | "MOD" => {
            let a = resolve_scalar_arg(args, 0, source)?;
            let b = resolve_scalar_arg(args, 1, source)?;
            Some(arith_binop(func_name, a, b))
        }
        "CONCAT" => {
            let mut out = String::new();
            for idx in 0..args.len() {
                let value = resolve_scalar_arg(args, idx, source)?;
                if matches!(value, Value::Null) {
                    continue;
                }
                out.push_str(&value.display_string());
            }
            Some(Value::Text(out))
        }
        "CASE" => {
            // CASE WHEN cond THEN val ... ELSE val END is encoded as
            //   Function("CASE", [Expression(cond1), val1,
            //                      Expression(cond2), val2,
            //                      ..., else_val?])
            // Even-length args => no ELSE; odd-length => last arg is ELSE.
            // Walk WHEN/THEN pairs left-to-right, short-circuit on first
            // matching predicate. Fall through to ELSE (or Null) when no
            // branch matches.
            let mut i = 0;
            while i + 1 < args.len() {
                if let Projection::Expression(filter, _) = &args[i] {
                    let matched = evaluate_runtime_filter(source, filter, None, None);
                    if matched {
                        return resolve_scalar_arg(args, i + 1, source).or(Some(Value::Null));
                    }
                    i += 2;
                } else {
                    break;
                }
            }
            if args.len() % 2 == 1 {
                return resolve_scalar_arg(args, args.len() - 1, source).or(Some(Value::Null));
            }
            Some(Value::Null)
        }
        "CAST" => {
            // CAST(expr AS type) is parsed into Function("CAST", [inner, Column("TYPE:<name>")]).
            // Resolve the source value, look up the target type by SQL name,
            // and reuse the existing string→Value coerce path. On any
            // failure (unknown type, coerce error) we emit Null so queries
            // keep running — CAST is advisory, not a hard assertion.
            let src = resolve_scalar_arg(args, 0, source)?;
            let Some(Projection::Column(col)) = args.get(1) else {
                return Some(Value::Null);
            };
            let Some(type_name) = col.strip_prefix("TYPE:") else {
                return Some(Value::Null);
            };
            let Some(target) = crate::storage::schema::types::DataType::from_sql_name(type_name)
            else {
                return Some(Value::Null);
            };
            Some(cast_value_to(&src, target))
        }
        "GEO_DISTANCE" | "HAVERSINE" => {
            let (lat1, lon1, lat2, lon2) = resolve_two_geo_points(args, source)?;
            Some(Value::Float(crate::geo::haversine_km(
                lat1, lon1, lat2, lon2,
            )))
        }
        "TIME_BUCKET" => {
            let bucket_ns = resolve_time_bucket_duration(args, 0)?;
            let timestamp_ns = resolve_time_bucket_timestamp(args, source)?;
            let bucket_start = if bucket_ns == 0 {
                timestamp_ns
            } else {
                (timestamp_ns / bucket_ns) * bucket_ns
            };
            Some(Value::UnsignedInteger(bucket_start))
        }
        "GEO_DISTANCE_VINCENTY" | "VINCENTY" => {
            let (lat1, lon1, lat2, lon2) = resolve_two_geo_points(args, source)?;
            Some(Value::Float(crate::geo::vincenty_km(
                lat1, lon1, lat2, lon2,
            )))
        }
        "GEO_BEARING" => {
            let (lat1, lon1, lat2, lon2) = resolve_two_geo_points(args, source)?;
            Some(Value::Float(crate::geo::bearing(lat1, lon1, lat2, lon2)))
        }
        "GEO_MIDPOINT" => {
            let (lat1, lon1, lat2, lon2) = resolve_two_geo_points(args, source)?;
            let (lat, lon) = crate::geo::midpoint(lat1, lon1, lat2, lon2);
            Some(Value::GeoPoint(
                crate::geo::deg_to_micro(lat),
                crate::geo::deg_to_micro(lon),
            ))
        }
        "UPPER" => {
            let val = resolve_scalar_arg(args, 0, source)?;
            match val {
                Value::Text(s) => Some(Value::Text(s.to_uppercase())),
                _ => Some(val),
            }
        }
        "LOWER" => {
            let val = resolve_scalar_arg(args, 0, source)?;
            match val {
                Value::Text(s) => Some(Value::Text(s.to_lowercase())),
                _ => Some(val),
            }
        }
        "LENGTH" | "CHAR_LENGTH" | "CHARACTER_LENGTH" => {
            let val = resolve_scalar_arg(args, 0, source)?;
            match val {
                Value::Text(s) => Some(Value::Integer(s.chars().count() as i64)),
                Value::Blob(b) => Some(Value::Integer(b.len() as i64)),
                Value::Array(a) => Some(Value::Integer(a.len() as i64)),
                _ => Some(Value::Null),
            }
        }
        "OCTET_LENGTH" => {
            let val = resolve_scalar_arg(args, 0, source)?;
            match val {
                Value::Text(s) => Some(Value::Integer(s.len() as i64)),
                Value::Blob(b) => Some(Value::Integer(b.len() as i64)),
                _ => Some(Value::Null),
            }
        }
        "BIT_LENGTH" => {
            let val = resolve_scalar_arg(args, 0, source)?;
            match val {
                Value::Text(s) => Some(Value::Integer((s.len() * 8) as i64)),
                Value::Blob(b) => Some(Value::Integer((b.len() * 8) as i64)),
                _ => Some(Value::Null),
            }
        }
        "SUBSTRING" | "SUBSTR" => {
            let text = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            match resolve_scalar_arg(args, 1, source)? {
                Value::Text(pattern) if func_name == "SUBSTRING" && args.len() == 2 => {
                    Some(match substring_pattern_text(&text, &pattern) {
                        Some(matched) => Value::Text(matched),
                        None => Value::Null,
                    })
                }
                start_value => {
                    let start = value_as_i64(&start_value)?;
                    let count = args.get(2).and_then(|_| {
                        resolve_scalar_arg(args, 2, source).and_then(|value| value_as_i64(&value))
                    });
                    Some(Value::Text(substring_text(&text, start, count)?))
                }
            }
        }
        "POSITION" => {
            let needle = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let haystack = match resolve_scalar_arg(args, 1, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            Some(Value::Integer(position_text(&needle, &haystack)))
        }
        "TRIM" | "BTRIM" => {
            let text = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let chars = match args
                .get(1)
                .and_then(|_| resolve_scalar_arg(args, 1, source))
            {
                None => None,
                Some(Value::Text(chars)) => Some(chars),
                Some(_) => return Some(Value::Null),
            };
            Some(Value::Text(trim_text(&text, chars.as_deref(), true, true)))
        }
        "LTRIM" => {
            let text = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let chars = match args
                .get(1)
                .and_then(|_| resolve_scalar_arg(args, 1, source))
            {
                None => None,
                Some(Value::Text(chars)) => Some(chars),
                Some(_) => return Some(Value::Null),
            };
            Some(Value::Text(trim_text(&text, chars.as_deref(), true, false)))
        }
        "RTRIM" => {
            let text = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let chars = match args
                .get(1)
                .and_then(|_| resolve_scalar_arg(args, 1, source))
            {
                None => None,
                Some(Value::Text(chars)) => Some(chars),
                Some(_) => return Some(Value::Null),
            };
            Some(Value::Text(trim_text(&text, chars.as_deref(), false, true)))
        }
        "CONCAT_WS" => {
            let separator = match resolve_scalar_arg(args, 0, source)? {
                Value::Null => return Some(Value::Null),
                Value::Text(text) => text,
                other => other.display_string(),
            };
            let mut parts = Vec::new();
            for idx in 1..args.len() {
                let value = resolve_scalar_arg(args, idx, source)?;
                if matches!(value, Value::Null) {
                    continue;
                }
                parts.push(value.display_string());
            }
            Some(Value::Text(parts.join(&separator)))
        }
        "REVERSE" => {
            let text = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            Some(Value::Text(text.chars().rev().collect()))
        }
        "LEFT" => {
            let text = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let count =
                resolve_scalar_arg(args, 1, source).and_then(|value| value_as_i64(&value))?;
            Some(Value::Text(slice_left_text(&text, count)))
        }
        "RIGHT" => {
            let text = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let count =
                resolve_scalar_arg(args, 1, source).and_then(|value| value_as_i64(&value))?;
            Some(Value::Text(slice_right_text(&text, count)))
        }
        "QUOTE_LITERAL" => match resolve_scalar_arg(args, 0, source)? {
            Value::Null => Some(Value::Null),
            Value::Text(text) => Some(Value::Text(quote_literal_text(&text))),
            other => Some(Value::Text(quote_literal_text(&other.display_string()))),
        },
        "ABS" => {
            let val = resolve_scalar_arg(args, 0, source)?;
            match val {
                Value::Float(f) => Some(Value::Float(f.abs())),
                Value::Integer(n) => Some(Value::Integer(n.abs())),
                _ => Some(Value::Null),
            }
        }
        "ROUND" => {
            let val = resolve_scalar_arg(args, 0, source)?;
            match val {
                Value::Float(f) => Some(Value::Float(f.round())),
                other => Some(other),
            }
        }
        "COALESCE" => {
            for (i, _) in args.iter().enumerate() {
                if let Some(val) = resolve_scalar_arg(args, i, source) {
                    if val != Value::Null {
                        return Some(val);
                    }
                }
            }
            Some(Value::Null)
        }
        "VERIFY_PASSWORD" => {
            // VERIFY_PASSWORD(column, 'candidate') — compares a
            // plaintext candidate against the argon2id hash stored in
            // a Value::Password column. Returns a boolean.
            let stored = resolve_scalar_arg(args, 0, source)?;
            let candidate = resolve_scalar_arg(args, 1, source)?;
            let hash = match stored {
                Value::Password(h) => h,
                Value::Text(h) => h,
                _ => return Some(Value::Boolean(false)),
            };
            let plain = match candidate {
                Value::Text(s) => s,
                _ => return Some(Value::Boolean(false)),
            };
            Some(Value::Boolean(crate::auth::store::verify_password(
                &plain, &hash,
            )))
        }
        "MONEY" => money_from_scalar_args(args, source),
        "MONEY_ASSET" => match resolve_scalar_arg(args, 0, source)? {
            Value::Money { asset_code, .. } => Some(Value::AssetCode(asset_code)),
            _ => Some(Value::Null),
        },
        "MONEY_MINOR" => match resolve_scalar_arg(args, 0, source)? {
            Value::Money { minor_units, .. } => Some(Value::BigInt(minor_units)),
            _ => Some(Value::Null),
        },
        "MONEY_SCALE" => match resolve_scalar_arg(args, 0, source)? {
            Value::Money { scale, .. } => Some(Value::Integer(i64::from(scale))),
            _ => Some(Value::Null),
        },
        // Session-context scalars — match the `expr_eval` filter-side
        // dispatcher so `SELECT CURRENT_TENANT(), CURRENT_USER, …`
        // (no FROM, scalar projection path) returns the same values
        // RLS policies see in their predicates. Honours `WITHIN …`,
        // `SET LOCAL TENANT`, and `SET TENANT` overrides via the
        // shared accessors.
        "CURRENT_TENANT" => Some(
            crate::runtime::impl_core::current_tenant()
                .map(Value::Text)
                .unwrap_or(Value::Null),
        ),
        "CURRENT_USER" | "SESSION_USER" | "USER" => Some(
            crate::runtime::impl_core::current_user_projected()
                .map(Value::Text)
                .unwrap_or(Value::Null),
        ),
        "CURRENT_ROLE" => Some(
            crate::runtime::impl_core::current_role_projected()
                .map(Value::Text)
                .unwrap_or(Value::Null),
        ),
        "PG_ADVISORY_LOCK" => {
            let key = value_as_i64(&resolve_scalar_arg(args, 0, source)?)?;
            crate::auth::locks::global()
                .acquire(key, crate::runtime::impl_core::current_connection_id());
            Some(Value::Null)
        }
        "PG_TRY_ADVISORY_LOCK" => {
            let key = value_as_i64(&resolve_scalar_arg(args, 0, source)?)?;
            Some(Value::Boolean(crate::auth::locks::global().try_acquire(
                key,
                crate::runtime::impl_core::current_connection_id(),
            )))
        }
        "PG_ADVISORY_UNLOCK" => {
            let key = value_as_i64(&resolve_scalar_arg(args, 0, source)?)?;
            Some(Value::Boolean(crate::auth::locks::global().release(
                key,
                crate::runtime::impl_core::current_connection_id(),
            )))
        }
        "PG_ADVISORY_UNLOCK_ALL" => {
            let dropped = crate::auth::locks::global()
                .release_all(crate::runtime::impl_core::current_connection_id());
            Some(Value::Integer(dropped as i64))
        }
        "NOW" | "CURRENT_TIMESTAMP" => {
            let ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            Some(Value::TimestampMs(ms))
        }
        "CURRENT_DATE" => {
            let ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            Some(Value::Date((ms / 86_400_000) as i32))
        }
        _ => Some(Value::Null),
    }
}

fn money_from_scalar_args(args: &[Projection], source: &UnifiedRecord) -> Option<Value> {
    let input = match args {
        [single] => money_arg_text(resolve_scalar_arg(std::slice::from_ref(single), 0, source)?)?,
        [left, right] => {
            let lhs = money_arg_text(resolve_scalar_arg(args, 0, source)?)?;
            let rhs = money_arg_text(resolve_scalar_arg(args, 1, source)?)?;
            format!("{} {}", lhs, rhs)
        }
        _ => return Some(Value::Null),
    };
    match crate::storage::schema::coerce::coerce(
        &input,
        crate::storage::schema::DataType::Money,
        None,
    ) {
        Ok(value) => Some(value),
        Err(_) if args.len() == 2 => {
            let lhs = money_arg_text(resolve_scalar_arg(args, 1, source)?)?;
            let rhs = money_arg_text(resolve_scalar_arg(args, 0, source)?)?;
            crate::storage::schema::coerce::coerce(
                &format!("{} {}", lhs, rhs),
                crate::storage::schema::DataType::Money,
                None,
            )
            .ok()
        }
        Err(_) => Some(Value::Null),
    }
}

fn money_arg_text(value: Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Text(text) => Some(text),
        Value::AssetCode(code) => Some(code),
        Value::Currency(code) => Some(String::from_utf8_lossy(&code).to_string()),
        other => Some(other.display_string()),
    }
}

/// Resolve a single scalar argument from a function's arg list.
/// Evaluate an arithmetic binary operator on two values. Promotes
/// heterogeneous numeric operands to Float when either side is Float;
/// preserves Integer when both sides are Integer. Non-numeric operands
/// and zero divisors collapse to `Value::Null` so queries keep running
/// — SQL-style "erroring on bad arithmetic" is the job of the type
/// system v2 (Fase 3), not Fase 1.3.
fn arith_binop(op: &str, a: Value, b: Value) -> Value {
    let (lhs, rhs) = match (value_as_number(&a), value_as_number(&b)) {
        (Some(l), Some(r)) => (l, r),
        _ => return Value::Null,
    };
    // Integer fast path when both operands are integers and the op
    // doesn't force a float (division always floats for predictability
    // — avoids surprising truncation).
    let force_float = matches!(op, "DIV") || lhs.is_float || rhs.is_float;
    let out = match op {
        "ADD" => lhs.as_f64() + rhs.as_f64(),
        "SUB" => lhs.as_f64() - rhs.as_f64(),
        "MUL" => lhs.as_f64() * rhs.as_f64(),
        "DIV" => {
            if rhs.as_f64() == 0.0 {
                return Value::Null;
            }
            lhs.as_f64() / rhs.as_f64()
        }
        "MOD" => {
            if rhs.as_f64() == 0.0 {
                return Value::Null;
            }
            lhs.as_f64() % rhs.as_f64()
        }
        _ => return Value::Null,
    };
    if force_float {
        Value::Float(out)
    } else {
        Value::Integer(out as i64)
    }
}

#[derive(Debug, Clone, Copy)]
struct NumOperand {
    int_val: i64,
    float_val: f64,
    is_float: bool,
}

impl NumOperand {
    fn as_f64(self) -> f64 {
        if self.is_float {
            self.float_val
        } else {
            self.int_val as f64
        }
    }
}

fn value_as_number(v: &Value) -> Option<NumOperand> {
    match v {
        Value::Integer(n) | Value::BigInt(n) => Some(NumOperand {
            int_val: *n,
            float_val: *n as f64,
            is_float: false,
        }),
        Value::UnsignedInteger(n) => Some(NumOperand {
            int_val: *n as i64,
            float_val: *n as f64,
            is_float: false,
        }),
        Value::Float(f) => Some(NumOperand {
            int_val: *f as i64,
            float_val: *f,
            is_float: true,
        }),
        Value::Decimal(d) => Some(NumOperand {
            int_val: (*d / 10_000) as i64,
            float_val: *d as f64 / 10_000.0,
            is_float: true,
        }),
        Value::Text(s) => {
            if let Ok(n) = s.parse::<i64>() {
                Some(NumOperand {
                    int_val: n,
                    float_val: n as f64,
                    is_float: false,
                })
            } else if let Ok(f) = s.parse::<f64>() {
                Some(NumOperand {
                    int_val: f as i64,
                    float_val: f,
                    is_float: true,
                })
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Convert a `Value` to a new `Value` of the requested `DataType`. Used
/// by the CAST scalar function. Covers the common numeric/text/boolean
/// paths directly (so `CAST(123 AS TEXT)` doesn't round-trip through the
/// schema coercion layer) and falls back to `schema::coerce::coerce`
/// on the value's `display_string()` for everything else — that reuses
/// the battle-tested input validators we already have for INSERT.
fn cast_value_to(src: &Value, target: crate::storage::schema::types::DataType) -> Value {
    use crate::storage::schema::types::DataType as DT;
    match (src, target) {
        (v, DT::Text) => Value::Text(v.display_string()),
        (Value::Integer(n), DT::Float) => Value::Float(*n as f64),
        (Value::Integer(n), DT::BigInt) => Value::BigInt(*n),
        (Value::Integer(n), DT::UnsignedInteger) if *n >= 0 => Value::UnsignedInteger(*n as u64),
        (Value::UnsignedInteger(n), DT::Integer) if *n <= i64::MAX as u64 => {
            Value::Integer(*n as i64)
        }
        (Value::UnsignedInteger(n), DT::Float) => Value::Float(*n as f64),
        (Value::Float(f), DT::Integer) => Value::Integer(*f as i64),
        (Value::Float(f), DT::UnsignedInteger) if *f >= 0.0 => Value::UnsignedInteger(*f as u64),
        (Value::Boolean(b), DT::Integer) => Value::Integer(if *b { 1 } else { 0 }),
        (Value::Integer(n), DT::Boolean) => Value::Boolean(*n != 0),
        (Value::Text(s), target) => match crate::storage::schema::coerce::coerce(s, target, None) {
            Ok(v) => v,
            Err(_) => Value::Null,
        },
        (v, target) => {
            match crate::storage::schema::coerce::coerce(&v.display_string(), target, None) {
                Ok(v) => v,
                Err(_) => Value::Null,
            }
        }
    }
}

fn resolve_scalar_arg(args: &[Projection], index: usize, source: &UnifiedRecord) -> Option<Value> {
    let arg = args.get(index)?;
    eval_projection_value(arg, source)
}

/// Evaluate a single Projection node into a Value for scalar-function
/// argument resolution. Handles:
/// - `Projection::Column` with the `LIT:<literal>` / raw column conventions,
/// - `Projection::Field` to walk FieldRef resolution,
/// - `Projection::Function` recursively (enables nested arithmetic / casts),
/// - `Projection::Expression` as a boolean coming from a Filter.
pub(super) fn eval_projection_value(proj: &Projection, source: &UnifiedRecord) -> Option<Value> {
    match proj {
        Projection::Column(col) => {
            if let Some(lit_val) = col.strip_prefix("LIT:") {
                if lit_val.is_empty() {
                    return Some(Value::Null);
                }
                if let Ok(n) = lit_val.parse::<i64>() {
                    return Some(Value::Integer(n));
                }
                if let Ok(n) = lit_val.parse::<f64>() {
                    return Some(Value::Float(n));
                }
                return Some(Value::Text(lit_val.to_string()));
            }
            source.values.get(col.as_str()).cloned()
        }
        Projection::Alias(col, _) => source.values.get(col.as_str()).cloned(),
        Projection::Field(field, _) => resolve_runtime_field(source, field, None, None),
        Projection::Function(name, inner_args) => {
            evaluate_scalar_function(name, inner_args, source)
        }
        Projection::Expression(filter, _) => Some(Value::Boolean(evaluate_runtime_filter(
            source, filter, None, None,
        ))),
        Projection::All => None,
    }
}

pub(super) fn eval_projection_value_with_db(
    db: Option<&RedDB>,
    proj: &Projection,
    source: &UnifiedRecord,
) -> Option<Value> {
    match proj {
        Projection::Function(name, inner_args) => {
            evaluate_scalar_function_with_db(db, name, inner_args, source)
        }
        Projection::Expression(filter, _) => Some(Value::Boolean(evaluate_runtime_filter_with_db(
            db, source, filter, None, None,
        ))),
        _ => eval_projection_value(proj, source),
    }
}

fn evaluate_projection_config_function(
    db: Option<&RedDB>,
    args: &[Projection],
    source: &UnifiedRecord,
) -> Option<Value> {
    let key = projection_path_text(args.first()?)?;
    if let Some(db) = db {
        if let Some(value) = super::expr_eval::lookup_latest_kv_value(db, "red_config", &key) {
            return Some(value);
        }
    }
    args.get(1)
        .and_then(|arg| projection_default_value_with_db(db, arg, source))
        .or(Some(Value::Null))
}

fn evaluate_projection_kv_function(
    db: Option<&RedDB>,
    args: &[Projection],
    source: &UnifiedRecord,
) -> Option<Value> {
    let collection = projection_path_text(args.first()?)?;
    let key = projection_path_text(args.get(1)?)?;
    if let Some(db) = db {
        if let Some(value) = super::expr_eval::lookup_latest_kv_value(db, &collection, &key) {
            return Some(value);
        }
    }
    args.get(2)
        .and_then(|arg| projection_default_value_with_db(db, arg, source))
        .or(Some(Value::Null))
}

fn projection_path_text(projection: &Projection) -> Option<String> {
    match projection {
        Projection::Field(field, _) => Some(field_ref_name(field)),
        Projection::Column(column) => column.strip_prefix("LIT:").map(|text| text.to_string()),
        Projection::Alias(column, _) => Some(column.clone()),
        _ => None,
    }
}

fn projection_default_value_with_db(
    db: Option<&RedDB>,
    projection: &Projection,
    source: &UnifiedRecord,
) -> Option<Value> {
    match projection {
        Projection::Field(field, _) => Some(Value::Text(field_ref_name(field))),
        _ => eval_projection_value_with_db(db, projection, source),
    }
}

fn resolve_time_bucket_duration(args: &[Projection], index: usize) -> Option<u64> {
    let Projection::Column(column) = args.get(index)? else {
        return None;
    };
    let literal = column.strip_prefix("LIT:")?;
    crate::storage::timeseries::retention::parse_duration_ns(literal)
}

fn resolve_time_bucket_timestamp(args: &[Projection], source: &UnifiedRecord) -> Option<u64> {
    if let Some(value) = args
        .get(1)
        .and_then(|_| resolve_scalar_arg(args, 1, source))
    {
        return value_to_bucket_timestamp_ns(&value);
    }

    source
        .get("timestamp_ns")
        .and_then(value_to_bucket_timestamp_ns)
        .or_else(|| {
            source
                .get("timestamp_ms")
                .and_then(value_to_bucket_timestamp_ns)
        })
        .or_else(|| {
            source
                .get("timestamp")
                .and_then(value_to_bucket_timestamp_ns)
        })
}

fn value_to_bucket_timestamp_ns(value: &Value) -> Option<u64> {
    match value {
        Value::UnsignedInteger(v) => Some(*v),
        Value::Integer(v) if *v >= 0 => Some(*v as u64),
        Value::BigInt(v) if *v >= 0 => Some(*v as u64),
        Value::Float(v) if *v >= 0.0 => Some(*v as u64),
        Value::Timestamp(v) if *v >= 0 => Some((*v as u64) * 1_000_000_000),
        Value::TimestampMs(v) if *v >= 0 => Some((*v as u64) * 1_000_000),
        _ => None,
    }
}

fn substring_text(text: &str, start: i64, count: Option<i64>) -> Option<String> {
    if count.is_some_and(|count| count < 0) {
        return None;
    }

    let chars: Vec<char> = text.chars().collect();
    let start_idx = if start <= 1 {
        0
    } else {
        usize::try_from(start - 1).ok()?
    };

    if start_idx >= chars.len() {
        return Some(String::new());
    }

    let end_idx = match count {
        Some(count) => start_idx.saturating_add(count as usize).min(chars.len()),
        None => chars.len(),
    };

    Some(chars[start_idx..end_idx].iter().collect())
}

fn substring_pattern_text(text: &str, pattern: &str) -> Option<String> {
    let regex = regex::Regex::new(pattern).ok()?;
    let captures = regex.captures(text)?;
    if captures.len() > 1 {
        return captures.get(1).map(|capture| capture.as_str().to_string());
    }
    captures.get(0).map(|capture| capture.as_str().to_string())
}

fn position_text(needle: &str, haystack: &str) -> i64 {
    if needle.is_empty() {
        return 1;
    }
    haystack
        .find(needle)
        .map(|byte_idx| haystack[..byte_idx].chars().count() as i64 + 1)
        .unwrap_or(0)
}

fn slice_left_text(text: &str, count: i64) -> String {
    let chars: Vec<char> = text.chars().collect();
    let take = normalized_slice_len(chars.len(), count);
    chars.into_iter().take(take).collect()
}

fn slice_right_text(text: &str, count: i64) -> String {
    let chars: Vec<char> = text.chars().collect();
    let take = normalized_slice_len(chars.len(), count);
    let len = chars.len();
    chars.into_iter().skip(len.saturating_sub(take)).collect()
}

fn normalized_slice_len(len: usize, count: i64) -> usize {
    if count >= 0 {
        usize::try_from(count).unwrap_or(usize::MAX).min(len)
    } else {
        len.saturating_sub(count.unsigned_abs() as usize)
    }
}

fn quote_literal_text(text: &str) -> String {
    let escaped = text.replace('\'', "''");
    if text.contains('\\') {
        format!("E'{}'", escaped.replace('\\', "\\\\"))
    } else {
        format!("'{escaped}'")
    }
}

fn trim_text(text: &str, chars: Option<&str>, left: bool, right: bool) -> String {
    match chars {
        Some(chars) => {
            let predicate = |ch| chars.contains(ch);
            match (left, right) {
                (true, true) => text.trim_matches(predicate).to_string(),
                (true, false) => text.trim_start_matches(predicate).to_string(),
                (false, true) => text.trim_end_matches(predicate).to_string(),
                (false, false) => text.to_string(),
            }
        }
        None => match (left, right) {
            (true, true) => text.trim().to_string(),
            (true, false) => text.trim_start().to_string(),
            (false, true) => text.trim_end().to_string(),
            (false, false) => text.to_string(),
        },
    }
}

/// Resolve two geographic points from function arguments.
/// Supports: (column, POINT(lat, lon)) or (col1, col2)
fn resolve_two_geo_points(
    args: &[Projection],
    source: &UnifiedRecord,
) -> Option<(f64, f64, f64, f64)> {
    if args.len() < 2 {
        return None;
    }

    let (lat1, lon1) = resolve_geo_arg(&args[0], source)?;
    let (lat2, lon2) = resolve_geo_arg(&args[1], source)?;
    Some((lat1, lon1, lat2, lon2))
}

/// Resolve a single geo argument — either a column (GeoPoint/Latitude/Longitude) or POINT literal.
fn resolve_geo_arg(arg: &Projection, source: &UnifiedRecord) -> Option<(f64, f64)> {
    match arg {
        Projection::Column(col) => {
            // POINT:lat:lon literal
            if let Some(rest) = col.strip_prefix("POINT:") {
                let parts: Vec<&str> = rest.splitn(2, ':').collect();
                if parts.len() == 2 {
                    let lat: f64 = parts[0].parse().ok()?;
                    let lon: f64 = parts[1].parse().ok()?;
                    return Some((lat, lon));
                }
            }
            // Column reference → look up in record values
            let val = source.values.get(col.as_str())?;
            match val {
                Value::GeoPoint(lat_micro, lon_micro) => Some((
                    crate::geo::micro_to_deg(*lat_micro),
                    crate::geo::micro_to_deg(*lon_micro),
                )),
                Value::Float(f) => {
                    // Could be a lat or lon — check for "lat"/"lon" sibling columns
                    let lat_keys = ["lat", "latitude"];
                    let lon_keys = ["lon", "longitude", "lng"];
                    if lat_keys.contains(&col.as_str()) {
                        let lon = lon_keys
                            .iter()
                            .find_map(|k| source.values.get(*k))
                            .and_then(|v| match v {
                                Value::Float(f) => Some(*f),
                                Value::Integer(n) => Some(*n as f64),
                                _ => None,
                            })?;
                        Some((*f, lon))
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::query::unified::MatchedNode;

    #[test]
    fn test_evaluate_metadata_field_compare_entity_type_is_case_insensitive() {
        let field = FieldRef::TableColumn {
            table: "any".to_string(),
            column: "red_entity_type".to_string(),
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
            column: "red_entity_type".to_string(),
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
            column: "red_entity_type".to_string(),
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

    #[test]
    fn test_resolve_runtime_field_node_property_from_node_properties() {
        let mut record = UnifiedRecord::new();
        let mut node_properties = HashMap::new();
        node_properties.insert(
            "nginx_version".to_string(),
            Value::Text("1.22.1".to_string()),
        );
        let node = MatchedNode {
            id: "svc:nginx:80".to_string(),
            label: "nginx".to_string(),
            node_type: GraphNodeType::Service,
            properties: node_properties,
        };
        record.set_node("svc", node);

        let field = FieldRef::node_prop("svc", "nginx_version");
        assert_eq!(
            resolve_runtime_field(&record, &field, None, None),
            Some(Value::Text("1.22.1".to_string()))
        );
    }

    #[test]
    fn test_compare_runtime_values_preserves_integer_unsigned_boundaries() {
        let above_i64_max = Value::UnsignedInteger(i64::MAX as u64 + 1);
        let max_i64 = Value::Integer(i64::MAX);

        assert!(compare_runtime_values(
            &above_i64_max,
            &max_i64,
            CompareOp::Gt
        ));
        assert!(compare_runtime_values(
            &above_i64_max,
            &max_i64,
            CompareOp::Ge
        ));
        assert!(!compare_runtime_values(
            &above_i64_max,
            &max_i64,
            CompareOp::Eq
        ));

        assert!(compare_runtime_values(
            &Value::Integer(-1),
            &Value::UnsignedInteger(0),
            CompareOp::Lt
        ));
        assert!(compare_runtime_values(
            &Value::UnsignedInteger(0),
            &Value::Integer(-1),
            CompareOp::Gt
        ));
    }
}
