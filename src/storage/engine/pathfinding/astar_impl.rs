use super::*;

impl AStar {
    /// Find shortest path using A* with a custom heuristic
    ///
    /// The heuristic function estimates distance from a node to the target.
    /// Must be admissible (never overestimate) for optimal paths.
    pub fn shortest_path<H>(
        graph: &GraphStore,
        source: &str,
        target: &str,
        heuristic: H,
    ) -> ShortestPathResult
    where
        H: Fn(&str, &str) -> f64,
    {
        if source == target {
            return ShortestPathResult {
                path: Some(Path::start(source)),
                nodes_visited: 1,
            };
        }

        let mut g_costs: HashMap<String, f64> = HashMap::new();
        let mut heap: BinaryHeap<AStarState> = BinaryHeap::new();
        let mut nodes_visited = 0;

        let h = heuristic(source, target);
        g_costs.insert(source.to_string(), 0.0);
        heap.push(AStarState {
            node: source.to_string(),
            g_cost: 0.0,
            f_cost: h,
            path: Path::start(source),
        });

        while let Some(AStarState {
            node, g_cost, path, ..
        }) = heap.pop()
        {
            nodes_visited += 1;

            if node == target {
                return ShortestPathResult {
                    path: Some(path),
                    nodes_visited,
                };
            }

            // Skip if we've found a better path
            if let Some(&g) = g_costs.get(&node) {
                if g_cost > g {
                    continue;
                }
            }

            for (edge_type, neighbor, weight) in graph.outgoing_edges(&node) {
                let new_g = g_cost + weight as f64;

                if !g_costs.contains_key(&neighbor) || new_g < g_costs[&neighbor] {
                    let h = heuristic(&neighbor, target);
                    let new_f = new_g + h;

                    g_costs.insert(neighbor.clone(), new_g);
                    heap.push(AStarState {
                        node: neighbor.clone(),
                        g_cost: new_g,
                        f_cost: new_f,
                        path: path.extend(&neighbor, edge_type, weight as f64),
                    });
                }
            }
        }

        ShortestPathResult {
            path: None,
            nodes_visited,
        }
    }

    /// A* with zero heuristic (equivalent to Dijkstra)
    pub fn shortest_path_no_heuristic(
        graph: &GraphStore,
        source: &str,
        target: &str,
    ) -> ShortestPathResult {
        Self::shortest_path(graph, source, target, |_, _| 0.0)
    }
}
