//! Runtime projection and visible-column helpers.
use super::*;

pub(in crate::runtime) fn project_runtime_record(
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
    .unwrap_or_else(|_| UnifiedRecord::new())
}

pub(in crate::runtime) fn project_runtime_record_with_db(
    db: Option<&RedDB>,
    source: &UnifiedRecord,
    projections: &[Projection],
    table_name: Option<&str>,
    table_alias: Option<&str>,
    document_projection: bool,
    entity_projection: bool,
) -> crate::RedDBResult<UnifiedRecord> {
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
                if let Some((expr, _)) =
                    crate::storage::query::sql_lowering::projection_to_expr(projection)
                {
                    let row = RecordRow {
                        record: source,
                        table_name,
                        table_alias,
                    };
                    match crate::storage::query::evaluator::evaluate(&expr, &row) {
                        Ok(value) => Some(value),
                        // A shape the typed evaluator does not cover (unknown
                        // function, or an unresolved reference such as a CAST
                        // target type lowered to a `TYPE:*` pseudo-column) falls
                        // back to the legacy path. Genuine data errors (JSON
                        // parse failure, overflow, …) still propagate.
                        Err(crate::storage::query::evaluator::EvalError::UnknownFunction {
                            ..
                        })
                        | Err(crate::storage::query::evaluator::EvalError::UnknownColumn(_)) => {
                            Some(Value::Boolean(evaluate_runtime_filter_with_db(
                                db,
                                source,
                                filter,
                                table_name,
                                table_alias,
                            )))
                        }
                        Err(err) => return Err(crate::RedDBError::Query(err.to_string())),
                    }
                } else {
                    Some(Value::Boolean(evaluate_runtime_filter_with_db(
                        db,
                        source,
                        filter,
                        table_name,
                        table_alias,
                    )))
                }
            }
            Projection::Function(ref name, ref args) => {
                // Route catalog-resolvable functions through the typed evaluator.
                // Falls back to the legacy dispatcher for CONFIG/KV/ML_*/geo/time
                // and any shape where argument resolution fails via evaluator.
                if let Some((expr, _)) =
                    crate::storage::query::sql_lowering::projection_to_expr(projection)
                {
                    let row = RecordRow {
                        record: source,
                        table_name,
                        table_alias,
                    };
                    match crate::storage::query::evaluator::evaluate(&expr, &row) {
                        Ok(value) => Some(value),
                        // A shape the typed evaluator does not cover (unknown
                        // function, or an unresolved reference such as a CAST
                        // target type lowered to a `TYPE:*` pseudo-column) falls
                        // back to the legacy dispatcher. Genuine data errors
                        // (JSON parse failure, overflow, …) still propagate.
                        Err(crate::storage::query::evaluator::EvalError::UnknownFunction {
                            ..
                        })
                        | Err(crate::storage::query::evaluator::EvalError::UnknownColumn(_)) => {
                            evaluate_scalar_function_with_db(db, name, args, source)
                        }
                        Err(err) => return Err(crate::RedDBError::Query(err.to_string())),
                    }
                } else {
                    evaluate_scalar_function_with_db(db, name, args, source)
                }
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

    Ok(record)
}

pub(in crate::runtime) fn resolve_runtime_projection_value(
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

pub(in crate::runtime) fn projected_columns(
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

pub(in crate::runtime) fn collect_visible_columns(records: &[UnifiedRecord]) -> Vec<String> {
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

pub(in crate::runtime) fn visible_value_keys(record: &UnifiedRecord) -> Vec<String> {
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

pub(in crate::runtime) fn projection_name(projection: &Projection) -> String {
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

pub(in crate::runtime) fn field_ref_name(field: &FieldRef) -> String {
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
