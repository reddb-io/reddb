use super::*;
use crate::runtime::authz::policy_columns::{collect_expr_columns, collect_field_ref_column};
use crate::runtime::{materialization_limit, query_budget};
use reddb_engine::query::runtime::value_functions::cron_timestamp_ms;
use reddb_rql::ast::Span;
use std::collections::BTreeSet;
use std::sync::Arc;

fn arguments(item: &SelectItem) -> Option<&[Expr]> {
    match item {
        SelectItem::Expr {
            expr: Expr::FunctionCall { name, args, .. },
            ..
        } if name.eq_ignore_ascii_case("cron_occurrences") => Some(args),
        _ => None,
    }
}

pub(super) fn projection_index(query: &TableQuery) -> Option<usize> {
    query
        .select_items
        .iter()
        .position(|item| arguments(item).is_some())
}

pub(super) fn execute(
    db: &RedDB,
    query: &TableQuery,
    index_store: Option<&super::index_store::IndexStore>,
    index: usize,
    projections: &[Projection],
) -> RedDBResult<UnifiedResult> {
    let Some([rule, start, end]) = arguments(&query.select_items[index]) else {
        return Err(RedDBError::Query(
            "cron_occurrences expects a CRON and two timestamps".into(),
        ));
    };
    if query.select_items[index + 1..]
        .iter()
        .any(|item| arguments(item).is_some())
        || has_aggregate_projections(projections)
        || !effective_table_group_by_exprs(query).is_empty()
        || effective_table_having_filter(query).is_some()
        || projections
            .iter()
            .any(|p| matches!(p, Projection::Window { .. } | Projection::All))
    {
        return Err(RedDBError::Query(
            "use one cron_occurrences projection with explicit scalar columns".into(),
        ));
    }

    let table = query.table.as_str();
    let alias = query.alias.as_deref();
    let columns = projected_columns(&[], projections);
    let output_schema: Arc<Vec<Arc<str>>> =
        Arc::new(columns.iter().map(|c| Arc::from(c.as_str())).collect());
    let mut sort_inputs = BTreeSet::new();
    for order in &query.order_by {
        if let Some(expr) = &order.expr {
            collect_expr_columns(expr, table, alias, &mut sort_inputs);
        } else {
            collect_field_ref_column(&order.field, table, alias, &mut sort_inputs);
        }
    }
    sort_inputs.retain(|column| !columns.contains(column));

    // Keep the base read on the ordinary authorized table/filter/index path.
    // Its pagination belongs to the expanded rows, not the source rules.
    let mut input = query.clone();
    let mut fields = sort_inputs.clone();
    for item in &query.select_items {
        if let SelectItem::Expr { expr, .. } = item {
            collect_expr_columns(expr, table, alias, &mut fields);
        }
    }
    if let Some(contract) = db.collection_contract(table) {
        if contract.retention_duration_ms.is_some() {
            fields.extend(crate::runtime::retention_filter::resolve_timestamp_column(
                &contract,
            ));
        }
    }
    input.select_items = fields
        .into_iter()
        .map(|column| SelectItem::Expr {
            expr: Expr::Column {
                field: FieldRef::column("", &column),
                span: Span::synthetic(),
            },
            alias: None,
        })
        .collect();
    if input.select_items.is_empty() {
        input.select_items.push(SelectItem::Expr {
            expr: Expr::Literal {
                value: Value::Integer(1),
                span: Span::synthetic(),
            },
            alias: None,
        });
    }
    input.columns = effective_table_projections(&input);
    input.order_by.clear();
    input.offset = None;
    input.limit = None;
    input.distinct = false;
    let sources = execute_runtime_table_query_materialized(db, &input, index_store)?;
    let max_rows = materialization_limit::max_materialized_rows(db)?;
    let mut records = Vec::new();
    let scalar_columns: Vec<_> = projections
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != index)
        .map(|(_, projection)| projection.clone())
        .collect();
    let early_limit = query.order_by.is_empty() && !query.distinct;
    let mut skip = if early_limit {
        query.offset.unwrap_or(0)
    } else {
        0
    };
    // A rule's instants strictly increase. When the first sort key is
    // the occurrence itself, later instants cannot displace its first k.
    let ordered_by_instant = query.order_by.first().is_some_and(|order| {
        order.ascending
            && order
                .expr
                .as_ref()
                .is_none_or(|expr| matches!(expr, Expr::Column { .. }))
            && matches!(&order.field, FieldRef::TableColumn { table, column }
                if table.is_empty() && column == &columns[index])
    });
    let per_rule_limit = query
        .limit
        .filter(|_| ordered_by_instant && !query.distinct)
        .map(|limit| {
            usize::try_from(limit.saturating_add(query.offset.unwrap_or(0))).unwrap_or(usize::MAX)
        })
        .unwrap_or(usize::MAX);

    'sources: for source in &sources.records {
        if early_limit && query.limit == Some(records.len() as u64) {
            break;
        }
        query_budget::charge(1)?;
        let evaluate = |expr: &Expr| -> RedDBResult<Value> {
            if super::super::join_filter::expression_uses_secret(expr, source, Some(table), alias) {
                return Err(RedDBError::Query(
                    "cron_occurrences requires public schedule and window arguments".into(),
                ));
            }
            Ok(super::super::expr_eval::try_evaluate_runtime_expr_with_db(
                Some(db),
                expr,
                source,
                Some(table),
                alias,
            )?
            .unwrap_or(Value::Null))
        };
        let (schedule, start, end) = (evaluate(rule)?, evaluate(start)?, evaluate(end)?);
        if schedule.is_null() || start.is_null() || end.is_null() {
            continue;
        }
        let Value::Cron(schedule) = schedule else {
            return Err(RedDBError::Query(
                "cron_occurrences expects a CRON schedule".into(),
            ));
        };
        let start = cron_timestamp_ms(&start).map_err(RedDBError::Query)?;
        let end = cron_timestamp_ms(&end).map_err(RedDBError::Query)?;
        let mut template = None;
        for instant in schedule
            .occurrences(start, end)
            .map_err(RedDBError::Query)?
            .take(per_rule_limit)
        {
            let instant = instant.map_err(RedDBError::Query)?;
            query_budget::charge(1)?;
            if skip > 0 {
                skip -= 1;
                continue;
            }
            materialization_limit::check("cron_occurrences", records.len() + 1, max_rows)?;
            if template.is_none() {
                let projected = if scalar_columns.is_empty() {
                    UnifiedRecord::new()
                } else {
                    project_runtime_record_with_db(
                        Some(db),
                        source,
                        &scalar_columns,
                        Some(table),
                        alias,
                        false,
                        false,
                    )?
                };
                let values = columns
                    .iter()
                    .map(|column| projected.get(column).cloned().unwrap_or(Value::Null))
                    .collect();
                let mut row = UnifiedRecord::with_schema(Arc::clone(&output_schema), values);
                for column in &sort_inputs {
                    if let Some(value) = source.get(column) {
                        row.set(column, value.clone());
                    }
                }
                template = Some(row);
            }
            let mut row = template
                .as_ref()
                .expect("initialized occurrence row")
                .clone();
            row.set_arc(
                Arc::clone(&output_schema[index]),
                Value::TimestampMs(instant),
            );
            records.push(row);
            if early_limit && query.limit == Some(records.len() as u64) {
                break 'sources;
            }
        }
    }
    if query.distinct {
        dedup_distinct_records(&mut records, &columns);
    }
    if !query.order_by.is_empty() {
        super::super::join_filter::try_sort_or_top_k_records_with_db(
            Some(db),
            &mut records,
            &query.order_by,
            query.offset,
            query.limit,
            Some(table),
            alias,
        )?;
    }
    if !early_limit {
        let offset = query.offset.unwrap_or(0).min(records.len() as u64) as usize;
        records.drain(..offset);
        if let Some(limit) = query.limit {
            records.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        }
    }
    if !sort_inputs.is_empty() {
        for record in &mut records {
            *record = UnifiedRecord::with_schema(
                Arc::clone(&output_schema),
                record.schema_values().to_vec(),
            );
        }
    }
    Ok(UnifiedResult {
        columns,
        records,
        stats: sources.stats,
        pre_serialized_json: None,
    })
}
