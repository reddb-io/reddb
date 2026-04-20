//! Execution of GRAPH and SEARCH SQL-like commands.
//!
//! Maps parsed `GraphCommand` and `SearchCommand` AST nodes to the existing
//! runtime graph analytics and search methods, returning results wrapped in
//! `RuntimeQueryResult`.

use super::*;

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
            } => {
                let dir = parse_direction(direction)?;
                let res = self.graph_neighborhood(source, dir, *depth as usize, None, None)?;
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
            GraphCommand::Properties => {
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
            } => {
                let dir = parse_direction(direction)?;
                let strat = parse_traversal_strategy(strategy)?;
                let res = self.graph_traverse(source, dir, *depth as usize, strat, None, None)?;
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
            GraphCommand::Centrality { algorithm } => {
                let alg = parse_centrality_algorithm(algorithm)?;
                let res = self.graph_centrality(alg, 100, false, None, None, None, None)?;
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
            GraphCommand::Components { mode } => {
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
                global_record.set("node_id", Value::text("__global__".into()));
                global_record.set("label", Value::text("global_clustering".into()));
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
            } => {
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
                    let response =
                        crate::ai::openai_embeddings(crate::ai::OpenAiEmbeddingRequest {
                            api_key,
                            model,
                            inputs: vec![query_text.clone()],
                            dimensions: None,
                            api_base: provider.resolve_api_base(),
                        })?;
                    response.embeddings.into_iter().next().ok_or_else(|| {
                        RedDBError::Query("embedding API returned no vectors".to_string())
                    })?
                } else {
                    vector.clone()
                };
                let results =
                    self.search_similar(collection, &search_vector, *limit, *min_score)?;
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
            } => {
                let collections = collection.as_ref().map(|c| vec![c.clone()]);
                let res = self.search_text(
                    query.clone(),
                    collections,
                    None,
                    None,
                    None,
                    Some(*limit),
                    *fuzzy,
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
            } => {
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
            } => {
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
            } => {
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
            } => {
                use crate::application::SearchContextInput;
                let res = self.search_context(SearchContextInput {
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
                })?;
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
            } => {
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
            } => {
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
            } => {
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
