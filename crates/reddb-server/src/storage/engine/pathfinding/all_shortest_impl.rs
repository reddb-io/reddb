use super::*;

impl AllShortestPaths {
    /// Find all paths with minimum length from source to target
    pub fn find(graph: &GraphStore, source: &str, target: &str) -> AllPathsResult {
        if source == target {
            return AllPathsResult {
                paths: vec![Path::start(source)],
                nodes_visited: 1,
            };
        }

        // First, find minimum distance using BFS
        let first_result = BFS::shortest_path(graph, source, target);
        let min_distance = match &first_result.path {
            Some(p) => p.len(),
            None => {
                return AllPathsResult {
                    paths: Vec::new(),
                    nodes_visited: first_result.nodes_visited,
                }
            }
        };

        // Then find all paths with that exact length
        let mut paths: Vec<Path> = Vec::new();
        let mut nodes_visited = 0;

        fn find_all(
            graph: &GraphStore,
            current_path: Path,
            target: &str,
            remaining_depth: usize,
            visited_in_path: &mut HashSet<String>,
            paths: &mut Vec<Path>,
            nodes_visited: &mut usize,
        ) {
            let current = current_path.nodes.last().unwrap().clone();
            *nodes_visited += 1;

            if remaining_depth == 0 {
                if current == target {
                    paths.push(current_path);
                }
                return;
            }

            for (edge_type, neighbor, weight) in graph.outgoing_edges(&current) {
                if !visited_in_path.contains(&neighbor) {
                    visited_in_path.insert(neighbor.clone());
                    let new_path = current_path.extend(&neighbor, edge_type, weight as f64);
                    find_all(
                        graph,
                        new_path,
                        target,
                        remaining_depth - 1,
                        visited_in_path,
                        paths,
                        nodes_visited,
                    );
                    visited_in_path.remove(&neighbor);
                }
            }
        }

        let mut visited_in_path: HashSet<String> = HashSet::new();
        visited_in_path.insert(source.to_string());
        find_all(
            graph,
            Path::start(source),
            target,
            min_distance,
            &mut visited_in_path,
            &mut paths,
            &mut nodes_visited,
        );

        AllPathsResult {
            paths,
            nodes_visited,
        }
    }
}
