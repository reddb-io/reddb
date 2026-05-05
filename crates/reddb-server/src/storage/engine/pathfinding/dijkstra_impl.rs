use super::*;

impl Dijkstra {
    /// Find shortest weighted path from source to target
    pub fn shortest_path(graph: &GraphStore, source: &str, target: &str) -> ShortestPathResult {
        if source == target {
            return ShortestPathResult {
                path: Some(Path::start(source)),
                nodes_visited: 1,
            };
        }

        let mut dist: HashMap<String, f64> = HashMap::new();
        let mut heap: BinaryHeap<DijkstraState> = BinaryHeap::new();
        let mut nodes_visited = 0;

        dist.insert(source.to_string(), 0.0);
        heap.push(DijkstraState {
            node: source.to_string(),
            cost: 0.0,
            path: Path::start(source),
        });

        while let Some(DijkstraState { node, cost, path }) = heap.pop() {
            nodes_visited += 1;

            // Found target
            if node == target {
                return ShortestPathResult {
                    path: Some(path),
                    nodes_visited,
                };
            }

            // Skip if we've found a better path
            if let Some(&d) = dist.get(&node) {
                if cost > d {
                    continue;
                }
            }

            // Explore neighbors
            for (edge_type, neighbor, weight) in graph.outgoing_edges(&node) {
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

        ShortestPathResult {
            path: None,
            nodes_visited,
        }
    }

    /// Find shortest paths from source to ALL reachable nodes
    pub fn shortest_paths_from(graph: &GraphStore, source: &str) -> HashMap<String, Path> {
        let mut dist: HashMap<String, f64> = HashMap::new();
        let mut paths: HashMap<String, Path> = HashMap::new();
        let mut heap: BinaryHeap<DijkstraState> = BinaryHeap::new();

        dist.insert(source.to_string(), 0.0);
        paths.insert(source.to_string(), Path::start(source));
        heap.push(DijkstraState {
            node: source.to_string(),
            cost: 0.0,
            path: Path::start(source),
        });

        while let Some(DijkstraState { node, cost, path }) = heap.pop() {
            // Skip if we've found a better path
            if let Some(&d) = dist.get(&node) {
                if cost > d {
                    continue;
                }
            }

            for (edge_type, neighbor, weight) in graph.outgoing_edges(&node) {
                let new_cost = cost + weight as f64;

                if !dist.contains_key(&neighbor) || new_cost < dist[&neighbor] {
                    let new_path = path.extend(&neighbor, edge_type, weight as f64);
                    dist.insert(neighbor.clone(), new_cost);
                    paths.insert(neighbor.clone(), new_path.clone());
                    heap.push(DijkstraState {
                        node: neighbor.clone(),
                        cost: new_cost,
                        path: new_path,
                    });
                }
            }
        }

        paths
    }
}
