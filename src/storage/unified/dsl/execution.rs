//! Query execution functions
//!
//! Execution logic for all query types.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use crate::storage::engine::{DistanceMetric, HnswConfig, HnswIndex};
use crate::storage::query::unified::ExecutionError;

use super::super::entity::{EntityData, EntityId, EntityKind, RefType, UnifiedEntity};
use super::super::store::UnifiedStore;
use super::builders::{
    CrossModalMatch, GraphQueryBuilder, GraphStartPoint, HybridQueryBuilder, JoinPhase, JoinStep,
    RefQueryBuilder, ScanQueryBuilder, TableQueryBuilder, TextSearchBuilder, ThreeWayJoinBuilder,
    TraversalDirection, VectorQueryBuilder,
};
use super::cross_modal::{
    cross_modal_entity_vectors, cross_modal_graph_neighbors, cross_modal_graph_node_matches_ref,
    cross_modal_lookup_graph_nodes_by_ref, cross_modal_ref_matches_edge_label,
    cross_modal_value_matches_entity, merge_cross_modal_match,
};
use super::helpers::{apply_filters, calculate_entity_similarity, extract_searchable_text};
use super::types::{MatchComponents, QueryResult, ScoredMatch};

fn normalize_query_matches(matches: &mut Vec<ScoredMatch>, limit: Option<usize>) {
    for item in matches.iter_mut() {
        if let Some(final_score) = item
            .components
            .final_score
            .filter(|score| score.is_finite())
        {
            item.score = final_score;
        } else {
            item.components.final_score = Some(item.score);
        }
    }

    matches.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.entity.id.raw().cmp(&right.entity.id.raw()))
    });

    if let Some(limit) = limit {
        matches.truncate(limit);
    }
}

/// Minimum number of dense vectors in a collection before we build an HNSW
/// index instead of scanning brute-force.
const HNSW_MIN_VECTORS: usize = 100;

/// Execute a vector similarity query.
///
/// For collections with >= `HNSW_MIN_VECTORS` dense vectors whose dimension
/// matches the query vector an HNSW index is built on-the-fly for fast
/// approximate nearest-neighbor search.  Smaller or mixed-type collections
/// fall back to an exact brute-force scan.
pub fn execute_vector_query(
    query: VectorQueryBuilder,
    store: &Arc<UnifiedStore>,
) -> Result<QueryResult, ExecutionError> {
    let start = Instant::now();
    let mut matches = Vec::new();
    let mut scanned = 0;

    let collections = query
        .collections
        .unwrap_or_else(|| store.list_collections());

    let query_dim = query.vector.len();
    let has_filters = !query.filters.is_empty();
    let has_embedding_slot = query.embedding_slot.is_some();

    for col_name in &collections {
        if let Some(manager) = store.get_collection(col_name) {
            let entities = manager.query_all(|_| true);

            // Pre-filter entities BEFORE HNSW decision — this allows HNSW
            // to be used even when filters are present, as long as the
            // filtered set is still large enough.
            let entities: Vec<_> = if has_filters {
                entities
                    .into_iter()
                    .filter(|e| apply_filters(e, &query.filters))
                    .collect()
            } else {
                entities
            };

            let use_hnsw = !has_embedding_slot && entities.len() >= HNSW_MIN_VECTORS;

            if use_hnsw {
                // Collect dense vectors with matching dimension.
                let id_vec_pairs: Vec<(u64, Vec<f32>)> = entities
                    .iter()
                    .filter_map(|e| match &e.data {
                        EntityData::Vector(v)
                            if !v.dense.is_empty() && v.dense.len() == query_dim =>
                        {
                            Some((e.id.raw(), v.dense.clone()))
                        }
                        _ => None,
                    })
                    .collect();

                if id_vec_pairs.len() >= HNSW_MIN_VECTORS {
                    scanned += id_vec_pairs.len();

                    let config = HnswConfig::with_m(16)
                        .with_metric(DistanceMetric::Cosine)
                        .with_ef_construction(100)
                        .with_ef_search(50);
                    let mut hnsw = HnswIndex::new(query_dim, config);
                    for (id, vec) in &id_vec_pairs {
                        hnsw.insert_with_id(*id, vec.clone());
                    }

                    let results = hnsw.search(&query.vector, query.k);
                    for dr in &results {
                        let entity_id = EntityId::new(dr.id);
                        if let Some(entity) = store.get(col_name, entity_id) {
                            let similarity = (1.0 - dr.distance).max(0.0);
                            if similarity >= query.min_similarity {
                                matches.push(ScoredMatch {
                                    entity,
                                    score: similarity,
                                    components: MatchComponents {
                                        vector_similarity: Some(similarity),
                                        final_score: Some(similarity),
                                        ..Default::default()
                                    },
                                    path: None,
                                });
                            }
                        }
                    }
                    continue; // Done with this collection via HNSW.
                }
            }

            // Brute-force fallback (entities already pre-filtered above).
            for entity in entities {
                scanned += 1;

                let similarity =
                    calculate_entity_similarity(&entity, &query.vector, &query.embedding_slot);

                if similarity >= query.min_similarity {
                    matches.push(ScoredMatch {
                        entity,
                        score: similarity,
                        components: MatchComponents {
                            vector_similarity: Some(similarity),
                            final_score: Some(similarity),
                            ..Default::default()
                        },
                        path: None,
                    });
                }
            }
        }
    }

    normalize_query_matches(&mut matches, Some(query.k));

    Ok(QueryResult {
        matches,
        scanned,
        execution_time_us: start.elapsed().as_micros() as u64,
        explanation: format!("Vector search in {} collections", collections.len()),
    })
}

/// Execute a graph traversal query
pub fn execute_graph_query(
    query: GraphQueryBuilder,
    store: &Arc<UnifiedStore>,
) -> Result<QueryResult, ExecutionError> {
    let start = Instant::now();
    let mut matches = Vec::new();
    let mut scanned = 0;

    // Find starting nodes
    let start_entities: Vec<UnifiedEntity> = match &query.start {
        GraphStartPoint::EntityId(id) => {
            if let Some((_, entity)) = store.get_any(*id) {
                vec![entity]
            } else {
                vec![]
            }
        }
        GraphStartPoint::NodeLabel(label) => {
            // Scan for matching labels
            let mut found = Vec::new();
            for col in store.list_collections() {
                if let Some(manager) = store.get_collection(&col) {
                    for entity in manager.query_all(|_| true) {
                        scanned += 1;
                        if let EntityKind::GraphNode(ref node) = &entity.kind {
                            if &node.label == label {
                                found.push(entity);
                            }
                        }
                    }
                }
            }
            found
        }
        GraphStartPoint::Pattern(pattern) => {
            let mut found = Vec::new();
            for col in store.list_collections() {
                if let Some(manager) = store.get_collection(&col) {
                    for entity in manager.query_all(|_| true) {
                        scanned += 1;
                        let matches_pattern = match &entity.kind {
                            EntityKind::GraphNode(ref node) => {
                                pattern.labels.is_empty() || pattern.labels.contains(&node.label)
                            }
                            _ => false,
                        };
                        if matches_pattern {
                            found.push(entity);
                        }
                    }
                }
            }
            found
        }
    };

    // For now, just return the starting entities with applied filters
    // (Full graph traversal would follow cross-refs)
    for entity in start_entities {
        if apply_filters(&entity, &query.filters) {
            let score = if let Some(ref vec) = query.ranking_vector {
                calculate_entity_similarity(&entity, vec, &None)
            } else {
                1.0
            };

            matches.push(ScoredMatch {
                entity,
                score,
                components: MatchComponents {
                    graph_match: Some(1.0),
                    vector_similarity: query.ranking_vector.as_ref().map(|_| score),
                    final_score: Some(score),
                    ..Default::default()
                },
                path: None,
            });
        }
    }

    normalize_query_matches(&mut matches, query.limit);

    Ok(QueryResult {
        matches,
        scanned,
        execution_time_us: start.elapsed().as_micros() as u64,
        explanation: "Graph traversal query".to_string(),
    })
}

/// Execute a table query
pub fn execute_table_query(
    query: TableQueryBuilder,
    store: &Arc<UnifiedStore>,
) -> Result<QueryResult, ExecutionError> {
    let start = Instant::now();
    let mut matches = Vec::new();
    let mut scanned = 0;

    if let Some(manager) = store.get_collection(&query.collection) {
        let entities = manager.query_all(|_| true);
        for entity in entities {
            scanned += 1;
            if apply_filters(&entity, &query.filters) {
                matches.push(ScoredMatch {
                    entity,
                    score: 1.0,
                    components: MatchComponents {
                        structured_match: Some(1.0),
                        filter_match: true,
                        final_score: Some(1.0),
                        ..Default::default()
                    },
                    path: None,
                });
            }
        }
    }

    normalize_query_matches(&mut matches, None);

    // Apply offset/limit
    matches = matches.into_iter().skip(query.offset).collect();
    if let Some(limit) = query.limit {
        matches.truncate(limit);
    }

    Ok(QueryResult {
        matches,
        scanned,
        execution_time_us: start.elapsed().as_micros() as u64,
        explanation: format!("Table query on {}", query.collection),
    })
}

/// Execute a scan query
pub fn execute_scan_query(
    query: ScanQueryBuilder,
    store: &Arc<UnifiedStore>,
) -> Result<QueryResult, ExecutionError> {
    let start = Instant::now();
    let mut matches = Vec::new();
    let mut scanned = 0;

    if let Some(manager) = store.get_collection(&query.collection) {
        let entities = manager.query_all(|_| true);
        for entity in entities {
            scanned += 1;
            if apply_filters(&entity, &query.filters) {
                matches.push(ScoredMatch {
                    entity,
                    score: 1.0,
                    components: MatchComponents {
                        structured_match: Some(1.0),
                        final_score: Some(1.0),
                        ..Default::default()
                    },
                    path: None,
                });
            }
        }
    }

    normalize_query_matches(&mut matches, None);

    if let Some(limit) = query.limit {
        matches.truncate(limit);
    }

    Ok(QueryResult {
        matches,
        scanned,
        execution_time_us: start.elapsed().as_micros() as u64,
        explanation: format!("Full scan of {}", query.collection),
    })
}

/// Execute a reference query
pub fn execute_ref_query(
    query: RefQueryBuilder,
    store: &Arc<UnifiedStore>,
) -> Result<QueryResult, ExecutionError> {
    let start = Instant::now();
    let mut matches = Vec::new();
    let mut visited = HashSet::new();
    let mut frontier = vec![(query.source_id, 0u32)];

    while let Some((current_id, depth)) = frontier.pop() {
        if depth > query.max_depth || visited.contains(&current_id) {
            continue;
        }
        visited.insert(current_id);

        if let Some((_, entity)) = store.get_any(current_id) {
            // Skip source unless include_source is set
            if (current_id != query.source_id || query.include_source)
                && apply_filters(&entity, &query.filters)
            {
                let score = 1.0 - (depth as f32 * 0.2);
                matches.push(ScoredMatch {
                    entity: entity.clone(),
                    score,
                    components: MatchComponents {
                        structured_match: Some(score),
                        hop_distance: Some(depth),
                        final_score: Some(score),
                        ..Default::default()
                    },
                    path: None,
                });
            }

            // Expand cross-refs via store index
            for (target_id, ref_type, _) in store.get_refs_from(current_id) {
                if ref_type == query.ref_type {
                    frontier.push((target_id, depth + 1));
                }
            }
        }
    }

    normalize_query_matches(&mut matches, None);

    Ok(QueryResult {
        matches,
        scanned: visited.len(),
        execution_time_us: start.elapsed().as_micros() as u64,
        explanation: format!("Cross-ref traversal from {:?}", query.source_id),
    })
}

/// Execute a text search query
pub fn execute_text_query(
    query: TextSearchBuilder,
    store: &Arc<UnifiedStore>,
) -> Result<QueryResult, ExecutionError> {
    let start = Instant::now();
    let mut matches = Vec::new();
    let mut scanned = 0;

    let query_lower = query.query.to_lowercase();
    let query_terms: Vec<&str> = query_lower.split_whitespace().collect();

    let collections = query
        .collections
        .unwrap_or_else(|| store.list_collections());

    for col_name in &collections {
        if let Some(manager) = store.get_collection(col_name) {
            let entities = manager.query_all(|_| true);
            for entity in entities {
                scanned += 1;

                // Extract searchable text from entity
                let text = extract_searchable_text(&entity);
                let text_lower = text.to_lowercase();

                // Calculate match score
                let match_count = query_terms
                    .iter()
                    .filter(|term| text_lower.contains(*term))
                    .count();

                if match_count > 0 {
                    let score = match_count as f32 / query_terms.len() as f32;
                    matches.push(ScoredMatch {
                        entity,
                        score,
                        components: MatchComponents {
                            text_relevance: Some(score),
                            final_score: Some(score),
                            ..Default::default()
                        },
                        path: None,
                    });
                }
            }
        }
    }

    normalize_query_matches(&mut matches, query.limit);

    Ok(QueryResult {
        matches,
        scanned,
        execution_time_us: start.elapsed().as_micros() as u64,
        explanation: format!("Text search for '{}'", query.query),
    })
}

/// Execute a hybrid query
pub fn execute_hybrid_query(
    query: HybridQueryBuilder,
    store: &Arc<UnifiedStore>,
) -> Result<QueryResult, ExecutionError> {
    let start = Instant::now();
    let mut entity_scores: HashMap<EntityId, (UnifiedEntity, f32, MatchComponents)> =
        HashMap::new();
    let mut scanned = 0;

    let collections = query
        .collections
        .unwrap_or_else(|| store.list_collections());

    // 1. Vector search if specified
    if let Some((ref vec, k)) = query.vector_query {
        for col_name in &collections {
            if let Some(manager) = store.get_collection(col_name) {
                let entities = manager.query_all(|_| true);
                for entity in entities {
                    scanned += 1;
                    let sim = calculate_entity_similarity(&entity, vec, &None);
                    if sim > 0.0 {
                        let entry = entity_scores
                            .entry(entity.id)
                            .or_insert_with(|| (entity.clone(), 0.0, MatchComponents::default()));
                        entry.1 += sim * query.weights.vector;
                        entry.2.vector_similarity = Some(sim);
                    }
                }
            }
        }
    }

    // 2. Graph pattern if specified
    if let Some(ref pattern) = query.graph_pattern {
        for col_name in &collections {
            if let Some(manager) = store.get_collection(col_name) {
                let entities = manager.query_all(|_| true);
                for entity in entities {
                    let matches = match (&entity.kind, &pattern.node_label, &pattern.node_type) {
                        (EntityKind::GraphNode(ref node), label_filter, type_filter) => {
                            label_filter.as_ref().is_none_or(|l| &node.label == l)
                                && type_filter.as_ref().is_none_or(|t| &node.node_type == t)
                        }
                        _ => false,
                    };

                    if matches {
                        let entry = entity_scores
                            .entry(entity.id)
                            .or_insert_with(|| (entity.clone(), 0.0, MatchComponents::default()));
                        entry.1 += query.weights.graph;
                        entry.2.graph_match = Some(1.0);
                    }
                }
            }
        }
    }

    // 3. Apply filters
    let mut matches: Vec<ScoredMatch> = entity_scores
        .into_iter()
        .filter(|(_, (entity, score, _))| {
            *score >= query.min_score && apply_filters(entity, &query.filters)
        })
        .map(|(_, (entity, score, components))| {
            let mut comps = components;
            comps.filter_match = true;
            comps.structured_match = Some(1.0);
            comps.final_score = Some(score);
            ScoredMatch {
                entity,
                score,
                components: comps,
                path: None,
            }
        })
        .collect();

    normalize_query_matches(&mut matches, query.limit);

    Ok(QueryResult {
        matches,
        scanned,
        execution_time_us: start.elapsed().as_micros() as u64,
        explanation: "Hybrid multi-modal query".to_string(),
    })
}

/// Execute a three-way cross-modal join
pub fn execute_three_way_join(
    query: ThreeWayJoinBuilder,
    store: &Arc<UnifiedStore>,
) -> Result<QueryResult, ExecutionError> {
    let start_time = Instant::now();
    let mut scanned = 0;

    // Track entity scores and origins across all modalities
    let mut results: HashMap<EntityId, CrossModalMatch> = HashMap::new();

    // Get starting entities based on start phase
    let start_phase = query
        .start
        .ok_or_else(|| ExecutionError::new("Three-way join requires a start phase"))?;

    match start_phase {
        JoinPhase::VectorStart { ref vector, k } => {
            // Find similar vectors
            let mut scored: Vec<(EntityId, UnifiedEntity, f32)> = Vec::new();
            for col_name in store.list_collections() {
                if let Some(manager) = store.get_collection(&col_name) {
                    let entities = manager.query_all(|_| true);

                    for entity in entities {
                        scanned += 1;
                        let sim = calculate_entity_similarity(&entity, vector, &None);
                        if sim > 0.0 {
                            scored.push((entity.id, entity, sim));
                        }
                    }
                }
            }

            scored.sort_by(|a, b| {
                b.2.partial_cmp(&a.2)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.raw().cmp(&b.0.raw()))
            });
            if k > 0 {
                scored.truncate(k);
            }

            for (id, entity, sim) in scored {
                merge_cross_modal_match(
                    &mut results,
                    id,
                    CrossModalMatch {
                        entity,
                        vector_score: sim,
                        graph_score: 0.0,
                        table_score: 0.0,
                        path: Vec::new(),
                    },
                );
            }
        }
        JoinPhase::GraphStart { ref node_id } => {
            // Start from a graph node
            for col_name in store.list_collections() {
                if let Some(manager) = store.get_collection(&col_name) {
                    let entities =
                        manager.query_all(|e| cross_modal_graph_node_matches_ref(e, node_id));

                    for entity in entities {
                        scanned += 1;
                        let eid = entity.id;
                        results.insert(
                            eid,
                            CrossModalMatch {
                                entity,
                                vector_score: 0.0,
                                graph_score: 1.0,
                                table_score: 0.0,
                                path: vec![eid],
                            },
                        );
                    }
                }
            }
        }
        JoinPhase::TableStart { ref table } => {
            // Start from table rows
            if let Some(manager) = store.get_collection(table) {
                let entities =
                    manager.query_all(|e| matches!(&e.kind, EntityKind::TableRow { .. }));

                for entity in entities {
                    scanned += 1;
                    results.insert(
                        entity.id,
                        CrossModalMatch {
                            entity,
                            vector_score: 0.0,
                            graph_score: 0.0,
                            table_score: 1.0,
                            path: Vec::new(),
                        },
                    );
                }
            }
        }
    }

    // Execute pipeline steps
    for step in &query.pipeline {
        let current_ids: Vec<EntityId> = results.keys().cloned().collect();

        match step {
            JoinStep::Traverse {
                edge_label,
                depth,
                direction,
            } => {
                // Graph traversal from current results
                let mut new_results: HashMap<EntityId, CrossModalMatch> = HashMap::new();

                for id in &current_ids {
                    if let Some(current) = results.get(id) {
                        let mut seen = HashSet::new();
                        seen.insert(*id);
                        let mut frontier = VecDeque::from([(
                            *id,
                            current.path.clone(),
                            0u32,
                            current.graph_score,
                        )]);

                        while let Some((frontier_id, frontier_path, hops, frontier_graph_score)) =
                            frontier.pop_front()
                        {
                            if hops >= *depth {
                                continue;
                            }

                            let frontier_is_graph_node = store
                                .get_any(frontier_id)
                                .map(|(_, entity)| matches!(entity.kind, EntityKind::GraphNode(_)))
                                .unwrap_or(false);

                            if frontier_is_graph_node {
                                for entity in cross_modal_graph_neighbors(
                                    store,
                                    frontier_id,
                                    direction,
                                    edge_label.as_deref(),
                                ) {
                                    if !seen.insert(entity.id) {
                                        continue;
                                    }

                                    scanned += 1;
                                    let mut path = if frontier_path.is_empty() {
                                        vec![*id]
                                    } else {
                                        frontier_path.clone()
                                    };
                                    path.push(entity.id);
                                    let graph_score =
                                        frontier_graph_score + 1.0 / (hops as f32 + 2.0);
                                    merge_cross_modal_match(
                                        &mut new_results,
                                        entity.id,
                                        CrossModalMatch {
                                            entity: entity.clone(),
                                            vector_score: current.vector_score,
                                            graph_score,
                                            table_score: current.table_score,
                                            path: path.clone(),
                                        },
                                    );
                                    frontier.push_back((entity.id, path, hops + 1, graph_score));
                                }
                                continue;
                            }

                            if matches!(
                                direction,
                                TraversalDirection::Out | TraversalDirection::Both
                            ) {
                                for (target_id, ref_type, target_collection) in
                                    store.get_refs_from(frontier_id)
                                {
                                    if !cross_modal_ref_matches_edge_label(
                                        ref_type,
                                        edge_label.as_deref(),
                                    ) || !seen.insert(target_id)
                                    {
                                        continue;
                                    }

                                    if let Some(entity) = store.get(&target_collection, target_id) {
                                        scanned += 1;
                                        let mut path = if frontier_path.is_empty() {
                                            vec![*id]
                                        } else {
                                            frontier_path.clone()
                                        };
                                        path.push(entity.id);
                                        let graph_score =
                                            frontier_graph_score + 1.0 / (hops as f32 + 2.0);
                                        merge_cross_modal_match(
                                            &mut new_results,
                                            entity.id,
                                            CrossModalMatch {
                                                entity: entity.clone(),
                                                vector_score: current.vector_score,
                                                graph_score,
                                                table_score: current.table_score,
                                                path: path.clone(),
                                            },
                                        );
                                        frontier.push_back((
                                            entity.id,
                                            path,
                                            hops + 1,
                                            graph_score,
                                        ));
                                    }
                                }
                            }

                            if matches!(
                                direction,
                                TraversalDirection::In | TraversalDirection::Both
                            ) {
                                for (source_id, ref_type, source_collection) in
                                    store.get_refs_to(frontier_id)
                                {
                                    if !cross_modal_ref_matches_edge_label(
                                        ref_type,
                                        edge_label.as_deref(),
                                    ) || !seen.insert(source_id)
                                    {
                                        continue;
                                    }

                                    if let Some(entity) = store.get(&source_collection, source_id) {
                                        scanned += 1;
                                        let mut path = if frontier_path.is_empty() {
                                            vec![*id]
                                        } else {
                                            frontier_path.clone()
                                        };
                                        path.push(entity.id);
                                        let graph_score =
                                            frontier_graph_score + 1.0 / (hops as f32 + 2.0);
                                        merge_cross_modal_match(
                                            &mut new_results,
                                            entity.id,
                                            CrossModalMatch {
                                                entity: entity.clone(),
                                                vector_score: current.vector_score,
                                                graph_score,
                                                table_score: current.table_score,
                                                path: path.clone(),
                                            },
                                        );
                                        frontier.push_back((
                                            entity.id,
                                            path,
                                            hops + 1,
                                            graph_score,
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }

                // Merge new results
                for (id, m) in new_results {
                    merge_cross_modal_match(&mut results, id, m);
                }
            }
            JoinStep::JoinTable { table, on_field } => {
                // Join with table
                if let Some(manager) = store.get_collection(table) {
                    let mut new_results: HashMap<EntityId, CrossModalMatch> = HashMap::new();

                    let table_entities: Vec<_> =
                        manager.query_all(|e| matches!(&e.kind, EntityKind::TableRow { .. }));

                    for id in &current_ids {
                        if let Some(current) = results.get(id) {
                            // Find matching table rows
                            for table_entity in &table_entities {
                                scanned += 1;
                                // Check if the relationship exists in either direction, or via
                                // explicit field match when requested.
                                let row_references_current = store
                                    .get_refs_from(table_entity.id)
                                    .iter()
                                    .any(|(target_id, _, _)| target_id == id);
                                let current_references_row = store
                                    .get_refs_from(*id)
                                    .iter()
                                    .any(|(target_id, _, _)| target_id == &table_entity.id);
                                let field_matches_current = on_field
                                    .as_ref()
                                    .map(|f| match &table_entity.data {
                                        EntityData::Row(row) => row
                                            .named
                                            .as_ref()
                                            .and_then(|n| n.get(f))
                                            .map(|v| {
                                                cross_modal_value_matches_entity(v, &current.entity)
                                            })
                                            .unwrap_or(false),
                                        _ => false,
                                    })
                                    .unwrap_or(false);
                                let matches = row_references_current
                                    || current_references_row
                                    || field_matches_current;

                                if matches {
                                    merge_cross_modal_match(
                                        &mut new_results,
                                        table_entity.id,
                                        CrossModalMatch {
                                            entity: table_entity.clone(),
                                            vector_score: current.vector_score,
                                            graph_score: current.graph_score,
                                            table_score: 1.0,
                                            path: current.path.clone(),
                                        },
                                    );
                                }
                            }
                        }
                    }

                    // Merge new results
                    for (id, m) in new_results {
                        merge_cross_modal_match(&mut results, id, m);
                    }
                }
            }
            JoinStep::VectorExpand { k } => {
                // Find vectors similar to current entities' embeddings
                let mut new_results: HashMap<EntityId, CrossModalMatch> = HashMap::new();

                for id in &current_ids {
                    if let Some(current) = results.get(id) {
                        // Get vectors from the current entity, whether they come from
                        // explicit embedding slots or from a native vector entity.
                        for vector in cross_modal_entity_vectors(&current.entity) {
                            let mut scored: Vec<(EntityId, UnifiedEntity, f32)> = Vec::new();
                            for col_name in store.list_collections() {
                                if let Some(manager) = store.get_collection(&col_name) {
                                    let entities = manager.query_all(|_| true);

                                    for entity in entities {
                                        if entity.id != *id {
                                            scanned += 1;
                                            let sim = calculate_entity_similarity(
                                                &entity, &vector, &None,
                                            );
                                            if sim.is_finite() && sim > 0.0 {
                                                scored.push((entity.id, entity, sim));
                                            }
                                        }
                                    }
                                }
                            }

                            scored.sort_by(|a, b| {
                                b.2.partial_cmp(&a.2)
                                    .unwrap_or(std::cmp::Ordering::Equal)
                                    .then_with(|| a.0.raw().cmp(&b.0.raw()))
                            });
                            if *k > 0 {
                                scored.truncate(*k);
                            }

                            for (eid, entity, sim) in scored {
                                merge_cross_modal_match(
                                    &mut new_results,
                                    eid,
                                    CrossModalMatch {
                                        entity,
                                        vector_score: sim,
                                        graph_score: current.graph_score,
                                        table_score: current.table_score,
                                        path: current.path.clone(),
                                    },
                                );
                            }
                        }
                    }
                }

                // Merge new results
                for (id, m) in new_results {
                    merge_cross_modal_match(&mut results, id, m);
                }
            }
        }
    }

    // Apply filters and compute final scores
    let mut matches: Vec<ScoredMatch> = results
        .into_iter()
        .filter(|(_, m)| apply_filters(&m.entity, &query.filters))
        .map(|(_, m)| {
            let score = m.vector_score * query.weights.vector
                + m.graph_score * query.weights.graph
                + m.table_score * query.weights.table;

            ScoredMatch {
                entity: m.entity,
                score,
                components: MatchComponents {
                    vector_similarity: if m.vector_score > 0.0 {
                        Some(m.vector_score)
                    } else {
                        None
                    },
                    text_relevance: None,
                    graph_match: if m.graph_score > 0.0 {
                        Some(m.graph_score)
                    } else {
                        None
                    },
                    structured_match: if m.table_score > 0.0 {
                        Some(m.table_score)
                    } else {
                        None
                    },
                    filter_match: true,
                    hop_distance: if m.path.is_empty() {
                        None
                    } else {
                        Some(m.path.len().saturating_sub(1) as u32)
                    },
                    final_score: Some(score),
                },
                path: if m.path.is_empty() {
                    None
                } else {
                    Some(m.path)
                },
            }
        })
        .filter(|m| m.score >= query.min_score)
        .collect();

    normalize_query_matches(&mut matches, query.limit);

    Ok(QueryResult {
        matches,
        scanned,
        execution_time_us: start_time.elapsed().as_micros() as u64,
        explanation: format!(
            "Three-way cross-modal JOIN: {} pipeline steps",
            query.pipeline.len()
        ),
    })
}
