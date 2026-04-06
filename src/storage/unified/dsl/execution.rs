//! Query execution functions
//!
//! Execution logic for all query types.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use crate::storage::query::unified::ExecutionError;

use super::super::entity::{EntityData, EntityId, EntityKind, UnifiedEntity};
use super::super::store::UnifiedStore;
use super::builders::{
    CrossModalMatch, GraphQueryBuilder, GraphStartPoint, HybridQueryBuilder, JoinPhase, JoinStep,
    RefQueryBuilder, ScanQueryBuilder, TableQueryBuilder, TextSearchBuilder, ThreeWayJoinBuilder,
    VectorQueryBuilder,
};
use super::helpers::{apply_filters, calculate_entity_similarity, extract_searchable_text};
use super::types::{MatchComponents, QueryResult, ScoredMatch};

/// Execute a vector similarity query
pub fn execute_vector_query(
    query: VectorQueryBuilder,
    store: &Arc<UnifiedStore>,
) -> Result<QueryResult, ExecutionError> {
    let start = Instant::now();
    let mut matches = Vec::new();
    let mut scanned = 0;

    // Get collections to search
    let collections = query
        .collections
        .unwrap_or_else(|| store.list_collections());

    for col_name in &collections {
        if let Some(manager) = store.get_collection(col_name) {
            let entities = manager.query_all(|_| true);
            for entity in entities {
                scanned += 1;

                // Apply filters first
                if !apply_filters(&entity, &query.filters) {
                    continue;
                }

                // Calculate similarity
                let similarity =
                    calculate_entity_similarity(&entity, &query.vector, &query.embedding_slot);

                if similarity >= query.min_similarity {
                    matches.push(ScoredMatch {
                        entity,
                        score: similarity,
                        components: MatchComponents {
                            vector_similarity: Some(similarity),
                            ..Default::default()
                        },
                        path: None,
                    });
                }
            }
        }
    }

    // Sort by score
    matches.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    matches.truncate(query.k);

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
                        if let EntityKind::GraphNode { label: l, .. } = &entity.kind {
                            if l == label {
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
                            EntityKind::GraphNode { label, .. } => {
                                pattern.labels.is_empty() || pattern.labels.contains(label)
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
                    ..Default::default()
                },
                path: None,
            });
        }
    }

    // Sort by score
    matches.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if let Some(limit) = query.limit {
        matches.truncate(limit);
    }

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
                        filter_match: true,
                        ..Default::default()
                    },
                    path: None,
                });
            }
        }
    }

    // Apply limit/offset
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
                    components: Default::default(),
                    path: None,
                });
            }
        }
    }

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
            if current_id != query.source_id || query.include_source {
                if apply_filters(&entity, &query.filters) {
                    let score = 1.0 - (depth as f32 * 0.2);
                    matches.push(ScoredMatch {
                        entity: entity.clone(),
                        score,
                        components: MatchComponents {
                            hop_distance: Some(depth),
                            ..Default::default()
                        },
                        path: None,
                    });
                }
            }

            // Expand cross-refs via store index
            for (target_id, ref_type, _) in store.get_refs_from(current_id) {
                if ref_type == query.ref_type {
                    frontier.push((target_id, depth + 1));
                }
            }
        }
    }

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
                        components: Default::default(),
                        path: None,
                    });
                }
            }
        }
    }

    // Sort by score
    matches.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if let Some(limit) = query.limit {
        matches.truncate(limit);
    }

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
                    let matches = match (&entity.kind, &pattern.node_label) {
                        (EntityKind::GraphNode { label, .. }, Some(l)) => label == l,
                        (_, None) => true,
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
            ScoredMatch {
                entity,
                score,
                components: comps,
                path: None,
            }
        })
        .collect();

    // Sort by score
    matches.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if let Some(limit) = query.limit {
        matches.truncate(limit);
    }

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
            for col_name in store.list_collections() {
                if let Some(manager) = store.get_collection(&col_name) {
                    let entities = manager.query_all(|_| true);
                    let mut scored: Vec<(EntityId, UnifiedEntity, f32)> = Vec::new();

                    for entity in entities {
                        scanned += 1;
                        let sim = calculate_entity_similarity(&entity, vector, &None);
                        if sim > 0.0 {
                            scored.push((entity.id, entity, sim));
                        }
                    }

                    // Sort and take top k
                    scored
                        .sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
                    scored.truncate(k);

                    for (id, entity, sim) in scored {
                        results.insert(
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
            }
        }
        JoinPhase::GraphStart { ref node_id } => {
            // Start from a graph node
            for col_name in store.list_collections() {
                if let Some(manager) = store.get_collection(&col_name) {
                    let entities = manager.query_all(|e| {
                        matches!(&e.kind, EntityKind::GraphNode { label, .. } if label == node_id)
                            || e.id.to_string() == *node_id
                    });

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

                        for (target_id, _, target_collection) in store.get_refs_from(*id) {
                            if let Some(entity) = store.get(&target_collection, target_id) {
                                if seen.insert(entity.id) {
                                    scanned += 1;
                                    let entry = new_results.entry(entity.id).or_insert_with(|| {
                                        let mut path = current.path.clone();
                                        path.push(entity.id);
                                        CrossModalMatch {
                                            entity: entity.clone(),
                                            vector_score: current.vector_score,
                                            graph_score: current.graph_score
                                                + 1.0 / (*depth as f32 + 1.0),
                                            table_score: current.table_score,
                                            path,
                                        }
                                    });
                                    entry.graph_score = entry
                                        .graph_score
                                        .max(current.graph_score + 1.0 / (*depth as f32 + 1.0));
                                }
                            }
                        }

                        for (source_id, _, source_collection) in store.get_refs_to(*id) {
                            if let Some(entity) = store.get(&source_collection, source_id) {
                                if seen.insert(entity.id) {
                                    scanned += 1;
                                    let entry = new_results.entry(entity.id).or_insert_with(|| {
                                        let mut path = current.path.clone();
                                        path.push(entity.id);
                                        CrossModalMatch {
                                            entity: entity.clone(),
                                            vector_score: current.vector_score,
                                            graph_score: current.graph_score
                                                + 1.0 / (*depth as f32 + 1.0),
                                            table_score: current.table_score,
                                            path,
                                        }
                                    });
                                    entry.graph_score = entry
                                        .graph_score
                                        .max(current.graph_score + 1.0 / (*depth as f32 + 1.0));
                                }
                            }
                        }
                    }
                }

                // Merge new results
                for (id, m) in new_results {
                    results.entry(id).or_insert(m);
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
                                // Check if table entity references current entity
                                let matches = store
                                    .get_refs_from(table_entity.id)
                                    .iter()
                                    .any(|(target_id, _, _)| target_id == id)
                                    || on_field
                                        .as_ref()
                                        .map(|f| match &table_entity.data {
                                            EntityData::Row(row) => row
                                                .named
                                                .as_ref()
                                                .and_then(|n| n.get(f))
                                                .map(|v| v.to_string() == id.to_string())
                                                .unwrap_or(false),
                                            _ => false,
                                        })
                                        .unwrap_or(false);

                                if matches {
                                    let entry =
                                        new_results.entry(table_entity.id).or_insert_with(|| {
                                            CrossModalMatch {
                                                entity: table_entity.clone(),
                                                vector_score: current.vector_score,
                                                graph_score: current.graph_score,
                                                table_score: 1.0,
                                                path: current.path.clone(),
                                            }
                                        });
                                    entry.table_score = entry.table_score.max(1.0);
                                }
                            }
                        }
                    }

                    // Merge new results
                    for (id, m) in new_results {
                        results.entry(id).or_insert(m);
                    }
                }
            }
            JoinStep::VectorExpand { k } => {
                // Find vectors similar to current entities' embeddings
                let mut new_results: HashMap<EntityId, CrossModalMatch> = HashMap::new();

                for id in &current_ids {
                    if let Some(current) = results.get(id) {
                        // Get embeddings from current entity
                        for emb in &current.entity.embeddings {
                            for col_name in store.list_collections() {
                                if let Some(manager) = store.get_collection(&col_name) {
                                    let entities = manager.query_all(|_| true);
                                    let mut scored: Vec<(EntityId, UnifiedEntity, f32)> =
                                        Vec::new();

                                    for entity in entities {
                                        if entity.id != *id {
                                            scanned += 1;
                                            let sim = calculate_entity_similarity(
                                                &entity,
                                                &emb.vector,
                                                &None,
                                            );
                                            if sim > 0.3 {
                                                scored.push((entity.id, entity, sim));
                                            }
                                        }
                                    }

                                    scored.sort_by(|a, b| {
                                        b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal)
                                    });
                                    scored.truncate(*k);

                                    for (eid, entity, sim) in scored {
                                        let entry = new_results.entry(eid).or_insert_with(|| {
                                            CrossModalMatch {
                                                entity,
                                                vector_score: sim,
                                                graph_score: current.graph_score,
                                                table_score: current.table_score,
                                                path: current.path.clone(),
                                            }
                                        });
                                        entry.vector_score = entry.vector_score.max(sim);
                                    }
                                }
                            }
                        }
                    }
                }

                // Merge new results
                for (id, m) in new_results {
                    results.entry(id).or_insert(m);
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
                    graph_match: if m.graph_score > 0.0 {
                        Some(m.graph_score)
                    } else {
                        None
                    },
                    filter_match: true,
                    hop_distance: if m.path.is_empty() {
                        None
                    } else {
                        Some(m.path.len() as u32)
                    },
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

    // Sort by score
    matches.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Apply limit
    if let Some(limit) = query.limit {
        matches.truncate(limit);
    }

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
