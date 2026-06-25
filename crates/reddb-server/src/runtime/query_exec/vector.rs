//! Vector similarity search executor.
//!
//! Handles `Vector` query expressions — ANN search over a collection's
//! registered vector index with optional metadata pre/post-filters.
//! Split out of `query_exec.rs` to keep the main executor focused on
//! table-scan paths.
//!
//! Uses `use super::*;` to inherit the parent executor's imports.

use super::*;
use crate::runtime::vector_index::{BruteForceVectorIndex, VectorIndexEntry};
use crate::storage::engine::distance::DistanceMetric;
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
                let metric = runtime_vector_metric(db, query);
                records.retain(|record| {
                    runtime_vector_record_within_threshold(record, metric, threshold)
                });
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
    validate_vector_query_shape(db, query, vector)?;
    let metric = runtime_vector_metric(db, query);
    let manager = db
        .store()
        .get_collection(&query.collection)
        .ok_or_else(|| RedDBError::NotFound(query.collection.clone()))?;

    // Issue #693 — `vector.turbo` SEARCH goes through the
    // TurboQuantIndex, which dispatches scoring through
    // `select_scorer()` (scalar / AVX2 / AVX-512BW / NEON, runtime
    // selected). Legacy `vector` collections continue on the
    // brute-force path below.
    if let Some(state) = db.turbo_state(&query.collection) {
        // Issue #673 — wait briefly for the background rebuild to
        // finish. If the timeout fires, return a structured NOT_READY
        // signal instead of silently blocking or returning empty.
        // The bounded wait lets fast rebuilds satisfy the caller
        // transparently while slow ones surface as actionable errors.
        let wait_ms = std::env::var("REDDB_TURBO_SEARCH_READY_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(500);
        if !state.wait_until_ready(std::time::Duration::from_millis(wait_ms)) {
            // Fall back to a synchronous populate from the calling
            // thread. The rebuild may have raced with the wait
            // (cheap collections finish before the worker is even
            // scheduled); the explicit populate doubles as a
            // last-chance unblock so a SEARCH after restart on a
            // single-vector collection never spuriously 503s.
            state.ensure_populated(&db.store(), &query.collection);
            if !state.is_ready() {
                return Err(RedDBError::InvalidOperation(format!(
                    "NOT_READY: vector.turbo collection '{}' is rebuilding (turbo index recovery); retry shortly",
                    query.collection
                )));
            }
        }
        // TurboQuant returns *approximate* (quantised) scores; on small or
        // low-dimensional collections the quantisation collapses the scores
        // and the approximate order is wrong (#1372). Over-fetch a generous
        // candidate set from the index, then re-rank with full-precision
        // exact distances so metric ordering is correct. The index still
        // prunes the candidate set on large collections.
        const RERANK_OVERFETCH: usize = 32;
        let k = query.k.max(1);
        let collection_count = manager.count().max(1);
        let search_k = if effective_vector_filter(query).is_some() {
            collection_count
        } else {
            k.saturating_mul(RERANK_OVERFETCH).min(collection_count)
        };
        let raw = {
            let index = state.index.lock();
            index.search(vector, search_k, metric)
        };
        let mut results = Vec::with_capacity(raw.len());
        for hit in raw {
            let Some(entity) = db.store().get(&query.collection, hit.entity_id) else {
                continue;
            };
            // The TurboQuant index is append-only and never prunes
            // deleted/superseded vectors (#673/#688 own removal). Post-
            // filter every candidate through the current-snapshot
            // visibility gate — which hides any tombstoned/superseded
            // physical version (`xmax != 0`) even in autocommit (where
            // there is no captured snapshot context) — so a deleted or
            // version-superseded vector never reaches the results.
            if !crate::runtime::impl_core::entity_visible_under_current_snapshot(&entity) {
                continue;
            }
            // Exact re-rank against the stored full-precision vector rather
            // than trusting the quantised approximate score.
            let (score, distance) = match &entity.data {
                EntityData::Vector(data) => {
                    let raw_distance =
                        crate::storage::engine::distance::distance(vector, &data.dense, metric);
                    let score = match metric {
                        DistanceMetric::Cosine => 1.0 - raw_distance,
                        DistanceMetric::InnerProduct | DistanceMetric::L2 => -raw_distance,
                    };
                    (score, raw_distance)
                }
                _ => {
                    let distance = match metric {
                        DistanceMetric::Cosine => 1.0 - hit.score,
                        DistanceMetric::InnerProduct | DistanceMetric::L2 => -hit.score,
                    };
                    (hit.score, distance)
                }
            };
            if let Some(threshold) = query.threshold {
                let pass = match metric {
                    DistanceMetric::L2 => distance <= threshold,
                    DistanceMetric::Cosine | DistanceMetric::InnerProduct => score >= threshold,
                };
                if !pass {
                    continue;
                }
            }
            results.push(SimilarResult {
                entity_id: hit.entity_id,
                score,
                distance,
                entity,
            });
        }
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.entity_id.raw().cmp(&b.entity_id.raw()))
        });
        results.truncate(k);
        return Ok(results);
    }

    let snap_ctx = crate::runtime::impl_core::capture_current_snapshot();
    let mut index = BruteForceVectorIndex::default();
    let search_k = if effective_vector_filter(query).is_some() {
        manager.count().max(1)
    } else {
        query.k.max(1)
    };

    for entity in manager.query_all(|entity| {
        // `query_all` may fan out across worker threads that do not
        // inherit the dispatch thread's snapshot thread-local, so we
        // pass the captured snapshot context explicitly. In autocommit
        // there is no captured context (`None`); the table-scan paths
        // would treat that as "always visible", but a deleted/superseded
        // vector carries `xmax != 0` and must be hidden — mirror the
        // autocommit rule of `entity_visible_under_current_snapshot`.
        if snap_ctx.is_none() {
            return entity.xmax == 0
                && !crate::runtime::ai::moderation::entity_moderation_hidden(entity);
        }
        crate::runtime::impl_core::entity_visible_with_context(snap_ctx.as_ref(), entity)
    }) {
        if let EntityData::Vector(data) = &entity.data {
            index.upsert(VectorIndexEntry {
                entity_id: entity.id,
                vector: data.dense.clone(),
                entity,
            });
        }
    }

    let out = index.search(vector, search_k, metric, query.threshold);
    eprintln!(
        "VECDBG matches metric={metric:?} k={search_k}: {:?}",
        out.iter()
            .map(|m| (m.entity_id.raw(), m.score))
            .collect::<Vec<_>>()
    );
    Ok(out)
}

pub(crate) fn runtime_vector_record_matches_filter(
    db: &RedDB,
    collection: &str,
    record: &UnifiedRecord,
    filter: &VectorMetadataFilter,
) -> bool {
    let entity_id = record
        .get("entity_id")
        .or_else(|| record.get("rid"))
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

pub(crate) fn runtime_vector_metric(db: &RedDB, query: &VectorQuery) -> DistanceMetric {
    query
        .metric
        .or_else(|| {
            db.collection_contract(&query.collection)
                .and_then(|contract| contract.vector_metric)
        })
        .unwrap_or(DistanceMetric::Cosine)
}

fn validate_vector_query_shape(db: &RedDB, query: &VectorQuery, vector: &[f32]) -> RedDBResult<()> {
    if let Some(expected) = db
        .collection_contract(&query.collection)
        .and_then(|contract| contract.vector_dimension)
    {
        if expected != vector.len() {
            return Err(RedDBError::Query(format!(
                "vector dimension mismatch for collection '{}': expected {}, got {}",
                query.collection,
                expected,
                vector.len()
            )));
        }
    }
    Ok(())
}

fn runtime_vector_record_within_threshold(
    record: &UnifiedRecord,
    metric: DistanceMetric,
    threshold: f32,
) -> bool {
    match metric {
        DistanceMetric::L2 => record
            .get("distance")
            .and_then(runtime_value_number)
            .is_some_and(|distance| distance <= threshold as f64),
        DistanceMetric::Cosine | DistanceMetric::InnerProduct => {
            runtime_record_rank_score(record) >= threshold as f64
        }
    }
}
