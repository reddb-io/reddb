use super::*;

pub(super) fn execute_runtime_table_query(
    db: &RedDB,
    query: &TableQuery,
) -> RedDBResult<UnifiedResult> {
    let records = execute_runtime_canonical_table_query(db, query)?;
    let columns = projected_columns(&records, &query.columns);

    Ok(UnifiedResult {
        columns,
        records,
        stats: Default::default(),
    })
}

pub(super) struct RuntimeTableExecutionContext<'a> {
    query: &'a TableQuery,
    table_name: &'a str,
    table_alias: &'a str,
}

pub(super) fn execute_runtime_canonical_table_query(
    db: &RedDB,
    query: &TableQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let plan = CanonicalPlanner::new(db).build(&QueryExpr::Table(query.clone()));
    let table_name = query.table.as_str();
    let table_alias = query.alias.as_deref().unwrap_or(table_name);
    let context = RuntimeTableExecutionContext {
        query,
        table_name,
        table_alias,
    };
    execute_runtime_canonical_table_node(db, &plan.root, &context)
}

pub(super) fn execute_runtime_canonical_table_node(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    context: &RuntimeTableExecutionContext<'_>,
) -> RedDBResult<Vec<UnifiedRecord>> {
    match node.operator.as_str() {
        "table_scan" | "index_seek" | "entity_scan" | "document_path_index_seek" => {
            scan_runtime_table_source_records(db, context.query.table.as_str())
        }
        "filter" | "entity_filter" => {
            let mut records = execute_runtime_canonical_table_child(db, node, context)?;
            if let Some(filter) = context.query.filter.as_ref() {
                records.retain(|record| {
                    evaluate_runtime_filter(
                        record,
                        filter,
                        Some(context.table_name),
                        Some(context.table_alias),
                    )
                });
            }
            Ok(records)
        }
        "document_path_filter" => {
            let mut records = execute_runtime_canonical_table_child(db, node, context)?;
            if let Some(filter) = context.query.filter.as_ref() {
                records.retain(|record| {
                    runtime_record_has_document_capability(record)
                        && evaluate_runtime_document_filter(
                            record,
                            filter,
                            Some(context.table_name),
                            Some(context.table_alias),
                        )
                });
            }
            Ok(records)
        }
        "sort" | "entity_sort" | "document_sort" => {
            let mut records = execute_runtime_canonical_table_child(db, node, context)?;
            if !context.query.order_by.is_empty() {
                records.sort_by(|left, right| {
                    compare_runtime_order(
                        left,
                        right,
                        &context.query.order_by,
                        Some(context.table_name),
                        Some(context.table_alias),
                    )
                });
            } else if node.operator == "entity_sort" {
                records.sort_by(compare_runtime_ranked_records);
            }
            Ok(records)
        }
        "offset" | "entity_offset" => {
            let records = execute_runtime_canonical_table_child(db, node, context)?;
            let offset = context.query.offset.unwrap_or(0) as usize;
            Ok(records.into_iter().skip(offset).collect())
        }
        "limit" | "entity_limit" => {
            let records = execute_runtime_canonical_table_child(db, node, context)?;
            let limit = context.query.limit.map(|value| value as usize);
            Ok(match limit {
                Some(limit) => records.into_iter().take(limit).collect(),
                None => records,
            })
        }
        "entity_search" => execute_runtime_canonical_table_child(db, node, context),
        "entity_topk" => {
            let mut records = execute_runtime_canonical_table_child(db, node, context)?;
            records.sort_by(compare_runtime_ranked_records);
            let limit = node
                .details
                .get("k")
                .and_then(|value| value.parse::<usize>().ok())
                .or_else(|| context.query.limit.map(|value| value as usize));
            Ok(match limit {
                Some(limit) => records.into_iter().take(limit).collect(),
                None => records,
            })
        }
        "projection" | "document_projection" | "entity_projection" => {
            let records = execute_runtime_canonical_table_child(db, node, context)?;
            let document_projection = node.operator == "document_projection";
            let entity_projection = node.operator == "entity_projection";
            Ok(records
                .iter()
                .map(|record| {
                    project_runtime_record(
                        record,
                        &context.query.columns,
                        Some(context.table_name),
                        Some(context.table_alias),
                        document_projection,
                        entity_projection,
                    )
                })
                .collect())
        }
        other => Err(RedDBError::Query(format!(
            "unsupported canonical table operator {other}"
        ))),
    }
}

pub(super) fn execute_runtime_canonical_table_child(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    context: &RuntimeTableExecutionContext<'_>,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let child = node.children.first().ok_or_else(|| {
        RedDBError::Query(format!(
            "canonical table operator {} is missing its child plan",
            node.operator
        ))
    })?;
    execute_runtime_canonical_table_node(db, child, context)
}

pub(super) fn runtime_record_has_document_capability(record: &UnifiedRecord) -> bool {
    record
        .values
        .get("_capabilities")
        .and_then(|value| match value {
            crate::storage::schema::Value::Text(value) => Some(value),
            _ => None,
        })
        .map(|capabilities| {
            capabilities
                .split(',')
                .any(|capability| capability.trim() == "document")
        })
        .unwrap_or(false)
}

pub(super) fn evaluate_runtime_document_filter(
    record: &UnifiedRecord,
    filter: &crate::storage::query::ast::Filter,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> bool {
    evaluate_runtime_filter(record, filter, table_name, table_alias)
}

pub(super) fn runtime_record_rank_score(record: &UnifiedRecord) -> f64 {
    [
        "_score",
        "hybrid_score",
        "final_score",
        "score",
        "graph_score",
        "table_score",
        "graph_match",
        "vector_score",
        "vector_similarity",
        "structured_score",
        "structured_match",
        "text_relevance",
    ]
    .into_iter()
    .find_map(|field| record.values.get(field).and_then(runtime_value_number))
    .unwrap_or(0.0)
}

pub(super) fn compare_runtime_ranked_records(
    left: &UnifiedRecord,
    right: &UnifiedRecord,
) -> Ordering {
    runtime_record_rank_score(right)
        .partial_cmp(&runtime_record_rank_score(left))
        .unwrap_or(Ordering::Equal)
        .then_with(|| runtime_record_identity_key(left).cmp(&runtime_record_identity_key(right)))
}

pub(super) fn execute_runtime_join_query(
    db: &RedDB,
    query: &JoinQuery,
) -> RedDBResult<UnifiedResult> {
    let records = execute_runtime_canonical_join_query(db, query)?;
    let columns = projected_columns(&records, &query.return_);

    Ok(UnifiedResult {
        columns,
        records,
        stats: Default::default(),
    })
}

pub(super) fn execute_runtime_canonical_join_query(
    db: &RedDB,
    query: &JoinQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let plan = CanonicalPlanner::new(db).build(&QueryExpr::Join(query.clone()));
    execute_runtime_canonical_join_node(db, &plan.root, query)
}

pub(super) fn execute_runtime_canonical_join_node(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    query: &JoinQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let (left_table_name, left_table_alias, right_table_name, right_table_alias) =
        runtime_join_table_context(query);

    match node.operator.as_str() {
        "filter" => {
            let mut records = execute_runtime_canonical_join_child(db, node, query)?;
            if let Some(filter) = query.filter.as_ref() {
                records.retain(|record| {
                    evaluate_runtime_join_filter(
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
                    compare_runtime_join_order(
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
            Ok(records
                .iter()
                .map(|record| {
                    project_runtime_join_record(
                        record,
                        &query.return_,
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

pub(super) fn execute_runtime_canonical_join_base(
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

pub(super) fn execute_runtime_canonical_join_child(
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

pub(super) fn runtime_join_table_context(
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
        | QueryExpr::SearchCommand(_) => (None, None),
    };

    (
        left_table_name,
        left_table_alias,
        right_table_name,
        right_table_alias,
    )
}

pub(super) fn resolve_runtime_join_field(
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

pub(super) fn project_runtime_join_record(
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
                Some(Value::Boolean(evaluate_runtime_join_filter(
                    source,
                    filter,
                    left_table_name,
                    left_table_alias,
                    right_table_name,
                    right_table_alias,
                )))
            }
            Projection::Function(_, _) => Some(Value::Null),
            Projection::All => None,
        };

        record.values.insert(label, value.unwrap_or(Value::Null));
    }

    record
}

pub(super) fn evaluate_runtime_join_filter(
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
        Filter::And(left, right) => {
            evaluate_runtime_join_filter(
                record,
                left,
                left_table_name,
                left_table_alias,
                right_table_name,
                right_table_alias,
            ) && evaluate_runtime_join_filter(
                record,
                right,
                left_table_name,
                left_table_alias,
                right_table_name,
                right_table_alias,
            )
        }
        Filter::Or(left, right) => {
            evaluate_runtime_join_filter(
                record,
                left,
                left_table_name,
                left_table_alias,
                right_table_name,
                right_table_alias,
            ) || evaluate_runtime_join_filter(
                record,
                right,
                left_table_name,
                left_table_alias,
                right_table_name,
                right_table_alias,
            )
        }
        Filter::Not(inner) => !evaluate_runtime_join_filter(
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

pub(super) fn compare_runtime_join_order(
    left: &UnifiedRecord,
    right: &UnifiedRecord,
    clauses: &[OrderByClause],
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
) -> Ordering {
    for clause in clauses {
        let left_value = resolve_runtime_join_field(
            left,
            &clause.field,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        );
        let right_value = resolve_runtime_join_field(
            right,
            &clause.field,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        );
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

pub(super) fn execute_runtime_canonical_expr_node(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    expr: &QueryExpr,
) -> RedDBResult<Vec<UnifiedRecord>> {
    match expr {
        QueryExpr::Table(table) => {
            let table_name = table.table.as_str();
            let table_alias = table.alias.as_deref().unwrap_or(table_name);
            let context = RuntimeTableExecutionContext {
                query: table,
                table_name,
                table_alias,
            };
            execute_runtime_canonical_table_node(db, node, &context)
        }
        QueryExpr::Graph(_) | QueryExpr::Path(_) => {
            let graph = materialize_graph(db.store().as_ref())?;
            let result = crate::storage::query::unified::UnifiedExecutor::execute_on(&graph, expr)
                .map_err(|err| RedDBError::Query(err.to_string()))?;
            Ok(result.records)
        }
        QueryExpr::Vector(vector) => Ok(execute_runtime_vector_query(db, vector)?.records),
        QueryExpr::Hybrid(hybrid) => Ok(execute_runtime_hybrid_query(db, hybrid)?.records),
        other => Err(RedDBError::Query(format!(
            "canonical join execution does not yet support {} child expressions",
            query_expr_name(other)
        ))),
    }
}

pub(super) fn execute_runtime_vector_query(
    db: &RedDB,
    query: &VectorQuery,
) -> RedDBResult<UnifiedResult> {
    let plan = CanonicalPlanner::new(db).build(&QueryExpr::Vector(query.clone()));
    let records = execute_runtime_canonical_vector_node(db, &plan.root, query)?;

    Ok(UnifiedResult {
        columns: collect_visible_columns(&records),
        records,
        stats: Default::default(),
    })
}

pub(super) fn execute_runtime_canonical_vector_node(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    query: &VectorQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    match node.operator.as_str() {
        "vector_ann_hnsw" | "vector_ann_ivf" | "vector_exact_scan" => {
            let vector = resolve_runtime_vector_source(db, &query.query_vector)?;
            let matches = runtime_vector_matches(db, query, &vector)?;
            Ok(matches
                .into_iter()
                .map(runtime_vector_record_from_match)
                .collect())
        }
        "metadata_filter" => {
            let mut records = execute_runtime_canonical_vector_child(db, node, query)?;
            if let Some(filter) = query.filter.as_ref() {
                records.retain(|record| {
                    runtime_vector_record_matches_filter(db, &query.collection, record, filter)
                });
            }
            Ok(records)
        }
        "similarity_threshold" => {
            let mut records = execute_runtime_canonical_vector_child(db, node, query)?;
            if let Some(threshold) = query.threshold {
                records.retain(|record| runtime_record_rank_score(record) >= threshold as f64);
            }
            Ok(records)
        }
        "topk" => {
            let mut records = execute_runtime_canonical_vector_child(db, node, query)?;
            records.sort_by(compare_runtime_ranked_records);
            Ok(records.into_iter().take(query.k.max(1)).collect())
        }
        "projection" => execute_runtime_canonical_vector_child(db, node, query),
        other => Err(RedDBError::Query(format!(
            "unsupported canonical vector operator {other}"
        ))),
    }
}

pub(super) fn execute_runtime_canonical_vector_child(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    query: &VectorQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let child = node.children.first().ok_or_else(|| {
        RedDBError::Query(format!(
            "canonical vector operator {} is missing its child plan",
            node.operator
        ))
    })?;
    execute_runtime_canonical_vector_node(db, child, query)
}

pub(super) fn runtime_vector_matches(
    db: &RedDB,
    query: &VectorQuery,
    vector: &[f32],
) -> RedDBResult<Vec<SimilarResult>> {
    let manager = db
        .store()
        .get_collection(&query.collection)
        .ok_or_else(|| RedDBError::NotFound(query.collection.clone()))?;

    if query.filter.is_none() {
        let mut results = db.similar(&query.collection, vector, manager.count().max(1));
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.entity_id.raw().cmp(&b.entity_id.raw()))
        });
        return Ok(results);
    }

    let mut results: Vec<SimilarResult> = manager
        .query_all(|_| true)
        .into_iter()
        .filter_map(|entity| {
            let score = runtime_entity_vector_similarity(&entity, vector);
            let distance = (1.0 - score).max(0.0);
            (score > 0.0).then_some(SimilarResult {
                entity_id: entity.id,
                score,
                distance,
                entity,
            })
        })
        .collect();

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.entity_id.raw().cmp(&b.entity_id.raw()))
    });
    Ok(results)
}

pub(super) fn runtime_vector_record_matches_filter(
    db: &RedDB,
    collection: &str,
    record: &UnifiedRecord,
    filter: &VectorMetadataFilter,
) -> bool {
    let entity_id = record
        .values
        .get("entity_id")
        .or_else(|| record.values.get("_entity_id"))
        .and_then(|value| match value {
            Value::UnsignedInteger(value) => Some(EntityId::new(*value)),
            Value::Integer(value) if *value >= 0 => Some(EntityId::new(*value as u64)),
            _ => None,
        });

    let Some(entity_id) = entity_id else {
        return false;
    };

    let metadata = db
        .store()
        .get_metadata(collection, entity_id)
        .unwrap_or_default();
    let entry = runtime_metadata_entry(&metadata);
    filter.matches(&entry)
}

pub(super) fn execute_runtime_hybrid_query(
    db: &RedDB,
    query: &HybridQuery,
) -> RedDBResult<UnifiedResult> {
    let plan = CanonicalPlanner::new(db).build(&QueryExpr::Hybrid(query.clone()));
    let mut records = execute_runtime_canonical_hybrid_node(db, &plan.root, query)?;
    if let Some(limit) = query.limit {
        records.truncate(limit);
    }

    Ok(UnifiedResult {
        columns: collect_visible_columns(&records),
        records,
        stats: Default::default(),
    })
}

pub(super) fn execute_runtime_canonical_hybrid_node(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    query: &HybridQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    match node.operator.as_str() {
        "entity_search" => execute_runtime_canonical_hybrid_child(db, node, query),
        "entity_topk" => {
            let mut records = execute_runtime_canonical_hybrid_child(db, node, query)?;
            records.sort_by(compare_runtime_ranked_records);
            let limit = node
                .details
                .get("k")
                .and_then(|value| value.parse::<usize>().ok())
                .or(query.limit);
            Ok(match limit {
                Some(limit) => records.into_iter().take(limit).collect(),
                None => records,
            })
        }
        "hybrid_fusion" => execute_runtime_canonical_hybrid_fusion(db, node, query),
        other => Err(RedDBError::Query(format!(
            "unsupported canonical hybrid operator {other}"
        ))),
    }
}

pub(super) fn execute_runtime_canonical_hybrid_child(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    query: &HybridQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let child = node.children.first().ok_or_else(|| {
        RedDBError::Query(format!(
            "canonical hybrid operator {} is missing its child plan",
            node.operator
        ))
    })?;
    execute_runtime_canonical_hybrid_node(db, child, query)
}

pub(super) fn execute_runtime_canonical_hybrid_fusion(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    query: &HybridQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    if node.children.len() != 2 {
        return Err(RedDBError::Query(
            "canonical hybrid_fusion operator must contain exactly two child plans".to_string(),
        ));
    }

    let structured =
        execute_runtime_canonical_expr_node(db, &node.children[0], query.structured.as_ref())?;
    let vector_expr = QueryExpr::Vector(query.vector.clone());
    let vector = execute_runtime_canonical_expr_node(db, &node.children[1], &vector_expr)?;

    let mut structured_map = HashMap::new();
    let mut structured_rank = HashMap::new();
    for (index, record) in structured.iter().cloned().enumerate() {
        let key = runtime_record_identity_key(&record);
        structured_rank.insert(key.clone(), index);
        structured_map.insert(key, record);
    }

    let mut vector_map = HashMap::new();
    let mut vector_rank = HashMap::new();
    for (index, record) in vector.iter().cloned().enumerate() {
        let key = runtime_record_identity_key(&record);
        vector_rank.insert(key.clone(), index);
        vector_map.insert(key, record);
    }

    let ordered_keys = hybrid_candidate_keys(&structured_map, &vector_map, &query.fusion);

    let mut scored_records = Vec::new();
    for key in ordered_keys {
        let structured_record = structured_map.get(&key);
        let vector_record = vector_map.get(&key);
        let s_rank = structured_rank.get(&key).copied();
        let v_rank = vector_rank.get(&key).copied();
        let s_score = structured_record
            .as_ref()
            .map_or(0.0, |record| runtime_structured_score(record, s_rank));
        let v_score = vector_record
            .as_ref()
            .map_or(0.0, |r| runtime_vector_score(r));

        let score = match &query.fusion {
            FusionStrategy::Rerank { weight } => {
                if structured_record.is_none() {
                    continue;
                }
                ((1.0 - *weight as f64) * s_score) + ((*weight as f64) * v_score)
            }
            FusionStrategy::FilterThenSearch | FusionStrategy::SearchThenFilter => {
                if structured_record.is_none() || vector_record.is_none() {
                    continue;
                }
                v_score
            }
            FusionStrategy::Intersection => {
                if structured_record.is_none() || vector_record.is_none() {
                    continue;
                }
                (s_score + v_score) / 2.0
            }
            FusionStrategy::Union {
                structured_weight,
                vector_weight,
            } => ((*structured_weight as f64) * s_score) + ((*vector_weight as f64) * v_score),
            FusionStrategy::RRF { k } => {
                let mut total = 0.0;
                if let Some(rank) = s_rank {
                    total += 1.0 / (*k as f64 + rank as f64 + 1.0);
                }
                if let Some(rank) = v_rank {
                    total += 1.0 / (*k as f64 + rank as f64 + 1.0);
                }
                total
            }
        };

        let mut record = merge_hybrid_records(structured_record, vector_record);
        record.set("score", Value::Float(score));
        record.set("_score", Value::Float(score));
        record.set("final_score", Value::Float(score));
        record.set("hybrid_score", Value::Float(score));
        record.set(
            "structured_score",
            if structured_record.is_some() {
                Value::Float(s_score)
            } else {
                Value::Null
            },
        );
        record.set(
            "vector_score",
            if vector_record.is_some() {
                Value::Float(v_score)
            } else {
                Value::Null
            },
        );
        record.set(
            "vector_similarity",
            if vector_record.is_some() {
                Value::Float(v_score)
            } else {
                Value::Null
            },
        );
        record.set(
            "structured_rank",
            s_rank
                .map(|value| Value::UnsignedInteger(value as u64))
                .unwrap_or(Value::Null),
        );
        record.set(
            "vector_rank",
            v_rank
                .map(|value| Value::UnsignedInteger(value as u64))
                .unwrap_or(Value::Null),
        );
        scored_records.push((score, record));
    }

    scored_records.sort_by(|left, right| compare_runtime_ranked_records(&left.1, &right.1));
    Ok(scored_records
        .into_iter()
        .map(|(_, record)| record)
        .collect())
}
