use super::*;

pub(super) fn execute_runtime_table_query(db: &RedDB, query: &TableQuery) -> RedDBResult<UnifiedResult> {
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
        "sort" | "entity_sort" => {
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
            records.sort_by(|left, right| {
                runtime_record_rank_score(right)
                    .partial_cmp(&runtime_record_rank_score(left))
                    .unwrap_or(Ordering::Equal)
            });
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
        "hybrid_score",
        "final_score",
        "_score",
        "vector_score",
        "structured_score",
        "text_relevance",
    ]
    .into_iter()
    .find_map(|field| record.values.get(field).and_then(runtime_value_number))
    .unwrap_or(0.0)
}

pub(super) fn execute_runtime_join_query(db: &RedDB, query: &JoinQuery) -> RedDBResult<UnifiedResult> {
    let records = execute_runtime_canonical_join_query(db, query)?;
    let columns = collect_visible_columns(&records);

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
    if node.operator != "join" {
        return Err(RedDBError::Query(format!(
            "expected canonical join operator, got {}",
            node.operator
        )));
    }

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

    let left_table_name = left_query.table.as_str();
    let left_table_alias = left_query.alias.as_deref().unwrap_or(left_table_name);
    let left_records = execute_runtime_canonical_expr_node(db, &node.children[0], query.left.as_ref())?;

    let (right_records, right_table_name, right_table_alias) = match query.right.as_ref() {
        QueryExpr::Graph(_) | QueryExpr::Path(_) => (
            execute_runtime_canonical_expr_node(db, &node.children[1], query.right.as_ref())?,
            None,
            None,
        ),
        QueryExpr::Table(table) => (
            execute_runtime_canonical_expr_node(db, &node.children[1], query.right.as_ref())?,
            Some(table.table.as_str()),
            Some(table.alias.as_deref().unwrap_or(table.table.as_str())),
        ),
        other => {
            return Err(RedDBError::Query(format!(
                "runtime joins do not yet support right-side {} expressions",
                query_expr_name(other)
            )))
        }
    };

    match join_strategy {
        CanonicalJoinStrategy::IndexedNestedLoop => {
            execute_runtime_indexed_join(
                left_query,
                &left_records,
                Some(left_table_name),
                Some(left_table_alias),
                &left_join_field,
                &right_records,
                right_table_name,
                right_table_alias,
                &right_join_field,
                join_type,
            )
        }
        CanonicalJoinStrategy::NestedLoop => execute_runtime_full_scan_join(
            left_query,
            &left_records,
            Some(left_table_name),
            Some(left_table_alias),
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
            Some(left_table_name),
            Some(left_table_alias),
            &left_join_field,
            &right_records,
            right_table_name,
            right_table_alias,
            &right_join_field,
            join_type,
        ),
    }
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
        other => Err(RedDBError::Query(format!(
            "canonical join execution does not yet support {} child expressions",
            query_expr_name(other)
        ))),
    }
}

pub(super) fn execute_runtime_vector_query(db: &RedDB, query: &VectorQuery) -> RedDBResult<UnifiedResult> {
    let vector = resolve_runtime_vector_source(db, &query.query_vector)?;
    let min_score = query.threshold.unwrap_or(f32::MIN);
    let matches = runtime_vector_matches(db, query, &vector)?
        .into_iter()
        .filter(|item| item.score >= min_score)
        .collect::<Vec<_>>();

    let records = matches
        .into_iter()
        .map(runtime_vector_record_from_match)
        .collect();

    Ok(UnifiedResult {
        columns: vec![
            "entity_id".to_string(),
            "score".to_string(),
            "collection".to_string(),
            "content".to_string(),
            "dimension".to_string(),
        ],
        records,
        stats: Default::default(),
    })
}

pub(super) fn runtime_vector_matches(
    db: &RedDB,
    query: &VectorQuery,
    vector: &[f32],
) -> RedDBResult<Vec<SimilarResult>> {
    if query.filter.is_none() {
        return Ok(db.similar(&query.collection, vector, query.k.max(1)));
    }

    let manager = db
        .store()
        .get_collection(&query.collection)
        .ok_or_else(|| RedDBError::NotFound(query.collection.clone()))?;
    let filter = query.filter.as_ref().unwrap();

    let mut results: Vec<SimilarResult> = manager
        .query_all(|_| true)
        .into_iter()
        .filter(|entity| runtime_vector_entity_matches_filter(db, &query.collection, entity, filter))
        .filter_map(|entity| {
            let score = runtime_entity_vector_similarity(&entity, vector);
            (score > 0.0).then_some(SimilarResult {
                entity_id: entity.id,
                score,
                entity,
            })
        })
        .collect();

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
    });
    results.truncate(query.k.max(1));
    Ok(results)
}

pub(super) fn execute_runtime_hybrid_query(db: &RedDB, query: &HybridQuery) -> RedDBResult<UnifiedResult> {
    let structured = execute_runtime_expr(db, query.structured.as_ref())?;
    let vector = execute_runtime_vector_query(db, &query.vector)?;

    let mut structured_map = HashMap::new();
    let mut structured_rank = HashMap::new();
    for (index, record) in structured.records.iter().cloned().enumerate() {
        if let Some(key) = runtime_record_identity_key(&record) {
            structured_rank.insert(key.clone(), index);
            structured_map.insert(key, record);
        }
    }

    let mut vector_map = HashMap::new();
    let mut vector_rank = HashMap::new();
    for (index, record) in vector.records.iter().cloned().enumerate() {
        if let Some(key) = runtime_record_identity_key(&record) {
            vector_rank.insert(key.clone(), index);
            vector_map.insert(key, record);
        }
    }

    let ordered_keys = hybrid_candidate_keys(
        &structured_map,
        &vector_map,
        &query.fusion,
    );

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
            .map_or(0.0, runtime_vector_score);

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

    scored_records.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(Ordering::Equal)
    });

    let mut records: Vec<UnifiedRecord> = scored_records.into_iter().map(|(_, record)| record).collect();
    if let Some(limit) = query.limit {
        records.truncate(limit);
    }

    Ok(UnifiedResult {
        columns: collect_visible_columns(&records),
        records,
        stats: Default::default(),
    })
}

