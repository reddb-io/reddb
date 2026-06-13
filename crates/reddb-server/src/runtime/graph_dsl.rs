use super::*;

pub(super) fn materialize_graph(store: &UnifiedStore) -> RedDBResult<GraphStore> {
    materialize_graph_with_projection(store, None)
}

pub(super) fn materialize_graph_with_projection(
    store: &UnifiedStore,
    projection: Option<&RuntimeGraphProjection>,
) -> RedDBResult<GraphStore> {
    let graph = GraphStore::new();
    // Phase 1.2 MVCC universal: capture the current connection's
    // snapshot before `query_all` spawns parallel scan threads — the
    // thread-local CURRENT_SNAPSHOT does not propagate into spawned
    // workers, so we hand the context to the filter closure by move.
    let snap_ctx = crate::runtime::impl_core::capture_current_snapshot();
    let entities = store.query_all(move |e| {
        crate::runtime::impl_core::entity_visible_with_context(snap_ctx.as_ref(), e)
    });
    let node_label_filters = projection
        .and_then(|projection| normalize_token_filter_list(projection.node_labels.clone()));
    let node_type_filters = projection
        .and_then(|projection| normalize_token_filter_list(projection.node_types.clone()));
    let edge_label_filters =
        projection.and_then(|projection| normalize_edge_filters(projection.edge_labels.clone()));
    let mut allowed_nodes = HashSet::new();

    for (_, entity) in &entities {
        if let EntityKind::GraphNode(ref node) = &entity.kind {
            if !matches_graph_node_projection(
                &node.label,
                &node.node_type,
                node_label_filters.as_ref(),
                node_type_filters.as_ref(),
            ) {
                continue;
            }
            graph
                .add_node_with_label(
                    &entity.id.raw().to_string(),
                    &node.label,
                    &graph_node_label(&node.node_type),
                )
                .map_err(|err| RedDBError::Query(err.to_string()))?;
            allowed_nodes.insert(entity.id.raw().to_string());
        }
    }

    for (_, entity) in &entities {
        if let EntityKind::GraphEdge(ref edge) = &entity.kind {
            if !allowed_nodes.contains(&edge.from_node) || !allowed_nodes.contains(&edge.to_node) {
                continue;
            }
            if !matches_graph_edge_projection(&edge.label, edge_label_filters.as_ref()) {
                continue;
            }
            let resolved_weight = match &entity.data {
                EntityData::Edge(e) => e.weight,
                _ => edge.weight as f32 / 1000.0,
            };

            graph
                .add_edge_with_label(
                    &edge.from_node,
                    &edge.to_node,
                    &graph_edge_label(&edge.label),
                    resolved_weight,
                )
                .map_err(|err| RedDBError::Query(err.to_string()))?;
        }
    }

    Ok(graph)
}

/// Lazy graph materialization — only loads nodes reachable from seed IDs via BFS.
/// Much faster than materialize_graph() when you only need a subgraph.
pub(super) fn materialize_graph_lazy(
    store: &UnifiedStore,
    seed_entity_ids: &[u64],
    max_depth: usize,
) -> RedDBResult<GraphStore> {
    let graph = GraphStore::new();
    let mut visited_nodes: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();

    // Phase 1: Load seed nodes
    for &id in seed_entity_ids {
        let id_str = id.to_string();
        if visited_nodes.contains(&id_str) {
            continue;
        }
        if let Some((_, entity)) = store.get_any(EntityId::new(id)) {
            if let EntityKind::GraphNode(ref node) = &entity.kind {
                let _ = graph.add_node_with_label(
                    &id_str,
                    &node.label,
                    &graph_node_label(&node.node_type),
                );
                visited_nodes.insert(id_str.clone());
                queue.push_back((id_str, 0));
            }
        }
    }

    // Phase 2: BFS — load neighbors on demand
    // Collect edges from all collections in parallel
    let collections = store.list_collections();
    let use_parallel = collections.len() > 1 && crate::runtime::SystemInfo::should_parallelize();
    let all_edges: Vec<UnifiedEntity> = if use_parallel {
        let store_ref = &store;
        let edge_batches: Vec<Vec<UnifiedEntity>> = std::thread::scope(|s| {
            collections
                .iter()
                .map(|col| {
                    s.spawn(move || {
                        store_ref
                            .get_collection(col)
                            .map(|m| m.query_all(|e| matches!(e.kind, EntityKind::GraphEdge(_))))
                            .unwrap_or_default()
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|h| h.join().unwrap_or_default())
                .collect()
        });
        edge_batches.into_iter().flatten().collect()
    } else {
        collections
            .iter()
            .flat_map(|col| {
                store
                    .get_collection(col)
                    .map(|m| m.query_all(|e| matches!(e.kind, EntityKind::GraphEdge(_))))
                    .unwrap_or_default()
            })
            .collect()
    };

    // Build adjacency from edges
    let mut adjacency: HashMap<String, Vec<(String, String, String, f32)>> = HashMap::new();
    for entity in &all_edges {
        if let EntityKind::GraphEdge(ref edge) = &entity.kind {
            let w = match &entity.data {
                EntityData::Edge(e) => e.weight,
                _ => edge.weight as f32 / 1000.0,
            };
            adjacency.entry(edge.from_node.clone()).or_default().push((
                edge.to_node.clone(),
                edge.label.clone(),
                entity.id.raw().to_string(),
                w,
            ));
            adjacency.entry(edge.to_node.clone()).or_default().push((
                edge.from_node.clone(),
                edge.label.clone(),
                entity.id.raw().to_string(),
                w,
            ));
        }
    }

    while let Some((node_id, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        if let Some(neighbors) = adjacency.get(&node_id) {
            for (neighbor_id, label, _edge_id, weight) in neighbors {
                // Add neighbor node if not visited
                if !visited_nodes.contains(neighbor_id) {
                    if let Ok(parsed) = neighbor_id.parse::<u64>() {
                        if let Some((_, entity)) = store.get_any(EntityId::new(parsed)) {
                            if let EntityKind::GraphNode(ref node) = &entity.kind {
                                let _ = graph.add_node_with_label(
                                    neighbor_id,
                                    &node.label,
                                    &graph_node_label(&node.node_type),
                                );
                                visited_nodes.insert(neighbor_id.clone());
                                queue.push_back((neighbor_id.clone(), depth + 1));
                            }
                        }
                    }
                }
                // Add edge
                if visited_nodes.contains(neighbor_id) {
                    let _ = graph.add_edge_with_label(
                        &node_id,
                        neighbor_id,
                        &graph_edge_label(label),
                        *weight,
                    );
                }
            }
        }
    }

    Ok(graph)
}

pub(super) fn materialize_graph_node_properties(
    store: &UnifiedStore,
) -> RedDBResult<HashMap<String, HashMap<String, Value>>> {
    let mut node_properties = HashMap::new();

    for (_, entity) in store.query_all(|_| true) {
        if let (EntityKind::GraphNode(_), EntityData::Node(node)) = (&entity.kind, &entity.data) {
            node_properties.insert(entity.id.raw().to_string(), node.properties.clone());
        }
    }

    Ok(node_properties)
}

pub(super) fn materialize_graph_edge_properties(
    store: &UnifiedStore,
) -> RedDBResult<crate::storage::query::unified::EdgeProperties> {
    let mut edge_properties = HashMap::new();

    for (_, entity) in store.query_all(|_| true) {
        if let (EntityKind::GraphEdge(edge), EntityData::Edge(edge_data)) =
            (&entity.kind, &entity.data)
        {
            edge_properties.insert(
                (
                    edge.from_node.clone(),
                    graph_edge_label(&edge.label),
                    edge.to_node.clone(),
                ),
                edge_data.properties.clone(),
            );
        }
    }

    Ok(edge_properties)
}

pub(super) fn normalize_token_filter_list(values: Option<Vec<String>>) -> Option<BTreeSet<String>> {
    values
        .map(|values| {
            values
                .into_iter()
                .map(|value| normalize_graph_token(&value))
                .filter(|value| !value.is_empty())
                .collect::<BTreeSet<_>>()
        })
        .filter(|set| !set.is_empty())
}

pub(super) fn matches_graph_node_projection(
    label: &str,
    node_type: &str,
    label_filters: Option<&BTreeSet<String>>,
    node_type_filters: Option<&BTreeSet<String>>,
) -> bool {
    let label_ok =
        label_filters.is_none_or(|filters| filters.contains(&normalize_graph_token(label)));
    let node_type_ok =
        node_type_filters.is_none_or(|filters| filters.contains(&normalize_graph_token(node_type)));
    label_ok && node_type_ok
}

pub(super) fn matches_graph_edge_projection(
    label: &str,
    edge_filters: Option<&BTreeSet<String>>,
) -> bool {
    edge_filters.is_none_or(|filters| filters.contains(&normalize_graph_token(label)))
}

pub(super) fn ensure_graph_node(graph: &GraphStore, id: &str) -> RedDBResult<()> {
    if graph.has_node(id) {
        Ok(())
    } else {
        Err(RedDBError::NotFound(id.to_string()))
    }
}

/// Resolve a user-supplied graph node reference to its canonical entity id.
///
/// Accepts either a numeric entity id (e.g. `"177"`) — returned as-is when the
/// node exists — or a node label (e.g. `"cinderella"`) resolved via the label
/// secondary index. Errors when the label resolves to more than one node, so
/// callers can fall back to the numeric id form.
pub(super) fn resolve_graph_node_id(graph: &GraphStore, input: &str) -> RedDBResult<String> {
    if graph.has_node(input) {
        return Ok(input.to_string());
    }
    let matches = graph.nodes_by_label(input);
    match matches.len() {
        0 => Err(RedDBError::NotFound(input.to_string())),
        1 => Ok(matches.into_iter().next().unwrap().id),
        n => Err(RedDBError::Query(format!(
            "ambiguous graph node reference '{input}': matches {n} nodes by label; use the numeric id"
        ))),
    }
}

pub(super) fn stored_node_to_runtime(node: StoredNode) -> RuntimeGraphNode {
    RuntimeGraphNode {
        id: node.id,
        label: node.label,
        node_type: node.node_type.as_str().to_string(),
        out_edge_count: node.out_edge_count,
        in_edge_count: node.in_edge_count,
    }
}

pub(super) fn path_to_runtime(
    graph: &GraphStore,
    path: &crate::storage::engine::pathfinding::Path,
) -> RuntimeGraphPath {
    let nodes = path
        .nodes
        .iter()
        .filter_map(|id| graph.get_node(id))
        .map(stored_node_to_runtime)
        .collect();

    let mut edges = Vec::new();
    for index in 0..path.edge_types.len() {
        let Some(source) = path.nodes.get(index) else {
            continue;
        };
        let Some(target) = path.nodes.get(index + 1) else {
            continue;
        };
        let Some(edge_type) = path.edge_types.get(index) else {
            continue;
        };
        let weight = graph
            .outgoing_edges(source)
            .into_iter()
            .find(|(candidate_type, candidate_target, _)| {
                candidate_type.as_str() == edge_type.as_str() && candidate_target == target
            })
            .map(|(_, _, weight)| weight)
            .unwrap_or(0.0);
        edges.push(RuntimeGraphEdge {
            source: source.clone(),
            target: target.clone(),
            edge_type: edge_type.as_str().to_string(),
            weight,
        });
    }

    RuntimeGraphPath {
        hop_count: path.len(),
        total_weight: path.total_weight,
        nodes,
        edges,
    }
}

pub(super) fn cycle_to_runtime(
    graph: &GraphStore,
    cycle: crate::storage::engine::Cycle,
) -> RuntimeGraphPath {
    let nodes = cycle
        .nodes
        .iter()
        .filter_map(|id| graph.get_node(id))
        .map(stored_node_to_runtime)
        .collect::<Vec<_>>();
    let mut edges = Vec::new();
    let mut total_weight = 0.0;

    for window in cycle.nodes.windows(2) {
        let Some(source) = window.first() else {
            continue;
        };
        let Some(target) = window.get(1) else {
            continue;
        };
        if let Some((edge_type, _, weight)) = graph
            .outgoing_edges(source)
            .into_iter()
            .find(|(_, candidate_target, _)| candidate_target == target)
        {
            total_weight += weight as f64;
            edges.push(RuntimeGraphEdge {
                source: source.clone(),
                target: target.clone(),
                edge_type: edge_type.as_str().to_string(),
                weight,
            });
        }
    }

    RuntimeGraphPath {
        hop_count: cycle.length,
        total_weight,
        nodes,
        edges,
    }
}

pub(super) fn normalize_edge_filters(edge_labels: Option<Vec<String>>) -> Option<BTreeSet<String>> {
    edge_labels
        .map(|labels| {
            labels
                .into_iter()
                .map(|label| normalize_graph_token(&graph_edge_label(&label)))
                .filter(|label| !label.is_empty())
                .collect()
        })
        .filter(|set: &BTreeSet<String>| !set.is_empty())
}

pub(super) fn merge_edge_filters(
    edge_labels: Option<Vec<String>>,
    projection: Option<&RuntimeGraphProjection>,
) -> Option<BTreeSet<String>> {
    let mut merged = BTreeSet::new();

    if let Some(filters) = normalize_edge_filters(edge_labels) {
        merged.extend(filters);
    }

    if let Some(filters) =
        projection.and_then(|projection| normalize_edge_filters(projection.edge_labels.clone()))
    {
        merged.extend(filters);
    }

    if merged.is_empty() {
        None
    } else {
        Some(merged)
    }
}

pub(super) fn merge_runtime_projection(
    base: Option<RuntimeGraphProjection>,
    overlay: Option<RuntimeGraphProjection>,
) -> Option<RuntimeGraphProjection> {
    let merge_list =
        |left: Option<Vec<String>>, right: Option<Vec<String>>| -> Option<Vec<String>> {
            let mut values = BTreeSet::new();
            if let Some(left) = left {
                values.extend(left);
            }
            if let Some(right) = right {
                values.extend(right);
            }
            if values.is_empty() {
                None
            } else {
                Some(values.into_iter().collect())
            }
        };

    let _ = base.clone().or(overlay.clone())?;

    Some(RuntimeGraphProjection {
        node_labels: merge_list(
            base.as_ref()
                .and_then(|projection| projection.node_labels.clone()),
            overlay
                .as_ref()
                .and_then(|projection| projection.node_labels.clone()),
        ),
        node_types: merge_list(
            base.as_ref()
                .and_then(|projection| projection.node_types.clone()),
            overlay
                .as_ref()
                .and_then(|projection| projection.node_types.clone()),
        ),
        edge_labels: merge_list(
            base.as_ref()
                .and_then(|projection| projection.edge_labels.clone()),
            overlay
                .as_ref()
                .and_then(|projection| projection.edge_labels.clone()),
        ),
    })
}

pub(super) fn edge_allowed(edge_label: &str, filters: Option<&BTreeSet<String>>) -> bool {
    filters.is_none_or(|filters| filters.contains(&normalize_graph_token(edge_label)))
}

pub(super) fn graph_adjacent_edges(
    graph: &GraphStore,
    node: &str,
    direction: RuntimeGraphDirection,
    edge_filters: Option<&BTreeSet<String>>,
) -> Vec<(String, RuntimeGraphEdge)> {
    let mut adjacent = Vec::new();

    if matches!(
        direction,
        RuntimeGraphDirection::Outgoing | RuntimeGraphDirection::Both
    ) {
        for (edge_type, target, weight) in graph.outgoing_edges(node) {
            if edge_allowed(edge_type.as_str(), edge_filters) {
                adjacent.push((
                    target.clone(),
                    RuntimeGraphEdge {
                        source: node.to_string(),
                        target,
                        edge_type: edge_type.as_str().to_string(),
                        weight,
                    },
                ));
            }
        }
    }

    if matches!(
        direction,
        RuntimeGraphDirection::Incoming | RuntimeGraphDirection::Both
    ) {
        for (edge_type, source, weight) in graph.incoming_edges(node) {
            if edge_allowed(edge_type.as_str(), edge_filters) {
                adjacent.push((
                    source.clone(),
                    RuntimeGraphEdge {
                        source,
                        target: node.to_string(),
                        edge_type: edge_type.as_str().to_string(),
                        weight,
                    },
                ));
            }
        }
    }

    adjacent
}

pub(super) fn push_runtime_edge(
    edges: &mut Vec<RuntimeGraphEdge>,
    seen_edges: &mut HashSet<(String, String, String, u32)>,
    edge: RuntimeGraphEdge,
) {
    let key = (
        edge.source.clone(),
        edge.target.clone(),
        edge.edge_type.clone(),
        edge.weight.to_bits(),
    );
    if seen_edges.insert(key) {
        edges.push(edge);
    }
}

#[derive(Clone)]
pub(super) struct RuntimeDijkstraState {
    node: String,
    cost: f64,
}

impl PartialEq for RuntimeDijkstraState {
    fn eq(&self, other: &Self) -> bool {
        self.node == other.node && self.cost == other.cost
    }
}

impl Eq for RuntimeDijkstraState {}

impl Ord for RuntimeDijkstraState {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .cost
            .partial_cmp(&self.cost)
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for RuntimeDijkstraState {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub(super) fn shortest_path_runtime(
    graph: &GraphStore,
    source: &str,
    target: &str,
    direction: RuntimeGraphDirection,
    algorithm: RuntimeGraphPathAlgorithm,
    edge_filters: Option<&BTreeSet<String>>,
) -> RedDBResult<RuntimeGraphPathResult> {
    let mut nodes_visited = 0;
    let (path, negative_cycle_detected) = match algorithm {
        RuntimeGraphPathAlgorithm::Bfs => {
            let mut queue = VecDeque::new();
            let mut visited = HashSet::new();
            let mut previous: HashMap<String, (String, RuntimeGraphEdge)> = HashMap::new();

            queue.push_back(source.to_string());
            visited.insert(source.to_string());

            while let Some(current) = queue.pop_front() {
                nodes_visited += 1;
                if current == target {
                    break;
                }
                let mut adjacent = graph_adjacent_edges(graph, &current, direction, edge_filters);
                adjacent.sort_by(|left, right| left.0.cmp(&right.0));
                for (neighbor, edge) in adjacent {
                    if visited.insert(neighbor.clone()) {
                        previous.insert(neighbor.clone(), (current.clone(), edge));
                        queue.push_back(neighbor);
                    }
                }
            }

            (rebuild_runtime_path(graph, source, target, &previous), None)
        }
        RuntimeGraphPathAlgorithm::Dijkstra | RuntimeGraphPathAlgorithm::AStar => {
            let mut dist: HashMap<String, f64> = HashMap::new();
            let mut previous: HashMap<String, (String, RuntimeGraphEdge)> = HashMap::new();
            let mut heap = BinaryHeap::new();

            dist.insert(source.to_string(), 0.0);
            heap.push(RuntimeDijkstraState {
                node: source.to_string(),
                cost: 0.0,
            });

            while let Some(RuntimeDijkstraState { node, cost }) = heap.pop() {
                nodes_visited += 1;
                if node == target {
                    break;
                }
                if let Some(best) = dist.get(&node) {
                    if cost > *best {
                        continue;
                    }
                }

                let mut adjacent = graph_adjacent_edges(graph, &node, direction, edge_filters);
                adjacent.sort_by(|left, right| left.0.cmp(&right.0));
                for (neighbor, edge) in adjacent {
                    let next_cost = cost + edge.weight as f64;
                    if dist.get(&neighbor).is_none_or(|best| next_cost < *best) {
                        dist.insert(neighbor.clone(), next_cost);
                        previous.insert(neighbor.clone(), (node.clone(), edge));
                        heap.push(RuntimeDijkstraState {
                            node: neighbor,
                            cost: next_cost,
                        });
                    }
                }
            }

            (rebuild_runtime_path(graph, source, target, &previous), None)
        }
        RuntimeGraphPathAlgorithm::BellmanFord => {
            let nodes: Vec<String> = graph.iter_nodes().map(|node| node.id.clone()).collect();
            let mut dist: HashMap<String, f64> = nodes
                .iter()
                .map(|node| (node.clone(), f64::INFINITY))
                .collect();
            let mut previous: HashMap<String, (String, RuntimeGraphEdge)> = HashMap::new();

            dist.insert(source.to_string(), 0.0);

            for _ in 0..nodes.len().saturating_sub(1) {
                let mut changed = false;

                for node in &nodes {
                    nodes_visited += 1;
                    let Some(current_dist) = dist.get(node).copied() else {
                        continue;
                    };
                    if !current_dist.is_finite() {
                        continue;
                    }

                    let mut adjacent = graph_adjacent_edges(graph, node, direction, edge_filters);
                    adjacent.sort_by(|left, right| left.0.cmp(&right.0));
                    for (neighbor, edge) in adjacent {
                        let next_cost = current_dist + edge.weight as f64;
                        if dist.get(&neighbor).is_none_or(|best| next_cost < *best) {
                            dist.insert(neighbor.clone(), next_cost);
                            previous.insert(neighbor, (node.clone(), edge));
                            changed = true;
                        }
                    }
                }

                if !changed {
                    break;
                }
            }

            let mut has_negative_cycle = false;
            for node in &nodes {
                let Some(current_dist) = dist.get(node).copied() else {
                    continue;
                };
                if !current_dist.is_finite() {
                    continue;
                }

                let adjacent = graph_adjacent_edges(graph, node, direction, edge_filters);
                for (neighbor, edge) in adjacent {
                    let next_cost = current_dist + edge.weight as f64;
                    if dist.get(&neighbor).is_none_or(|best| next_cost < *best) {
                        has_negative_cycle = true;
                        break;
                    }
                }

                if has_negative_cycle {
                    break;
                }
            }

            let path = if has_negative_cycle {
                None
            } else {
                rebuild_runtime_path(graph, source, target, &previous)
            };
            (path, Some(has_negative_cycle))
        }
    };

    Ok(RuntimeGraphPathResult {
        source: source.to_string(),
        target: target.to_string(),
        direction,
        algorithm,
        nodes_visited,
        negative_cycle_detected,
        path,
    })
}

pub(super) fn rebuild_runtime_path(
    graph: &GraphStore,
    source: &str,
    target: &str,
    previous: &HashMap<String, (String, RuntimeGraphEdge)>,
) -> Option<RuntimeGraphPath> {
    if source != target && !previous.contains_key(target) {
        return None;
    }

    let mut node_ids = vec![target.to_string()];
    let mut edges = Vec::new();
    let mut current = target.to_string();

    while current != source {
        let (parent, edge) = previous.get(&current)?.clone();
        edges.push(edge);
        node_ids.push(parent.clone());
        current = parent;
    }

    node_ids.reverse();
    edges.reverse();

    let total_weight = edges.iter().map(|edge| edge.weight as f64).sum();
    let nodes = node_ids
        .iter()
        .filter_map(|id| graph.get_node(id))
        .map(stored_node_to_runtime)
        .collect();

    Some(RuntimeGraphPath {
        hop_count: node_ids.len().saturating_sub(1),
        total_weight,
        nodes,
        edges,
    })
}

pub(super) fn top_runtime_scores(
    graph: &GraphStore,
    scores: HashMap<String, f64>,
    top_k: usize,
) -> Vec<RuntimeGraphCentralityScore> {
    let mut pairs: Vec<_> = scores.into_iter().collect();
    pairs.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    pairs.truncate(top_k.max(1));
    pairs
        .into_iter()
        .filter_map(|(node_id, score)| {
            graph
                .get_node(&node_id)
                .map(|node| RuntimeGraphCentralityScore {
                    node: stored_node_to_runtime(node),
                    score,
                })
        })
        .collect()
}

/// Normalise a user-supplied node-type token to its canonical lower-snake-case
/// form. Pentest-flavoured aliases (`tech`, `cert`) are kept as a courtesy
/// but the result is just a label string the caller can intern into the
/// [`crate::storage::engine::graph_store::LabelRegistry`].
pub(super) fn graph_node_label(input: &str) -> String {
    let token = normalize_graph_token(input);
    match token.as_str() {
        "host" | "service" | "credential" | "vulnerability" | "endpoint" | "technology"
        | "user" | "domain" | "certificate" => token,
        "tech" => "technology".to_string(),
        "cert" => "certificate".to_string(),
        // Unknown token: pass through so callers can register new labels.
        _ if !token.is_empty() => token,
        _ => "endpoint".to_string(),
    }
}

/// Edge-label counterpart to [`graph_node_label`].
pub(super) fn graph_edge_label(input: &str) -> String {
    let token = normalize_graph_token(input);
    match token.as_str() {
        "hasservice" => "has_service".to_string(),
        "hasendpoint" => "has_endpoint".to_string(),
        "usestech" | "usestechnology" => "uses_tech".to_string(),
        "authaccess" | "hascredential" => "auth_access".to_string(),
        "affectedby" => "affected_by".to_string(),
        "contains" => "contains".to_string(),
        "connectsto" | "connects" => "connects_to".to_string(),
        "relatedto" | "related" => "related_to".to_string(),
        "hasuser" => "has_user".to_string(),
        "hascert" | "hascertificate" => "has_cert".to_string(),
        _ if !token.is_empty() => input.trim().to_ascii_lowercase(),
        _ => "related_to".to_string(),
    }
}

pub(super) fn normalize_graph_token(input: &str) -> String {
    input
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

#[derive(Debug, Clone)]
pub struct RuntimeGraphPattern {
    pub node_label: Option<String>,
    pub node_type: Option<String>,
    pub edge_labels: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeGraphProjection {
    pub node_labels: Option<Vec<String>>,
    pub node_types: Option<Vec<String>>,
    pub edge_labels: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy)]
pub struct RuntimeQueryWeights {
    pub vector: f32,
    pub graph: f32,
    pub filter: f32,
}

#[derive(Debug, Clone)]
pub struct RuntimeFilter {
    pub field: String,
    pub op: String,
    pub value: Option<RuntimeFilterValue>,
}

#[derive(Debug, Clone)]
pub enum RuntimeFilterValue {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
    List(Vec<RuntimeFilterValue>),
    Range(Box<RuntimeFilterValue>, Box<RuntimeFilterValue>),
}

pub(super) fn runtime_filter_to_dsl(filter: RuntimeFilter) -> RedDBResult<DslFilter> {
    Ok(DslFilter {
        field: filter.field,
        op: parse_runtime_filter_op(&filter.op)?,
        value: match filter.value {
            Some(value) => runtime_filter_value_to_dsl(value),
            None => DslFilterValue::Null,
        },
    })
}

pub(super) fn parse_runtime_filter_op(op: &str) -> RedDBResult<DslFilterOp> {
    match op.trim().to_ascii_lowercase().as_str() {
        "eq" | "equals" => Ok(DslFilterOp::Equals),
        "ne" | "not_equals" | "not-equals" => Ok(DslFilterOp::NotEquals),
        "gt" | "greater_than" | "greater-than" => Ok(DslFilterOp::GreaterThan),
        "gte" | "greater_than_or_equals" | "greater-than-or-equals" => {
            Ok(DslFilterOp::GreaterThanOrEquals)
        }
        "lt" | "less_than" | "less-than" => Ok(DslFilterOp::LessThan),
        "lte" | "less_than_or_equals" | "less-than-or-equals" => Ok(DslFilterOp::LessThanOrEquals),
        "contains" => Ok(DslFilterOp::Contains),
        "starts_with" | "starts-with" => Ok(DslFilterOp::StartsWith),
        "ends_with" | "ends-with" => Ok(DslFilterOp::EndsWith),
        "in" | "in_list" | "in-list" => Ok(DslFilterOp::In),
        "between" => Ok(DslFilterOp::Between),
        "is_null" | "is-null" => Ok(DslFilterOp::IsNull),
        "is_not_null" | "is-not-null" => Ok(DslFilterOp::IsNotNull),
        other => Err(RedDBError::Query(format!(
            "unsupported hybrid filter op: {other}"
        ))),
    }
}

pub(super) fn runtime_filter_value_to_dsl(value: RuntimeFilterValue) -> DslFilterValue {
    match value {
        RuntimeFilterValue::String(value) => DslFilterValue::String(value),
        RuntimeFilterValue::Int(value) => DslFilterValue::Int(value),
        RuntimeFilterValue::Float(value) => DslFilterValue::Float(value),
        RuntimeFilterValue::Bool(value) => DslFilterValue::Bool(value),
        RuntimeFilterValue::Null => DslFilterValue::Null,
        RuntimeFilterValue::List(values) => DslFilterValue::List(
            values
                .into_iter()
                .map(runtime_filter_value_to_dsl)
                .collect(),
        ),
        RuntimeFilterValue::Range(start, end) => DslFilterValue::Range(
            Box::new(runtime_filter_value_to_dsl(*start)),
            Box::new(runtime_filter_value_to_dsl(*end)),
        ),
    }
}
