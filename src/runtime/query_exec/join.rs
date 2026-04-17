//! Join query executor.
//!
//! Handles INNER/LEFT/RIGHT/FULL joins across the canonical plan tree,
//! plus filter/sort/projection passes on join results. Split out of
//! `query_exec.rs` to isolate the ~580-line join code from the rest
//! of the table-scan executor.
//!
//! Uses `use super::*;` to inherit everything query_exec.rs already
//! imports from the runtime module. Public entry point is
//! [`execute_runtime_join_query`].

use super::super::*;
use super::*;
use crate::storage::query::sql_lowering::{effective_join_filter, effective_join_projections};

pub(crate) fn execute_runtime_join_query(
    db: &RedDB,
    query: &JoinQuery,
) -> RedDBResult<UnifiedResult> {
    let records = execute_runtime_canonical_join_query(db, query)?;
    let effective_projections = effective_join_projections(query);
    let columns = projected_columns(&records, &effective_projections);

    Ok(UnifiedResult {
        columns,
        records,
        stats: Default::default(),
        pre_serialized_json: None,
    })
}

pub(crate) fn execute_runtime_canonical_join_query(
    db: &RedDB,
    query: &JoinQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let plan = CanonicalPlanner::new(db).build(&QueryExpr::Join(query.clone()));
    execute_runtime_canonical_join_node(db, &plan.root, query)
}

pub(crate) fn execute_runtime_canonical_join_node(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    query: &JoinQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let (left_table_name, left_table_alias, right_table_name, right_table_alias) =
        runtime_join_table_context(query);

    match node.operator.as_str() {
        "filter" => {
            let mut records = execute_runtime_canonical_join_child(db, node, query)?;
            if let Some(filter) = effective_join_filter(query).as_ref() {
                records.retain(|record| {
                    evaluate_runtime_join_filter_with_db(
                        Some(db),
                        record,
                        filter,
                        left_table_name,
                        left_table_alias,
                        right_table_name,
                        right_table_alias,
                    )
                });
            }
            Ok(records)
        }
        "sort" | "document_sort" | "entity_sort" => {
            let mut records = execute_runtime_canonical_join_child(db, node, query)?;
            if !query.order_by.is_empty() {
                records.sort_by(|left, right| {
                    compare_runtime_join_order_with_db(
                        Some(db),
                        left,
                        right,
                        &query.order_by,
                        left_table_name,
                        left_table_alias,
                        right_table_name,
                        right_table_alias,
                    )
                });
            } else if node.operator == "entity_sort" {
                records.sort_by(compare_runtime_ranked_records);
            }
            Ok(records)
        }
        "offset" => {
            let records = execute_runtime_canonical_join_child(db, node, query)?;
            let offset = query.offset.unwrap_or(0) as usize;
            Ok(records.into_iter().skip(offset).collect())
        }
        "limit" => {
            let records = execute_runtime_canonical_join_child(db, node, query)?;
            let limit = query.limit.map(|value| value as usize);
            Ok(match limit {
                Some(limit) => records.into_iter().take(limit).collect(),
                None => records,
            })
        }
        "projection" => {
            let records = execute_runtime_canonical_join_child(db, node, query)?;
            let effective_projections = effective_join_projections(query);
            Ok(records
                .iter()
                .map(|record| {
                    project_runtime_join_record_with_db(
                        Some(db),
                        record,
                        &effective_projections,
                        left_table_name,
                        left_table_alias,
                        right_table_name,
                        right_table_alias,
                    )
                })
                .collect())
        }
        "join" => execute_runtime_canonical_join_base(
            db,
            node,
            query,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        ),
        other => Err(RedDBError::Query(format!(
            "unsupported canonical join operator {other}"
        ))),
    }
}

pub(crate) fn execute_runtime_canonical_join_base(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    query: &JoinQuery,
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
) -> RedDBResult<Vec<UnifiedRecord>> {
    if node.children.len() != 2 {
        return Err(RedDBError::Query(
            "canonical join operator must contain exactly two child plans".to_string(),
        ));
    }

    let join_type = canonical_join_type(node)?;
    let left_join_field = canonical_join_field(node, "left_field")?;
    let right_join_field = canonical_join_field(node, "right_field")?;
    let join_strategy = canonical_join_strategy(node)?;

    let left_query = match query.left.as_ref() {
        QueryExpr::Table(table) => table,
        _ => {
            return Err(RedDBError::Query(
                "runtime joins currently require a table expression on the left side".to_string(),
            ))
        }
    };

    let left_records =
        execute_runtime_canonical_expr_node(db, &node.children[0], query.left.as_ref())?;

    let right_records =
        execute_runtime_canonical_expr_node(db, &node.children[1], query.right.as_ref())?;

    // Auto-upgrade to hash join for large datasets
    let join_strategy = if matches!(join_strategy, CanonicalJoinStrategy::NestedLoop)
        && left_records.len() * right_records.len() > 10_000
    {
        CanonicalJoinStrategy::HashJoin
    } else {
        join_strategy
    };

    match join_strategy {
        CanonicalJoinStrategy::IndexedNestedLoop => execute_runtime_indexed_join(
            left_query,
            &left_records,
            left_table_name,
            left_table_alias,
            &left_join_field,
            &right_records,
            right_table_name,
            right_table_alias,
            &right_join_field,
            join_type,
        ),
        CanonicalJoinStrategy::HashJoin => execute_runtime_hash_join(
            left_query,
            &left_records,
            left_table_name,
            left_table_alias,
            &left_join_field,
            &right_records,
            right_table_name,
            right_table_alias,
            &right_join_field,
            join_type,
        ),
        CanonicalJoinStrategy::NestedLoop => execute_runtime_full_scan_join(
            left_query,
            &left_records,
            left_table_name,
            left_table_alias,
            &left_join_field,
            &right_records,
            right_table_name,
            right_table_alias,
            &right_join_field,
            join_type,
        ),
        CanonicalJoinStrategy::GraphLookupJoin => execute_runtime_graph_lookup_join(
            left_query,
            &left_records,
            left_table_name,
            left_table_alias,
            &left_join_field,
            &right_records,
            right_table_name,
            right_table_alias,
            &right_join_field,
            join_type,
        ),
    }
}

pub(crate) fn execute_runtime_canonical_join_child(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    query: &JoinQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let child = node.children.first().ok_or_else(|| {
        RedDBError::Query(format!(
            "canonical join operator {} is missing its child plan",
            node.operator
        ))
    })?;
    execute_runtime_canonical_join_node(db, child, query)
}

pub(crate) fn runtime_join_table_context(
    query: &JoinQuery,
) -> (Option<&str>, Option<&str>, Option<&str>, Option<&str>) {
    let (left_table_name, left_table_alias) = match query.left.as_ref() {
        QueryExpr::Table(table) => (
            Some(table.table.as_str()),
            Some(table.alias.as_deref().unwrap_or(table.table.as_str())),
        ),
        _ => (None, None),
    };
    let (right_table_name, right_table_alias) = match query.right.as_ref() {
        QueryExpr::Table(table) => (
            Some(table.table.as_str()),
            Some(table.alias.as_deref().unwrap_or(table.table.as_str())),
        ),
        QueryExpr::Graph(graph) => (Some("graph"), graph.alias.as_deref().or(Some("graph"))),
        QueryExpr::Path(path) => (Some("path"), path.alias.as_deref().or(Some("path"))),
        QueryExpr::Vector(vector) => (Some("vector"), vector.alias.as_deref().or(Some("vector"))),
        QueryExpr::Hybrid(hybrid) => (Some("hybrid"), hybrid.alias.as_deref().or(Some("hybrid"))),
        QueryExpr::Join(_) => (Some("join"), Some("join")),
        QueryExpr::Insert(_)
        | QueryExpr::Update(_)
        | QueryExpr::Delete(_)
        | QueryExpr::CreateTable(_)
        | QueryExpr::DropTable(_)
        | QueryExpr::AlterTable(_)
        | QueryExpr::GraphCommand(_)
        | QueryExpr::SearchCommand(_)
        | QueryExpr::CreateIndex(_)
        | QueryExpr::DropIndex(_)
        | QueryExpr::ProbabilisticCommand(_)
        | QueryExpr::Ask(_)
        | QueryExpr::SetConfig { .. }
        | QueryExpr::ShowConfig { .. }
        | QueryExpr::SetTenant(_)
        | QueryExpr::ShowTenant
        | QueryExpr::CreateTimeSeries(_)
        | QueryExpr::DropTimeSeries(_)
        | QueryExpr::CreateQueue(_)
        | QueryExpr::DropQueue(_)
        | QueryExpr::QueueCommand(_)
        | QueryExpr::CreateTree(_)
        | QueryExpr::DropTree(_)
        | QueryExpr::TreeCommand(_)
        | QueryExpr::ExplainAlter(_)
        | QueryExpr::TransactionControl(_)
        | QueryExpr::MaintenanceCommand(_)
        | QueryExpr::CreateSchema(_)
        | QueryExpr::DropSchema(_)
        | QueryExpr::CreateSequence(_)
        | QueryExpr::DropSequence(_)
        | QueryExpr::CopyFrom(_)
        | QueryExpr::CreateView(_)
        | QueryExpr::DropView(_)
        | QueryExpr::RefreshMaterializedView(_)
        | QueryExpr::CreatePolicy(_)
        | QueryExpr::DropPolicy(_)
        | QueryExpr::CreateServer(_)
        | QueryExpr::DropServer(_)
        | QueryExpr::CreateForeignTable(_)
        | QueryExpr::DropForeignTable(_) => (None, None),
    };

    (
        left_table_name,
        left_table_alias,
        right_table_name,
        right_table_alias,
    )
}

pub(crate) fn resolve_runtime_join_field(
    record: &UnifiedRecord,
    field: &FieldRef,
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
) -> Option<Value> {
    match field {
        FieldRef::TableColumn { table, column } if !table.is_empty() => {
            if let Some(value) = record.values.get(&format!("{table}.{column}")) {
                return Some(value.clone());
            }

            let matches_left =
                runtime_table_context_matches(table.as_str(), left_table_name, left_table_alias);
            let matches_right =
                runtime_table_context_matches(table.as_str(), right_table_name, right_table_alias);
            if !(matches_left || matches_right) {
                return None;
            }

            record
                .values
                .get(column)
                .cloned()
                .or_else(|| resolve_runtime_document_path(record, column))
        }
        _ => resolve_runtime_field(record, field, None, None),
    }
}

pub(crate) fn project_runtime_join_record(
    source: &UnifiedRecord,
    projections: &[Projection],
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
) -> UnifiedRecord {
    project_runtime_join_record_with_db(
        None,
        source,
        projections,
        left_table_name,
        left_table_alias,
        right_table_name,
        right_table_alias,
    )
}

pub(crate) fn project_runtime_join_record_with_db(
    db: Option<&RedDB>,
    source: &UnifiedRecord,
    projections: &[Projection],
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
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
            Projection::Column(column) | Projection::Alias(column, _) => source
                .values
                .get(column)
                .cloned()
                .or_else(|| resolve_runtime_document_path(source, column)),
            Projection::Field(field, _) => resolve_runtime_join_field(
                source,
                field,
                left_table_name,
                left_table_alias,
                right_table_name,
                right_table_alias,
            ),
            Projection::Expression(filter, _) => {
                Some(Value::Boolean(evaluate_runtime_join_filter_with_db(
                    db,
                    source,
                    filter,
                    left_table_name,
                    left_table_alias,
                    right_table_name,
                    right_table_alias,
                )))
            }
            Projection::Function(name, args) => {
                super::super::join_filter::evaluate_scalar_function_with_db(db, name, args, source)
            }
            Projection::All => None,
        };

        record.values.insert(label, value.unwrap_or(Value::Null));
    }

    record
}

pub(crate) fn evaluate_runtime_join_filter(
    record: &UnifiedRecord,
    filter: &Filter,
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
) -> bool {
    evaluate_runtime_join_filter_with_db(
        None,
        record,
        filter,
        left_table_name,
        left_table_alias,
        right_table_name,
        right_table_alias,
    )
}

pub(crate) fn evaluate_runtime_join_filter_with_db(
    db: Option<&RedDB>,
    record: &UnifiedRecord,
    filter: &Filter,
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
) -> bool {
    match filter {
        Filter::Compare { field, op, value } => resolve_runtime_join_field(
            record,
            field,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        )
        .as_ref()
        .and_then(|candidate| evaluate_metadata_field_compare(field, candidate, *op, value))
        .or_else(|| {
            resolve_runtime_join_field(
                record,
                field,
                left_table_name,
                left_table_alias,
                right_table_name,
                right_table_alias,
            )
            .as_ref()
            .map(|candidate| compare_runtime_values(candidate, value, *op))
        })
        .unwrap_or(false),
        Filter::CompareFields { left, op, right } => {
            let left_val = resolve_runtime_join_field(
                record,
                left,
                left_table_name,
                left_table_alias,
                right_table_name,
                right_table_alias,
            );
            let right_val = resolve_runtime_join_field(
                record,
                right,
                left_table_name,
                left_table_alias,
                right_table_name,
                right_table_alias,
            );
            match (left_val, right_val) {
                (Some(l), Some(r)) => compare_runtime_values(&l, &r, *op),
                _ => false,
            }
        }
        Filter::CompareExpr { lhs, op, rhs } => {
            // Join-record expression evaluation: delegate to the
            // single-side expr_eval walker using the LEFT table's
            // scope — join-scoped field references embed their own
            // table qualifier, so the walker resolves them via
            // resolve_runtime_field's qualified-column path.
            let l = super::super::expr_eval::evaluate_runtime_expr_with_db(
                db,
                lhs,
                record,
                left_table_name,
                left_table_alias,
            );
            let r = super::super::expr_eval::evaluate_runtime_expr_with_db(
                db,
                rhs,
                record,
                left_table_name,
                left_table_alias,
            );
            match (l, r) {
                (Some(lv), Some(rv)) => compare_runtime_values(&lv, &rv, *op),
                _ => false,
            }
        }
        Filter::And(left, right) => {
            evaluate_runtime_join_filter_with_db(
                db,
                record,
                left,
                left_table_name,
                left_table_alias,
                right_table_name,
                right_table_alias,
            ) && evaluate_runtime_join_filter_with_db(
                db,
                record,
                right,
                left_table_name,
                left_table_alias,
                right_table_name,
                right_table_alias,
            )
        }
        Filter::Or(left, right) => {
            evaluate_runtime_join_filter_with_db(
                db,
                record,
                left,
                left_table_name,
                left_table_alias,
                right_table_name,
                right_table_alias,
            ) || evaluate_runtime_join_filter_with_db(
                db,
                record,
                right,
                left_table_name,
                left_table_alias,
                right_table_name,
                right_table_alias,
            )
        }
        Filter::Not(inner) => !evaluate_runtime_join_filter_with_db(
            db,
            record,
            inner,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        ),
        Filter::IsNull(field) => resolve_runtime_join_field(
            record,
            field,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        )
        .map(|value| value == Value::Null)
        .unwrap_or(true),
        Filter::IsNotNull(field) => resolve_runtime_join_field(
            record,
            field,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        )
        .map(|value| value != Value::Null)
        .unwrap_or(false),
        Filter::In { field, values } => resolve_runtime_join_field(
            record,
            field,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        )
        .as_ref()
        .is_some_and(|candidate| {
            evaluate_metadata_field_in(field, candidate, values).unwrap_or_else(|| {
                values
                    .iter()
                    .any(|value| compare_runtime_values(candidate, value, CompareOp::Eq))
            })
        }),
        Filter::Between { field, low, high } => resolve_runtime_join_field(
            record,
            field,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        )
        .as_ref()
        .is_some_and(|candidate| {
            compare_runtime_values(candidate, low, CompareOp::Ge)
                && compare_runtime_values(candidate, high, CompareOp::Le)
        }),
        Filter::Like { field, pattern } => resolve_runtime_join_field(
            record,
            field,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        )
        .as_ref()
        .and_then(runtime_value_text)
        .is_some_and(|value| like_matches(&value, pattern)),
        Filter::StartsWith { field, prefix } => resolve_runtime_join_field(
            record,
            field,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        )
        .as_ref()
        .and_then(runtime_value_text)
        .is_some_and(|value| value.starts_with(prefix)),
        Filter::EndsWith { field, suffix } => resolve_runtime_join_field(
            record,
            field,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        )
        .as_ref()
        .and_then(runtime_value_text)
        .is_some_and(|value| value.ends_with(suffix)),
        Filter::Contains { field, substring } => resolve_runtime_join_field(
            record,
            field,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        )
        .as_ref()
        .and_then(runtime_value_text)
        .is_some_and(|value| value.contains(substring)),
    }
}

pub(crate) fn compare_runtime_join_order(
    left: &UnifiedRecord,
    right: &UnifiedRecord,
    clauses: &[OrderByClause],
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
) -> Ordering {
    compare_runtime_join_order_with_db(
        None,
        left,
        right,
        clauses,
        left_table_name,
        left_table_alias,
        right_table_name,
        right_table_alias,
    )
}

pub(crate) fn compare_runtime_join_order_with_db(
    db: Option<&RedDB>,
    left: &UnifiedRecord,
    right: &UnifiedRecord,
    clauses: &[OrderByClause],
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
) -> Ordering {
    for clause in clauses {
        let (left_value, right_value) = if let Some(ref expr) = clause.expr {
            (
                super::super::expr_eval::evaluate_runtime_expr_with_db(
                    db,
                    expr,
                    left,
                    left_table_name,
                    left_table_alias,
                ),
                super::super::expr_eval::evaluate_runtime_expr_with_db(
                    db,
                    expr,
                    right,
                    left_table_name,
                    left_table_alias,
                ),
            )
        } else {
            (
                resolve_runtime_join_field(
                    left,
                    &clause.field,
                    left_table_name,
                    left_table_alias,
                    right_table_name,
                    right_table_alias,
                ),
                resolve_runtime_join_field(
                    right,
                    &clause.field,
                    left_table_name,
                    left_table_alias,
                    right_table_name,
                    right_table_alias,
                ),
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
