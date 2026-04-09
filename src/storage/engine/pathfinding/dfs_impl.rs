use super::*;

impl DFS {
    /// Find any path from source to target (not necessarily shortest)
    pub fn find_path(graph: &GraphStore, source: &str, target: &str) -> ShortestPathResult {
        let mut visited: HashSet<String> = HashSet::new();
        let mut nodes_visited = 0;

        fn dfs_recursive(
            graph: &GraphStore,
            current: &str,
            target: &str,
            path: Path,
            visited: &mut HashSet<String>,
            nodes_visited: &mut usize,
        ) -> Option<Path> {
            *nodes_visited += 1;
            visited.insert(current.to_string());

            if current == target {
                return Some(path);
            }

            for (edge_type, neighbor, weight) in graph.outgoing_edges(current) {
                if !visited.contains(&neighbor) {
                    let new_path = path.extend(&neighbor, edge_type, weight as f64);
                    if let Some(result) =
                        dfs_recursive(graph, &neighbor, target, new_path, visited, nodes_visited)
                    {
                        return Some(result);
                    }
                }
            }

            None
        }

        let path = dfs_recursive(
            graph,
            source,
            target,
            Path::start(source),
            &mut visited,
            &mut nodes_visited,
        );

        ShortestPathResult {
            path,
            nodes_visited,
        }
    }

    /// DFS traversal returning all nodes in DFS order
    pub fn traverse(graph: &GraphStore, source: &str) -> Vec<String> {
        let mut visited: HashSet<String> = HashSet::new();
        let mut result: Vec<String> = Vec::new();

        fn dfs_visit(
            graph: &GraphStore,
            current: &str,
            visited: &mut HashSet<String>,
            result: &mut Vec<String>,
        ) {
            visited.insert(current.to_string());
            result.push(current.to_string());

            for (_, neighbor, _) in graph.outgoing_edges(current) {
                if !visited.contains(&neighbor) {
                    dfs_visit(graph, &neighbor, visited, result);
                }
            }
        }

        dfs_visit(graph, source, &mut visited, &mut result);
        result
    }

    /// Find all paths from source to target (with depth limit)
    pub fn all_paths(
        graph: &GraphStore,
        source: &str,
        target: &str,
        max_depth: usize,
    ) -> AllPathsResult {
        let mut paths: Vec<Path> = Vec::new();
        let mut nodes_visited = 0;

        fn dfs_all(
            graph: &GraphStore,
            target: &str,
            path: Path,
            max_depth: usize,
            paths: &mut Vec<Path>,
            visited_in_path: &mut HashSet<String>,
            nodes_visited: &mut usize,
        ) {
            let current = path.nodes.last().unwrap().clone();
            *nodes_visited += 1;

            if current == target {
                paths.push(path);
                return;
            }

            if path.len() >= max_depth {
                return;
            }

            for (edge_type, neighbor, weight) in graph.outgoing_edges(&current) {
                if !visited_in_path.contains(&neighbor) {
                    visited_in_path.insert(neighbor.clone());
                    let new_path = path.extend(&neighbor, edge_type, weight as f64);
                    dfs_all(
                        graph,
                        target,
                        new_path,
                        max_depth,
                        paths,
                        visited_in_path,
                        nodes_visited,
                    );
                    visited_in_path.remove(&neighbor);
                }
            }
        }

        let mut visited_in_path: HashSet<String> = HashSet::new();
        visited_in_path.insert(source.to_string());
        dfs_all(
            graph,
            target,
            Path::start(source),
            max_depth,
            &mut paths,
            &mut visited_in_path,
            &mut nodes_visited,
        );

        AllPathsResult {
            paths,
            nodes_visited,
        }
    }

    /// Topological sort (returns None if graph has cycles)
    pub fn topological_sort(graph: &GraphStore) -> Option<Vec<String>> {
        let nodes: Vec<String> = graph.iter_nodes().map(|n| n.id.clone()).collect();
        let mut visited: HashSet<String> = HashSet::new();
        let mut temp_marks: HashSet<String> = HashSet::new();
        let mut result: Vec<String> = Vec::new();

        fn visit(
            graph: &GraphStore,
            node: &str,
            visited: &mut HashSet<String>,
            temp_marks: &mut HashSet<String>,
            result: &mut Vec<String>,
        ) -> bool {
            if temp_marks.contains(node) {
                return false; // Cycle detected
            }
            if visited.contains(node) {
                return true;
            }

            temp_marks.insert(node.to_string());

            for (_, neighbor, _) in graph.outgoing_edges(node) {
                if !visit(graph, &neighbor, visited, temp_marks, result) {
                    return false;
                }
            }

            temp_marks.remove(node);
            visited.insert(node.to_string());
            result.push(node.to_string());
            true
        }

        for node in &nodes {
            if !visited.contains(node)
                && !visit(graph, node, &mut visited, &mut temp_marks, &mut result)
            {
                return None; // Cycle detected
            }
        }

        result.reverse();
        Some(result)
    }
}
