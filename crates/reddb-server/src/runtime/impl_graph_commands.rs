//! Execution of GRAPH and SEARCH SQL-like commands.
//!
//! Maps parsed `GraphCommand` and `SearchCommand` AST nodes to the existing
//! runtime graph analytics and search methods, returning results wrapped in
//! `RuntimeQueryResult`.

use super::*;
use crate::storage::query::ast::GraphCommandOrderBy;
use std::cmp::Ordering;

impl RedDBRuntime {
    /// Execute a GRAPH analytics command.
    pub fn execute_graph_command(
        &self,
        raw_query: &str,
        cmd: &GraphCommand,
    ) -> RedDBResult<RuntimeQueryResult> {
        match cmd {
            GraphCommand::Neighborhood {
                source,
                depth,
                direction,
                edge_labels,
            } => {
                let dir = parse_direction(direction)?;
                let res = self.graph_neighborhood(
                    source,
                    dir,
                    *depth as usize,
                    edge_labels.clone(),
                    None,
                )?;
                let mut result = UnifiedResult::with_columns(vec![
                    "node_id".into(),
                    "label".into(),
                    "node_type".into(),
                    "depth".into(),
                ]);
                for visit in &res.nodes {
                    let mut record = UnifiedRecord::new();
                    record.set("node_id", Value::text(visit.node.id.clone()));
                    record.set("label", Value::text(visit.node.label.clone()));
                    record.set("node_type", Value::text(visit.node.node_type.clone()));
                    record.set("depth", Value::Integer(visit.depth as i64));
                    result.push(record);
                }
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "graph_neighborhood",
                    engine: "runtime-graph",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            GraphCommand::ShortestPath {
                source,
                target,
                algorithm,
                direction,
                limit,
                order_by,
            } => {
                let dir = parse_direction(direction)?;
                let alg = parse_path_algorithm(algorithm)?;
                let res = self.graph_shortest_path(source, target, dir, alg, None, None)?;
                let mut result = UnifiedResult::with_columns(vec![
                    "source".into(),
                    "target".into(),
                    "nodes_visited".into(),
                    "negative_cycle_detected".into(),
                    "hop_count".into(),
                    "total_weight".into(),
                ]);
                let mut record = UnifiedRecord::new();
                record.set("source", Value::text(res.source));
                record.set("target", Value::text(res.target));
                record.set("nodes_visited", Value::Integer(res.nodes_visited as i64));
                record.set(
                    "negative_cycle_detected",
                    match res.negative_cycle_detected {
                        Some(value) => Value::Boolean(value),
                        None => Value::Null,
                    },
                );
                if let Some(ref path) = res.path {
                    record.set("hop_count", Value::Integer(path.hop_count as i64));
                    record.set("total_weight", Value::Float(path.total_weight));
                } else {
                    record.set("hop_count", Value::Null);
                    record.set("total_weight", Value::Null);
                }
                result.push(record);
                apply_graph_order_and_limit(
                    &mut result,
                    "graph_shortest_path",
                    order_by.as_ref(),
                    limit.map(|n| n as usize),
                )?;
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "graph_shortest_path",
                    engine: "runtime-graph",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            GraphCommand::Properties { source } => {
                if let Some(node_ref) = source {
                    // Per-node property lookup (#423). Uses the same label
                    // resolution as NEIGHBORHOOD/TRAVERSE so '<label>' and
                    // '<numeric id>' both work.
                    let graph =
                        materialize_graph_with_projection(self.inner.db.store().as_ref(), None)?;
                    let resolved = resolve_graph_node_id(&graph, node_ref)?;
                    let stored = graph
                        .get_node(&resolved)
                        .ok_or_else(|| RedDBError::NotFound(node_ref.to_string()))?;
                    let node_type = self
                        .inner
                        .db
                        .store()
                        .query_all(|entity| {
                            entity.id.raw().to_string() == resolved
                                && matches!(
                                    entity.kind,
                                    crate::storage::unified::EntityKind::GraphNode(_)
                                )
                        })
                        .into_iter()
                        .find_map(|(_, entity)| match entity.kind {
                            crate::storage::unified::EntityKind::GraphNode(node) => {
                                Some(node.node_type)
                            }
                            _ => None,
                        })
                        .unwrap_or_else(|| stored.node_type.clone());
                    let all_props =
                        materialize_graph_node_properties(self.inner.db.store().as_ref())?;
                    let props = all_props.get(&resolved).cloned().unwrap_or_default();

                    // Fixed columns first, then property keys in sorted order so
                    // the schema is stable across snapshots / wire renders.
                    let mut prop_keys: Vec<&String> = props.keys().collect();
                    prop_keys.sort();
                    let mut columns: Vec<String> = Vec::with_capacity(3 + prop_keys.len());
                    columns.push("node_id".into());
                    columns.push("label".into());
                    columns.push("node_type".into());
                    for k in &prop_keys {
                        columns.push((*k).clone());
                    }
                    let mut result = UnifiedResult::with_columns(columns);
                    let mut record = UnifiedRecord::new();
                    record.set("node_id", Value::text(stored.id.clone()));
                    record.set("label", Value::text(stored.label.clone()));
                    record.set("node_type", Value::text(node_type));
                    for k in &prop_keys {
                        if let Some(v) = props.get(*k) {
                            record.set(k.as_str(), v.clone());
                        }
                    }
                    result.push(record);
                    return Ok(RuntimeQueryResult {
                        query: raw_query.to_string(),
                        mode: QueryMode::Sql,
                        statement: "graph_properties",
                        engine: "runtime-graph",
                        result,
                        affected_rows: 0,
                        statement_type: "select",
                    });
                }
                let res = self.graph_properties(None)?;
                let mut result = UnifiedResult::with_columns(vec![
                    "node_count".into(),
                    "edge_count".into(),
                    "is_connected".into(),
                    "is_complete".into(),
                    "is_cyclic".into(),
                    "density".into(),
                ]);
                let mut record = UnifiedRecord::new();
                record.set("node_count", Value::Integer(res.node_count as i64));
                record.set("edge_count", Value::Integer(res.edge_count as i64));
                record.set("is_connected", Value::Boolean(res.is_connected));
                record.set("is_complete", Value::Boolean(res.is_complete));
                record.set("is_cyclic", Value::Boolean(res.is_cyclic));
                record.set("density", Value::Float(res.density));
                result.push(record);
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "graph_properties",
                    engine: "runtime-graph",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            GraphCommand::Traverse {
                source,
                strategy,
                depth,
                direction,
                edge_labels,
            } => {
                let dir = parse_direction(direction)?;
                let strat = parse_traversal_strategy(strategy)?;
                let res = self.graph_traverse(
                    source,
                    dir,
                    *depth as usize,
                    strat,
                    edge_labels.clone(),
                    None,
                )?;
                let mut result = UnifiedResult::with_columns(vec![
                    "node_id".into(),
                    "label".into(),
                    "node_type".into(),
                    "depth".into(),
                ]);
                for visit in &res.visits {
                    let mut record = UnifiedRecord::new();
                    record.set("node_id", Value::text(visit.node.id.clone()));
                    record.set("label", Value::text(visit.node.label.clone()));
                    record.set("node_type", Value::text(visit.node.node_type.clone()));
                    record.set("depth", Value::Integer(visit.depth as i64));
                    result.push(record);
                }
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "graph_traverse",
                    engine: "runtime-graph",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            GraphCommand::Centrality {
                algorithm,
                limit,
                order_by,
            } => {
                let alg = parse_centrality_algorithm(algorithm)?;
                // `limit = None` keeps historical implicit top-100 cap.
                // `Some(0)` returns zero rows (standard SQL LIMIT 0 semantics).
                let limit_usize = limit.map(|n| n as usize);
                let order_needs_full_set = order_by
                    .as_ref()
                    .map(|order| order.ascending)
                    .unwrap_or(false);
                let top_k = if order_needs_full_set {
                    usize::MAX
                } else {
                    limit_usize.unwrap_or(100).max(1)
                };
                let res = self.graph_centrality(alg, top_k, false, None, None, None, None)?;
                let mut result = UnifiedResult::with_columns(vec![
                    "node_id".into(),
                    "label".into(),
                    "score".into(),
                ]);
                for score in &res.scores {
                    let mut record = UnifiedRecord::new();
                    record.set("node_id", Value::text(score.node.id.clone()));
                    record.set("label", Value::text(score.node.label.clone()));
                    record.set("score", Value::Float(score.score));
                    result.push(record);
                }
                for ds in &res.degree_scores {
                    let mut record = UnifiedRecord::new();
                    record.set("node_id", Value::text(ds.node.id.clone()));
                    record.set("label", Value::text(ds.node.label.clone()));
                    record.set("score", Value::Float(ds.total_degree as f64));
                    result.push(record);
                }
                apply_graph_order_and_limit(
                    &mut result,
                    "graph_centrality",
                    order_by.as_ref(),
                    Some(limit_usize.unwrap_or(100)),
                )?;
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "graph_centrality",
                    engine: "runtime-graph",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            GraphCommand::Community {
                algorithm,
                max_iterations,
                limit,
                order_by,
            } => {
                let alg = parse_community_algorithm(algorithm)?;
                let res =
                    self.graph_communities(alg, 1, Some(*max_iterations as usize), None, None)?;
                let mut result =
                    UnifiedResult::with_columns(vec!["community_id".into(), "size".into()]);
                for community in &res.communities {
                    let mut record = UnifiedRecord::new();
                    record.set("community_id", Value::text(community.id.clone()));
                    record.set("size", Value::Integer(community.size as i64));
                    result.push(record);
                }
                apply_graph_order_and_limit(
                    &mut result,
                    "graph_community",
                    order_by.as_ref(),
                    limit.map(|n| n as usize),
                )?;
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "graph_community",
                    engine: "runtime-graph",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            GraphCommand::Components {
                mode,
                limit,
                order_by,
            } => {
                let m = parse_components_mode(mode)?;
                let res = self.graph_components(m, 1, None)?;
                let mut result =
                    UnifiedResult::with_columns(vec!["component_id".into(), "size".into()]);
                for component in &res.components {
                    let mut record = UnifiedRecord::new();
                    record.set("component_id", Value::text(component.id.clone()));
                    record.set("size", Value::Integer(component.size as i64));
                    result.push(record);
                }
                apply_graph_order_and_limit(
                    &mut result,
                    "graph_components",
                    order_by.as_ref(),
                    limit.map(|n| n as usize),
                )?;
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "graph_components",
                    engine: "runtime-graph",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            GraphCommand::Cycles { max_length } => {
                let res = self.graph_cycles(*max_length as usize, 100, None)?;
                let mut result =
                    UnifiedResult::with_columns(vec!["cycle_index".into(), "length".into()]);
                for (i, cycle) in res.cycles.iter().enumerate() {
                    let mut record = UnifiedRecord::new();
                    record.set("cycle_index", Value::Integer(i as i64));
                    record.set("length", Value::Integer(cycle.nodes.len() as i64));
                    result.push(record);
                }
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "graph_cycles",
                    engine: "runtime-graph",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            GraphCommand::Clustering => {
                let res = self.graph_clustering(100, true, None)?;
                let mut result = UnifiedResult::with_columns(vec![
                    "node_id".into(),
                    "label".into(),
                    "score".into(),
                ]);
                // First row: global coefficient
                let mut global_record = UnifiedRecord::new();
                global_record.set("node_id", Value::text("__global__"));
                global_record.set("label", Value::text("global_clustering"));
                global_record.set("score", Value::Float(res.global));
                result.push(global_record);
                for score in &res.local {
                    let mut record = UnifiedRecord::new();
                    record.set("node_id", Value::text(score.node.id.clone()));
                    record.set("label", Value::text(score.node.label.clone()));
                    record.set("score", Value::Float(score.score));
                    result.push(record);
                }
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "graph_clustering",
                    engine: "runtime-graph",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            GraphCommand::TopologicalSort => {
                let res = self.graph_topological_sort(None)?;
                let mut result = UnifiedResult::with_columns(vec![
                    "order".into(),
                    "node_id".into(),
                    "label".into(),
                ]);
                for (i, node) in res.ordered_nodes.iter().enumerate() {
                    let mut record = UnifiedRecord::new();
                    record.set("order", Value::Integer(i as i64));
                    record.set("node_id", Value::text(node.id.clone()));
                    record.set("label", Value::text(node.label.clone()));
                    result.push(record);
                }
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "graph_topological_sort",
                    engine: "runtime-graph",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
        }
    }

    /// Execute a SEARCH command.
    pub fn execute_search_command(
        &self,
        raw_query: &str,
        cmd: &SearchCommand,
    ) -> RedDBResult<RuntimeQueryResult> {
        match cmd {
            SearchCommand::Similar {
                vector,
                text,
                provider,
                collection,
                limit,
                min_score,
                vector_param,
                limit_param,
                min_score_param,
                text_param,
            } => {
                if vector_param.is_some() {
                    return Err(RedDBError::Query(
                        "SEARCH SIMILAR $N vector parameter was not bound before execution"
                            .to_string(),
                    ));
                }
                if limit_param.is_some() {
                    return Err(RedDBError::Query(
                        "SEARCH SIMILAR LIMIT $N parameter was not bound before execution"
                            .to_string(),
                    ));
                }
                if min_score_param.is_some() {
                    return Err(RedDBError::Query(
                        "SEARCH SIMILAR MIN_SCORE $N parameter was not bound before execution"
                            .to_string(),
                    ));
                }
                if text_param.is_some() {
                    return Err(RedDBError::Query(
                        "SEARCH SIMILAR TEXT $N parameter was not bound before execution"
                            .to_string(),
                    ));
                }
                // If text provided, generate embedding first (semantic search)
                let search_vector = if let Some(query_text) = text {
                    let (default_provider, _) = crate::ai::resolve_defaults_from_runtime(self);
                    let provider = match provider.as_deref() {
                        Some(p) => crate::ai::parse_provider(p)?,
                        None => default_provider,
                    };
                    let api_key = crate::ai::resolve_api_key_from_runtime(&provider, None, self)?;
                    let model = std::env::var("REDDB_OPENAI_EMBEDDING_MODEL")
                        .ok()
                        .unwrap_or_else(|| provider.default_embedding_model().to_string());
                    let transport = crate::runtime::ai::transport::AiTransport::from_runtime(self);
                    let request = crate::ai::OpenAiEmbeddingRequest {
                        api_key,
                        model,
                        inputs: vec![query_text.clone()],
                        dimensions: None,
                        api_base: provider.resolve_api_base(),
                    };
                    let response = crate::runtime::ai::block_on_ai(async move {
                        crate::ai::openai_embeddings_async(&transport, request).await
                    })
                    .and_then(|result| result)?;
                    response.embeddings.into_iter().next().ok_or_else(|| {
                        RedDBError::Query("embedding API returned no vectors".to_string())
                    })?
                } else {
                    vector.clone()
                };
                // Issue #119: route through AuthorizedSearch so the
                // candidate set is gated by `EffectiveScope.visible_collections`
                // before any similarity score is computed.
                let scope = self.ai_scope();
                let results =
                    if super::statement_frame::ReadFrame::visible_collections(&scope).is_some() {
                        crate::runtime::authorized_search::AuthorizedSearch::execute_similar(
                            self,
                            &scope,
                            collection,
                            &search_vector,
                            *limit,
                            *min_score,
                        )?
                    } else {
                        // Embedded / no-auth caller: keep legacy behaviour.
                        self.search_similar(collection, &search_vector, *limit, *min_score)?
                    };
                let mut result =
                    UnifiedResult::with_columns(vec!["entity_id".into(), "score".into()]);
                for sr in &results {
                    let mut record = UnifiedRecord::new();
                    record.set("entity_id", Value::UnsignedInteger(sr.entity_id.raw()));
                    record.set("score", Value::Float(sr.score as f64));
                    result.push(record);
                }
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "search_similar",
                    engine: "runtime-search",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            SearchCommand::Text {
                query,
                collection,
                limit,
                fuzzy,
                limit_param,
            } => {
                if limit_param.is_some() {
                    return Err(RedDBError::Query(
                        "SEARCH TEXT LIMIT $N parameter was not bound before execution".to_string(),
                    ));
                }
                let collections = collection.as_ref().map(|c| vec![c.clone()]);
                // Issue #119: gate the candidate set by visible_collections.
                let scope = self.ai_scope();
                let res =
                    if super::statement_frame::ReadFrame::visible_collections(&scope).is_some() {
                        crate::runtime::authorized_search::AuthorizedSearch::execute_text(
                            self,
                            &scope,
                            query.clone(),
                            collections,
                            None,
                            None,
                            None,
                            Some(*limit),
                            *fuzzy,
                        )?
                    } else {
                        self.search_text(
                            query.clone(),
                            collections,
                            None,
                            None,
                            None,
                            Some(*limit),
                            *fuzzy,
                        )?
                    };
                let mut result =
                    UnifiedResult::with_columns(vec!["entity_id".into(), "score".into()]);
                for item in &res.matches {
                    let mut record = UnifiedRecord::new();
                    record.set("entity_id", Value::UnsignedInteger(item.entity.id.raw()));
                    record.set("score", Value::Float(item.score as f64));
                    result.push(record);
                }
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "search_text",
                    engine: "runtime-search",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            SearchCommand::Hybrid {
                vector,
                query,
                collection,
                limit,
                limit_param,
            } => {
                if limit_param.is_some() {
                    return Err(RedDBError::Query(
                        "SEARCH HYBRID LIMIT $N parameter was not bound before execution"
                            .to_string(),
                    ));
                }
                let res = self.search_hybrid(
                    vector.clone(),
                    query.clone(),
                    Some(*limit),
                    Some(vec![collection.clone()]),
                    None,
                    None,
                    None,
                    Vec::new(),
                    None,
                    None,
                    Some(*limit),
                )?;
                let mut result =
                    UnifiedResult::with_columns(vec!["entity_id".into(), "score".into()]);
                for item in &res.matches {
                    let mut record = UnifiedRecord::new();
                    record.set("entity_id", Value::UnsignedInteger(item.entity.id.raw()));
                    record.set("score", Value::Float(item.score as f64));
                    result.push(record);
                }
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "search_hybrid",
                    engine: "runtime-search",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            SearchCommand::Multimodal {
                query,
                collection,
                limit,
                limit_param,
            } => {
                if limit_param.is_some() {
                    return Err(RedDBError::Query(
                        "SEARCH MULTIMODAL LIMIT $N parameter was not bound before execution"
                            .to_string(),
                    ));
                }
                let collections = collection.as_ref().map(|c| vec![c.clone()]);
                let res =
                    self.search_multimodal(query.clone(), collections, None, None, Some(*limit))?;
                let mut result =
                    UnifiedResult::with_columns(vec!["entity_id".into(), "score".into()]);
                for item in &res.matches {
                    let mut record = UnifiedRecord::new();
                    record.set("entity_id", Value::UnsignedInteger(item.entity.id.raw()));
                    record.set("score", Value::Float(item.score as f64));
                    result.push(record);
                }
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "search_multimodal",
                    engine: "runtime-search",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            SearchCommand::Index {
                index,
                value,
                collection,
                limit,
                exact,
                limit_param,
            } => {
                if limit_param.is_some() {
                    return Err(RedDBError::Query(
                        "SEARCH INDEX LIMIT $N parameter was not bound before execution"
                            .to_string(),
                    ));
                }
                let collections = collection.as_ref().map(|c| vec![c.clone()]);
                let res = self.search_index(
                    index.clone(),
                    value.clone(),
                    *exact,
                    collections,
                    None,
                    None,
                    Some(*limit),
                )?;
                let mut result =
                    UnifiedResult::with_columns(vec!["entity_id".into(), "score".into()]);
                for item in &res.matches {
                    let mut record = UnifiedRecord::new();
                    record.set("entity_id", Value::UnsignedInteger(item.entity.id.raw()));
                    record.set("score", Value::Float(item.score as f64));
                    result.push(record);
                }
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "search_index",
                    engine: "runtime-search",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            SearchCommand::Context {
                query,
                field,
                collection,
                limit,
                depth,
                limit_param,
            } => {
                if limit_param.is_some() {
                    return Err(RedDBError::Query(
                        "SEARCH CONTEXT LIMIT $N parameter was not bound before execution"
                            .to_string(),
                    ));
                }
                use crate::application::SearchContextInput;
                // Issue #119: route through AuthorizedSearch so the
                // candidate set + every expansion bucket is bounded by
                // `EffectiveScope.visible_collections`.
                let input = SearchContextInput {
                    query: query.clone(),
                    field: field.clone(),
                    vector: None,
                    collections: collection.as_ref().map(|c| vec![c.clone()]),
                    graph_depth: Some(*depth),
                    graph_max_edges: None,
                    max_cross_refs: None,
                    follow_cross_refs: None,
                    expand_graph: None,
                    global_scan: None,
                    reindex: None,
                    limit: Some(*limit),
                    min_score: None,
                };
                let scope = self.ai_scope();
                let res =
                    if super::statement_frame::ReadFrame::visible_collections(&scope).is_some() {
                        crate::runtime::authorized_search::AuthorizedSearch::execute_context(
                            self, &scope, input,
                        )?
                    } else {
                        self.search_context(input)?
                    };
                let mut result = UnifiedResult::with_columns(vec![
                    "entity_id".into(),
                    "collection".into(),
                    "score".into(),
                    "discovery".into(),
                    "kind".into(),
                ]);
                let all_entities = res
                    .tables
                    .iter()
                    .map(|e| (e, "table"))
                    .chain(res.graph.nodes.iter().map(|e| (e, "graph_node")))
                    .chain(res.graph.edges.iter().map(|e| (e, "graph_edge")))
                    .chain(res.vectors.iter().map(|e| (e, "vector")))
                    .chain(res.documents.iter().map(|e| (e, "document")))
                    .chain(res.key_values.iter().map(|e| (e, "kv")));
                for (entity, kind) in all_entities {
                    let mut record = UnifiedRecord::new();
                    record.set("entity_id", Value::UnsignedInteger(entity.entity.id.raw()));
                    record.set("collection", Value::text(entity.collection.clone()));
                    record.set("score", Value::Float(entity.score as f64));
                    record.set("discovery", Value::text(format!("{:?}", entity.discovery)));
                    record.set("kind", Value::text(kind.to_string()));
                    result.push(record);
                }
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "search_context",
                    engine: "runtime-context",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            SearchCommand::SpatialRadius {
                center_lat,
                center_lon,
                radius_km,
                collection,
                column,
                limit,
                limit_param,
            } => {
                if limit_param.is_some() {
                    return Err(RedDBError::Query(
                        "SEARCH SPATIAL RADIUS LIMIT $N parameter was not bound before execution"
                            .to_string(),
                    ));
                }
                use crate::storage::unified::spatial_index::haversine_km;
                let _ = column; // Column indicates which field holds geo data
                let store = self.inner.db.store();
                let entities = store
                    .get_collection(collection)
                    .map(|m| m.query_all(|_| true))
                    .unwrap_or_default();

                let mut hits: Vec<(u64, f64)> = Vec::new();
                for entity in &entities {
                    // Extract lat/lon from GeoPoint values in entity data
                    if let Some((lat, lon)) = extract_geo_from_entity(entity) {
                        let dist = haversine_km(*center_lat, *center_lon, lat, lon);
                        if dist <= *radius_km {
                            hits.push((entity.id.raw(), dist));
                        }
                    }
                }
                hits.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
                hits.truncate(*limit);

                let mut result =
                    UnifiedResult::with_columns(vec!["entity_id".into(), "distance_km".into()]);
                for (id, dist) in &hits {
                    let mut record = UnifiedRecord::new();
                    record.set("entity_id", Value::UnsignedInteger(*id));
                    record.set("distance_km", Value::Float(*dist));
                    result.push(record);
                }
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "search_spatial_radius",
                    engine: "runtime-spatial",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            SearchCommand::SpatialBbox {
                min_lat,
                min_lon,
                max_lat,
                max_lon,
                collection,
                column,
                limit,
                limit_param,
            } => {
                if limit_param.is_some() {
                    return Err(RedDBError::Query(
                        "SEARCH SPATIAL BBOX LIMIT $N parameter was not bound before execution"
                            .to_string(),
                    ));
                }
                let _ = column;
                let store = self.inner.db.store();
                let entities = store
                    .get_collection(collection)
                    .map(|m| m.query_all(|_| true))
                    .unwrap_or_default();

                let mut result = UnifiedResult::with_columns(vec!["entity_id".into()]);
                let mut count = 0;
                for entity in &entities {
                    if count >= *limit {
                        break;
                    }
                    if let Some((lat, lon)) = extract_geo_from_entity(entity) {
                        if lat >= *min_lat && lat <= *max_lat && lon >= *min_lon && lon <= *max_lon
                        {
                            let mut record = UnifiedRecord::new();
                            record.set("entity_id", Value::UnsignedInteger(entity.id.raw()));
                            result.push(record);
                            count += 1;
                        }
                    }
                }
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "search_spatial_bbox",
                    engine: "runtime-spatial",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            SearchCommand::SpatialNearest {
                lat,
                lon,
                k,
                collection,
                column,
                k_param,
            } => {
                if k_param.is_some() {
                    return Err(RedDBError::Query(
                        "SEARCH SPATIAL NEAREST K $N parameter was not bound before execution"
                            .to_string(),
                    ));
                }
                use crate::storage::unified::spatial_index::haversine_km;
                let _ = column;
                let store = self.inner.db.store();
                let entities = store
                    .get_collection(collection)
                    .map(|m| m.query_all(|_| true))
                    .unwrap_or_default();

                let mut hits: Vec<(u64, f64)> = Vec::new();
                for entity in &entities {
                    if let Some((elat, elon)) = extract_geo_from_entity(entity) {
                        let dist = haversine_km(*lat, *lon, elat, elon);
                        hits.push((entity.id.raw(), dist));
                    }
                }
                hits.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
                hits.truncate(*k);

                let mut result =
                    UnifiedResult::with_columns(vec!["entity_id".into(), "distance_km".into()]);
                for (id, dist) in &hits {
                    let mut record = UnifiedRecord::new();
                    record.set("entity_id", Value::UnsignedInteger(*id));
                    record.set("distance_km", Value::Float(*dist));
                    result.push(record);
                }
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "search_spatial_nearest",
                    engine: "runtime-spatial",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
        }
    }
}

fn apply_graph_order_and_limit(
    result: &mut UnifiedResult,
    statement: &str,
    order_by: Option<&GraphCommandOrderBy>,
    limit: Option<usize>,
) -> RedDBResult<()> {
    if let Some(order) = order_by {
        let column = graph_order_metric_column(statement, &order.metric)?;
        let columns = result.columns.clone();
        result.records.sort_by(|left, right| {
            let cmp = compare_graph_values(left.get(column), right.get(column));
            let cmp = if order.ascending { cmp } else { cmp.reverse() };
            if cmp == Ordering::Equal {
                compare_graph_rows(left, right, &columns)
            } else {
                cmp
            }
        });
    }
    if let Some(limit) = limit {
        result.records.truncate(limit);
    }
    Ok(())
}

fn graph_order_metric_column(statement: &str, metric: &str) -> RedDBResult<&'static str> {
    let metric = metric.to_ascii_lowercase();
    match (statement, metric.as_str()) {
        ("graph_centrality", "score" | "centrality_score") => Ok("score"),
        ("graph_community", "size" | "community_size") => Ok("size"),
        ("graph_components", "size" | "component_size") => Ok("size"),
        ("graph_shortest_path", "hop_count" | "total_weight" | "nodes_visited") => {
            Ok(match metric.as_str() {
                "total_weight" => "total_weight",
                "nodes_visited" => "nodes_visited",
                _ => "hop_count",
            })
        }
        _ => Err(RedDBError::Query(format!(
            "unsupported ORDER BY metric '{metric}' for GRAPH {}",
            statement.trim_start_matches("graph_")
        ))),
    }
}

fn compare_graph_rows(left: &UnifiedRecord, right: &UnifiedRecord, columns: &[String]) -> Ordering {
    for column in columns {
        let cmp = compare_graph_values(left.get(column), right.get(column));
        if cmp != Ordering::Equal {
            return cmp;
        }
    }
    Ordering::Equal
}

fn compare_graph_values(left: Option<&Value>, right: Option<&Value>) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(Value::Null), Some(Value::Null)) => Ordering::Equal,
        (Some(Value::Null), Some(_)) => Ordering::Less,
        (Some(_), Some(Value::Null)) => Ordering::Greater,
        (Some(Value::Integer(left)), Some(Value::Integer(right))) => left.cmp(right),
        (Some(Value::UnsignedInteger(left)), Some(Value::UnsignedInteger(right))) => {
            left.cmp(right)
        }
        (Some(Value::Float(left)), Some(Value::Float(right))) => {
            left.partial_cmp(right).unwrap_or(Ordering::Equal)
        }
        (Some(Value::Integer(left)), Some(Value::Float(right))) => {
            (*left as f64).partial_cmp(right).unwrap_or(Ordering::Equal)
        }
        (Some(Value::Float(left)), Some(Value::Integer(right))) => left
            .partial_cmp(&(*right as f64))
            .unwrap_or(Ordering::Equal),
        (Some(Value::UnsignedInteger(left)), Some(Value::Float(right))) => {
            (*left as f64).partial_cmp(right).unwrap_or(Ordering::Equal)
        }
        (Some(Value::Float(left)), Some(Value::UnsignedInteger(right))) => left
            .partial_cmp(&(*right as f64))
            .unwrap_or(Ordering::Equal),
        (Some(Value::Integer(left)), Some(Value::UnsignedInteger(right))) => {
            (*left as i128).cmp(&(*right as i128))
        }
        (Some(Value::UnsignedInteger(left)), Some(Value::Integer(right))) => {
            (*left as i128).cmp(&(*right as i128))
        }
        (Some(Value::Timestamp(left)), Some(Value::Timestamp(right))) => left.cmp(right),
        (Some(Value::Text(left)), Some(Value::Text(right))) => left.cmp(right),
        (Some(Value::Boolean(left)), Some(Value::Boolean(right))) => left.cmp(right),
        (Some(left), Some(right)) => format!("{left:?}").cmp(&format!("{right:?}")),
    }
}

// =============================================================================
// Conversion helpers for string -> enum
// =============================================================================

fn parse_direction(s: &str) -> RedDBResult<RuntimeGraphDirection> {
    match s.to_lowercase().as_str() {
        "outgoing" | "out" => Ok(RuntimeGraphDirection::Outgoing),
        "incoming" | "in" => Ok(RuntimeGraphDirection::Incoming),
        "both" | "any" => Ok(RuntimeGraphDirection::Both),
        _ => Err(RedDBError::Query(format!(
            "unknown direction: '{s}', expected outgoing|incoming|both"
        ))),
    }
}

fn parse_path_algorithm(s: &str) -> RedDBResult<RuntimeGraphPathAlgorithm> {
    match s.to_lowercase().as_str() {
        "bfs" => Ok(RuntimeGraphPathAlgorithm::Bfs),
        "dijkstra" => Ok(RuntimeGraphPathAlgorithm::Dijkstra),
        "astar" | "a*" => Ok(RuntimeGraphPathAlgorithm::AStar),
        "bellman_ford" | "bellmanford" => Ok(RuntimeGraphPathAlgorithm::BellmanFord),
        _ => Err(RedDBError::Query(format!(
            "unknown path algorithm: '{s}', expected bfs|dijkstra|astar|bellman_ford"
        ))),
    }
}

fn parse_traversal_strategy(s: &str) -> RedDBResult<RuntimeGraphTraversalStrategy> {
    match s.to_lowercase().as_str() {
        "bfs" => Ok(RuntimeGraphTraversalStrategy::Bfs),
        "dfs" => Ok(RuntimeGraphTraversalStrategy::Dfs),
        _ => Err(RedDBError::Query(format!(
            "unknown traversal strategy: '{s}', expected bfs|dfs"
        ))),
    }
}

fn parse_centrality_algorithm(s: &str) -> RedDBResult<RuntimeGraphCentralityAlgorithm> {
    match s.to_lowercase().as_str() {
        "degree" => Ok(RuntimeGraphCentralityAlgorithm::Degree),
        "closeness" => Ok(RuntimeGraphCentralityAlgorithm::Closeness),
        "betweenness" => Ok(RuntimeGraphCentralityAlgorithm::Betweenness),
        "eigenvector" => Ok(RuntimeGraphCentralityAlgorithm::Eigenvector),
        "pagerank" | "page_rank" => Ok(RuntimeGraphCentralityAlgorithm::PageRank),
        _ => Err(RedDBError::Query(format!(
            "unknown centrality algorithm: '{s}', expected degree|closeness|betweenness|eigenvector|pagerank"
        ))),
    }
}

fn parse_community_algorithm(s: &str) -> RedDBResult<RuntimeGraphCommunityAlgorithm> {
    match s.to_lowercase().as_str() {
        "label_propagation" | "labelpropagation" => {
            Ok(RuntimeGraphCommunityAlgorithm::LabelPropagation)
        }
        "louvain" => Ok(RuntimeGraphCommunityAlgorithm::Louvain),
        _ => Err(RedDBError::Query(format!(
            "unknown community algorithm: '{s}', expected label_propagation|louvain"
        ))),
    }
}

fn parse_components_mode(s: &str) -> RedDBResult<RuntimeGraphComponentsMode> {
    match s.to_lowercase().as_str() {
        "connected" => Ok(RuntimeGraphComponentsMode::Connected),
        "weak" | "weakly_connected" => Ok(RuntimeGraphComponentsMode::Weak),
        "strong" | "strongly_connected" => Ok(RuntimeGraphComponentsMode::Strong),
        _ => Err(RedDBError::Query(format!(
            "unknown components mode: '{s}', expected connected|weak|strong"
        ))),
    }
}

/// Extract (latitude, longitude) from an entity.
///
/// Looks for GeoPoint values in the entity data (row columns or node properties)
/// or dedicated lat/lon fields. Returns degrees.
fn extract_geo_from_entity(entity: &UnifiedEntity) -> Option<(f64, f64)> {
    match &entity.data {
        EntityData::Row(row) => {
            // Search named columns for GeoPoint or lat/lon pairs
            if let Some(ref named) = row.named {
                // Direct GeoPoint value
                for value in named.values() {
                    if let Value::GeoPoint(lat_micro, lon_micro) = value {
                        return Some((
                            *lat_micro as f64 / 1_000_000.0,
                            *lon_micro as f64 / 1_000_000.0,
                        ));
                    }
                }
                // Try lat/lon or latitude/longitude named fields
                let lat =
                    named
                        .get("lat")
                        .or_else(|| named.get("latitude"))
                        .and_then(|v| match v {
                            Value::Float(f) => Some(*f),
                            Value::Integer(i) => Some(*i as f64),
                            _ => None,
                        });
                let lon = named
                    .get("lon")
                    .or_else(|| named.get("lng"))
                    .or_else(|| named.get("longitude"))
                    .and_then(|v| match v {
                        Value::Float(f) => Some(*f),
                        Value::Integer(i) => Some(*i as f64),
                        _ => None,
                    });
                if let (Some(la), Some(lo)) = (lat, lon) {
                    return Some((la, lo));
                }
            }
            // Search positional columns for GeoPoint
            for value in &row.columns {
                if let Value::GeoPoint(lat_micro, lon_micro) = value {
                    return Some((
                        *lat_micro as f64 / 1_000_000.0,
                        *lon_micro as f64 / 1_000_000.0,
                    ));
                }
            }
            None
        }
        EntityData::Node(node) => {
            // Search node properties
            for value in node.properties.values() {
                if let Value::GeoPoint(lat_micro, lon_micro) = value {
                    return Some((
                        *lat_micro as f64 / 1_000_000.0,
                        *lon_micro as f64 / 1_000_000.0,
                    ));
                }
            }
            let lat = node
                .properties
                .get("lat")
                .or_else(|| node.properties.get("latitude"))
                .and_then(|v| match v {
                    Value::Float(f) => Some(*f),
                    Value::Integer(i) => Some(*i as f64),
                    _ => None,
                });
            let lon = node
                .properties
                .get("lon")
                .or_else(|| node.properties.get("lng"))
                .or_else(|| node.properties.get("longitude"))
                .and_then(|v| match v {
                    Value::Float(f) => Some(*f),
                    Value::Integer(i) => Some(*i as f64),
                    _ => None,
                });
            if let (Some(la), Some(lo)) = (lat, lon) {
                return Some((la, lo));
            }
            None
        }
        _ => None,
    }
}
