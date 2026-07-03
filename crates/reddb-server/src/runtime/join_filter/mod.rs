use super::*;

mod joins;
mod ordering;
pub(crate) use joins::*;
pub(crate) use ordering::*;

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
            if let Some(value) = source.get(key.as_str()) {
                let cloned = value.clone();
                record.set_arc(std::sync::Arc::from(key), cloned);
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
            Projection::Expression(filter, _) => {
                // Route through typed evaluator; fall back to filter-boolean path for
                // shapes the evaluator doesn't cover (CONFIG / KV / ML_* references).
                crate::storage::query::sql_lowering::projection_to_expr(projection)
                    .and_then(|(expr, _)| {
                        let row = RecordRow {
                            record: source,
                            table_name,
                            table_alias,
                        };
                        crate::storage::query::evaluator::evaluate(&expr, &row).ok()
                    })
                    .or_else(|| {
                        Some(Value::Boolean(evaluate_runtime_filter_with_db(
                            db,
                            source,
                            filter,
                            table_name,
                            table_alias,
                        )))
                    })
            }
            Projection::Function(ref name, ref args) => {
                // Route catalog-resolvable functions through the typed evaluator.
                // Falls back to the legacy dispatcher for CONFIG/KV/ML_*/geo/time
                // and any shape where argument resolution fails via evaluator.
                crate::storage::query::sql_lowering::projection_to_expr(projection)
                    .and_then(|(expr, _)| {
                        let row = RecordRow {
                            record: source,
                            table_name,
                            table_alias,
                        };
                        crate::storage::query::evaluator::evaluate(&expr, &row).ok()
                    })
                    .or_else(|| evaluate_scalar_function_with_db(db, name, args, source))
            }
            Projection::All => None,
            // Slice 7b (#590): the window phase has already
            // materialised this column onto `source` under the
            // projection's output label; just read it back.
            Projection::Window { name, alias, .. } => {
                let label: String = alias.clone().unwrap_or_else(|| name.clone());
                source.get(label.as_str()).cloned()
            }
        };

        record.set_arc(std::sync::Arc::from(label), value.unwrap_or(Value::Null));
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
    if column.starts_with("LIT:") {
        return eval_projection_value(&Projection::Column(column.to_string()), source);
    }
    source
        .get(column)
        .cloned()
        .or_else(|| {
            // Explicit `SELECT entity_id` / `red_collection` /
            // `red_kind` resolves to the rid-envelope field even on the
            // lean fast paths that don't pre-materialize the legacy alias
            // column. `SELECT *` never names these, so the envelope stays
            // clean.
            legacy_runtime_system_alias(column).and_then(|canonical| source.get(canonical).cloned())
        })
        .or_else(|| {
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
        // column_names() merges columnar side-channel + HashMap so
        // scan fast-path rows contribute their schema.
        let first_cols = first.column_names();
        let mut seen: std::collections::HashSet<String> =
            std::collections::HashSet::with_capacity(first_cols.len());
        let mut keys: Vec<String> = Vec::with_capacity(first_cols.len());
        for key in &first_cols {
            let k: &str = key;
            if !k.starts_with('_') && seen.insert(k.to_string()) {
                keys.push(k.to_string());
            }
        }

        let n = records.len();
        let step = (n / 8).max(1);
        let mut uniform = true;
        let mut idx = step;
        while idx < n {
            let rec = &records[idx];
            for key in rec.column_names() {
                let k: &str = &key;
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
        for key in record.column_names() {
            let k: &str = &key;
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
        .iter_fields()
        .filter_map(|(key, _)| {
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
        Projection::Function(name, args) => {
            if let Some((_, alias)) = name.split_once(':') {
                alias.to_string()
            } else {
                // #1370 — an unaliased function / operator projection is labeled
                // with its source-text form (`UPPER(name)`, `id * 2`,
                // `name || '!'`), reconstructed from the lowered projection.
                render_projection_label(name, args)
            }
        }
        Projection::Expression(_, alias) => alias.clone().unwrap_or_else(|| "expr".to_string()),
        Projection::Field(field, alias) => alias.clone().unwrap_or_else(|| field_ref_name(field)),
        Projection::Window { name, alias, .. } => alias.clone().unwrap_or_else(|| name.clone()),
    }
}

/// SQL infix symbol for an arithmetic / concat operator that
/// `sql_lowering::projection_binop_name` lowered to a function name.
fn projection_operator_symbol(name: &str) -> Option<&'static str> {
    match name {
        "ADD" => Some("+"),
        "SUB" => Some("-"),
        "MUL" => Some("*"),
        "DIV" => Some("/"),
        "MOD" => Some("%"),
        "CONCAT" => Some("||"),
        _ => None,
    }
}

/// Render one argument of an unaliased function/operator projection back to
/// its source-text label.
fn projection_arg_label(projection: &Projection) -> String {
    match projection {
        Projection::Column(column) => match column.strip_prefix("LIT:") {
            // Numbers / bool / null render bare; string literals are re-quoted
            // to match the source text the user wrote (#1370).
            Some(lit)
                if lit.is_empty()
                    || lit.parse::<f64>().is_ok()
                    || lit.eq_ignore_ascii_case("true")
                    || lit.eq_ignore_ascii_case("false")
                    || lit.eq_ignore_ascii_case("null") =>
            {
                lit.to_string()
            }
            Some(lit) => format!("'{lit}'"),
            None => column.clone(),
        },
        Projection::Function(name, args) => {
            let base = name.split_once(':').map(|(b, _)| b).unwrap_or(name);
            render_projection_label(base, args)
        }
        Projection::Field(field, _) => field_ref_name(field),
        Projection::Alias(_, alias) => alias.clone(),
        other => projection_name(other),
    }
}

/// Reconstruct the source-text label of an unaliased function / operator
/// projection: operators render infix (`id * 2`), other functions render as
/// calls (`UPPER(name)`, `COALESCE(name, 'fb')`).
fn render_projection_label(name: &str, args: &[Projection]) -> String {
    if let Some(symbol) = projection_operator_symbol(name) {
        args.iter()
            .map(projection_arg_label)
            .collect::<Vec<_>>()
            .join(&format!(" {symbol} "))
    } else {
        format!(
            "{}({})",
            name,
            args.iter()
                .map(projection_arg_label)
                .collect::<Vec<_>>()
                .join(", ")
        )
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

/// Row adapter for the typed scalar evaluator — bridges `UnifiedRecord`
/// to `crate::storage::query::evaluator::Row` so that `evaluator::evaluate`
/// can look up columns via the same `resolve_runtime_field` path used by
/// the rest of the runtime filter evaluation.
struct RecordRow<'a> {
    record: &'a UnifiedRecord,
    table_name: Option<&'a str>,
    table_alias: Option<&'a str>,
}

impl crate::storage::query::evaluator::Row for RecordRow<'_> {
    fn get(&self, field: &FieldRef) -> Option<Value> {
        resolve_runtime_field(self.record, field, self.table_name, self.table_alias)
    }
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
            // Route through the typed evaluator (catalog-resolved
            // operator / cast / function dispatch). Falls back to the
            // untyped expr_eval walker for CONFIG / KV / ML_* and any
            // other shape the evaluator does not cover yet.
            let row = RecordRow {
                record,
                table_name,
                table_alias,
            };
            let eval_side = |expr| {
                crate::storage::query::evaluator::evaluate(expr, &row)
                    .ok()
                    .or_else(|| {
                        super::expr_eval::evaluate_runtime_expr_with_db(
                            db,
                            expr,
                            record,
                            table_name,
                            table_alias,
                        )
                    })
            };
            match (eval_side(lhs), eval_side(rhs)) {
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
                .is_some_and(|value| runtime_value_contains(value, substring))
        }
    }
}

fn runtime_value_contains(value: &Value, needle: &str) -> bool {
    match value {
        Value::Array(values) => values
            .iter()
            .any(|value| runtime_value_contains(value, needle)),
        Value::Json(bytes) => {
            crate::serde_json::from_slice::<JsonValue>(bytes)
                .ok()
                .is_some_and(|json| json_value_contains(&json, needle))
                || String::from_utf8_lossy(bytes).contains(needle)
        }
        other => runtime_value_text(other).is_some_and(|value| value.contains(needle)),
    }
}

fn json_value_contains(value: &JsonValue, needle: &str) -> bool {
    match value {
        JsonValue::Array(values) => values
            .iter()
            .any(|value| json_value_contains(value, needle)),
        JsonValue::String(value) => value == needle,
        JsonValue::Number(value) => value.to_string() == needle,
        JsonValue::Bool(value) => value.to_string() == needle,
        JsonValue::Null | JsonValue::Object(_) => false,
    }
}

/// Map a legacy public-identity column name to its canonical rid-envelope
/// field. The rid-envelope refactor exposes identity under `rid` /
/// `collection` / `kind`, but WHERE/ORDER predicates written against the
/// older `entity_id` / `red_collection` / `red_kind` names must still
/// resolve. We only consult this alias when the literal column is absent
/// from the materialized record, so it never shadows a real user column and
/// never adds these names to `SELECT *` output (that stays envelope-clean).
pub(super) fn legacy_runtime_system_alias(column: &str) -> Option<&'static str> {
    match column {
        "entity_id" => Some("rid"),
        "red_collection" => Some("collection"),
        "red_kind" => Some("kind"),
        _ => None,
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

pub(crate) fn resolve_runtime_document_path_from_value(
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

pub(super) fn resolve_runtime_document_json_path(
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

pub(super) fn runtime_json_value_to_runtime_value(value: &JsonValue) -> Option<Value> {
    match value {
        JsonValue::Null => Some(Value::Null),
        JsonValue::Bool(value) => Some(Value::Boolean(*value)),
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

pub(crate) fn parse_runtime_document_path(path: &str) -> Vec<String> {
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

    match (left, right) {
        (Value::Timestamp(left), Value::Timestamp(right)) => return Some(left.cmp(right)),
        (Value::TimestampMs(left), Value::TimestampMs(right)) => return Some(left.cmp(right)),
        (Value::Date(left), Value::Date(right)) => return Some(left.cmp(right)),
        (Value::Time(left), Value::Time(right)) => return Some(left.cmp(right)),
        (Value::Duration(left), Value::Duration(right)) => return Some(left.cmp(right)),
        _ => {}
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

/// Coerce a value to `u64` — used by the H3 scalars whose cell ids are
/// full 64-bit unsigned (#1575).
fn value_as_u64(value: &Value) -> Option<u64> {
    match value {
        Value::UnsignedInteger(value) => Some(*value),
        Value::Integer(value) | Value::BigInt(value) => u64::try_from(*value).ok(),
        _ => None,
    }
}

pub(super) fn runtime_value_text(value: &Value) -> Option<String> {
    match value {
        Value::Text(value) => Some(value.to_string()),
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
        Value::Json(bytes) => String::from_utf8(bytes.clone()).ok(),
        Value::Blob(_) | Value::Vector(_) => None,
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
        Value::Text(s) => Some(s.as_ref()),
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

mod scalar_functions;
pub(crate) use scalar_functions::*;

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
                // Composite sentinel — Array/Vector/Blob roundtrip via
                // JSON-ish encoding (see `serialize_value_json` in
                // `sql_lowering`). Preserves shape across the
                // Projection-based legacy scalar dispatcher.
                if let Some(payload) = lit_val.strip_prefix("@RL:") {
                    if let Some(v) = parse_rl_literal(payload) {
                        return Some(v);
                    }
                }
                if let Ok(n) = lit_val.parse::<i64>() {
                    return Some(Value::Integer(n));
                }
                if let Ok(n) = lit_val.parse::<f64>() {
                    return Some(Value::Float(n));
                }
                return Some(Value::text(lit_val.to_string()));
            }
            source.get(col.as_str()).cloned()
        }
        Projection::Alias(col, _) => {
            eval_projection_value(&Projection::Column(col.clone()), source)
        }
        Projection::Field(field, _) => resolve_runtime_field(source, field, None, None),
        Projection::Function(name, inner_args) => {
            crate::storage::query::sql_lowering::projection_to_expr(proj)
                .and_then(|(expr, _)| {
                    let row = RecordRow {
                        record: source,
                        table_name: None,
                        table_alias: None,
                    };
                    crate::storage::query::evaluator::evaluate(&expr, &row).ok()
                })
                .or_else(|| evaluate_scalar_function(name, inner_args, source))
        }
        Projection::Expression(filter, _) => {
            crate::storage::query::sql_lowering::projection_to_expr(proj)
                .and_then(|(expr, _)| {
                    let row = RecordRow {
                        record: source,
                        table_name: None,
                        table_alias: None,
                    };
                    crate::storage::query::evaluator::evaluate(&expr, &row).ok()
                })
                .or_else(|| {
                    Some(Value::Boolean(evaluate_runtime_filter(
                        source, filter, None, None,
                    )))
                })
        }
        Projection::All => None,
        // Slice 7b (#590): window output is pre-materialised on the
        // record under the alias by `runtime::window_phase::apply`.
        Projection::Window { name, alias, .. } => {
            let label: String = alias.clone().unwrap_or_else(|| name.clone());
            source.get(label.as_str()).cloned()
        }
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

/// Handle ML_CLASSIFY / ML_PREDICT_PROBA / SEMANTIC_CACHE_* scalars.
///
/// Calling convention:
/// - `ML_CLASSIFY(model_name, features)` — `features` is either a
///   `Value::Vector(Vec<f32>)` or a `Value::Array(Vec<numeric>)`.
///   Returns the predicted class id as `Value::Integer`, or `Null`
///   when the model is unknown / features shape mismatch.
/// - `ML_PREDICT_PROBA(model_name, features)` — same shapes; returns
///   a `Value::Array(Vec<Float>)` of per-class probabilities.
/// - `SEMANTIC_CACHE_GET(namespace, embedding)` — returns the cached
///   response `Value::Text` if cosine similarity ≥ the cache's
///   configured threshold; `Value::Null` otherwise. `namespace` is
///   reserved for future per-tenant isolation; currently shared.
/// - `SEMANTIC_CACHE_PUT(namespace, prompt, response, embedding)` —
///   inserts. Returns `Value::Boolean(true)` on success.
fn evaluate_ml_scalar(
    db: &RedDB,
    name: &str,
    args: &[Projection],
    source: &UnifiedRecord,
) -> Option<Value> {
    match name {
        "ML_CLASSIFY" => ml_classify(db, args, source, /*probas=*/ false),
        "ML_PREDICT_PROBA" => ml_classify(db, args, source, /*probas=*/ true),
        "SEMANTIC_CACHE_GET" => semantic_cache_get(db, args, source),
        "SEMANTIC_CACHE_PUT" => semantic_cache_put(db, args, source),
        "EMBED" => {
            let text = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(s) => s.to_string(),
                other => other.display_string(),
            };
            let provider_hint = args.get(1).and_then(|_| {
                resolve_scalar_arg(args, 1, source).and_then(|v| match v {
                    Value::Text(s) => Some(s.to_string()),
                    _ => None,
                })
            });
            super::expr_eval::embed_text_public(db, &text, provider_hint.as_deref())
        }
        _ => None,
    }
}

fn resolve_feature_vector(
    args: &[Projection],
    idx: usize,
    source: &UnifiedRecord,
) -> Option<Vec<f32>> {
    let val = resolve_scalar_arg(args, idx, source)?;
    match val {
        Value::Vector(v) => Some(v),
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let n = value_as_number(&item)?;
                out.push(n.as_f64() as f32);
            }
            Some(out)
        }
        _ => None,
    }
}

fn ml_classify(
    db: &RedDB,
    args: &[Projection],
    source: &UnifiedRecord,
    probas: bool,
) -> Option<Value> {
    let model_name = match resolve_scalar_arg(args, 0, source)? {
        Value::Text(s) => s.to_string(),
        _ => return None,
    };
    let features = resolve_feature_vector(args, 1, source)?;

    let version = db.ml_runtime().registry().get_active(&model_name).ok()??;
    // Model kind is stamped into `hyperparams_json` as `{"kind":"logreg"|"nb", ...}`.
    // Fall back to `logreg` when unset (pre-existing models only ever
    // registered logreg).
    let kind = parse_model_kind(&version.hyperparams_json);
    let weights_json = std::str::from_utf8(&version.weights_blob).ok()?;

    use crate::storage::ml::classifier::IncrementalClassifier;
    let (class, probs) = match kind.as_str() {
        "nb" | "naive_bayes" => {
            let m = crate::storage::ml::classifier::MultinomialNaiveBayes::from_json(weights_json)?;
            (m.predict(&features), m.predict_proba(&features))
        }
        _ => {
            let m = crate::storage::ml::classifier::LogisticRegression::from_json(weights_json)?;
            (m.predict(&features), m.predict_proba(&features))
        }
    };

    if probas {
        Some(Value::Array(
            probs.into_iter().map(|p| Value::Float(p as f64)).collect(),
        ))
    } else {
        class.map(|c| Value::Integer(c as i64))
    }
}

fn parse_model_kind(hyperparams_json: &str) -> String {
    crate::serde_json::from_str::<crate::serde_json::Value>(hyperparams_json)
        .ok()
        .as_ref()
        .and_then(|v| v.get("kind"))
        .and_then(|k| k.as_str())
        .unwrap_or("logreg")
        .to_ascii_lowercase()
}

fn semantic_cache_get(db: &RedDB, args: &[Projection], source: &UnifiedRecord) -> Option<Value> {
    // args[0] = namespace (reserved, currently ignored)
    let _ns = resolve_scalar_arg(args, 0, source)?;
    let embedding = resolve_feature_vector(args, 1, source)?;
    match db.semantic_cache().lookup(&embedding) {
        Some(response) => Some(Value::text(response)),
        None => Some(Value::Null),
    }
}

fn semantic_cache_put(db: &RedDB, args: &[Projection], source: &UnifiedRecord) -> Option<Value> {
    let _ns = resolve_scalar_arg(args, 0, source)?;
    let prompt = match resolve_scalar_arg(args, 1, source)? {
        Value::Text(s) => s.to_string(),
        other => other.display_string(),
    };
    let response = match resolve_scalar_arg(args, 2, source)? {
        Value::Text(s) => s.to_string(),
        other => other.display_string(),
    };
    let embedding = resolve_feature_vector(args, 3, source)?;
    db.semantic_cache()
        .insert(prompt, response, embedding, None);
    Some(Value::Boolean(true))
}

fn evaluate_projection_config_function(
    db: Option<&RedDB>,
    args: &[Projection],
    source: &UnifiedRecord,
) -> Option<Value> {
    let key = projection_path_text(args.first()?)?;
    if let Some(value) = crate::runtime::impl_core::current_config_value(&key) {
        return Some(value);
    }
    if let Some(db) = db {
        // `$config.<path>` desugars to CONFIG("red.config/<path>") but SET CONFIG
        // stores under the bare key — try the stripped key too (#1370). This is
        // the WHERE-clause / projection legacy path (evaluate_scalar_function_with_db).
        let key_str: &str = key.as_ref();
        let bare = key_str.strip_prefix("red.config/").unwrap_or(key_str);
        if let Some(value) = super::expr_eval::lookup_latest_kv_value(db, "red_config", &key)
            .or_else(|| super::expr_eval::lookup_latest_kv_value(db, "red_config", bare))
        {
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

fn evaluate_projection_secret_ref(args: &[Projection]) -> Option<Value> {
    let key = projection_path_text(args.first()?)?.to_ascii_lowercase();
    if crate::runtime::impl_core::current_secret_value(&key).is_some() {
        Some(Value::text("***"))
    } else {
        Some(Value::Null)
    }
}

/// Resolve `$kv.*` in a projection. Unlike secrets, plain KV values are
/// not masked — the resolver already enforces `kv:read`, and denied/absent
/// keys fall through to NULL (#1602).
fn evaluate_projection_kv_ref(args: &[Projection]) -> Option<Value> {
    let key = projection_path_text(args.first()?)?.to_ascii_lowercase();
    crate::runtime::impl_core::current_kv_value(&key)
        .map(Value::text)
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
        Projection::Field(field, _) => Some(Value::text(field_ref_name(field))),
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
            let val = source.get(col.as_str())?;
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
                        let lon =
                            lon_keys
                                .iter()
                                .find_map(|k| source.get(k))
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
                &Value::text("table".to_string()),
                CompareOp::Eq,
                &Value::text("TABLE".to_string()),
            ),
            Some(true)
        );

        assert_eq!(
            evaluate_metadata_field_compare(
                &field,
                &Value::text("graph_node".to_string()),
                CompareOp::Ne,
                &Value::text("GRAPH_NODE".to_string()),
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
                &Value::text("vector".to_string()),
                &[
                    Value::text("TABLE".to_string()),
                    Value::text("vector".to_string()),
                    Value::text("graph_node".to_string()),
                ],
            ),
            Some(true)
        );

        assert_eq!(
            evaluate_metadata_field_in(
                &field,
                &Value::text("document".to_string()),
                &[
                    Value::text("TABLE".to_string()),
                    Value::text("GRAPH_NODE".to_string()),
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
                &Value::text("vector".to_string()),
                CompareOp::Gt,
                &Value::text("vector".to_string()),
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
            Value::text("1.22.1".to_string()),
        );
        let node = MatchedNode {
            id: "svc:nginx:80".to_string(),
            label: "nginx".to_string(),
            node_label: "service".to_string(),
            properties: node_properties,
        };
        record.set_node("svc", node);

        let field = FieldRef::node_prop("svc", "nginx_version");
        assert_eq!(
            resolve_runtime_field(&record, &field, None, None),
            Some(Value::text("1.22.1".to_string()))
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

    // ── Top-K parity tests ───────────────────────────────────────────
    // Each test asserts `top_k_records_by_order_by_with_db(k)` returns
    // a result byte-for-byte identical to `sort_records_by_order_by_with_db`
    // followed by `records.truncate(k)`. Any divergence here is a bug.

    fn rec_i(col: &str, v: i64) -> UnifiedRecord {
        let mut r = UnifiedRecord::with_capacity(1);
        r.set(col, Value::Integer(v));
        r
    }

    fn rec_f(col: &str, v: f64) -> UnifiedRecord {
        let mut r = UnifiedRecord::with_capacity(1);
        r.set(col, Value::Float(v));
        r
    }

    fn rec_t(col: &str, v: &str) -> UnifiedRecord {
        let mut r = UnifiedRecord::with_capacity(1);
        r.set(col, Value::text(v.to_string()));
        r
    }

    fn rec_pair(c1: &str, v1: Value, c2: &str, v2: Value) -> UnifiedRecord {
        let mut r = UnifiedRecord::with_capacity(2);
        r.set(c1, v1);
        r.set(c2, v2);
        r
    }

    fn order_by_col(col: &str, asc: bool, nulls_first: bool) -> OrderByClause {
        OrderByClause {
            field: FieldRef::TableColumn {
                table: String::new(),
                column: col.to_string(),
            },
            expr: None,
            ascending: asc,
            nulls_first,
        }
    }

    fn reference_sort_truncate(
        mut records: Vec<UnifiedRecord>,
        ob: &[OrderByClause],
        k: usize,
    ) -> Vec<UnifiedRecord> {
        sort_records_by_order_by_with_db(None, &mut records, ob, None, None);
        records.truncate(k);
        records
    }

    fn topk_via(records: Vec<UnifiedRecord>, ob: &[OrderByClause], k: usize) -> Vec<UnifiedRecord> {
        let mut v = records;
        top_k_records_by_order_by_with_db(None, &mut v, ob, k, None, None);
        v
    }

    fn extract_i(records: &[UnifiedRecord], col: &str) -> Vec<Option<i64>> {
        records
            .iter()
            .map(|r| match r.get(col) {
                Some(Value::Integer(n)) => Some(*n),
                _ => None,
            })
            .collect()
    }

    fn extract_f(records: &[UnifiedRecord], col: &str) -> Vec<Option<f64>> {
        records
            .iter()
            .map(|r| match r.get(col) {
                Some(Value::Float(n)) => Some(*n),
                _ => None,
            })
            .collect()
    }

    fn extract_t(records: &[UnifiedRecord], col: &str) -> Vec<Option<String>> {
        records
            .iter()
            .map(|r| match r.get(col) {
                Some(Value::Text(s)) => Some(s.as_ref().to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn topk_asc_smaller_k_matches_sort_truncate() {
        let rows: Vec<_> = [5i64, 3, 8, 1, 9, 4, 7, 2, 6, 0]
            .iter()
            .map(|n| rec_i("a", *n))
            .collect();
        let ob = vec![order_by_col("a", true, false)];
        for k in [1usize, 3, 5, 9] {
            let expected = reference_sort_truncate(rows.clone(), &ob, k);
            let actual = topk_via(rows.clone(), &ob, k);
            assert_eq!(extract_i(&actual, "a"), extract_i(&expected, "a"), "k={k}");
        }
    }

    #[test]
    fn topk_desc_matches_sort_truncate() {
        let rows: Vec<_> = [5i64, 3, 8, 1, 9, 4, 7, 2, 6, 0]
            .iter()
            .map(|n| rec_i("a", *n))
            .collect();
        let ob = vec![order_by_col("a", false, false)];
        for k in [1usize, 3, 5, 9] {
            let expected = reference_sort_truncate(rows.clone(), &ob, k);
            let actual = topk_via(rows.clone(), &ob, k);
            assert_eq!(extract_i(&actual, "a"), extract_i(&expected, "a"), "k={k}");
        }
    }

    #[test]
    fn topk_ties_preserve_stable_order() {
        // Multiple records with the same sort key but distinct secondary
        // column — stable sort keeps insertion order, top-k must match.
        let rows = vec![
            rec_pair("k", Value::Integer(1), "tag", Value::text("a")),
            rec_pair("k", Value::Integer(2), "tag", Value::text("b")),
            rec_pair("k", Value::Integer(1), "tag", Value::text("c")),
            rec_pair("k", Value::Integer(2), "tag", Value::text("d")),
            rec_pair("k", Value::Integer(1), "tag", Value::text("e")),
        ];
        let ob = vec![order_by_col("k", true, false)];
        for k in [1usize, 2, 3, 4] {
            let expected = reference_sort_truncate(rows.clone(), &ob, k);
            let actual = topk_via(rows.clone(), &ob, k);
            assert_eq!(
                extract_t(&actual, "tag"),
                extract_t(&expected, "tag"),
                "k={k}"
            );
        }
    }

    #[test]
    fn topk_multi_key_mixed_asc_desc() {
        let rows = vec![
            rec_pair("a", Value::Integer(1), "b", Value::Integer(10)),
            rec_pair("a", Value::Integer(2), "b", Value::Integer(5)),
            rec_pair("a", Value::Integer(1), "b", Value::Integer(30)),
            rec_pair("a", Value::Integer(2), "b", Value::Integer(25)),
            rec_pair("a", Value::Integer(1), "b", Value::Integer(20)),
        ];
        let ob = vec![
            order_by_col("a", true, false),
            order_by_col("b", false, false),
        ];
        for k in [1usize, 2, 3, 4] {
            let expected = reference_sort_truncate(rows.clone(), &ob, k);
            let actual = topk_via(rows.clone(), &ob, k);
            assert_eq!(
                extract_i(&actual, "a"),
                extract_i(&expected, "a"),
                "k={k} a"
            );
            assert_eq!(
                extract_i(&actual, "b"),
                extract_i(&expected, "b"),
                "k={k} b"
            );
        }
    }

    #[test]
    fn topk_nulls_first_and_last() {
        let rows = vec![
            rec_i("a", 3),
            {
                let mut r = UnifiedRecord::with_capacity(1);
                r.set("a", Value::Null);
                r
            },
            rec_i("a", 1),
            {
                let mut r = UnifiedRecord::with_capacity(1);
                r.set("a", Value::Null);
                r
            },
            rec_i("a", 2),
        ];
        for nulls_first in [false, true] {
            let ob = vec![order_by_col("a", true, nulls_first)];
            for k in [1usize, 2, 3, 4] {
                let expected = reference_sort_truncate(rows.clone(), &ob, k);
                let actual = topk_via(rows.clone(), &ob, k);
                let exp_i = extract_i(&expected, "a");
                let act_i = extract_i(&actual, "a");
                assert_eq!(act_i, exp_i, "nulls_first={nulls_first} k={k}");
            }
        }
    }

    #[test]
    fn topk_nan_float_count_and_subset() {
        // NaN breaks total ordering: both `sort_by` and our quickselect
        // produce implementation-defined orderings when NaN participates.
        // The real invariant is "doesn't panic" + "returns k elements
        // drawn from the input" — verify that much.
        let rows = vec![
            rec_f("a", 1.5),
            rec_f("a", f64::NAN),
            rec_f("a", 0.5),
            rec_f("a", f64::NAN),
            rec_f("a", 2.5),
        ];
        let ob = vec![order_by_col("a", true, false)];
        for k in [1usize, 2, 3, 4, 5] {
            let actual = topk_via(rows.clone(), &ob, k);
            assert_eq!(actual.len(), k.min(rows.len()), "k={k}");
            for rec in &actual {
                let v = extract_f(std::slice::from_ref(rec), "a").pop().flatten();
                assert!(
                    matches!(v, Some(f) if f.is_nan() || [0.5_f64, 1.5, 2.5].contains(&f)),
                    "k={k} value not from input: {v:?}"
                );
            }
        }
    }

    #[test]
    fn topk_k_zero_clears() {
        let rows: Vec<_> = (0..5).map(|n| rec_i("a", n)).collect();
        let ob = vec![order_by_col("a", true, false)];
        let got = topk_via(rows, &ob, 0);
        assert!(got.is_empty());
    }

    #[test]
    fn topk_k_ge_len_full_sorted() {
        let rows: Vec<_> = [5i64, 3, 8, 1, 9, 4]
            .iter()
            .map(|n| rec_i("a", *n))
            .collect();
        let ob = vec![order_by_col("a", true, false)];
        let expected = reference_sort_truncate(rows.clone(), &ob, 100);
        let actual = topk_via(rows, &ob, 100);
        assert_eq!(extract_i(&actual, "a"), extract_i(&expected, "a"));
    }

    #[test]
    fn topk_text_abbrev_path_matches_sort() {
        // Text sort triggers the abbreviated u64 prefix fast path in
        // both functions — ensures both traverse it identically.
        let rows: Vec<_> = [
            "delta", "alpha", "echo", "bravo", "charlie", "foxtrot", "golf",
        ]
        .iter()
        .map(|s| rec_t("a", s))
        .collect();
        let ob = vec![order_by_col("a", true, false)];
        for k in [1usize, 2, 3, 4, 5, 6] {
            let expected = reference_sort_truncate(rows.clone(), &ob, k);
            let actual = topk_via(rows.clone(), &ob, k);
            assert_eq!(extract_t(&actual, "a"), extract_t(&expected, "a"), "k={k}");
        }
    }

    #[test]
    fn topk_property_random_matches_sort() {
        // Pseudo-random but deterministic — same seed each run so a
        // failure reproduces. 200 rows × 4 k-values × 2 directions.
        let mut rows: Vec<UnifiedRecord> = Vec::with_capacity(200);
        let mut state: u64 = 0x9E3779B97F4A7C15;
        for _ in 0..200 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let v = (state % 50) as i64; // intentionally high collision rate
            rows.push(rec_i("a", v));
        }
        for asc in [true, false] {
            let ob = vec![order_by_col("a", asc, false)];
            for k in [1usize, 10, 50, 100, 199] {
                let expected = reference_sort_truncate(rows.clone(), &ob, k);
                let actual = topk_via(rows.clone(), &ob, k);
                assert_eq!(
                    extract_i(&actual, "a"),
                    extract_i(&expected, "a"),
                    "asc={asc} k={k}"
                );
            }
        }
    }

    // ── Runtime join semantics coverage (#1339) ──────────────────────────────
    // Tests exercise the lowest-level join algorithms directly so changes to
    // the join-key representation cannot silently break observable query results.
    // Both `execute_runtime_nested_loop_join` and `execute_runtime_hash_join`
    // are exercised; they must produce the same row count for all cases where
    // their semantics agree.

    fn jrec(col: &str, val: Value) -> UnifiedRecord {
        let mut r = UnifiedRecord::with_capacity(1);
        r.set(col, val);
        r
    }

    fn jfield(col: &str) -> FieldRef {
        FieldRef::TableColumn {
            table: String::new(),
            column: col.to_string(),
        }
    }

    fn left_tq() -> TableQuery {
        TableQuery::new("L")
    }

    fn both_join_count(
        left: &[UnifiedRecord],
        right: &[UnifiedRecord],
        lf: &FieldRef,
        rf: &FieldRef,
        join_type: JoinType,
        expected: usize,
        label: &str,
    ) {
        let tq = left_tq();
        let nl = execute_runtime_nested_loop_join(
            &tq, left, None, None, lf, right, None, None, rf, join_type,
        )
        .unwrap_or_else(|e| panic!("nested_loop {label}: {e}"));
        assert_eq!(nl.len(), expected, "nested_loop {label}");

        let hj =
            execute_runtime_hash_join(&tq, left, None, None, lf, right, None, None, rf, join_type)
                .unwrap_or_else(|e| panic!("hash_join {label}: {e}"));
        assert_eq!(hj.len(), expected, "hash_join {label}");
    }

    #[test]
    fn join_semantics_integer_inner_basic() {
        // left [1,2,3] ⋈ right [2,3,4] on integer key → 2 matched pairs
        let left: Vec<_> = [1i64, 2, 3]
            .iter()
            .map(|v| jrec("k", Value::Integer(*v)))
            .collect();
        let right: Vec<_> = [2i64, 3, 4]
            .iter()
            .map(|v| jrec("k", Value::Integer(*v)))
            .collect();
        both_join_count(
            &left,
            &right,
            &jfield("k"),
            &jfield("k"),
            JoinType::Inner,
            2,
            "integer_inner",
        );
    }

    #[test]
    fn join_semantics_float_inner_basic() {
        // float values matched by equality
        let left: Vec<_> = [1.0f64, 2.5, 3.0]
            .iter()
            .map(|v| jrec("f", Value::Float(*v)))
            .collect();
        let right: Vec<_> = [2.5f64, 3.0, 4.0]
            .iter()
            .map(|v| jrec("f", Value::Float(*v)))
            .collect();
        both_join_count(
            &left,
            &right,
            &jfield("f"),
            &jfield("f"),
            JoinType::Inner,
            2,
            "float_inner",
        );
    }

    #[test]
    fn join_semantics_boolean_inner() {
        // left [T,F,T] ⋈ right [T,F] → (T,T),(T,T),(F,F) = 3 rows
        let left = vec![
            jrec("b", Value::Boolean(true)),
            jrec("b", Value::Boolean(false)),
            jrec("b", Value::Boolean(true)),
        ];
        let right = vec![
            jrec("b", Value::Boolean(true)),
            jrec("b", Value::Boolean(false)),
        ];
        both_join_count(
            &left,
            &right,
            &jfield("b"),
            &jfield("b"),
            JoinType::Inner,
            3,
            "boolean_inner",
        );
    }

    #[test]
    fn join_semantics_text_inner() {
        // text key matching is case-sensitive
        let left: Vec<_> = ["alice", "bob", "carol"]
            .iter()
            .map(|s| jrec("name", Value::text(s.to_string())))
            .collect();
        let right: Vec<_> = ["bob", "dave", "carol"]
            .iter()
            .map(|s| jrec("name", Value::text(s.to_string())))
            .collect();
        both_join_count(
            &left,
            &right,
            &jfield("name"),
            &jfield("name"),
            JoinType::Inner,
            2,
            "text_inner",
        );
    }

    #[test]
    fn join_semantics_null_both_sides_inner() {
        // Both join types treat NULL = NULL as a match via their respective
        // equality paths (PartialEq for nested loop; identical "NULL" hash key
        // for hash join). This protects the current semantics.
        let left = vec![jrec("k", Value::Null), jrec("k", Value::Integer(1))];
        let right = vec![jrec("k", Value::Null), jrec("k", Value::Integer(1))];
        let tq = left_tq();
        let lf = jfield("k");
        let rf = jfield("k");
        let nl = execute_runtime_nested_loop_join(
            &tq,
            &left,
            None,
            None,
            &lf,
            &right,
            None,
            None,
            &rf,
            JoinType::Inner,
        )
        .unwrap();
        let hj = execute_runtime_hash_join(
            &tq,
            &left,
            None,
            None,
            &lf,
            &right,
            None,
            None,
            &rf,
            JoinType::Inner,
        )
        .unwrap();
        // Both must agree: Null=Null matches, Integer(1)=Integer(1) matches → 2 rows
        assert_eq!(nl.len(), 2, "nested_loop null=null inner");
        assert_eq!(
            nl.len(),
            hj.len(),
            "nested_loop and hash_join must agree on null semantics"
        );
    }

    #[test]
    fn join_semantics_missing_field_nested_loop_no_inner_match() {
        // Nested loop returns false when the join field is absent from a record
        // (join_condition_matches → None on either side → false).
        let left = vec![jrec("x", Value::Integer(1))]; // no "k" column
        let right = vec![jrec("y", Value::Integer(1))]; // no "k" column
        let tq = left_tq();
        let lf = jfield("k");
        let rf = jfield("k");
        let nl = execute_runtime_nested_loop_join(
            &tq,
            &left,
            None,
            None,
            &lf,
            &right,
            None,
            None,
            &rf,
            JoinType::Inner,
        )
        .unwrap();
        assert_eq!(
            nl.len(),
            0,
            "nested_loop: absent join field → no inner match"
        );
    }

    #[test]
    fn join_semantics_duplicate_key_many_to_many() {
        // 2 left rows with key=1 × 2 right rows with key=1 → 4 result rows
        let left = vec![jrec("k", Value::Integer(1)), jrec("k", Value::Integer(1))];
        let right = vec![jrec("k", Value::Integer(1)), jrec("k", Value::Integer(1))];
        both_join_count(
            &left,
            &right,
            &jfield("k"),
            &jfield("k"),
            JoinType::Inner,
            4,
            "dup_key_n_to_m",
        );
    }

    #[test]
    fn join_semantics_left_outer_unmatched_left_row() {
        // key=2 has no right match; left outer includes it padded with nulls
        let left = vec![jrec("k", Value::Integer(1)), jrec("k", Value::Integer(2))];
        let right = vec![jrec("k", Value::Integer(1))];
        both_join_count(
            &left,
            &right,
            &jfield("k"),
            &jfield("k"),
            JoinType::LeftOuter,
            2,
            "left_outer",
        );
    }

    #[test]
    fn join_semantics_right_outer_unmatched_right_row() {
        // right key=5 has no left match; right outer includes it
        let left = vec![jrec("k", Value::Integer(1))];
        let right = vec![jrec("k", Value::Integer(1)), jrec("k", Value::Integer(5))];
        both_join_count(
            &left,
            &right,
            &jfield("k"),
            &jfield("k"),
            JoinType::RightOuter,
            2,
            "right_outer",
        );
    }

    #[test]
    fn join_semantics_full_outer_both_sides_unmatched() {
        // left key=2 and right key=3 are both unmatched; full outer emits both
        let left = vec![jrec("k", Value::Integer(1)), jrec("k", Value::Integer(2))];
        let right = vec![jrec("k", Value::Integer(1)), jrec("k", Value::Integer(3))];
        // (1,1) matched + left 2 unmatched + right 3 unmatched = 3 rows
        both_join_count(
            &left,
            &right,
            &jfield("k"),
            &jfield("k"),
            JoinType::FullOuter,
            3,
            "full_outer",
        );
    }

    #[test]
    fn join_semantics_cross_join_cartesian_product() {
        // 3 × 4 = 12 rows; join key is irrelevant for cross join
        let left: Vec<_> = (0i64..3).map(|i| jrec("a", Value::Integer(i))).collect();
        let right: Vec<_> = (0i64..4).map(|i| jrec("b", Value::Integer(i))).collect();
        both_join_count(
            &left,
            &right,
            &jfield("a"),
            &jfield("b"),
            JoinType::Cross,
            12,
            "cross_cartesian",
        );
    }

    #[test]
    fn join_semantics_mixed_type_integer_float_numeric_equality() {
        // Integer(2) on left equals Float(2.0) on right via numeric coercion in
        // both join algorithms (nested loop: runtime_value_number path;
        // hash join: identical Display strings "2").
        let left = vec![jrec("k", Value::Integer(2)), jrec("k", Value::Integer(3))];
        let right = vec![jrec("k", Value::Float(2.0)), jrec("k", Value::Float(4.0))];
        both_join_count(
            &left,
            &right,
            &jfield("k"),
            &jfield("k"),
            JoinType::Inner,
            1,
            "mixed_int_float",
        );
    }

    #[test]
    fn join_semantics_noderef_identity_match() {
        // NodeRef values match when the underlying id string is equal (both
        // nested loop via text-str borrow path and hash join via to_string key).
        let left = vec![
            jrec("id", Value::NodeRef("svc:1".to_string())),
            jrec("id", Value::NodeRef("svc:2".to_string())),
        ];
        let right = vec![
            jrec("ref", Value::NodeRef("svc:2".to_string())),
            jrec("ref", Value::NodeRef("svc:3".to_string())),
        ];
        both_join_count(
            &left,
            &right,
            &jfield("id"),
            &jfield("ref"),
            JoinType::Inner,
            1,
            "noderef_identity",
        );
    }

    #[test]
    fn join_semantics_result_columns_preserved_after_join() {
        // Verify that the merged records produced by both join algorithms carry
        // the expected columns from both sides. Uses distinct column names to
        // avoid the name-collision prefix logic in merge_join_records.
        let left = vec![jrec("l_id", Value::Integer(7))];
        let right = vec![jrec("r_val", Value::text("hello".to_string()))];
        // No match — both produce 0 inner rows. Use left outer to get a merged row.
        let tq = left_tq();
        let lf = jfield("l_id");
        let rf = jfield("l_id"); // right has no "l_id" → left outer pads right with nulls
        let nl = execute_runtime_nested_loop_join(
            &tq,
            &left,
            None,
            None,
            &lf,
            &right,
            None,
            None,
            &rf,
            JoinType::LeftOuter,
        )
        .unwrap();
        assert_eq!(nl.len(), 1, "left outer must emit the unmatched left row");
        // The merged row must carry the left-side column
        assert_eq!(nl[0].get("l_id"), Some(&Value::Integer(7)));
    }

    #[test]
    fn indexed_join_borrowed_candidate_list_matches_hash_join() {
        // execute_runtime_indexed_join probes the right-side hash bucket by
        // borrowing the candidate index list in place (no per-left-row clone,
        // #1346). Duplicate keys force multi-element candidate lists — the exact
        // path the borrow touches — and both inner and outer results must stay
        // identical to the hash join over the same inputs.
        let left: Vec<_> = [1i64, 1, 2, 3]
            .iter()
            .map(|v| jrec("k", Value::Integer(*v)))
            .collect();
        let right: Vec<_> = [1i64, 1, 2, 4]
            .iter()
            .map(|v| jrec("k", Value::Integer(*v)))
            .collect();
        let tq = left_tq();
        let lf = jfield("k");
        let rf = jfield("k");

        let indexed_inner = execute_runtime_indexed_join(
            &tq,
            &left,
            None,
            None,
            &lf,
            &right,
            None,
            None,
            &rf,
            JoinType::Inner,
        )
        .expect("indexed inner join");
        let hashed_inner = execute_runtime_hash_join(
            &tq,
            &left,
            None,
            None,
            &lf,
            &right,
            None,
            None,
            &rf,
            JoinType::Inner,
        )
        .expect("hash inner join");
        // key 1: 2 left × 2 right = 4, key 2: 1 × 1 = 1, keys 3/4 unmatched → 5
        assert_eq!(indexed_inner.len(), 5, "indexed inner many-to-many fan-out");
        assert_eq!(
            indexed_inner.len(),
            hashed_inner.len(),
            "indexed inner row count must match hash join after borrow change"
        );

        // Left outer pads the unmatched left key (3) with a null right side.
        let indexed_left = execute_runtime_indexed_join(
            &tq,
            &left,
            None,
            None,
            &lf,
            &right,
            None,
            None,
            &rf,
            JoinType::LeftOuter,
        )
        .expect("indexed left outer join");
        assert_eq!(
            indexed_left.len(),
            6,
            "indexed left outer must pad the unmatched left row"
        );
    }

    // ── Benchmark-style timing measurement (#1339) ───────────────────────────
    // Run to capture the baseline cost of hash/nested-loop join build+probe:
    //   CARGO_BUILD_JOBS=1 RUSTFLAGS="-C debuginfo=0" \
    //   cargo nextest run -p reddb-io-server --lib \
    //     -- join_filter::tests::benchmark_join_build_probe_timing --nocapture
    //
    // Baseline recorded 2026-06-25 on the reddb guard host (14G, debug profile):
    //   hash_join   n=   10: avg=66µs    (20 iters)
    //   nested_loop n=   10: avg=90µs    (20 iters)
    //   hash_join   n=  100: avg=654µs   (20 iters)
    //   nested_loop n=  100: avg=4906µs  (20 iters)
    //   hash_join   n= 1000: avg=6385µs  (20 iters)
    //   nested_loop n= 1000: avg=432443µs (20 iters)
    // The O(n) vs O(n²) gap confirms hash join wins at n=100+ (7.5× faster)
    // and dominates at n=1000 (68× faster). These unoptimized numbers are the
    // baseline; future key-representation changes must not widen this gap.
    #[test]
    fn benchmark_join_build_probe_timing() {
        for &n in &[10usize, 100, 1_000] {
            let left: Vec<_> = (0i64..n as i64)
                .map(|i| jrec("k", Value::Integer(i)))
                .collect();
            let right: Vec<_> = (0i64..n as i64)
                .map(|i| jrec("k", Value::Integer(i)))
                .collect();
            let tq = left_tq();
            let lf = jfield("k");
            let rf = jfield("k");

            // warmup
            for _ in 0..3 {
                let _ = execute_runtime_hash_join(
                    &tq,
                    &left,
                    None,
                    None,
                    &lf,
                    &right,
                    None,
                    None,
                    &rf,
                    JoinType::Inner,
                );
            }

            let iters = 20usize;
            let t0 = std::time::Instant::now();
            for _ in 0..iters {
                let _ = execute_runtime_hash_join(
                    &tq,
                    &left,
                    None,
                    None,
                    &lf,
                    &right,
                    None,
                    None,
                    &rf,
                    JoinType::Inner,
                );
            }
            let hj_us = t0.elapsed().as_micros() / iters as u128;

            let t0 = std::time::Instant::now();
            for _ in 0..iters {
                let _ = execute_runtime_nested_loop_join(
                    &tq,
                    &left,
                    None,
                    None,
                    &lf,
                    &right,
                    None,
                    None,
                    &rf,
                    JoinType::Inner,
                );
            }
            let nl_us = t0.elapsed().as_micros() / iters as u128;

            // Indexed join shares the hash-bucket build but now borrows the
            // candidate list during probing instead of cloning it per left row
            // (#1346); measured here to show the clone-reduction impact.
            let t0 = std::time::Instant::now();
            for _ in 0..iters {
                let _ = execute_runtime_indexed_join(
                    &tq,
                    &left,
                    None,
                    None,
                    &lf,
                    &right,
                    None,
                    None,
                    &rf,
                    JoinType::Inner,
                );
            }
            let ix_us = t0.elapsed().as_micros() / iters as u128;

            println!("hash_join    n={n:>5}: avg={hj_us}µs  ({iters} iters)");
            println!("nested_loop  n={n:>5}: avg={nl_us}µs  ({iters} iters)");
            println!("indexed_join n={n:>5}: avg={ix_us}µs  ({iters} iters)");
        }
    }

    // ── Typed internal join key + indexed-join coverage (#1345) ───────────────
    // The indexed and graph-lookup join paths now build/probe a typed
    // `RuntimeJoinKey` index instead of formatted, prefix-namespaced strings.
    // These tests pin the key-class namespacing and prove the indexed join
    // still agrees with the nested-loop reference on the key classes it covers.

    fn indexed_count(
        left: &[UnifiedRecord],
        right: &[UnifiedRecord],
        lf: &FieldRef,
        rf: &FieldRef,
        join_type: JoinType,
    ) -> usize {
        let tq = left_tq();
        execute_runtime_indexed_join(&tq, left, None, None, lf, right, None, None, rf, join_type)
            .unwrap()
            .len()
    }

    #[test]
    fn typed_join_key_classes_are_disjoint_namespaces() {
        // numeric 1 and textual "1" never collide (was "n:1" vs "t:1")
        assert_ne!(
            runtime_join_lookup_key(&Value::Integer(1)),
            runtime_join_lookup_key(&Value::text("1".to_string()))
        );
        // boolean true and textual "true" never collide (was "b:true" vs "t:true")
        assert_ne!(
            runtime_join_lookup_key(&Value::Boolean(true)),
            runtime_join_lookup_key(&Value::text("true".to_string()))
        );
        // Integer(2) and Float(2.0) share one numeric key (was both "n:2")
        assert_eq!(
            runtime_join_lookup_key(&Value::Integer(2)),
            runtime_join_lookup_key(&Value::Float(2.0))
        );
        assert_eq!(
            runtime_join_lookup_key(&Value::Integer(2)),
            Some(RuntimeJoinKey::Number(2.0f64.to_bits()))
        );
        // null / array / blob produce no value key (unchanged)
        assert_eq!(runtime_join_lookup_key(&Value::Null), None);
    }

    #[test]
    fn typed_identity_key_matches_numeric_to_reference_suffix() {
        // A numeric value's identity key collides with the trailing-segment
        // identity of a reference string, preserving the graph-join identity
        // match that used to compare "id:2" == "id:2".
        let num_keys = runtime_join_lookup_keys(&Value::Integer(2));
        let ref_keys = runtime_join_lookup_keys(&Value::NodeRef("svc:2".to_string()));
        assert!(num_keys.contains(&RuntimeJoinKey::Identity("2".to_string())));
        assert!(ref_keys.contains(&RuntimeJoinKey::Identity("2".to_string())));
    }

    #[test]
    fn indexed_join_integer_inner_matches_reference() {
        let left: Vec<_> = [1i64, 2, 3]
            .iter()
            .map(|v| jrec("k", Value::Integer(*v)))
            .collect();
        let right: Vec<_> = [2i64, 3, 4]
            .iter()
            .map(|v| jrec("k", Value::Integer(*v)))
            .collect();
        assert_eq!(
            indexed_count(&left, &right, &jfield("k"), &jfield("k"), JoinType::Inner),
            2
        );
    }

    #[test]
    fn indexed_join_text_inner_matches_reference() {
        let left: Vec<_> = ["alice", "bob", "carol"]
            .iter()
            .map(|s| jrec("name", Value::text(s.to_string())))
            .collect();
        let right: Vec<_> = ["bob", "dave", "carol"]
            .iter()
            .map(|s| jrec("name", Value::text(s.to_string())))
            .collect();
        assert_eq!(
            indexed_count(
                &left,
                &right,
                &jfield("name"),
                &jfield("name"),
                JoinType::Inner
            ),
            2
        );
    }

    #[test]
    fn indexed_join_mixed_int_float_inner() {
        // Integer(2) key probes the same numeric bucket as Float(2.0) and the
        // candidate is confirmed by join_condition_matches → 1 matched pair.
        let left = vec![jrec("k", Value::Integer(2)), jrec("k", Value::Integer(3))];
        let right = vec![jrec("k", Value::Float(2.0)), jrec("k", Value::Float(4.0))];
        assert_eq!(
            indexed_count(&left, &right, &jfield("k"), &jfield("k"), JoinType::Inner),
            1
        );
    }

    #[test]
    fn indexed_join_duplicate_key_many_to_many() {
        let left = vec![jrec("k", Value::Integer(1)), jrec("k", Value::Integer(1))];
        let right = vec![jrec("k", Value::Integer(1)), jrec("k", Value::Integer(1))];
        assert_eq!(
            indexed_count(&left, &right, &jfield("k"), &jfield("k"), JoinType::Inner),
            4
        );
    }

    #[test]
    fn indexed_join_left_outer_pads_unmatched() {
        // key=2 has no right candidate → padded as an unmatched left row.
        let left = vec![jrec("k", Value::Integer(1)), jrec("k", Value::Integer(2))];
        let right = vec![jrec("k", Value::Integer(1))];
        assert_eq!(
            indexed_count(
                &left,
                &right,
                &jfield("k"),
                &jfield("k"),
                JoinType::LeftOuter
            ),
            2
        );
    }
}
