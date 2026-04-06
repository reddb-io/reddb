//! Path Finding Algorithms for RedDB
//!
//! Shortest path and graph traversal algorithms optimized for attack path analysis:
//! - BFS: Unweighted shortest paths
//! - DFS: Deep exploration with backtracking
//! - Dijkstra: Weighted shortest paths (non-negative weights)
//! - A*: Heuristic-guided shortest paths
//! - Bellman-Ford: Handles negative weights
//! - All Shortest Paths: Find all minimum-length paths
//! - K-Shortest Paths: Find k best paths (Yen's algorithm)
//!
//! # Security Use Cases
//!
//! - **Attack Path Analysis**: Find shortest exploitation paths
//! - **Lateral Movement**: Discover movement paths between hosts
//! - **Privilege Escalation**: Map paths to high-value targets
//! - **Risk Assessment**: Weight paths by exploit difficulty

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};

use super::graph_store::{GraphEdgeType, GraphStore};

// ============================================================================
// Path Result Types
// ============================================================================

/// A path through the graph
#[derive(Debug, Clone)]
pub struct Path {
    /// Ordered list of node IDs from source to target
    pub nodes: Vec<String>,
    /// Total weight/cost of the path
    pub total_weight: f64,
    /// Edge types along the path
    pub edge_types: Vec<GraphEdgeType>,
}

impl Path {
    /// Create a path with a single node (start)
    pub fn start(node: &str) -> Self {
        Self {
            nodes: vec![node.to_string()],
            total_weight: 0.0,
            edge_types: Vec::new(),
        }
    }

    /// Extend path with a new node
    pub fn extend(&self, node: &str, edge_type: GraphEdgeType, weight: f64) -> Self {
        let mut nodes = self.nodes.clone();
        nodes.push(node.to_string());
        let mut edge_types = self.edge_types.clone();
        edge_types.push(edge_type);
        Self {
            nodes,
            total_weight: self.total_weight + weight,
            edge_types,
        }
    }

    /// Path length (number of edges)
    pub fn len(&self) -> usize {
        self.nodes.len().saturating_sub(1)
    }

    /// Check if path is empty
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Get source node
    pub fn source(&self) -> Option<&str> {
        self.nodes.first().map(|s| s.as_str())
    }

    /// Get target node
    pub fn target(&self) -> Option<&str> {
        self.nodes.last().map(|s| s.as_str())
    }
}

/// Result of a shortest path query
#[derive(Debug, Clone)]
pub struct ShortestPathResult {
    /// The shortest path (None if no path exists)
    pub path: Option<Path>,
    /// Number of nodes visited during search
    pub nodes_visited: usize,
}

impl ShortestPathResult {
    /// Check if a path was found
    pub fn found(&self) -> bool {
        self.path.is_some()
    }

    /// Get the path length
    pub fn distance(&self) -> Option<usize> {
        self.path.as_ref().map(|p| p.len())
    }

    /// Get the total weight
    pub fn total_weight(&self) -> Option<f64> {
        self.path.as_ref().map(|p| p.total_weight)
    }
}

/// Result of all shortest paths query
#[derive(Debug, Clone)]
pub struct AllPathsResult {
    /// All shortest paths found
    pub paths: Vec<Path>,
    /// Number of nodes visited during search
    pub nodes_visited: usize,
}

// ============================================================================
// BFS - Breadth-First Search
// ============================================================================

/// Breadth-First Search for unweighted shortest paths
///
/// Use when all edges have equal weight (or you only care about hop count).
/// Time: O(V + E), Space: O(V)
pub struct BFS;

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

// ============================================================================
// DFS - Depth-First Search
// ============================================================================

/// Depth-First Search for deep graph exploration
///
/// Use for finding paths, detecting cycles, topological sorting.
/// Time: O(V + E), Space: O(V)
pub struct DFS;

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
            if !visited.contains(node) {
                if !visit(graph, node, &mut visited, &mut temp_marks, &mut result) {
                    return None; // Cycle detected
                }
            }
        }

        result.reverse();
        Some(result)
    }
}

// ============================================================================
// Dijkstra - Weighted Shortest Paths
// ============================================================================

/// State for Dijkstra's priority queue
#[derive(Clone)]
struct DijkstraState {
    node: String,
    cost: f64,
    path: Path,
}

impl Eq for DijkstraState {}

impl PartialEq for DijkstraState {
    fn eq(&self, other: &Self) -> bool {
        self.node == other.node && (self.cost - other.cost).abs() < f64::EPSILON
    }
}

impl Ord for DijkstraState {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse ordering for min-heap
        other
            .cost
            .partial_cmp(&self.cost)
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for DijkstraState {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Dijkstra's Algorithm for weighted shortest paths
///
/// Use when edges have non-negative weights (e.g., exploit difficulty).
/// Time: O((V + E) log V), Space: O(V)
pub struct Dijkstra;

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

// ============================================================================
// A* - Heuristic Shortest Paths
// ============================================================================

/// A* state for priority queue
#[derive(Clone)]
struct AStarState {
    node: String,
    g_cost: f64, // Actual cost from start
    f_cost: f64, // g_cost + heuristic
    path: Path,
}

impl Eq for AStarState {}

impl PartialEq for AStarState {
    fn eq(&self, other: &Self) -> bool {
        self.node == other.node && (self.f_cost - other.f_cost).abs() < f64::EPSILON
    }
}

impl Ord for AStarState {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .f_cost
            .partial_cmp(&self.f_cost)
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for AStarState {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A* Algorithm for heuristic-guided shortest paths
///
/// Use when you have a heuristic estimate of distance to target.
/// Faster than Dijkstra when heuristic is good.
/// Time: O((V + E) log V) typical, Space: O(V)
pub struct AStar;

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

// ============================================================================
// Bellman-Ford - Handles Negative Weights
// ============================================================================

/// Result of Bellman-Ford algorithm
#[derive(Debug, Clone)]
pub struct BellmanFordResult {
    /// Shortest path to target (None if no path or negative cycle)
    pub path: Option<Path>,
    /// Distances from source to all reachable nodes
    pub distances: HashMap<String, f64>,
    /// Whether a negative cycle was detected
    pub has_negative_cycle: bool,
    /// Nodes visited during computation
    pub nodes_visited: usize,
}

/// Bellman-Ford Algorithm for graphs with negative weights
///
/// Use when edges can have negative weights.
/// Also detects negative cycles.
/// Time: O(V * E), Space: O(V)
pub struct BellmanFord;

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

// ============================================================================
// K-Shortest Paths (Yen's Algorithm)
// ============================================================================

/// K-Shortest Paths using Yen's Algorithm
///
/// Find the k shortest loopless paths from source to target.
/// Useful for finding alternative attack paths.
/// Time: O(k * V * (V + E) log V), Space: O(k * V)
pub struct KShortestPaths;

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

/// Candidate path for k-shortest paths
struct PathCandidate {
    path: Path,
}

impl Eq for PathCandidate {}

impl PartialEq for PathCandidate {
    fn eq(&self, other: &Self) -> bool {
        (self.path.total_weight - other.path.total_weight).abs() < f64::EPSILON
    }
}

impl Ord for PathCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .path
            .total_weight
            .partial_cmp(&self.path.total_weight)
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for PathCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// ============================================================================
// All Shortest Paths
// ============================================================================

/// Find all shortest paths (same minimum length) between two nodes
pub struct AllShortestPaths;

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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::engine::graph_store::GraphNodeType;

    fn create_simple_graph() -> GraphStore {
        let graph = GraphStore::new();

        // A -> B -> C
        // |    |
        // v    v
        // D -> E
        graph.add_node("A", "Node A", GraphNodeType::Host);
        graph.add_node("B", "Node B", GraphNodeType::Host);
        graph.add_node("C", "Node C", GraphNodeType::Host);
        graph.add_node("D", "Node D", GraphNodeType::Host);
        graph.add_node("E", "Node E", GraphNodeType::Host);

        graph.add_edge("A", "B", GraphEdgeType::ConnectsTo, 1.0);
        graph.add_edge("B", "C", GraphEdgeType::ConnectsTo, 1.0);
        graph.add_edge("A", "D", GraphEdgeType::ConnectsTo, 1.0);
        graph.add_edge("B", "E", GraphEdgeType::ConnectsTo, 1.0);
        graph.add_edge("D", "E", GraphEdgeType::ConnectsTo, 1.0);

        graph
    }

    fn create_weighted_graph() -> GraphStore {
        let graph = GraphStore::new();

        // A --(1)--> B --(2)--> D
        // |          ^
        // (4)        (1)
        // v          |
        // C --(1)--> E
        graph.add_node("A", "Node A", GraphNodeType::Host);
        graph.add_node("B", "Node B", GraphNodeType::Host);
        graph.add_node("C", "Node C", GraphNodeType::Host);
        graph.add_node("D", "Node D", GraphNodeType::Host);
        graph.add_node("E", "Node E", GraphNodeType::Host);

        graph.add_edge("A", "B", GraphEdgeType::ConnectsTo, 1.0);
        graph.add_edge("A", "C", GraphEdgeType::ConnectsTo, 4.0);
        graph.add_edge("B", "D", GraphEdgeType::ConnectsTo, 2.0);
        graph.add_edge("C", "E", GraphEdgeType::ConnectsTo, 1.0);
        graph.add_edge("E", "B", GraphEdgeType::ConnectsTo, 1.0);

        graph
    }

    // BFS Tests
    #[test]
    fn test_bfs_shortest_path() {
        let graph = create_simple_graph();
        let result = BFS::shortest_path(&graph, "A", "C");

        assert!(result.found());
        assert_eq!(result.distance(), Some(2)); // A -> B -> C
    }

    #[test]
    fn test_bfs_same_source_target() {
        let graph = create_simple_graph();
        let result = BFS::shortest_path(&graph, "A", "A");

        assert!(result.found());
        assert_eq!(result.distance(), Some(0));
    }

    #[test]
    fn test_bfs_no_path() {
        let graph = GraphStore::new();
        graph.add_node("A", "A", GraphNodeType::Host);
        graph.add_node("B", "B", GraphNodeType::Host);
        // No edge between A and B

        let result = BFS::shortest_path(&graph, "A", "B");
        assert!(!result.found());
    }

    #[test]
    fn test_bfs_reachable() {
        let graph = create_simple_graph();
        let reachable = BFS::reachable(&graph, "A", 2);

        assert!(reachable.iter().any(|(n, _)| n == "A"));
        assert!(reachable.iter().any(|(n, _)| n == "B"));
        assert!(reachable.iter().any(|(n, _)| n == "D"));
    }

    // DFS Tests
    #[test]
    fn test_dfs_find_path() {
        let graph = create_simple_graph();
        let result = DFS::find_path(&graph, "A", "E");

        assert!(result.found());
        // Path exists (either A->B->E or A->D->E)
    }

    #[test]
    fn test_dfs_all_paths() {
        let graph = create_simple_graph();
        let result = DFS::all_paths(&graph, "A", "E", 3);

        assert!(!result.paths.is_empty());
        // Should find both A->B->E and A->D->E
        assert!(result.paths.len() >= 2);
    }

    #[test]
    fn test_dfs_topological_sort() {
        let graph = GraphStore::new();
        graph.add_node("A", "A", GraphNodeType::Host);
        graph.add_node("B", "B", GraphNodeType::Host);
        graph.add_node("C", "C", GraphNodeType::Host);
        graph.add_edge("A", "B", GraphEdgeType::ConnectsTo, 1.0);
        graph.add_edge("B", "C", GraphEdgeType::ConnectsTo, 1.0);

        let result = DFS::topological_sort(&graph);
        assert!(result.is_some());

        let order = result.unwrap();
        let a_pos = order.iter().position(|n| n == "A").unwrap();
        let b_pos = order.iter().position(|n| n == "B").unwrap();
        let c_pos = order.iter().position(|n| n == "C").unwrap();
        assert!(a_pos < b_pos);
        assert!(b_pos < c_pos);
    }

    // Dijkstra Tests
    #[test]
    fn test_dijkstra_weighted() {
        let graph = create_weighted_graph();
        let result = Dijkstra::shortest_path(&graph, "A", "D");

        assert!(result.found());
        // Shortest path should be A->B->D (weight 3) not A->C->E->B->D (weight 7)
        assert_eq!(result.total_weight(), Some(3.0));
    }

    #[test]
    fn test_dijkstra_all_paths() {
        let graph = create_weighted_graph();
        let paths = Dijkstra::shortest_paths_from(&graph, "A");

        assert!(paths.contains_key("A"));
        assert!(paths.contains_key("B"));
        assert!(paths.contains_key("D"));
    }

    // A* Tests
    #[test]
    fn test_astar_zero_heuristic() {
        let graph = create_weighted_graph();
        let result = AStar::shortest_path_no_heuristic(&graph, "A", "D");

        // Should match Dijkstra result
        assert!(result.found());
        assert_eq!(result.total_weight(), Some(3.0));
    }

    // Bellman-Ford Tests
    #[test]
    fn test_bellman_ford_positive() {
        let graph = create_weighted_graph();
        let result = BellmanFord::shortest_path(&graph, "A", "D");

        assert!(!result.has_negative_cycle);
        assert!(result.path.is_some());
    }

    // K-Shortest Paths Tests
    #[test]
    fn test_k_shortest_paths() {
        let graph = create_simple_graph();
        let paths = KShortestPaths::find(&graph, "A", "E", 2);

        assert!(!paths.is_empty());
        // Should find at least 2 different paths to E
    }

    // All Shortest Paths Tests
    #[test]
    fn test_all_shortest_paths() {
        let graph = create_simple_graph();
        let result = AllShortestPaths::find(&graph, "A", "E");

        // Both A->B->E and A->D->E have length 2
        assert!(result.paths.len() >= 2);
        for path in &result.paths {
            assert_eq!(path.len(), 2);
        }
    }
}
