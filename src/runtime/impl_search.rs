use super::*;
use crate::application::multimodal_index::{
    entity_multimodal_tokens_for_search, metadata_key_for_field_lookup,
    metadata_key_for_multimodal_token, query_lookup_index_tokens, query_multimodal_tokens,
    query_multimodal_tokens_exact,
};

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

        let query_tokens = query_multimodal_tokens(&query);
        if query_tokens.is_empty() {
            return Err(RedDBError::Query(
                "query does not contain indexable tokens".to_string(),
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
        let mut scored: HashMap<u64, (UnifiedEntity, usize)> = HashMap::new();
        let mut scanned = 0usize;
        let mut index_hits = 0usize;

        for token in &query_tokens {
            let metadata_key = metadata_key_for_multimodal_token(token);
            let matches =
                store.filter_metadata_all(&[(metadata_key, UnifiedMetadataFilter::IsNotNull)]);
            scanned += matches.len();
            for (collection, id) in matches {
                if allowed_collections
                    .as_ref()
                    .is_some_and(|allowed| !allowed.contains(&collection))
                {
                    continue;
                }
                let Some(entity) = store.get(&collection, id) else {
                    continue;
                };
                index_hits += 1;
                let entry = scored.entry(entity.id.raw()).or_insert((entity, 0usize));
                entry.1 = entry.1.saturating_add(1);
            }
        }

        if scored.is_empty() {
            if let Some(collections) = collection_scope {
                for collection in collections {
                    let Some(manager) = store.get_collection(&collection) else {
                        continue;
                    };
                    for entity in manager.query_all(|_| true) {
                        scanned = scanned.saturating_add(1);
                        let entity_tokens = entity_multimodal_tokens_for_search(&entity);
                        let overlap = query_tokens
                            .iter()
                            .filter(|token| entity_tokens.binary_search(token).is_ok())
                            .count();
                        if overlap == 0 {
                            continue;
                        }
                        let entry = scored.entry(entity.id.raw()).or_insert((entity, 0usize));
                        entry.1 = entry.1.max(overlap);
                    }
                }
            }
        }

        let token_count = query_tokens.len().max(1) as f32;
        let mut result = DslQueryResult {
            matches: scored
                .into_values()
                .map(|(entity, overlap)| {
                    let score = (overlap as f32 / token_count).min(1.0);
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
            scanned,
            execution_time_us: started.elapsed().as_micros() as u64,
            explanation: format!(
                "Multimodal indexed search for '{query}' ({} tokens, {index_hits} index hits)",
                query_tokens.len()
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

        let index_tokens = query_lookup_index_tokens(&index);
        let value_tokens = if exact {
            query_multimodal_tokens_exact(&value)
        } else {
            query_multimodal_tokens(&value)
        };

        if index_tokens.is_empty() || value_tokens.is_empty() {
            return Err(RedDBError::Query(
                "lookup index or value does not contain indexable tokens".to_string(),
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

        let store = self.inner.db.store();
        let mut scored: HashMap<u64, (UnifiedEntity, usize)> = HashMap::new();
        let mut scanned = 0usize;
        let mut index_hits = 0usize;

        for index_token in &index_tokens {
            for value_token in &value_tokens {
                let metadata_key = metadata_key_for_field_lookup(index_token, value_token);
                let matches =
                    store.filter_metadata_all(&[(metadata_key, UnifiedMetadataFilter::IsNotNull)]);
                scanned += matches.len();
                for (collection, id) in matches {
                    if allowed_collections
                        .as_ref()
                        .is_some_and(|allowed| !allowed.contains(&collection))
                    {
                        continue;
                    }
                    let Some(entity) = store.get(&collection, id) else {
                        continue;
                    };
                    index_hits += 1;
                    let entry = scored.entry(entity.id.raw()).or_insert((entity, 0usize));
                    entry.1 = entry.1.saturating_add(1);
                }
            }
        }

        if scored.is_empty() {
            return self.search_multimodal(
                format!("{index}:{value}"),
                collections,
                entity_types,
                capabilities,
                limit,
            );
        }

        let max_overlap = (index_tokens.len() * value_tokens.len()).max(1) as f32;
        let mut result = DslQueryResult {
            matches: scored
                .into_values()
                .map(|(entity, overlap)| {
                    let score = (overlap as f32 / max_overlap).min(1.0);
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
            scanned,
            execution_time_us: started.elapsed().as_micros() as u64,
            explanation: format!(
                "Indexed lookup for {index}={value} (exact={exact}, {}x{} tokens, {index_hits} index hits)",
                index_tokens.len(),
                value_tokens.len(),
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
}
