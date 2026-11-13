//! Hybrid ranking and one-hop expansion share one authorized snapshot.
use super::*;
use crate::storage::unified::dsl::{calculate_entity_similarity, extract_searchable_text};

struct Candidate {
    collection: String,
    entity: UnifiedEntity,
    text: f32,
    vector: f32,
}

fn matches_node(entity: &UnifiedEntity, pattern: &RuntimeGraphPattern) -> bool {
    matches!(&entity.kind, EntityKind::GraphNode(node)
        if pattern.node_label.as_ref().is_none_or(|label| *label == node.label)
            && pattern.node_type.as_ref().is_none_or(|kind| *kind == node.node_type))
}

impl RedDBRuntime {
    pub fn search_hybrid(
        &self,
        vector: Option<Vec<f32>>,
        query: Option<String>,
        k: Option<usize>,
        collections: Option<Vec<String>>,
        entity_types: Option<Vec<String>>,
        capabilities: Option<Vec<String>>,
        graph_pattern: Option<RuntimeGraphPattern>,
        filters: Vec<RuntimeFilter>,
        weights: Option<RuntimeQueryWeights>,
        min_score: Option<f32>,
        limit: Option<usize>,
    ) -> RedDBResult<DslQueryResult> {
        let started = std::time::Instant::now();
        let query = query
            .map(|text| text.trim().to_lowercase())
            .filter(|text| !text.is_empty());
        if vector.is_none() && query.is_none() {
            return Err(RedDBError::Query(
                "field 'query' or 'vector' is required for hybrid search".into(),
            ));
        }
        if vector.as_ref().is_some_and(|values| {
            values.is_empty() || values.iter().any(|value| !value.is_finite())
        }) {
            return Err(RedDBError::Query(
                "hybrid search vector must contain finite values".into(),
            ));
        }
        let weights = weights.unwrap_or(RuntimeQueryWeights {
            vector: 0.5,
            graph: 0.3,
            filter: 0.2,
        });
        if [weights.vector, weights.graph, weights.filter]
            .iter()
            .any(|weight| !weight.is_finite() || *weight < 0.0)
        {
            return Err(RedDBError::Query(
                "hybrid search weights must be finite and nonnegative".into(),
            ));
        }
        let filters = filters
            .into_iter()
            .map(runtime_filter_to_dsl)
            .collect::<RedDBResult<Vec<_>>>()?;
        let limit = limit.or(k).unwrap_or(10).max(1);
        let min_score = min_score
            .filter(|score| score.is_finite())
            .unwrap_or(0.0)
            .max(0.0);
        let relation_labels = graph_pattern
            .as_ref()
            .map(|pattern| pattern.edge_labels.as_slice())
            .unwrap_or_default();
        let relations = !relation_labels.is_empty();
        let explicit_scope = collections.as_ref().is_some_and(|names| !names.is_empty());
        let collections =
            runtime_search_collections(&self.inner.db, collections).unwrap_or_default();
        let (snapshot, _retention) = self.native_read_snapshot()?;
        let _snapshot = super::execution_context::CurrentSnapshotGuard::install(snapshot.clone());
        self.refresh_memory_accounting();
        let store = self.db().store();
        let mut candidates = Vec::new();
        let mut reservations = Vec::new();
        let mut policies = HashMap::new();
        let terms: Vec<_> = query
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .collect();
        for collection in collections {
            if super::impl_ddl::is_system_schema_name(&collection) || collection.starts_with("red_")
            {
                continue;
            }
            if let Err(error) =
                self.check_collection_privilege(reddb_rql::ast::PolicyAction::Select, &collection)
            {
                if explicit_scope {
                    return Err(error);
                }
                continue;
            }
            let Some(manager) = store.get_collection(&collection) else {
                continue;
            };
            let (_scan_memory, read_bytes_max) = self.reserve_index_scan(&manager)?;
            let mut rows = Vec::new();
            super::function_budget::try_scan_result(
                |visit| manager.try_for_each_entity(read_bytes_max, visit),
                |entity| {
                    if super::execution_context::entity_visible_with_context(
                        Some(&snapshot),
                        entity,
                    ) {
                        // Own candidate payloads, ranking copies and adjacency keys. Reserve
                        // from the sampled ledger here; never evict under a storage lock.
                        let bytes = crate::storage::unified::memory_size::entity_bytes(entity)
                            .saturating_mul(3)
                            .saturating_add(collection.len().saturating_mul(4))
                            .saturating_add(1024);
                        let reservation = self
                            .try_reserve_query_buffer_from_sample(bytes)
                            .ok_or_else(|| {
                                RedDBError::Query(
                                    "hybrid search exceeds the query memory budget".into(),
                                )
                            })?;
                        reservations.push(reservation);
                        rows.push(entity.clone());
                    }
                    Ok(true)
                },
            )?;
            // RLS may read metadata or query storage; evaluate outside segment locks.
            for entity in rows {
                super::function_budget::charge(1)?;
                if !self.search_entity_allowed(&collection, &entity, Some(&snapshot), &mut policies)
                {
                    continue;
                }
                let text = if terms.is_empty() {
                    0.0
                } else {
                    let body = extract_searchable_text(&entity).to_lowercase();
                    terms.iter().filter(|term| body.contains(**term)).count() as f32
                        / terms.len() as f32
                };
                let similarity = vector.as_ref().map_or(0.0, |vector| {
                    calculate_entity_similarity(&entity, vector, &None)
                });
                candidates.push(Candidate {
                    collection: collection.clone(),
                    entity,
                    text,
                    vector: similarity,
                });
            }
        }
        let mut matches: HashMap<EntityId, ScoredMatch> = HashMap::new();
        for candidate in &candidates {
            let graph = if !relations
                && vector.is_some()
                && graph_pattern
                    .as_ref()
                    .is_some_and(|pattern| matches_node(&candidate.entity, pattern))
            {
                1.0
            } else {
                0.0
            };
            let score = if vector.is_none() && !relations {
                candidate.text
            } else {
                candidate.vector * weights.vector
                    + candidate.text * weights.filter
                    + graph * weights.graph
            };
            if score > 0.0 {
                matches.insert(
                    candidate.entity.id,
                    ScoredMatch {
                        entity: candidate.entity.clone(),
                        score,
                        path: None,
                        components: MatchComponents {
                            vector_similarity: (candidate.vector > 0.0).then_some(candidate.vector),
                            text_relevance: (candidate.text > 0.0).then_some(candidate.text),
                            graph_match: (graph > 0.0).then_some(graph),
                            ..Default::default()
                        },
                    },
                );
            }
        }
        if relations {
            expand_relations(
                &candidates,
                relation_labels,
                k.unwrap_or(limit).max(1),
                weights.graph,
                &mut matches,
            )?;
        }
        let mut result = DslQueryResult {
            matches: matches
                .into_values()
                .filter(|item| {
                    item.score >= min_score
                        && apply_filters(&item.entity, &filters)
                        && graph_pattern.as_ref().is_none_or(|pattern| {
                            // Legacy vector patterns boost nodes; relation patterns and
                            // text-only patterns constrain the returned node types.
                            (!relations && vector.is_some()) || matches_node(&item.entity, pattern)
                        })
                })
                .collect(),
            scanned: candidates.len(),
            execution_time_us: started.elapsed().as_micros() as u64,
            explanation: if relations {
                "Hybrid search with authorized one-hop relations"
            } else {
                "Hybrid multi-modal query"
            }
            .into(),
        };
        for item in &mut result.matches {
            item.components.filter_match = true;
            item.components.structured_match = Some(1.0);
            item.components.final_score = Some(item.score);
        }
        normalize_runtime_dsl_result_scores(&mut result);
        runtime_filter_dsl_result(&mut result, entity_types, capabilities);
        result.matches.truncate(limit);
        Ok(result)
    }
}

fn expand_relations(
    candidates: &[Candidate],
    labels: &[String],
    seed_limit: usize,
    weight: f32,
    matches: &mut HashMap<EntityId, ScoredMatch>,
) -> RedDBResult<()> {
    let mut seeds: Vec<_> = candidates
        .iter()
        .filter(|candidate| matches!(candidate.entity.kind, EntityKind::GraphNode(_)))
        .map(|candidate| (candidate.entity.id, candidate.text.max(candidate.vector)))
        .filter(|(_, score)| *score > 0.0)
        .collect();
    seeds.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    seeds.truncate(seed_limit);
    let seeds: HashMap<_, _> = seeds.into_iter().collect();
    if seeds.is_empty() || weight == 0.0 {
        return Ok(());
    }
    let mut nodes: HashMap<(String, String), Vec<usize>> = HashMap::new();
    for (index, candidate) in candidates.iter().enumerate() {
        if let EntityKind::GraphNode(node) = &candidate.entity.kind {
            for alias in [
                node.label.clone(),
                candidate.entity.id.raw().to_string(),
                candidate.entity.logical_id().raw().to_string(),
            ] {
                let entries = nodes
                    .entry((candidate.collection.clone(), alias))
                    .or_default();
                if entries.last() != Some(&index) {
                    entries.push(index);
                }
            }
        }
    }
    for candidate in candidates {
        let EntityKind::GraphEdge(edge) = &candidate.entity.kind else {
            continue;
        };
        if !labels.contains(&edge.label) {
            continue;
        }
        let Some(from) = nodes.get(&(candidate.collection.clone(), edge.from_node.clone())) else {
            continue;
        };
        let Some(to) = nodes.get(&(candidate.collection.clone(), edge.to_node.clone())) else {
            continue;
        };
        for &source in from {
            for &target in to {
                super::function_budget::charge(1)?;
                for (source, target) in [(source, target), (target, source)] {
                    let source = &candidates[source].entity;
                    let target = &candidates[target].entity;
                    if source.id == target.id {
                        continue;
                    }
                    let Some(&strength) = seeds.get(&source.id) else {
                        continue;
                    };
                    let path = vec![source.id, candidate.entity.id, target.id];
                    let item = matches.entry(target.id).or_insert_with(|| ScoredMatch {
                        entity: target.clone(),
                        score: 0.0,
                        components: MatchComponents::default(),
                        path: None,
                    });
                    let previous = item.components.graph_match.unwrap_or(0.0);
                    if strength > previous
                        || (strength == previous
                            && item.path.as_ref().is_none_or(|old| path < *old))
                    {
                        item.score += (strength - previous) * weight;
                        item.components.graph_match = Some(strength);
                        item.components.hop_distance = Some(1);
                        item.path = Some(path);
                    }
                }
            }
        }
    }
    Ok(())
}
