use super::*;

impl RedDBRuntime {
    pub fn graph_neighborhood(
        &self,
        node: &str,
        direction: RuntimeGraphDirection,
        max_depth: usize,
        edge_labels: Option<Vec<String>>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphNeighborhoodResult> {
        let graph =
            materialize_graph_with_projection(self.inner.db.store().as_ref(), projection.as_ref())?;
        ensure_graph_node(&graph, node)?;
        let edge_filters = merge_edge_filters(edge_labels, projection.as_ref());

        let mut visited: HashMap<String, usize> = HashMap::new();
        let mut queue = VecDeque::new();
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut seen_edges = HashSet::new();

        visited.insert(node.to_string(), 0);
        queue.push_back((node.to_string(), 0usize));

        while let Some((current, depth)) = queue.pop_front() {
            if let Some(stored) = graph.get_node(&current) {
                nodes.push(RuntimeGraphVisit {
                    depth,
                    node: stored_node_to_runtime(stored),
                });
            }

            if depth >= max_depth {
                continue;
            }

            let mut adjacent =
                graph_adjacent_edges(&graph, &current, direction, edge_filters.as_ref());
            adjacent.sort_by(|left, right| left.0.cmp(&right.0));

            for (neighbor, edge) in adjacent {
                push_runtime_edge(&mut edges, &mut seen_edges, edge);
                if !visited.contains_key(&neighbor) {
                    visited.insert(neighbor.clone(), depth + 1);
                    queue.push_back((neighbor, depth + 1));
                }
            }
        }

        Ok(RuntimeGraphNeighborhoodResult {
            source: node.to_string(),
            direction,
            max_depth,
            nodes,
            edges,
        })
    }

    pub fn graph_traverse(
        &self,
        source: &str,
        direction: RuntimeGraphDirection,
        max_depth: usize,
        strategy: RuntimeGraphTraversalStrategy,
        edge_labels: Option<Vec<String>>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphTraversalResult> {
        let graph =
            materialize_graph_with_projection(self.inner.db.store().as_ref(), projection.as_ref())?;
        ensure_graph_node(&graph, source)?;
        let edge_filters = merge_edge_filters(edge_labels, projection.as_ref());

        let mut visits = Vec::new();
        let mut edges = Vec::new();
        let mut seen_nodes = HashSet::new();
        let mut seen_edges = HashSet::new();

        match strategy {
            RuntimeGraphTraversalStrategy::Bfs => {
                let mut queue = VecDeque::new();
                queue.push_back((source.to_string(), 0usize));
                seen_nodes.insert(source.to_string());

                while let Some((current, depth)) = queue.pop_front() {
                    if let Some(stored) = graph.get_node(&current) {
                        visits.push(RuntimeGraphVisit {
                            depth,
                            node: stored_node_to_runtime(stored),
                        });
                    }

                    if depth >= max_depth {
                        continue;
                    }

                    let mut adjacent =
                        graph_adjacent_edges(&graph, &current, direction, edge_filters.as_ref());
                    adjacent.sort_by(|left, right| left.0.cmp(&right.0));
                    for (neighbor, edge) in adjacent {
                        push_runtime_edge(&mut edges, &mut seen_edges, edge);
                        if seen_nodes.insert(neighbor.clone()) {
                            queue.push_back((neighbor, depth + 1));
                        }
                    }
                }
            }
            RuntimeGraphTraversalStrategy::Dfs => {
                let mut stack = vec![(source.to_string(), 0usize)];
                while let Some((current, depth)) = stack.pop() {
                    if !seen_nodes.insert(current.clone()) {
                        continue;
                    }

                    if let Some(stored) = graph.get_node(&current) {
                        visits.push(RuntimeGraphVisit {
                            depth,
                            node: stored_node_to_runtime(stored),
                        });
                    }

                    if depth >= max_depth {
                        continue;
                    }

                    let mut adjacent =
                        graph_adjacent_edges(&graph, &current, direction, edge_filters.as_ref());
                    adjacent.sort_by(|left, right| right.0.cmp(&left.0));
                    for (neighbor, edge) in adjacent {
                        push_runtime_edge(&mut edges, &mut seen_edges, edge);
                        if !seen_nodes.contains(&neighbor) {
                            stack.push((neighbor, depth + 1));
                        }
                    }
                }
            }
        }

        Ok(RuntimeGraphTraversalResult {
            source: source.to_string(),
            direction,
            strategy,
            max_depth,
            visits,
            edges,
        })
    }

    pub fn graph_shortest_path(
        &self,
        source: &str,
        target: &str,
        direction: RuntimeGraphDirection,
        algorithm: RuntimeGraphPathAlgorithm,
        edge_labels: Option<Vec<String>>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphPathResult> {
        let graph =
            materialize_graph_with_projection(self.inner.db.store().as_ref(), projection.as_ref())?;
        ensure_graph_node(&graph, source)?;
        ensure_graph_node(&graph, target)?;

        let merged_edge_filters = merge_edge_filters(edge_labels, projection.as_ref());
        let path = match (direction, merged_edge_filters.as_ref()) {
            (RuntimeGraphDirection::Outgoing, None) => match algorithm {
                RuntimeGraphPathAlgorithm::Bfs => {
                    let result = BFS::shortest_path(&graph, source, target);
                    RuntimeGraphPathResult {
                        source: source.to_string(),
                        target: target.to_string(),
                        direction,
                        algorithm,
                        nodes_visited: result.nodes_visited,
                        negative_cycle_detected: None,
                        path: result.path.map(|path| path_to_runtime(&graph, &path)),
                    }
                }
                RuntimeGraphPathAlgorithm::Dijkstra => {
                    let result = Dijkstra::shortest_path(&graph, source, target);
                    RuntimeGraphPathResult {
                        source: source.to_string(),
                        target: target.to_string(),
                        direction,
                        algorithm,
                        nodes_visited: result.nodes_visited,
                        negative_cycle_detected: None,
                        path: result.path.map(|path| path_to_runtime(&graph, &path)),
                    }
                }
                RuntimeGraphPathAlgorithm::AStar => {
                    let result = AStar::shortest_path_no_heuristic(&graph, source, target);
                    RuntimeGraphPathResult {
                        source: source.to_string(),
                        target: target.to_string(),
                        direction,
                        algorithm,
                        nodes_visited: result.nodes_visited,
                        negative_cycle_detected: None,
                        path: result.path.map(|path| path_to_runtime(&graph, &path)),
                    }
                }
                RuntimeGraphPathAlgorithm::BellmanFord => {
                    let result = BellmanFord::shortest_path(&graph, source, target);
                    RuntimeGraphPathResult {
                        source: source.to_string(),
                        target: target.to_string(),
                        direction,
                        algorithm,
                        nodes_visited: result.nodes_visited,
                        negative_cycle_detected: Some(result.has_negative_cycle),
                        path: result.path.map(|path| path_to_runtime(&graph, &path)),
                    }
                }
            },
            _ => shortest_path_runtime(
                &graph,
                source,
                target,
                direction,
                algorithm,
                merged_edge_filters.as_ref(),
            )?,
        };

        Ok(path)
    }

    pub fn graph_components(
        &self,
        mode: RuntimeGraphComponentsMode,
        min_size: usize,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphComponentsResult> {
        let graph =
            materialize_graph_with_projection(self.inner.db.store().as_ref(), projection.as_ref())?;
        let min_size = min_size.max(1);
        let components = match mode {
            RuntimeGraphComponentsMode::Connected => ConnectedComponents::find(&graph)
                .components
                .into_iter()
                .filter(|component| component.size >= min_size)
                .map(|component| RuntimeGraphComponent {
                    id: component.id,
                    size: component.size,
                    nodes: component.nodes,
                })
                .collect::<Vec<_>>(),
            RuntimeGraphComponentsMode::Weak => WeaklyConnectedComponents::find(&graph)
                .components
                .into_iter()
                .filter(|component| component.len() >= min_size)
                .enumerate()
                .map(|(index, nodes)| RuntimeGraphComponent {
                    id: format!("wcc:{index}"),
                    size: nodes.len(),
                    nodes,
                })
                .collect::<Vec<_>>(),
            RuntimeGraphComponentsMode::Strong => StronglyConnectedComponents::find(&graph)
                .components
                .into_iter()
                .filter(|component| component.len() >= min_size)
                .enumerate()
                .map(|(index, nodes)| RuntimeGraphComponent {
                    id: format!("scc:{index}"),
                    size: nodes.len(),
                    nodes,
                })
                .collect::<Vec<_>>(),
        };

        Ok(RuntimeGraphComponentsResult {
            mode,
            count: components.len(),
            components,
        })
    }

    pub fn graph_centrality(
        &self,
        algorithm: RuntimeGraphCentralityAlgorithm,
        top_k: usize,
        normalize: bool,
        max_iterations: Option<usize>,
        epsilon: Option<f64>,
        alpha: Option<f64>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphCentralityResult> {
        let graph =
            materialize_graph_with_projection(self.inner.db.store().as_ref(), projection.as_ref())?;
        let top_k = top_k.max(1);

        match algorithm {
            RuntimeGraphCentralityAlgorithm::Degree => {
                let result = DegreeCentrality::compute(&graph);
                let mut degree_scores = Vec::new();
                let mut pairs: Vec<_> = result
                    .total_degree
                    .iter()
                    .map(|(node_id, total_degree)| (node_id.clone(), *total_degree))
                    .collect();
                pairs
                    .sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
                pairs.truncate(top_k);

                for (node_id, total_degree) in pairs {
                    if let Some(node) = graph.get_node(&node_id) {
                        degree_scores.push(RuntimeGraphDegreeScore {
                            node: stored_node_to_runtime(node),
                            in_degree: result.in_degree.get(&node_id).copied().unwrap_or(0),
                            out_degree: result.out_degree.get(&node_id).copied().unwrap_or(0),
                            total_degree,
                        });
                    }
                }

                Ok(RuntimeGraphCentralityResult {
                    algorithm,
                    normalized: None,
                    iterations: None,
                    converged: None,
                    scores: Vec::new(),
                    degree_scores,
                })
            }
            RuntimeGraphCentralityAlgorithm::Closeness => {
                let result = ClosenessCentrality::compute(&graph);
                Ok(RuntimeGraphCentralityResult {
                    algorithm,
                    normalized: None,
                    iterations: None,
                    converged: None,
                    scores: top_runtime_scores(&graph, result.scores, top_k),
                    degree_scores: Vec::new(),
                })
            }
            RuntimeGraphCentralityAlgorithm::Betweenness => {
                let result = BetweennessCentrality::compute(&graph, normalize);
                Ok(RuntimeGraphCentralityResult {
                    algorithm,
                    normalized: Some(result.normalized),
                    iterations: None,
                    converged: None,
                    scores: top_runtime_scores(&graph, result.scores, top_k),
                    degree_scores: Vec::new(),
                })
            }
            RuntimeGraphCentralityAlgorithm::Eigenvector => {
                let mut runner = EigenvectorCentrality::new();
                if let Some(max_iterations) = max_iterations {
                    runner.max_iterations = max_iterations.max(1);
                }
                if let Some(epsilon) = epsilon {
                    runner.epsilon = epsilon.max(0.0);
                }
                let result = runner.compute(&graph);
                Ok(RuntimeGraphCentralityResult {
                    algorithm,
                    normalized: None,
                    iterations: Some(result.iterations),
                    converged: Some(result.converged),
                    scores: top_runtime_scores(&graph, result.scores, top_k),
                    degree_scores: Vec::new(),
                })
            }
            RuntimeGraphCentralityAlgorithm::PageRank => {
                let mut runner = PageRank::new();
                if let Some(max_iterations) = max_iterations {
                    runner = runner.max_iterations(max_iterations.max(1));
                }
                if let Some(alpha) = alpha {
                    runner = runner.alpha(alpha);
                }
                if let Some(epsilon) = epsilon {
                    runner = runner.epsilon(epsilon);
                }
                let result = runner.run(&graph);
                Ok(RuntimeGraphCentralityResult {
                    algorithm,
                    normalized: None,
                    iterations: Some(result.iterations),
                    converged: Some(result.converged),
                    scores: top_runtime_scores(&graph, result.scores, top_k),
                    degree_scores: Vec::new(),
                })
            }
        }
    }

    pub fn graph_communities(
        &self,
        algorithm: RuntimeGraphCommunityAlgorithm,
        min_size: usize,
        max_iterations: Option<usize>,
        resolution: Option<f64>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphCommunityResult> {
        let graph =
            materialize_graph_with_projection(self.inner.db.store().as_ref(), projection.as_ref())?;
        let min_size = min_size.max(1);

        match algorithm {
            RuntimeGraphCommunityAlgorithm::LabelPropagation => {
                let mut runner = LabelPropagation::new();
                if let Some(max_iterations) = max_iterations {
                    runner = runner.max_iterations(max_iterations.max(1));
                }
                let result = runner.run(&graph);
                let communities = result
                    .communities
                    .into_iter()
                    .filter(|community| community.size >= min_size)
                    .map(|community| RuntimeGraphCommunity {
                        id: community.label,
                        size: community.size,
                        nodes: community.nodes,
                    })
                    .collect::<Vec<_>>();
                Ok(RuntimeGraphCommunityResult {
                    algorithm,
                    count: communities.len(),
                    iterations: Some(result.iterations),
                    converged: Some(result.converged),
                    modularity: None,
                    passes: None,
                    communities,
                })
            }
            RuntimeGraphCommunityAlgorithm::Louvain => {
                let mut runner = Louvain::new();
                if let Some(max_iterations) = max_iterations {
                    runner = runner.max_iterations(max_iterations.max(1));
                }
                if let Some(resolution) = resolution {
                    runner = runner.resolution(resolution.max(0.0));
                }
                let result = runner.run(&graph);
                let mut communities = result
                    .community_sizes()
                    .into_iter()
                    .filter(|(_, size)| *size >= min_size)
                    .map(|(id, size)| RuntimeGraphCommunity {
                        id: format!("community:{id}"),
                        size,
                        nodes: result.get_community(id),
                    })
                    .collect::<Vec<_>>();
                communities.sort_by(|left, right| {
                    right
                        .size
                        .cmp(&left.size)
                        .then_with(|| left.id.cmp(&right.id))
                });
                Ok(RuntimeGraphCommunityResult {
                    algorithm,
                    count: communities.len(),
                    iterations: None,
                    converged: None,
                    modularity: Some(result.modularity),
                    passes: Some(result.passes),
                    communities,
                })
            }
        }
    }

    pub fn graph_clustering(
        &self,
        top_k: usize,
        include_triangles: bool,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphClusteringResult> {
        let graph =
            materialize_graph_with_projection(self.inner.db.store().as_ref(), projection.as_ref())?;
        let top_k = top_k.max(1);
        let result = ClusteringCoefficient::compute(&graph);
        let triangle_count = if include_triangles {
            Some(crate::storage::engine::TriangleCounting::count(&graph).count)
        } else {
            None
        };

        Ok(RuntimeGraphClusteringResult {
            global: result.global,
            local: top_runtime_scores(&graph, result.local, top_k),
            triangle_count,
        })
    }

    pub fn graph_personalized_pagerank(
        &self,
        seeds: Vec<String>,
        top_k: usize,
        alpha: Option<f64>,
        epsilon: Option<f64>,
        max_iterations: Option<usize>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphCentralityResult> {
        let graph =
            materialize_graph_with_projection(self.inner.db.store().as_ref(), projection.as_ref())?;
        if seeds.is_empty() {
            return Err(RedDBError::Query(
                "personalized pagerank requires at least one seed".to_string(),
            ));
        }
        for seed in &seeds {
            ensure_graph_node(&graph, seed)?;
        }

        let mut runner = PersonalizedPageRank::new(seeds);
        if let Some(alpha) = alpha {
            runner = runner.alpha(alpha);
        }
        if let Some(epsilon) = epsilon {
            runner = runner.epsilon(epsilon);
        }
        if let Some(max_iterations) = max_iterations {
            runner = runner.max_iterations(max_iterations.max(1));
        }
        let result = runner.run(&graph);

        Ok(RuntimeGraphCentralityResult {
            algorithm: RuntimeGraphCentralityAlgorithm::PageRank,
            normalized: None,
            iterations: Some(result.iterations),
            converged: Some(result.converged),
            scores: top_runtime_scores(&graph, result.scores, top_k.max(1)),
            degree_scores: Vec::new(),
        })
    }

    pub fn graph_hits(
        &self,
        top_k: usize,
        epsilon: Option<f64>,
        max_iterations: Option<usize>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphHitsResult> {
        let graph =
            materialize_graph_with_projection(self.inner.db.store().as_ref(), projection.as_ref())?;
        let mut runner = HITS::new();
        if let Some(epsilon) = epsilon {
            runner.epsilon = epsilon.max(0.0);
        }
        if let Some(max_iterations) = max_iterations {
            runner.max_iterations = max_iterations.max(1);
        }
        let result = runner.compute(&graph);

        Ok(RuntimeGraphHitsResult {
            iterations: result.iterations,
            converged: result.converged,
            hubs: top_runtime_scores(&graph, result.hub_scores, top_k.max(1)),
            authorities: top_runtime_scores(&graph, result.authority_scores, top_k.max(1)),
        })
    }

    pub fn graph_cycles(
        &self,
        max_length: usize,
        max_cycles: usize,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphCyclesResult> {
        let graph =
            materialize_graph_with_projection(self.inner.db.store().as_ref(), projection.as_ref())?;
        let result = CycleDetector::new()
            .max_length(max_length.max(2))
            .max_cycles(max_cycles.max(1))
            .find(&graph);

        Ok(RuntimeGraphCyclesResult {
            limit_reached: result.limit_reached,
            cycles: result
                .cycles
                .into_iter()
                .map(|cycle| cycle_to_runtime(&graph, cycle))
                .collect(),
        })
    }

    pub fn graph_topological_sort(
        &self,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphTopologicalSortResult> {
        let graph =
            materialize_graph_with_projection(self.inner.db.store().as_ref(), projection.as_ref())?;
        let ordered_nodes = match DFS::topological_sort(&graph) {
            Some(order) => order
                .into_iter()
                .filter_map(|id| graph.get_node(&id))
                .map(stored_node_to_runtime)
                .collect(),
            None => Vec::new(),
        };

        Ok(RuntimeGraphTopologicalSortResult {
            acyclic: !ordered_nodes.is_empty() || graph.node_count() == 0,
            ordered_nodes,
        })
    }

    pub fn graph_properties(
        &self,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphPropertiesResult> {
        let graph =
            materialize_graph_with_projection(self.inner.db.store().as_ref(), projection.as_ref())?;
        let node_count = graph.node_count() as usize;
        let edges = graph.iter_all_edges();
        let edge_count = edges.len();

        let connected = ConnectedComponents::find(&graph);
        let weak = WeaklyConnectedComponents::find(&graph);
        let strong = StronglyConnectedComponents::find(&graph);
        let cycle_result = CycleDetector::new()
            .max_length(node_count.max(2))
            .max_cycles(1)
            .find(&graph);

        let mut self_loop_count = 0usize;
        let mut negative_edge_count = 0usize;
        let mut directed_pairs = HashSet::new();
        let mut undirected_pairs = HashSet::new();

        for edge in &edges {
            if edge.weight < 0.0 {
                negative_edge_count += 1;
            }
            if edge.source_id == edge.target_id {
                self_loop_count += 1;
                continue;
            }

            directed_pairs.insert((edge.source_id.clone(), edge.target_id.clone()));
            let (left, right) = if edge.source_id <= edge.target_id {
                (edge.source_id.clone(), edge.target_id.clone())
            } else {
                (edge.target_id.clone(), edge.source_id.clone())
            };
            undirected_pairs.insert((left, right));
        }

        let expected_undirected_pairs = node_count.saturating_mul(node_count.saturating_sub(1)) / 2;
        let expected_directed_pairs = node_count.saturating_mul(node_count.saturating_sub(1));
        let density = if expected_undirected_pairs == 0 {
            0.0
        } else {
            undirected_pairs.len() as f64 / expected_undirected_pairs as f64
        };
        let density_directed = if expected_directed_pairs == 0 {
            0.0
        } else {
            directed_pairs.len() as f64 / expected_directed_pairs as f64
        };

        let is_empty = node_count == 0;
        let is_connected = node_count <= 1 || connected.count == 1;
        let is_weakly_connected = node_count <= 1 || weak.count == 1;
        let is_strongly_connected = node_count <= 1 || strong.count == 1;
        let is_cyclic = !cycle_result.cycles.is_empty();

        Ok(RuntimeGraphPropertiesResult {
            node_count,
            edge_count,
            self_loop_count,
            negative_edge_count,
            connected_component_count: connected.count,
            weak_component_count: weak.count,
            strong_component_count: strong.count,
            is_empty,
            is_connected,
            is_weakly_connected,
            is_strongly_connected,
            is_complete: node_count <= 1 || undirected_pairs.len() == expected_undirected_pairs,
            is_complete_directed: node_count <= 1
                || directed_pairs.len() == expected_directed_pairs,
            is_cyclic,
            is_circular: is_cyclic,
            is_acyclic: !is_cyclic,
            is_tree: node_count > 0 && is_connected && undirected_pairs.len() + 1 == node_count,
            density,
            density_directed,
        })
    }
}
