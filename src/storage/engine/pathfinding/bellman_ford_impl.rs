use super::*;

impl BellmanFord {
    /// Find shortest path, handling negative weights
    pub fn shortest_path(graph: &GraphStore, source: &str, target: &str) -> BellmanFordResult {
        let nodes: Vec<String> = graph.iter_nodes().map(|n| n.id.clone()).collect();
        let n = nodes.len();

        // Initialize distances
        let mut dist: HashMap<String, f64> = HashMap::new();
        let mut predecessor: HashMap<String, (String, GraphEdgeType)> = HashMap::new();

        for node in &nodes {
            dist.insert(node.clone(), f64::INFINITY);
        }
        dist.insert(source.to_string(), 0.0);

        let mut nodes_visited = 0;

        // Relax edges V-1 times
        for _ in 0..n - 1 {
            let mut changed = false;
            for node in &nodes {
                nodes_visited += 1;
                let d = *dist.get(node).unwrap_or(&f64::INFINITY);
                if d == f64::INFINITY {
                    continue;
                }

                for (edge_type, neighbor, weight) in graph.outgoing_edges(node) {
                    let new_dist = d + weight as f64;
                    if new_dist < *dist.get(&neighbor).unwrap_or(&f64::INFINITY) {
                        dist.insert(neighbor.clone(), new_dist);
                        predecessor.insert(neighbor.clone(), (node.clone(), edge_type));
                        changed = true;
                    }
                }
            }
            if !changed {
                break; // Early termination if no changes
            }
        }

        // Check for negative cycles
        let mut has_negative_cycle = false;
        for node in &nodes {
            let d = *dist.get(node).unwrap_or(&f64::INFINITY);
            if d == f64::INFINITY {
                continue;
            }

            for (_, neighbor, weight) in graph.outgoing_edges(node) {
                let new_dist = d + weight as f64;
                if new_dist < *dist.get(&neighbor).unwrap_or(&f64::INFINITY) {
                    has_negative_cycle = true;
                    break;
                }
            }
            if has_negative_cycle {
                break;
            }
        }

        // Reconstruct path to target
        let path = if has_negative_cycle {
            None
        } else if dist.get(target).map(|d| d.is_finite()).unwrap_or(false) {
            let mut path_nodes = vec![target.to_string()];
            let mut path_edges = Vec::new();
            let mut current = target.to_string();

            while let Some((pred, edge_type)) = predecessor.get(&current) {
                path_nodes.push(pred.clone());
                path_edges.push(*edge_type);
                current = pred.clone();
                if current == source {
                    break;
                }
            }

            path_nodes.reverse();
            path_edges.reverse();

            Some(Path {
                nodes: path_nodes,
                total_weight: *dist.get(target).unwrap_or(&0.0),
                edge_types: path_edges,
            })
        } else {
            None
        };

        BellmanFordResult {
            path,
            distances: dist,
            has_negative_cycle,
            nodes_visited,
        }
    }
}
