//! Vector similarity search executor.
//!
//! Handles `Vector` query expressions — exact or explicitly approximate search over a collection's
//! registered vector index with optional metadata pre/post-filters.
//! Split out of `query_exec.rs` to keep the main executor focused on
//! table-scan paths.
//!
//! Uses `use super::*;` to inherit the parent executor's imports.

use super::*;
use crate::runtime::vector_index::VectorTopK;
use crate::storage::engine::distance::DistanceMetric;
use crate::storage::query::unified::{QueryStats, VectorQueryStats};
use reddb_rql::ast::VectorSearchMode;
use reddb_rql::sql_lowering::effective_vector_filter;

pub(crate) fn execute_runtime_vector_query(
    runtime: &RedDBRuntime,
    query: &VectorQuery,
) -> RedDBResult<UnifiedResult> {
    let db = &runtime.inner.db;
    let started = std::time::Instant::now();
    let mut vector_stats = VectorQueryStats::default();
    let plan = CanonicalPlanner::new(db).build(&QueryExpr::Vector(query.clone()));
    let records =
        execute_runtime_canonical_vector_node(runtime, &plan.root, query, &mut vector_stats)?;

    vector_stats.rows_returned = records.len() as u64;
    Ok(UnifiedResult {
        columns: collect_visible_columns(&records),
        records,
        stats: QueryStats {
            rows_scanned: vector_stats.candidates_examined,
            exec_time_us: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
            vector: Some(vector_stats),
            ..Default::default()
        },
        pre_serialized_json: None,
    })
}

pub(crate) fn execute_runtime_canonical_vector_node(
    runtime: &RedDBRuntime,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    query: &VectorQuery,
    stats: &mut VectorQueryStats,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let db = &runtime.inner.db;
    let started = std::time::Instant::now();
    let input_rows;
    let records: RedDBResult<Vec<UnifiedRecord>> = match node.operator.as_str() {
        "vector_turbo_search" | "vector_exact_scan" => {
            let vector = resolve_runtime_vector_source(runtime, &query.query_vector)?;
            let before = stats.candidates_examined;
            let matches = runtime_vector_matches(runtime, query, &vector, stats)?;
            input_rows = stats.candidates_examined - before;
            Ok(matches
                .into_iter()
                .map(runtime_vector_record_from_match)
                .collect())
        }
        "metadata_filter" => {
            let mut records = execute_runtime_canonical_vector_child(runtime, node, query, stats)?;
            input_rows = records.len() as u64;
            if let Some(filter) = effective_vector_filter(query).as_ref() {
                records.retain(|record| {
                    runtime_vector_record_matches_filter(db, &query.collection, record, filter)
                });
            }
            Ok(records)
        }
        "similarity_threshold" => {
            let mut records = execute_runtime_canonical_vector_child(runtime, node, query, stats)?;
            input_rows = records.len() as u64;
            if let Some(threshold) = query.threshold {
                let metric = runtime_vector_metric(db, query);
                records.retain(|record| {
                    runtime_vector_record_within_threshold(record, metric, threshold)
                });
            }
            Ok(records)
        }
        "topk" => {
            let mut records = execute_runtime_canonical_vector_child(runtime, node, query, stats)?;
            input_rows = records.len() as u64;
            records.sort_by(compare_runtime_ranked_records);
            Ok(records.into_iter().take(query.k.max(1)).collect())
        }
        "projection" => {
            let records = execute_runtime_canonical_vector_child(runtime, node, query, stats)?;
            input_rows = records.len() as u64;
            Ok(records)
        }
        other => {
            return Err(RedDBError::Query(format!(
                "unsupported canonical vector operator {other}"
            )))
        }
    };
    let records = records?;
    stats
        .operators
        .push(crate::storage::query::unified::VectorOperatorStats {
            operator: node.operator.clone(),
            input_rows,
            output_rows: records.len() as u64,
            inclusive_time_us: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
        });
    Ok(records)
}

pub(crate) fn execute_runtime_canonical_vector_child(
    runtime: &RedDBRuntime,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    query: &VectorQuery,
    stats: &mut VectorQueryStats,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let child = node.children.first().ok_or_else(|| {
        RedDBError::Query(format!(
            "canonical vector operator {} is missing its child plan",
            node.operator
        ))
    })?;
    execute_runtime_canonical_vector_node(runtime, child, query, stats)
}

pub(crate) fn runtime_vector_matches(
    runtime: &RedDBRuntime,
    query: &VectorQuery,
    vector: &[f32],
    stats: &mut VectorQueryStats,
) -> RedDBResult<Vec<SimilarResult>> {
    let db = &runtime.inner.db;
    validate_vector_query_shape(db, query, vector)?;
    let metric = runtime_vector_metric(db, query);
    let rls_enabled = runtime.is_rls_enabled(&query.collection);
    let mut rls_cache = HashMap::new();
    let manager = db
        .store()
        .get_collection(&query.collection)
        .ok_or_else(|| RedDBError::NotFound(query.collection.clone()))?;

    // Issue #693 — `vector.turbo` SEARCH goes through the
    // TurboQuantIndex, which dispatches scoring through
    // `select_scorer()` (scalar / AVX2 / AVX-512BW / NEON, runtime
    // selected). Legacy `vector` collections continue on the
    // brute-force path below.
    stats.mode_requested = match query.mode {
        VectorSearchMode::Exact => "exact",
        VectorSearchMode::Approximate => "approximate",
    }
    .to_string();
    let turbo = (query.mode == VectorSearchMode::Approximate)
        .then(|| db.turbo_state(&query.collection))
        .flatten();
    if let Some(state) = turbo {
        stats.mode_executed = "approximate".to_string();
        stats.access_path = "vector_turbo_search".to_string();
        stats.index_used = true;
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
        // exact distances. Packed scoring still visits the entire collection;
        // only the full-precision reranking candidate set is reduced.
        const RERANK_OVERFETCH: usize = 32;
        let k = query.k.max(1);
        let collection_count = manager.count().max(1);
        // An RLS predicate can reject every normally overfetched candidate.
        // Consider the whole index before authorizing and exact ranking;
        // limiting first would lose lower-scoring records the caller may read.
        let search_k = if rls_enabled || effective_vector_filter(query).is_some() {
            collection_count
        } else {
            k.saturating_mul(RERANK_OVERFETCH).min(collection_count)
        };
        let raw = {
            let index = state.index.lock();
            stats.approximate_distance_evaluations = index.len() as u64;
            if crate::runtime::function_budget::active() {
                index.search_checked(
                    vector,
                    search_k,
                    metric,
                    crate::runtime::function_budget::charge,
                )?
            } else {
                index.search(vector, search_k, metric)
            }
        };
        let mut top = VectorTopK::new(k);
        let filter = effective_vector_filter(query);
        for hit in raw {
            crate::runtime::function_budget::charge(1)?;
            stats.candidates_examined += 1;
            let Some(entity) = db.store().get(&query.collection, hit.entity_id) else {
                stats.visibility_rejected += 1;
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
                stats.visibility_rejected += 1;
                continue;
            }
            if rls_enabled
                && !runtime.search_entity_rls_allowed(&query.collection, &entity, &mut rls_cache)
            {
                stats.rls_rejected += 1;
                continue;
            }
            if let Some(filter) = filter.as_ref() {
                if !runtime_vector_entity_matches_filter(db, &query.collection, entity.id, filter) {
                    stats.metadata_rejected += 1;
                    continue;
                }
            }
            // Exact re-rank against the stored full-precision vector rather
            // than trusting the quantised approximate score.
            let (score, distance) = match &entity.data {
                EntityData::Vector(data) => {
                    stats.exact_distance_evaluations += 1;
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
            top.consider(&entity, score, distance);
        }
        stats.peak_topk_entries = top.len() as u64;
        return Ok(top.finish());
    }

    stats.access_path = "vector_exact_scan".to_string();
    stats.mode_executed = "exact".to_string();
    if query.mode == VectorSearchMode::Approximate {
        stats.fallback_reason = Some("no operational approximate index".to_string());
    }
    let snapshot = crate::runtime::impl_core::capture_current_snapshot();
    let filter = effective_vector_filter(query);
    let mut top = VectorTopK::new(query.k.max(1));
    let mut consider = |entity: &UnifiedEntity| {
        stats.candidates_examined += 1;
        if (snapshot.is_none() && entity.xmax != 0)
            || !crate::runtime::impl_core::entity_visible_with_context(snapshot.as_ref(), entity)
        {
            stats.visibility_rejected += 1;
            return;
        }
        if rls_enabled
            && !runtime.search_entity_rls_allowed(&query.collection, entity, &mut rls_cache)
        {
            stats.rls_rejected += 1;
            return;
        }
        if filter.as_ref().is_some_and(|filter| {
            !runtime_vector_entity_matches_filter(db, &query.collection, entity.id, filter)
        }) {
            stats.metadata_rejected += 1;
            return;
        }
        let EntityData::Vector(data) = &entity.data else {
            return;
        };
        if data.dense.len() != vector.len() {
            return;
        }
        stats.exact_distance_evaluations += 1;
        let distance = crate::storage::engine::distance::distance(vector, &data.dense, metric);
        let score = match metric {
            DistanceMetric::Cosine => 1.0 - distance,
            DistanceMetric::InnerProduct | DistanceMetric::L2 => -distance,
        };
        if query.threshold.is_some_and(|threshold| match metric {
            DistanceMetric::L2 => distance > threshold,
            DistanceMetric::Cosine | DistanceMetric::InnerProduct => score < threshold,
        }) {
            return;
        }
        top.consider(entity, score, distance);
    };
    // Keep distance calculation and top-k payload cloning outside segment
    // locks. Borrowed scoring held the growing segment for the entire scan
    // and regressed concurrent writes. The ID list is O(N); payload copies
    // remain bounded to one batch. Predicates may also re-enter storage.
    let mut ids = Vec::new();
    crate::runtime::function_budget::scan(
        |visit| manager.scan_for_each(snapshot.as_ref(), visit),
        |entity| {
            ids.push(entity.id);
            true
        },
    )?;
    for batch in ids.chunks(256) {
        crate::runtime::function_budget::charge(0)?;
        for entity in manager.get_many(batch).into_iter().flatten() {
            crate::runtime::function_budget::charge(1)?;
            consider(&entity);
        }
    }
    stats.peak_topk_entries = top.len() as u64;
    Ok(top.finish())
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
    runtime_metadata_matches_vector_filter(&metadata, filter)
}

fn runtime_vector_entity_matches_filter(
    db: &RedDB,
    collection: &str,
    entity_id: EntityId,
    filter: &VectorMetadataFilter,
) -> bool {
    let metadata = db
        .store()
        .get_metadata(collection, entity_id)
        .unwrap_or_default();
    runtime_metadata_matches_vector_filter(&metadata, filter)
}

fn runtime_metadata_matches_vector_filter(
    metadata: &Metadata,
    filter: &VectorMetadataFilter,
) -> bool {
    match filter {
        VectorMetadataFilter::GeoRadius {
            key,
            center_lat,
            center_lon,
            radius_km,
        } => metadata
            .get(key)
            .and_then(runtime_metadata_geo_point)
            .is_some_and(|(lat, lon)| {
                crate::geo::haversine_km(*center_lat, *center_lon, lat, lon) <= *radius_km
            }),
        VectorMetadataFilter::And(filters) => filters
            .iter()
            .all(|filter| runtime_metadata_matches_vector_filter(metadata, filter)),
        VectorMetadataFilter::Or(filters) => filters
            .iter()
            .any(|filter| runtime_metadata_matches_vector_filter(metadata, filter)),
        VectorMetadataFilter::Not(inner) => {
            !runtime_metadata_matches_vector_filter(metadata, inner)
        }
        _ => {
            let entry = runtime_metadata_entry(metadata);
            filter.matches(&entry)
        }
    }
}

fn runtime_metadata_geo_point(value: &UnifiedMetadataValue) -> Option<(f64, f64)> {
    let fields = match value {
        UnifiedMetadataValue::Geo { lat, lon } => vec![
            ("lat".to_string(), Value::Float(*lat)),
            ("lon".to_string(), Value::Float(*lon)),
        ],
        UnifiedMetadataValue::Object(object) => object
            .iter()
            .filter_map(|(key, value)| {
                runtime_metadata_geo_field_value(value).map(|value| (key.clone(), value))
            })
            .collect(),
        _ => return None,
    };
    crate::geo::recognize_geo_fields(|key| {
        fields
            .iter()
            .find_map(|(field, value)| (field == key).then_some(value))
    })
}

fn runtime_metadata_geo_field_value(value: &UnifiedMetadataValue) -> Option<Value> {
    match value {
        UnifiedMetadataValue::Int(value) => Some(Value::Integer(*value)),
        UnifiedMetadataValue::Float(value) => Some(Value::Float(*value)),
        _ => None,
    }
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
