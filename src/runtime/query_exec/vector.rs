//! Vector similarity search executor.
//!
//! Handles `Vector` query expressions — ANN search over a collection's
//! registered vector index with optional metadata pre/post-filters.
//! Split out of `query_exec.rs` to keep the main executor focused on
//! table-scan paths.
//!
//! Uses `use super::*;` to inherit the parent executor's imports.

use super::*;
use crate::storage::query::sql_lowering::effective_vector_filter;

pub(crate) fn execute_runtime_vector_query(
    db: &RedDB,
    query: &VectorQuery,
) -> RedDBResult<UnifiedResult> {
    let plan = CanonicalPlanner::new(db).build(&QueryExpr::Vector(query.clone()));
    let records = execute_runtime_canonical_vector_node(db, &plan.root, query)?;

    Ok(UnifiedResult {
        columns: collect_visible_columns(&records),
        records,
        stats: Default::default(),
        pre_serialized_json: None,
    })
}

pub(crate) fn execute_runtime_canonical_vector_node(
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
            if let Some(filter) = effective_vector_filter(query).as_ref() {
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

pub(crate) fn execute_runtime_canonical_vector_child(
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

pub(crate) fn runtime_vector_matches(
    db: &RedDB,
    query: &VectorQuery,
    vector: &[f32],
) -> RedDBResult<Vec<SimilarResult>> {
    let manager = db
        .store()
        .get_collection(&query.collection)
        .ok_or_else(|| RedDBError::NotFound(query.collection.clone()))?;

    if effective_vector_filter(query).is_none() {
        let mut results = db.similar(&query.collection, vector, manager.count().max(1));
        // Phase 1.2 MVCC universal: the ANN path (`db.similar`) bypasses
        // the general scan filter, so post-filter the top-K list here
        // against the connection's snapshot. Each `SimilarResult` owns
        // its entity, so visibility is cheap (no extra fetch).
        let snap_ctx = crate::runtime::impl_core::capture_current_snapshot();
        if snap_ctx.is_some() {
            results.retain(|r| {
                crate::runtime::impl_core::entity_visible_with_context(
                    snap_ctx.as_ref(),
                    &r.entity,
                )
            });
        }
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.entity_id.raw().cmp(&b.entity_id.raw()))
        });
        return Ok(results);
    }

    let snap_ctx = crate::runtime::impl_core::capture_current_snapshot();
    let mut results: Vec<SimilarResult> = manager
        .query_all(|entity| {
            crate::runtime::impl_core::entity_visible_with_context(snap_ctx.as_ref(), entity)
        })
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

pub(crate) fn runtime_vector_record_matches_filter(
    db: &RedDB,
    collection: &str,
    record: &UnifiedRecord,
    filter: &VectorMetadataFilter,
) -> bool {
    let entity_id = record
        .values
        .get("entity_id")
        .or_else(|| record.values.get("red_entity_id"))
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
