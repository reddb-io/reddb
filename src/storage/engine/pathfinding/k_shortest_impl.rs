use super::*;

impl KShortestPaths {
    /// Find k shortest paths from source to target
    pub fn find(graph: &GraphStore, source: &str, target: &str, k: usize) -> Vec<Path> {
        if k == 0 {
            return Vec::new();
        }

        // Find the first shortest path
        let first = Dijkstra::shortest_path(graph, source, target);
        let mut result: Vec<Path> = Vec::new();

        if let Some(path) = first.path {
            result.push(path);
        } else {
            return result;
        }

        // Candidates for the next shortest path
        let mut candidates: BinaryHeap<PathCandidate> = BinaryHeap::new();

        for i in 1..k {
            let prev_path = &result[i - 1];

            // For each spur node in the previous path
            for spur_idx in 0..prev_path.nodes.len() - 1 {
                let spur_node = &prev_path.nodes[spur_idx];
                let root_path: Vec<String> = prev_path.nodes[..=spur_idx].to_vec();

                // Edges to exclude (edges used by existing paths at this spur)
                let mut excluded_edges: HashSet<(String, String)> = HashSet::new();
                for existing_path in &result {
                    if existing_path.nodes.len() > spur_idx
                        && existing_path.nodes[..=spur_idx] == root_path
                    {
                        if let Some(next) = existing_path.nodes.get(spur_idx + 1) {
                            excluded_edges.insert((spur_node.clone(), next.clone()));
                        }
                    }
                }

                // Nodes to exclude (nodes in root path except spur)
                let excluded_nodes: HashSet<String> =
                    root_path[..spur_idx].iter().cloned().collect();

                // Find spur path
                if let Some(spur_path) = Self::dijkstra_with_exclusions(
                    graph,
                    spur_node,
                    target,
                    &excluded_edges,
                    &excluded_nodes,
                ) {
                    // Combine root path and spur path
                    let mut total_path = Path {
                        nodes: root_path.clone(),
                        total_weight: Self::path_weight_up_to(prev_path, spur_idx),
                        edge_types: prev_path.edge_types[..spur_idx].to_vec(),
                    };

                    // Add spur path (skip first node as it's the spur node)
                    for (j, node) in spur_path.nodes.iter().enumerate().skip(1) {
                        total_path.nodes.push(node.clone());
                        total_path.total_weight += spur_path
                            .edge_types
                            .get(j - 1)
                            .map(|_| 1.0) // Simplified weight
                            .unwrap_or(0.0);
                        if let Some(&et) = spur_path.edge_types.get(j - 1) {
                            total_path.edge_types.push(et);
                        }
                    }
                    total_path.total_weight =
                        spur_path.total_weight + Self::path_weight_up_to(prev_path, spur_idx);

                    candidates.push(PathCandidate { path: total_path });
                }
            }

            // Get the best candidate
            while let Some(candidate) = candidates.pop() {
                // Check if this path is unique
                let is_duplicate = result.iter().any(|p| p.nodes == candidate.path.nodes);
                if !is_duplicate {
                    result.push(candidate.path);
                    break;
                }
            }

            if result.len() <= i {
                break; // No more paths found
            }
        }

        result
    }

    /// Dijkstra with edge and node exclusions
    fn dijkstra_with_exclusions(
        graph: &GraphStore,
        source: &str,
        target: &str,
        excluded_edges: &HashSet<(String, String)>,
        excluded_nodes: &HashSet<String>,
    ) -> Option<Path> {
        let mut dist: HashMap<String, f64> = HashMap::new();
        let mut heap: BinaryHeap<DijkstraState> = BinaryHeap::new();

        dist.insert(source.to_string(), 0.0);
        heap.push(DijkstraState {
            node: source.to_string(),
            cost: 0.0,
            path: Path::start(source),
        });

        while let Some(DijkstraState { node, cost, path }) = heap.pop() {
            if node == target {
                return Some(path);
            }

            if let Some(&d) = dist.get(&node) {
                if cost > d {
                    continue;
                }
            }

            for (edge_type, neighbor, weight) in graph.outgoing_edges(&node) {
                // Skip excluded edges and nodes
                if excluded_edges.contains(&(node.clone(), neighbor.clone())) {
                    continue;
                }
                if excluded_nodes.contains(&neighbor) {
                    continue;
                }

                let new_cost = cost + weight as f64;

                if !dist.contains_key(&neighbor) || new_cost < dist[&neighbor] {
                    dist.insert(neighbor.clone(), new_cost);
                    heap.push(DijkstraState {
                        node: neighbor.clone(),
                        cost: new_cost,
                        path: path.extend(&neighbor, edge_type, weight as f64),
                    });
                }
            }
        }

        None
    }

    /// Calculate path weight up to a given index
    fn path_weight_up_to(path: &Path, idx: usize) -> f64 {
        // Simplified: sum of edge weights up to idx
        // In real implementation, track weights in Path struct
        idx as f64 // Placeholder
    }
}
