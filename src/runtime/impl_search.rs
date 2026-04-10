use super::*;
use crate::application::SearchContextInput;
use crate::storage::unified::context_index::{entity_tokens_for_search, tokenize_query};

impl RedDBRuntime {
    pub fn explain_query(&self, query: &str) -> RedDBResult<RuntimeQueryExplain> {
        let mode = detect_mode(query);
        if matches!(mode, QueryMode::Unknown) {
            return Err(RedDBError::Query("unable to detect query mode".to_string()));
        }

        let expr = parse_multi(query).map_err(|err| RedDBError::Query(err.to_string()))?;
        let statement = query_expr_name(&expr);
        let mut planner = QueryPlanner::new();
        let plan = planner.plan(expr.clone());
        let cardinality = CostEstimator::new().estimate_cardinality(&plan.optimized);

        let is_universal = match &expr {
            QueryExpr::Table(t) => is_universal_query_source(&t.table),
            _ => false,
        };
        Ok(RuntimeQueryExplain {
            query: query.to_string(),
            mode,
            statement,
            is_universal,
            plan_cost: plan.cost,
            estimated_rows: cardinality.rows,
            estimated_selectivity: cardinality.selectivity,
            estimated_confidence: cardinality.confidence,
            passes_applied: plan.passes_applied,
            logical_plan: CanonicalPlanner::new(&self.inner.db).build(&plan.optimized),
        })
    }

    pub fn search_similar(
        &self,
        collection: &str,
        vector: &[f32],
        k: usize,
        min_score: f32,
    ) -> RedDBResult<Vec<SimilarResult>> {
        let mut results = self.inner.db.similar(collection, vector, k.max(1));
        if results.is_empty() && self.inner.db.store().get_collection(collection).is_none() {
            return Err(RedDBError::NotFound(collection.to_string()));
        }
        results.retain(|result| result.score >= min_score);
        results.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.entity_id.raw().cmp(&right.entity_id.raw()))
        });
        Ok(results)
    }

    pub fn search_ivf(
        &self,
        collection: &str,
        vector: &[f32],
        k: usize,
        n_lists: usize,
        n_probes: Option<usize>,
    ) -> RedDBResult<RuntimeIvfSearchResult> {
        let store = self.inner.db.store();
        let manager = store
            .get_collection(collection)
            .ok_or_else(|| RedDBError::NotFound(collection.to_string()))?;

        let vectors: Vec<(u64, Vec<f32>)> = manager
            .query_all(|_| true)
            .into_iter()
            .filter_map(|entity| match &entity.data {
                EntityData::Vector(data) if !data.dense.is_empty() => {
                    Some((entity.id.raw(), data.dense.clone()))
                }
                _ => None,
            })
            .collect();

        if vectors.is_empty() {
            return Err(RedDBError::Query(format!(
                "collection '{collection}' does not contain vector entities"
            )));
        }

        let dimension = vectors[0].1.len();
        if vector.len() != dimension {
            return Err(RedDBError::Query(format!(
                "query vector dimension mismatch: expected {dimension}, got {}",
                vector.len()
            )));
        }

        let consistent: Vec<(u64, Vec<f32>)> = vectors
            .into_iter()
            .filter(|(_, item)| item.len() == dimension)
            .collect();
        if consistent.is_empty() {
            return Err(RedDBError::Query(format!(
                "collection '{collection}' does not contain consistent vector dimensions"
            )));
        }

        let probes = n_probes.unwrap_or_else(|| (n_lists.max(1) / 10).max(1));
        let mut ivf = IvfIndex::new(IvfConfig::new(dimension, n_lists.max(1)).with_probes(probes));
        let training_vectors: Vec<Vec<f32>> =
            consistent.iter().map(|(_, item)| item.clone()).collect();
        ivf.train(&training_vectors);
        ivf.add_batch_with_ids(consistent);

        let stats = ivf.stats();
        let mut matches: Vec<_> = ivf
            .search_with_probes(vector, k.max(1), probes)
            .into_iter()
            .map(|result| RuntimeIvfMatch {
                entity_id: result.id,
                distance: result.distance,
                entity: self.inner.db.get(EntityId::new(result.id)),
            })
            .collect();
        matches.sort_by(|left, right| {
            left.distance
                .partial_cmp(&right.distance)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.entity_id.cmp(&right.entity_id))
        });

        Ok(RuntimeIvfSearchResult {
            collection: collection.to_string(),
            k: k.max(1),
            n_lists: stats.n_lists,
            n_probes: probes,
            stats,
            matches,
        })
    }

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
        let query = query.and_then(|query| {
            let trimmed = query.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });
        let collection_scope = runtime_search_collections(&self.inner.db, collections);
        if vector.is_none() && query.is_none() {
            return Err(RedDBError::Query(
                "field 'query' or 'vector' is required for hybrid search".to_string(),
            ));
        }

        let dsl_filters = filters
            .into_iter()
            .map(runtime_filter_to_dsl)
            .collect::<RedDBResult<Vec<_>>>()?;
        let weights = weights.unwrap_or(RuntimeQueryWeights {
            vector: 0.5,
            graph: 0.3,
            filter: 0.2,
        });
        let result_limit = limit.or(k).unwrap_or(10).max(1);
        let min_score = min_score
            .filter(|v| v.is_finite())
            .unwrap_or(0.0f32)
            .max(0.0);
        let graph_pattern_filter = graph_pattern.clone();
        let has_entity_type_filters = entity_types
            .as_ref()
            .is_some_and(|items| items.iter().any(|item| !item.trim().is_empty()));
        let has_capability_filters = capabilities
            .as_ref()
            .is_some_and(|items| items.iter().any(|item| !item.trim().is_empty()));
        let needs_fetch_expansion = query.is_some()
            || min_score > 0.0
            || !dsl_filters.is_empty()
            || graph_pattern_filter.is_some()
            || has_entity_type_filters
            || has_capability_filters;
        let fetch_k = if needs_fetch_expansion {
            k.unwrap_or(result_limit)
                .max(result_limit)
                .saturating_mul(4)
                .max(32)
        } else {
            k.unwrap_or(result_limit).max(1)
        };
        let text_fetch_limit = if needs_fetch_expansion {
            Some(fetch_k)
        } else {
            Some(result_limit)
        };

        let matches_graph_pattern = |entity: &UnifiedEntity| {
            let Some(pattern) = graph_pattern_filter.as_ref() else {
                return true;
            };
            match &entity.kind {
                EntityKind::GraphNode { label, node_type } => {
                    pattern.node_label.as_ref().is_none_or(|n| label == n)
                        && pattern.node_type.as_ref().is_none_or(|t| node_type == t)
                }
                _ => false,
            }
        };

        if vector.is_none() {
            let query = query
                .as_ref()
                .expect("query required for text-only hybrid search");
            let mut result = self.search_text(
                query.clone(),
                collection_scope,
                None,
                None,
                None,
                text_fetch_limit,
                false,
            )?;
            if min_score > 0.0 {
                result.matches.retain(|item| item.score >= min_score);
            }
            if !dsl_filters.is_empty() {
                result.matches.retain(|item| {
                    apply_filters(&item.entity, &dsl_filters) && matches_graph_pattern(&item.entity)
                });
            } else if graph_pattern_filter.is_some() {
                result
                    .matches
                    .retain(|item| matches_graph_pattern(&item.entity));
            }

            runtime_filter_dsl_result(&mut result, entity_types.clone(), capabilities.clone());
            for item in &mut result.matches {
                item.components.text_relevance = Some(item.score);
                item.components.final_score = Some(item.score);
            }
            result.matches.truncate(result_limit);
            return Ok(result);
        }

        let vector = vector.expect("vector required for vector-enabled hybrid search");
        let mut builder = HybridQueryBuilder::new();
        if let Some(pattern) = graph_pattern {
            builder.graph_pattern = Some(GraphPatternDsl {
                node_label: pattern.node_label,
                node_type: pattern.node_type,
                edge_labels: pattern.edge_labels,
            });
        }
        builder = builder.with_weights(weights.vector, weights.graph, weights.filter);
        if min_score > 0.0 {
            builder = builder.min_score(min_score);
        }
        builder = builder.similar_to(&vector, fetch_k);
        if let Some(collections) = collection_scope.clone() {
            for collection in collections {
                builder = builder.in_collection(collection);
            }
        }
        builder.filters = dsl_filters.clone();

        let mut result = builder
            .execute(&self.inner.db.store())
            .map_err(|err| RedDBError::Query(err.to_string()))?;
        normalize_runtime_dsl_result_scores(&mut result);

        if let Some(query) = query {
            let mut text_result = self.search_text(
                query,
                collection_scope.clone(),
                None,
                None,
                None,
                text_fetch_limit,
                false,
            )?;
            if min_score > 0.0 {
                text_result.matches.retain(|item| item.score >= min_score);
            }
            if !dsl_filters.is_empty() {
                text_result.matches.retain(|item| {
                    apply_filters(&item.entity, &dsl_filters) && matches_graph_pattern(&item.entity)
                });
            } else if graph_pattern_filter.is_some() {
                text_result
                    .matches
                    .retain(|item| matches_graph_pattern(&item.entity));
            }

            let mut merged_scores: HashMap<u64, ScoredMatch> = HashMap::new();
            for item in result.matches.drain(..) {
                merged_scores.insert(item.entity.id.raw(), item);
            }

            for mut item in text_result.matches {
                item.score *= weights.filter;
                item.components.final_score = Some(item.score);
                if let Some(current) = item.components.text_relevance {
                    item.components.text_relevance = Some(current);
                }
                let id = item.entity.id.raw();
                match merged_scores.get_mut(&id) {
                    Some(existing) => {
                        existing.score += item.score;
                        if let Some(text_relevance) = item.components.text_relevance {
                            existing.components.text_relevance = existing
                                .components
                                .text_relevance
                                .map(|value| value.max(text_relevance))
                                .or(Some(text_relevance));
                        }
                        existing.components.final_score = Some(existing.score);
                    }
                    None => {
                        merged_scores.insert(id, item);
                    }
                }
            }

            let mut merged = DslQueryResult {
                matches: merged_scores.into_values().collect(),
                scanned: result.scanned + text_result.scanned,
                execution_time_us: result.execution_time_us + text_result.execution_time_us,
                explanation: result.explanation,
            };
            normalize_runtime_dsl_result_scores(&mut merged);
            if min_score > 0.0 {
                merged.matches.retain(|item| item.score >= min_score);
            }

            runtime_filter_dsl_result(&mut merged, entity_types.clone(), capabilities.clone());
            merged.matches.truncate(result_limit);
            return Ok(merged);
        }

        runtime_filter_dsl_result(&mut result, entity_types.clone(), capabilities.clone());
        result.matches.truncate(result_limit);
        Ok(result)
    }

    pub fn search_multimodal(
        &self,
        query: String,
        collections: Option<Vec<String>>,
        entity_types: Option<Vec<String>>,
        capabilities: Option<Vec<String>>,
        limit: Option<usize>,
    ) -> RedDBResult<DslQueryResult> {
        let started = std::time::Instant::now();
        let query = query.trim().to_string();
        if query.is_empty() {
            return Err(RedDBError::Query(
                "field 'query' cannot be empty".to_string(),
            ));
        }

        let collection_scope = runtime_search_collections(&self.inner.db, collections);
        let allowed_collections: Option<BTreeSet<String>> =
            collection_scope.as_ref().map(|items| {
                items
                    .iter()
                    .map(|item| item.trim().to_string())
                    .filter(|item| !item.is_empty())
                    .collect()
            });
        let result_limit = limit.unwrap_or(25).max(1);

        let store = self.inner.db.store();
        let fetch_limit = result_limit.saturating_mul(4).max(32);

        // Use the dedicated ContextIndex instead of _mm_index metadata
        let hits = store
            .context_index()
            .search(&query, fetch_limit, allowed_collections.as_ref());
        let index_hits = hits.len();

        let mut scored: HashMap<u64, (UnifiedEntity, usize)> = HashMap::new();
        for hit in &hits {
            if let Some(entity) = store.get(&hit.collection, hit.entity_id) {
                scored
                    .entry(hit.entity_id.raw())
                    .or_insert((entity, hit.matched_tokens));
            }
        }

        // Fallback: global scan if ContextIndex returned nothing
        if scored.is_empty() {
            let query_tokens = tokenize_query(&query);
            if let Some(collections) = collection_scope {
                for collection in collections {
                    let Some(manager) = store.get_collection(&collection) else {
                        continue;
                    };
                    for entity in manager.query_all(|_| true) {
                        let entity_tokens = entity_tokens_for_search(&entity);
                        let overlap = query_tokens
                            .iter()
                            .filter(|token| entity_tokens.binary_search(token).is_ok())
                            .count();
                        if overlap > 0 {
                            scored.entry(entity.id.raw()).or_insert((entity, overlap));
                        }
                    }
                }
            }
        }

        let query_tokens_len = tokenize_query(&query).len().max(1) as f32;
        let mut result = DslQueryResult {
            matches: scored
                .into_values()
                .map(|(entity, overlap)| {
                    let score = (overlap as f32 / query_tokens_len).min(1.0);
                    ScoredMatch {
                        entity,
                        score,
                        components: MatchComponents {
                            text_relevance: Some(score),
                            structured_match: Some(score),
                            filter_match: true,
                            final_score: Some(score),
                            ..Default::default()
                        },
                        path: None,
                    }
                })
                .collect(),
            scanned: index_hits,
            execution_time_us: started.elapsed().as_micros() as u64,
            explanation: format!(
                "Multimodal search for '{query}' ({index_hits} index hits via ContextIndex)",
            ),
        };

        normalize_runtime_dsl_result_scores(&mut result);
        runtime_filter_dsl_result(&mut result, entity_types, capabilities);
        result.matches.truncate(result_limit);
        Ok(result)
    }

    pub fn search_index(
        &self,
        index: String,
        value: String,
        exact: bool,
        collections: Option<Vec<String>>,
        entity_types: Option<Vec<String>>,
        capabilities: Option<Vec<String>>,
        limit: Option<usize>,
    ) -> RedDBResult<DslQueryResult> {
        let started = std::time::Instant::now();
        let index = index.trim().to_string();
        let value = value.trim().to_string();

        if index.is_empty() {
            return Err(RedDBError::Query(
                "field 'index' cannot be empty".to_string(),
            ));
        }
        if value.is_empty() {
            return Err(RedDBError::Query(
                "field 'value' cannot be empty".to_string(),
            ));
        }

        let collection_scope = runtime_search_collections(&self.inner.db, collections.clone());
        let allowed_collections: Option<BTreeSet<String>> =
            collection_scope.as_ref().map(|items| {
                items
                    .iter()
                    .map(|item| item.trim().to_string())
                    .filter(|item| !item.is_empty())
                    .collect()
            });
        let result_limit = limit.unwrap_or(25).max(1);
        let fetch_limit = result_limit.saturating_mul(4).max(32);

        let store = self.inner.db.store();

        // Use the dedicated ContextIndex field-value lookup instead of _mm_field_index metadata
        let hits = store.context_index().search_field(
            &index,
            &value,
            exact,
            fetch_limit,
            allowed_collections.as_ref(),
        );
        let index_hits = hits.len();

        if hits.is_empty() {
            // Fallback to multimodal token search
            return self.search_multimodal(
                format!("{index}:{value}"),
                collections,
                entity_types,
                capabilities,
                limit,
            );
        }

        let mut result = DslQueryResult {
            matches: hits
                .into_iter()
                .filter_map(|hit| {
                    store.get(&hit.collection, hit.entity_id).map(|entity| {
                        ScoredMatch {
                            entity,
                            score: hit.score,
                            components: MatchComponents {
                                text_relevance: Some(hit.score),
                                structured_match: Some(hit.score),
                                filter_match: true,
                                final_score: Some(hit.score),
                                ..Default::default()
                            },
                            path: None,
                        }
                    })
                })
                .collect(),
            scanned: index_hits,
            execution_time_us: started.elapsed().as_micros() as u64,
            explanation: format!(
                "Indexed lookup for {index}={value} (exact={exact}, {index_hits} hits via ContextIndex)",
            ),
        };

        normalize_runtime_dsl_result_scores(&mut result);
        runtime_filter_dsl_result(&mut result, entity_types, capabilities);
        result.matches.truncate(result_limit);
        Ok(result)
    }

    pub fn search_text(
        &self,
        query: String,
        collections: Option<Vec<String>>,
        entity_types: Option<Vec<String>>,
        capabilities: Option<Vec<String>>,
        fields: Option<Vec<String>>,
        limit: Option<usize>,
        fuzzy: bool,
    ) -> RedDBResult<DslQueryResult> {
        let mut builder = TextSearchBuilder::new(query);
        let collection_scope = runtime_search_collections(&self.inner.db, collections);

        if let Some(collections) = collection_scope {
            for collection in collections {
                builder = builder.in_collection(collection);
            }
        }

        if let Some(fields) = fields {
            for field in fields {
                builder = builder.in_field(field);
            }
        }

        if fuzzy {
            builder = builder.fuzzy();
        }

        let mut result = builder
            .execute(&self.inner.db.store())
            .map_err(|err| RedDBError::Query(err.to_string()))?;
        for item in &mut result.matches {
            item.components.text_relevance = Some(item.score);
            item.components.final_score = Some(item.score);
        }
        runtime_filter_dsl_result(&mut result, entity_types, capabilities);
        if let Some(limit) = limit {
            result.matches.truncate(limit.max(1));
        }
        Ok(result)
    }

    pub fn search_context(&self, input: SearchContextInput) -> RedDBResult<ContextSearchResult> {
        let started = std::time::Instant::now();
        let result_limit = input.limit.unwrap_or(25).max(1);
        let graph_depth = input.graph_depth.unwrap_or(1).min(3);
        let graph_max_edges = input.graph_max_edges.unwrap_or(20);
        let max_cross_refs = input.max_cross_refs.unwrap_or(10);
        let follow_cross_refs = input.follow_cross_refs.unwrap_or(true);
        let expand_graph = input.expand_graph.unwrap_or(true);
        let do_global_scan = input.global_scan.unwrap_or(true);
        let do_reindex = input.reindex.unwrap_or(true);
        let min_score = input.min_score.unwrap_or(0.0).max(0.0);
        let query = input.query.trim().to_string();
        if query.is_empty() {
            return Err(RedDBError::Query(
                "field 'query' cannot be empty".to_string(),
            ));
        }

        let store = self.inner.db.store();
        let collection_scope = runtime_search_collections(&self.inner.db, input.collections);
        let allowed_collections: Option<BTreeSet<String>> =
            collection_scope.as_ref().map(|items| {
                items
                    .iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            });

        let mut scored: HashMap<u64, (UnifiedEntity, f32, DiscoveryMethod, String)> =
            HashMap::new();
        let mut tiers_used: Vec<String> = Vec::new();
        let mut entities_reindexed = 0usize;
        let mut collections_searched = 0usize;

        // ── Tier 1: Field-value index lookup ────────────────────────────
        if let Some(ref field) = input.field {
            let hits = store.context_index().search_field(
                field,
                &query,
                true,
                result_limit.saturating_mul(4).max(32),
                allowed_collections.as_ref(),
            );
            if !hits.is_empty() {
                tiers_used.push("index".to_string());
            }
            for hit in hits {
                if hit.score >= min_score {
                    if let Some(entity) = store.get(&hit.collection, hit.entity_id) {
                        scored.entry(hit.entity_id.raw()).or_insert((
                            entity,
                            hit.score,
                            DiscoveryMethod::Indexed {
                                field: field.clone(),
                            },
                            hit.collection,
                        ));
                    }
                }
            }
        }

        // ── Tier 2: Token index ─────────────────────────────────────────
        {
            let hits = store.context_index().search(
                &query,
                result_limit.saturating_mul(4).max(32),
                allowed_collections.as_ref(),
            );
            if !hits.is_empty() && !tiers_used.contains(&"multimodal".to_string()) {
                tiers_used.push("multimodal".to_string());
            }
            for hit in hits {
                if hit.score >= min_score {
                    if let Some(entity) = store.get(&hit.collection, hit.entity_id) {
                        scored.entry(hit.entity_id.raw()).or_insert((
                            entity,
                            hit.score,
                            DiscoveryMethod::Indexed {
                                field: "_token".to_string(),
                            },
                            hit.collection,
                        ));
                    }
                }
            }
        }

        // ── Tier 3: Global scan (fallback) ──────────────────────────────
        if do_global_scan && scored.len() < result_limit {
            let all_collections = match &collection_scope {
                Some(cols) => cols.clone(),
                None => store.list_collections(),
            };
            collections_searched = all_collections.len();

            let query_tokens = tokenize_query(&query);
            if !query_tokens.is_empty() {
                let mut scan_found = false;
                for collection_name in &all_collections {
                    let Some(manager) = store.get_collection(collection_name) else {
                        continue;
                    };
                    for entity in manager.query_all(|_| true) {
                        if scored.contains_key(&entity.id.raw()) {
                            continue;
                        }
                        let entity_tokens = entity_tokens_for_search(&entity);
                        let overlap = query_tokens
                            .iter()
                            .filter(|t| entity_tokens.binary_search(t).is_ok())
                            .count();
                        if overlap == 0 {
                            continue;
                        }
                        let score =
                            (overlap as f32 / query_tokens.len().max(1) as f32).min(1.0) * 0.9;
                        if score >= min_score {
                            scan_found = true;
                            if do_reindex {
                                store.context_index().index_entity(collection_name, &entity);
                                entities_reindexed += 1;
                            }
                            scored.insert(
                                entity.id.raw(),
                                (
                                    entity,
                                    score,
                                    DiscoveryMethod::GlobalScan,
                                    collection_name.clone(),
                                ),
                            );
                        }
                        if scored.len() >= result_limit.saturating_mul(4) {
                            break;
                        }
                    }
                    if scored.len() >= result_limit.saturating_mul(4) {
                        break;
                    }
                }
                if scan_found {
                    tiers_used.push("scan".to_string());
                }
            }
        }

        let direct_matches = scored.len();

        // ── Expansion: Cross-references ─────────────────────────────────
        let mut expanded_cross_refs = 0usize;
        if follow_cross_refs {
            let seed: Vec<(u64, f32, Vec<crate::storage::CrossRef>)> = scored
                .values()
                .filter(|(entity, _, _, _)| !entity.cross_refs.is_empty())
                .map(|(entity, score, _, _)| (entity.id.raw(), *score, entity.cross_refs.clone()))
                .collect();

            for (source_id, source_score, cross_refs) in seed {
                for xref in cross_refs.iter().take(max_cross_refs) {
                    if scored.contains_key(&xref.target.raw()) {
                        continue;
                    }
                    if let Some(target) = self.inner.db.get(xref.target) {
                        let decayed_score = source_score * xref.weight * 0.8;
                        if decayed_score >= min_score {
                            expanded_cross_refs += 1;
                            scored.insert(
                                xref.target.raw(),
                                (
                                    target,
                                    decayed_score,
                                    DiscoveryMethod::CrossReference {
                                        source_id,
                                        ref_type: format!("{:?}", xref.ref_type),
                                    },
                                    xref.target_collection.clone(),
                                ),
                            );
                        }
                    }
                }
            }
        }

        // ── Expansion: Graph traversal ──────────────────────────────────
        let mut expanded_graph = 0usize;
        if expand_graph && graph_depth > 0 {
            let seed_node_ids: Vec<(u64, String, f32)> = scored
                .values()
                .filter_map(|(entity, score, _, _)| {
                    if matches!(entity.kind, EntityKind::GraphNode { .. }) {
                        Some((entity.id.raw(), entity.id.raw().to_string(), *score))
                    } else {
                        None
                    }
                })
                .collect();

            if !seed_node_ids.is_empty() {
                if let Ok(graph) = materialize_graph(store.as_ref()) {
                    for (source_id, node_id_str, source_score) in &seed_node_ids {
                        let mut visited: HashSet<String> = HashSet::new();
                        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
                        visited.insert(node_id_str.clone());
                        queue.push_back((node_id_str.clone(), 0));

                        while let Some((current, depth)) = queue.pop_front() {
                            if depth >= graph_depth {
                                continue;
                            }
                            let neighbors = graph_adjacent_edges(
                                &graph,
                                &current,
                                RuntimeGraphDirection::Both,
                                None,
                            );
                            for (neighbor_id, _edge) in neighbors.into_iter().take(graph_max_edges)
                            {
                                if !visited.insert(neighbor_id.clone()) {
                                    continue;
                                }
                                if let Ok(parsed) = neighbor_id.parse::<u64>() {
                                    if scored.contains_key(&parsed) {
                                        continue;
                                    }
                                    if let Some(entity) = self.inner.db.get(EntityId::new(parsed)) {
                                        let decay = 0.7f32.powi((depth + 1) as i32);
                                        let decayed_score = source_score * decay;
                                        if decayed_score >= min_score {
                                            expanded_graph += 1;
                                            let collection = entity.kind.collection().to_string();
                                            scored.insert(
                                                parsed,
                                                (
                                                    entity,
                                                    decayed_score,
                                                    DiscoveryMethod::GraphTraversal {
                                                        source_id: *source_id,
                                                        edge_type: "adjacent".to_string(),
                                                        depth: depth + 1,
                                                    },
                                                    collection,
                                                ),
                                            );
                                        }
                                    }
                                }
                                queue.push_back((neighbor_id, depth + 1));
                            }
                        }
                    }
                }
            }
        }

        // ── Expansion: Vectors ──────────────────────────────────────────
        let mut expanded_vectors = 0usize;
        if let Some(ref vector) = input.vector {
            let vec_collections = collection_scope.unwrap_or_else(|| store.list_collections());
            for collection in &vec_collections {
                if let Ok(results) =
                    self.search_similar(collection, vector, result_limit, min_score)
                {
                    for result in results {
                        if scored.contains_key(&result.entity_id.raw()) {
                            continue;
                        }
                        if let Some(entity) = self.inner.db.get(result.entity_id) {
                            expanded_vectors += 1;
                            scored.insert(
                                result.entity_id.raw(),
                                (
                                    entity,
                                    result.score * 0.9,
                                    DiscoveryMethod::VectorQuery {
                                        similarity: result.score,
                                    },
                                    collection.clone(),
                                ),
                            );
                        }
                    }
                }
            }
        }

        // ── Build connections map ───────────────────────────────────────
        let mut connections: Vec<ContextConnection> = Vec::new();
        let found_ids: HashSet<u64> = scored.keys().copied().collect();
        for (entity, _, _, _) in scored.values() {
            for xref in &entity.cross_refs {
                if found_ids.contains(&xref.target.raw()) {
                    connections.push(ContextConnection {
                        from_id: entity.id.raw(),
                        to_id: xref.target.raw(),
                        connection_type: ContextConnectionType::CrossRef(format!(
                            "{:?}",
                            xref.ref_type
                        )),
                        weight: xref.weight,
                    });
                }
            }
            if let EntityKind::GraphEdge {
                from_node, to_node, ..
            } = &entity.kind
            {
                if let (Ok(from), Ok(to)) = (from_node.parse::<u64>(), to_node.parse::<u64>()) {
                    if found_ids.contains(&from) || found_ids.contains(&to) {
                        connections.push(ContextConnection {
                            from_id: from,
                            to_id: to,
                            connection_type: ContextConnectionType::GraphEdge(
                                entity.kind.collection().to_string(),
                            ),
                            weight: match &entity.data {
                                EntityData::Edge(e) => e.weight / 1000.0,
                                _ => 1.0,
                            },
                        });
                    }
                }
            }
        }

        // ── Group by entity kind ────────────────────────────────────────
        let mut tables = Vec::new();
        let mut graph_nodes = Vec::new();
        let mut graph_edges = Vec::new();
        let mut vectors = Vec::new();
        let mut documents = Vec::new();
        let mut key_values = Vec::new();

        let mut all: Vec<(UnifiedEntity, f32, DiscoveryMethod, String)> =
            scored.into_values().collect();
        all.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.id.raw().cmp(&b.0.id.raw()))
        });

        for (entity, score, discovery, collection) in all {
            let ctx_entity = ContextEntity {
                score,
                discovery,
                collection,
                entity,
            };

            let (entity_type, _) = runtime_entity_type_and_capabilities(&ctx_entity.entity);
            match entity_type {
                "table" => tables.push(ctx_entity),
                "kv" => key_values.push(ctx_entity),
                "document" => documents.push(ctx_entity),
                "graph_node" => graph_nodes.push(ctx_entity),
                "graph_edge" => graph_edges.push(ctx_entity),
                "vector" => vectors.push(ctx_entity),
                _ => tables.push(ctx_entity),
            }
        }

        // Truncate each bucket
        tables.truncate(result_limit);
        graph_nodes.truncate(result_limit);
        graph_edges.truncate(result_limit);
        vectors.truncate(result_limit);
        documents.truncate(result_limit);
        key_values.truncate(result_limit);

        let total = tables.len()
            + graph_nodes.len()
            + graph_edges.len()
            + vectors.len()
            + documents.len()
            + key_values.len();

        Ok(ContextSearchResult {
            query,
            tables,
            graph: ContextGraphResult {
                nodes: graph_nodes,
                edges: graph_edges,
            },
            vectors,
            documents,
            key_values,
            connections,
            summary: ContextSummary {
                total_entities: total,
                direct_matches,
                expanded_via_graph: expanded_graph,
                expanded_via_cross_refs: expanded_cross_refs,
                expanded_via_vector_query: expanded_vectors,
                collections_searched,
                execution_time_us: started.elapsed().as_micros() as u64,
                tiers_used,
                entities_reindexed,
            },
        })
    }

    /// Execute an ASK query: search context + LLM synthesis.
    pub fn execute_ask(
        &self,
        raw_query: &str,
        ask: &crate::storage::query::ast::AskQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::ai::{
            anthropic_prompt, openai_prompt, parse_provider, resolve_api_key_from_runtime,
            AiProvider, AnthropicPromptRequest, OpenAiPromptRequest,
        };

        // Step 1: Search context to find relevant entities
        let context_result = self.search_context(SearchContextInput {
            query: ask.question.clone(),
            field: None,
            vector: None,
            collections: ask.collection.as_ref().map(|c| vec![c.clone()]),
            graph_depth: ask.depth,
            graph_max_edges: None,
            max_cross_refs: None,
            follow_cross_refs: None,
            expand_graph: None,
            global_scan: None,
            reindex: None,
            limit: ask.limit,
            min_score: None,
        })?;

        // Step 2: Build rich context with schema + search results
        let context_json =
            crate::presentation::query_json::context_search_result_json(&context_result);
        let context_str =
            crate::json::to_string(&context_json).unwrap_or_else(|_| "{}".to_string());

        // Include database schema info so the LLM knows the structure
        let store = self.inner.db.store();
        let all_collections = store.list_collections();
        let mut schema_lines = Vec::new();
        schema_lines.push("Database structure:".to_string());
        for col_name in &all_collections {
            if let Some(manager) = store.get_collection(col_name) {
                let stats = manager.stats();
                schema_lines.push(format!(
                    "- Collection '{col_name}': {} entities",
                    stats.total_entities
                ));
            }
        }

        // Include graph structure if there are graph entities
        if !context_result.graph.nodes.is_empty() || !context_result.graph.edges.is_empty() {
            schema_lines.push(String::new());
            schema_lines.push("Graph relationships found:".to_string());
            for edge in &context_result.graph.edges {
                if let EntityKind::GraphEdge {
                    label,
                    from_node,
                    to_node,
                    ..
                } = &edge.entity.kind
                {
                    schema_lines.push(format!("- Edge '{label}': {from_node} → {to_node}"));
                }
            }
        }

        // Include connections map
        if !context_result.connections.is_empty() {
            schema_lines.push(String::new());
            schema_lines.push("Entity connections:".to_string());
            for conn in &context_result.connections {
                let conn_type = match &conn.connection_type {
                    ContextConnectionType::CrossRef(rt) => format!("cross-ref ({rt})"),
                    ContextConnectionType::GraphEdge(et) => format!("graph edge ({et})"),
                    ContextConnectionType::VectorSimilarity(s) => {
                        format!("vector similarity ({s:.2})")
                    }
                };
                schema_lines.push(format!(
                    "- Entity {} → Entity {}: {conn_type} (weight={:.2})",
                    conn.from_id, conn.to_id, conn.weight
                ));
            }
        }

        let schema_info = schema_lines.join("\n");

        let system_prompt = format!(
            "You are an AI assistant answering questions about data in RedDB, a multi-modal database \
             that stores tables, graphs, vectors, documents, and key-values.\n\n\
             {schema_info}\n\n\
             Search results (entities found matching the question):\n{context_str}\n\n\
             Instructions:\n\
             - Use the database structure and search results above to answer the user's question.\n\
             - For questions about table structure, refer to the collection descriptors.\n\
             - For questions about relationships, refer to graph edges and connections.\n\
             - If the context does not contain enough information, say so.\n\
             - Cite which collections and entity types your answer is based on."
        );

        let full_prompt = format!("{system_prompt}\n\nQuestion: {}", ask.question);

        // Step 3: Call LLM
        let provider = parse_provider(ask.provider.as_deref().unwrap_or("openai"))?;
        let api_key = resolve_api_key_from_runtime(&provider, None, self)?;
        let model = ask.model.clone().unwrap_or_else(|| {
            std::env::var(provider.prompt_model_env_name())
                .ok()
                .unwrap_or_else(|| provider.default_prompt_model().to_string())
        });
        let api_base = provider.resolve_api_base();

        let response = match provider {
            AiProvider::Anthropic => {
                let resp = anthropic_prompt(AnthropicPromptRequest {
                    api_key,
                    model: model.clone(),
                    prompt: full_prompt,
                    temperature: Some(0.3),
                    max_output_tokens: Some(1024),
                    api_base,
                    anthropic_version: crate::ai::DEFAULT_ANTHROPIC_VERSION.to_string(),
                })?;
                (
                    resp.output_text,
                    resp.prompt_tokens.unwrap_or(0),
                    resp.completion_tokens.unwrap_or(0),
                )
            }
            _ => {
                let resp = openai_prompt(OpenAiPromptRequest {
                    api_key,
                    model: model.clone(),
                    prompt: full_prompt,
                    temperature: Some(0.3),
                    max_output_tokens: Some(1024),
                    api_base,
                })?;
                (
                    resp.output_text,
                    resp.prompt_tokens.unwrap_or(0),
                    resp.completion_tokens.unwrap_or(0),
                )
            }
        };

        let (answer, prompt_tokens, completion_tokens) = response;

        // Step 4: Build result
        let mut result = UnifiedResult::with_columns(vec![
            "answer".into(),
            "provider".into(),
            "model".into(),
            "prompt_tokens".into(),
            "completion_tokens".into(),
            "sources_count".into(),
        ]);
        let mut record = UnifiedRecord::new();
        record.set("answer", Value::Text(answer));
        record.set("provider", Value::Text(provider.token().to_string()));
        record.set("model", Value::Text(model));
        record.set("prompt_tokens", Value::Integer(prompt_tokens as i64));
        record.set(
            "completion_tokens",
            Value::Integer(completion_tokens as i64),
        );
        record.set(
            "sources_count",
            Value::Integer(context_result.summary.total_entities as i64),
        );
        result.push(record);

        Ok(RuntimeQueryResult {
            query: raw_query.to_string(),
            mode: QueryMode::Sql,
            statement: "ask",
            engine: "runtime-ai",
            result,
            affected_rows: 0,
            statement_type: "select",
        })
    }
}
