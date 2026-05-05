//! Hybrid search executor.
//!
//! Fuses vector similarity with text / filter predicates via the
//! canonical hybrid plan. Split out of `query_exec.rs` so the vector
//! and hybrid paths live next to each other.
//!
//! Uses `use super::*;` to inherit the parent executor's imports.

use super::*;

pub(crate) fn execute_runtime_hybrid_query(
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
        pre_serialized_json: None,
    })
}

pub(crate) fn execute_runtime_canonical_hybrid_node(
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

pub(crate) fn execute_runtime_canonical_hybrid_child(
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

pub(crate) fn execute_runtime_canonical_hybrid_fusion(
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
