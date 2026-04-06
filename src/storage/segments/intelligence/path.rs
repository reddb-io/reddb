//! Path-Centric Intelligence
//!
//! Answers: "How do I get from A to B?"

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

use crate::storage::segments::graph::GraphSegment;

/// Attack path analysis
#[derive(Debug, Clone)]
pub struct AttackPath {
    pub from: String,
    pub to: String,
    pub hops: Vec<PathHop>,
    pub total_weight: f64,
    pub credentials_required: Vec<String>,
    pub vulns_exploited: Vec<String>,
}

impl AttackPath {
    pub fn display(&self) -> String {
        let mut s = String::new();
        s.push_str("┌─────────────────────────────────────────────────────────────────┐\n");
        s.push_str(&format!("│  ATTACK PATH: {} → {}\n", self.from, self.to));
        s.push_str("├─────────────────────────────────────────────────────────────────┤\n");
        s.push_str(&format!(
            "│  HOPS: {} | WEIGHT: {:.2}\n",
            self.hops.len(),
            self.total_weight
        ));

        s.push_str("│                                                                 │\n");
        for (i, hop) in self.hops.iter().enumerate() {
            let arrow = if i < self.hops.len() - 1 {
                "──►"
            } else {
                ""
            };
            let hop_str = format!("[{}] ──{}──{}", hop.node, hop.edge_type, arrow);
            s.push_str(&format!("│  {:<61} │\n", hop_str));
        }

        if !self.credentials_required.is_empty() {
            s.push_str("│                                                                 │\n");
            s.push_str("│  CREDENTIALS REQUIRED:                                          │\n");
            for cred in &self.credentials_required {
                s.push_str(&format!("│    • {:<55} │\n", cred));
            }
        }

        if !self.vulns_exploited.is_empty() {
            s.push_str("│                                                                 │\n");
            s.push_str("│  VULNERABILITIES EXPLOITED:                                     │\n");
            for vuln in &self.vulns_exploited {
                s.push_str(&format!("│    • {:<55} │\n", vuln));
            }
        }

        s.push_str("└─────────────────────────────────────────────────────────────────┘\n");
        s
    }
}

/// A hop in an attack path
#[derive(Debug, Clone)]
pub struct PathHop {
    pub node: String,
    pub edge_type: String,
    pub credential: Option<String>,
    pub vuln: Option<String>,
}

/// State for Dijkstra's algorithm
#[derive(Clone, Eq, PartialEq)]
struct State {
    cost: u64, // Using u64 to avoid float comparison issues
    node: String,
    path: Vec<(String, String)>, // (node_id, edge_type)
}

impl Ord for State {
    fn cmp(&self, other: &Self) -> Ordering {
        other.cost.cmp(&self.cost) // Reverse for min-heap
    }
}

impl PartialOrd for State {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Path-centric intelligence queries
pub struct PathIntelligence<'a> {
    graph: &'a GraphSegment,
}

impl<'a> PathIntelligence<'a> {
    pub fn new(graph: &'a GraphSegment) -> Self {
        Self { graph }
    }

    /// Find shortest attack path using Dijkstra
    pub fn find(&self, from: &str, to: &str) -> Option<AttackPath> {
        let from_id = self.normalize_id(from);
        let to_id = self.normalize_id(to);

        // Dijkstra's algorithm
        let mut heap = BinaryHeap::new();
        let mut distances: HashMap<String, u64> = HashMap::new();

        heap.push(State {
            cost: 0,
            node: from_id.clone(),
            path: vec![(from_id.clone(), "start".to_string())],
        });
        distances.insert(from_id.clone(), 0);

        while let Some(State { cost, node, path }) = heap.pop() {
            if node == to_id {
                return Some(self.build_attack_path(&from_id, &to_id, &path));
            }

            if let Some(&best) = distances.get(&node) {
                if cost > best {
                    continue;
                }
            }

            if let Some(graph_node) = self.graph.get_node(&node) {
                for edge in &graph_node.out_edges {
                    let edge_cost = (edge.weight * 100.0) as u64;
                    let next_cost = cost + edge_cost;

                    let is_better = distances
                        .get(&edge.target_id)
                        .map_or(true, |&d| next_cost < d);

                    if is_better {
                        distances.insert(edge.target_id.clone(), next_cost);
                        let mut new_path = path.clone();
                        new_path
                            .push((edge.target_id.clone(), edge.edge_type.as_str().to_string()));
                        heap.push(State {
                            cost: next_cost,
                            node: edge.target_id.clone(),
                            path: new_path,
                        });
                    }
                }
            }
        }

        None
    }

    /// Find all attack paths (up to limit)
    pub fn all(&self, from: &str, to: &str, limit: usize) -> Vec<AttackPath> {
        let from_id = self.normalize_id(from);
        let to_id = self.normalize_id(to);

        let mut paths = Vec::new();
        let mut visited = HashSet::new();

        self.dfs_all_paths(
            &from_id,
            &to_id,
            &mut visited,
            &mut Vec::new(),
            &mut paths,
            limit,
        );

        paths
    }

    /// DFS for all paths
    fn dfs_all_paths(
        &self,
        current: &str,
        target: &str,
        visited: &mut HashSet<String>,
        path: &mut Vec<(String, String)>,
        results: &mut Vec<AttackPath>,
        limit: usize,
    ) {
        if results.len() >= limit {
            return;
        }

        if current == target {
            let from = path.first().map(|(n, _)| n.clone()).unwrap_or_default();
            results.push(self.build_attack_path(&from, target, path));
            return;
        }

        visited.insert(current.to_string());

        if let Some(node) = self.graph.get_node(current) {
            for edge in &node.out_edges {
                if !visited.contains(&edge.target_id) {
                    path.push((edge.target_id.clone(), edge.edge_type.as_str().to_string()));
                    self.dfs_all_paths(&edge.target_id, target, visited, path, results, limit);
                    path.pop();
                }
            }
        }

        visited.remove(current);
    }

    /// Find paths using specific edge types
    pub fn via(&self, from: &str, to: &str, edge_types: &[&str]) -> Vec<AttackPath> {
        let from_id = self.normalize_id(from);
        let to_id = self.normalize_id(to);

        let mut paths = Vec::new();
        let mut visited = HashSet::new();
        let edge_set: HashSet<&str> = edge_types.iter().copied().collect();

        self.dfs_via_paths(
            &from_id,
            &to_id,
            &edge_set,
            &mut visited,
            &mut Vec::new(),
            &mut paths,
            10,
        );

        paths
    }

    fn dfs_via_paths(
        &self,
        current: &str,
        target: &str,
        allowed_edges: &HashSet<&str>,
        visited: &mut HashSet<String>,
        path: &mut Vec<(String, String)>,
        results: &mut Vec<AttackPath>,
        limit: usize,
    ) {
        if results.len() >= limit {
            return;
        }

        if current == target && !path.is_empty() {
            let from = path.first().map(|(n, _)| n.clone()).unwrap_or_default();
            results.push(self.build_attack_path(&from, target, path));
            return;
        }

        visited.insert(current.to_string());

        if let Some(node) = self.graph.get_node(current) {
            for edge in &node.out_edges {
                if !visited.contains(&edge.target_id)
                    && allowed_edges.contains(edge.edge_type.as_str())
                {
                    path.push((edge.target_id.clone(), edge.edge_type.as_str().to_string()));
                    self.dfs_via_paths(
                        &edge.target_id,
                        target,
                        allowed_edges,
                        visited,
                        path,
                        results,
                        limit,
                    );
                    path.pop();
                }
            }
        }

        visited.remove(current);
    }

    /// Find all nodes reachable from source
    pub fn reachable(&self, from: &str) -> Vec<String> {
        let from_id = self.normalize_id(from);
        let mut visited = HashSet::new();
        let mut queue = vec![from_id.clone()];

        while let Some(current) = queue.pop() {
            if visited.contains(&current) {
                continue;
            }
            visited.insert(current.clone());

            if let Some(node) = self.graph.get_node(&current) {
                for edge in &node.out_edges {
                    if !visited.contains(&edge.target_id) {
                        queue.push(edge.target_id.clone());
                    }
                }
            }
        }

        visited.remove(&from_id);
        visited.into_iter().collect()
    }

    /// Find all nodes that can reach target
    pub fn incoming(&self, to: &str) -> Vec<String> {
        let to_id = self.normalize_id(to);
        let mut visited = HashSet::new();
        let mut queue = vec![to_id.clone()];

        while let Some(current) = queue.pop() {
            if visited.contains(&current) {
                continue;
            }
            visited.insert(current.clone());

            if let Some(node) = self.graph.get_node(&current) {
                for edge in &node.in_edges {
                    if !visited.contains(&edge.target_id) {
                        queue.push(edge.target_id.clone());
                    }
                }
            }
        }

        visited.remove(&to_id);
        visited.into_iter().collect()
    }

    /// Find critical chokepoint nodes
    pub fn chokepoints(&self) -> Vec<(String, usize)> {
        let mut path_counts: HashMap<String, usize> = HashMap::new();

        for node in self.graph.all_nodes() {
            let in_count = node.in_edges.len();
            let out_count = node.out_edges.len();

            if in_count > 0 && out_count > 0 {
                path_counts.insert(node.id.clone(), in_count * out_count);
            }
        }

        let mut sorted: Vec<_> = path_counts.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));
        sorted.truncate(10);
        sorted
    }

    /// Get credentials needed for a path
    pub fn credentials(&self, from: &str, to: &str) -> Vec<String> {
        if let Some(path) = self.find(from, to) {
            path.credentials_required
        } else {
            vec![]
        }
    }

    /// Normalize node ID (add prefix if needed)
    fn normalize_id(&self, id: &str) -> String {
        if id.contains(':') {
            id.to_string()
        } else if id.parse::<std::net::Ipv4Addr>().is_ok() {
            format!("host:{}", id)
        } else {
            id.to_string()
        }
    }

    /// Build AttackPath from path data
    fn build_attack_path(&self, from: &str, to: &str, path: &[(String, String)]) -> AttackPath {
        let mut hops = Vec::new();
        let mut creds = Vec::new();
        let mut vulns = Vec::new();

        for (node, edge_type) in path {
            // Check if this is a credential node
            if node.starts_with("cred:") {
                creds.push(node.trim_start_matches("cred:").to_string());
            }
            if node.starts_with("vuln:") {
                vulns.push(node.trim_start_matches("vuln:").to_string());
            }

            hops.push(PathHop {
                node: node.clone(),
                edge_type: edge_type.clone(),
                credential: if edge_type == "auth_access" {
                    Some(node.clone())
                } else {
                    None
                },
                vuln: if edge_type == "affected_by" {
                    Some(node.clone())
                } else {
                    None
                },
            });
        }

        let total_weight: f64 = path
            .iter()
            .filter_map(|(node, _)| self.graph.get_node(node))
            .flat_map(|n| n.out_edges.iter())
            .map(|e| e.weight as f64)
            .sum();

        AttackPath {
            from: from.to_string(),
            to: to.to_string(),
            hops,
            total_weight,
            credentials_required: creds,
            vulns_exploited: vulns,
        }
    }
}
