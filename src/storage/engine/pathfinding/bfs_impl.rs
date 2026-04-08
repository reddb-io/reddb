use super::*;

impl BFS {
    /// Find shortest path (by hop count) from source to target
    pub fn shortest_path(graph: &GraphStore, source: &str, target: &str) -> ShortestPathResult {
        if source == target {
            return ShortestPathResult {
                path: Some(Path::start(source)),
                nodes_visited: 1,
            };
        }

        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<Path> = VecDeque::new();
        let mut nodes_visited = 0;

        queue.push_back(Path::start(source));
        visited.insert(source.to_string());

        while let Some(current_path) = queue.pop_front() {
            let current = current_path.nodes.last().unwrap();
            nodes_visited += 1;

            for (edge_type, neighbor, weight) in graph.outgoing_edges(current) {
                if neighbor == target {
                    return ShortestPathResult {
                        path: Some(current_path.extend(&neighbor, edge_type, weight as f64)),
                        nodes_visited,
                    };
                }

                if !visited.contains(&neighbor) {
                    visited.insert(neighbor.clone());
                    queue.push_back(current_path.extend(&neighbor, edge_type, weight as f64));
                }
            }
        }

        ShortestPathResult {
            path: None,
            nodes_visited,
        }
    }

    /// Find all nodes reachable from source within max_depth hops
    pub fn reachable(graph: &GraphStore, source: &str, max_depth: usize) -> Vec<(String, usize)> {
        let mut visited: HashMap<String, usize> = HashMap::new();
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();

        queue.push_back((source.to_string(), 0));
        visited.insert(source.to_string(), 0);

        while let Some((current, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }

            for (_, neighbor, _) in graph.outgoing_edges(&current) {
                if !visited.contains_key(&neighbor) {
                    visited.insert(neighbor.clone(), depth + 1);
                    queue.push_back((neighbor, depth + 1));
                }
            }
        }

        let mut result: Vec<_> = visited.into_iter().collect();
        result.sort_by_key(|(_, depth)| *depth);
        result
    }

    /// BFS traversal returning all nodes in BFS order
    pub fn traverse(graph: &GraphStore, source: &str) -> Vec<String> {
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        let mut result: Vec<String> = Vec::new();

        queue.push_back(source.to_string());
        visited.insert(source.to_string());

        while let Some(current) = queue.pop_front() {
            result.push(current.clone());

            for (_, neighbor, _) in graph.outgoing_edges(&current) {
                if !visited.contains(&neighbor) {
                    visited.insert(neighbor.clone());
                    queue.push_back(neighbor);
                }
            }
        }

        result
    }
}
