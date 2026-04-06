//! Cycle Detection Algorithm
//!
//! DFS-based cycle detection for finding:
//! - Lateral movement loops
//! - Circular dependencies
//! - Trust cycles

use std::collections::HashSet;

use super::super::graph_store::GraphStore;

// ============================================================================
// Cycle Detection
// ============================================================================

/// DFS-based cycle detection
///
/// Finds cycles in the graph, useful for detecting:
/// - Lateral movement loops
/// - Circular dependencies
/// - Trust cycles
pub struct CycleDetector {
    /// Maximum cycle length to find
    pub max_length: usize,
    /// Maximum number of cycles to return
    pub max_cycles: usize,
}

impl Default for CycleDetector {
    fn default() -> Self {
        Self {
            max_length: 10,
            max_cycles: 100,
        }
    }
}

/// A cycle in the graph
#[derive(Debug, Clone)]
pub struct Cycle {
    /// Nodes in the cycle (first == last)
    pub nodes: Vec<String>,
    /// Length of the cycle (number of edges)
    pub length: usize,
}

/// Result of cycle detection
#[derive(Debug, Clone)]
pub struct CyclesResult {
    /// Found cycles
    pub cycles: Vec<Cycle>,
    /// Whether max_cycles limit was reached
    pub limit_reached: bool,
}

impl CycleDetector {
    /// Create with default parameters
    pub fn new() -> Self {
        Self::default()
    }

    /// Set maximum cycle length
    pub fn max_length(mut self, max: usize) -> Self {
        self.max_length = max;
        self
    }

    /// Set maximum number of cycles to find
    pub fn max_cycles(mut self, max: usize) -> Self {
        self.max_cycles = max;
        self
    }

    /// Find all cycles in the graph
    pub fn find(&self, graph: &GraphStore) -> CyclesResult {
        let nodes: Vec<String> = graph.iter_nodes().map(|n| n.id.clone()).collect();
        let mut cycles: Vec<Cycle> = Vec::new();
        let mut visited_global: HashSet<String> = HashSet::new();

        for start in &nodes {
            if cycles.len() >= self.max_cycles {
                return CyclesResult {
                    cycles,
                    limit_reached: true,
                };
            }

            // DFS from this node
            let mut stack: Vec<(String, Vec<String>)> = vec![(start.clone(), vec![start.clone()])];
            let mut visited_in_path: HashSet<String> = HashSet::new();
            visited_in_path.insert(start.clone());

            while let Some((current, path)) = stack.pop() {
                // Skip if path already exceeds max_length (length = edges = nodes - 1)
                if path.len() > self.max_length {
                    continue;
                }

                for (_, neighbor, _) in graph.outgoing_edges(&current) {
                    // Found a cycle back to start
                    if neighbor == *start && path.len() > 1 {
                        let mut cycle_nodes = path.clone();
                        cycle_nodes.push(start.clone());

                        // Check if this is a new cycle (not just a rotation)
                        if !Self::is_duplicate_cycle(&cycles, &cycle_nodes) {
                            cycles.push(Cycle {
                                length: cycle_nodes.len() - 1,
                                nodes: cycle_nodes,
                            });

                            if cycles.len() >= self.max_cycles {
                                return CyclesResult {
                                    cycles,
                                    limit_reached: true,
                                };
                            }
                        }
                    } else if !visited_in_path.contains(&neighbor)
                        && !visited_global.contains(&neighbor)
                    {
                        let mut new_path = path.clone();
                        new_path.push(neighbor.clone());
                        visited_in_path.insert(neighbor.clone());
                        stack.push((neighbor, new_path));
                    }
                }
            }

            visited_global.insert(start.clone());
        }

        CyclesResult {
            cycles,
            limit_reached: false,
        }
    }

    /// Check if a cycle is a rotation of an existing one
    fn is_duplicate_cycle(existing: &[Cycle], new_cycle: &[String]) -> bool {
        if new_cycle.len() < 2 {
            return true;
        }

        // Get cycle without the repeated end node
        let cycle_core: Vec<&str> = new_cycle[..new_cycle.len() - 1]
            .iter()
            .map(|s| s.as_str())
            .collect();

        for existing_cycle in existing {
            if existing_cycle.length != cycle_core.len() {
                continue;
            }

            let existing_core: Vec<&str> = existing_cycle.nodes[..existing_cycle.nodes.len() - 1]
                .iter()
                .map(|s| s.as_str())
                .collect();

            // Check if it's a rotation
            if Self::is_rotation(&existing_core, &cycle_core) {
                return true;
            }
        }

        false
    }

    /// Check if two sequences are rotations of each other
    fn is_rotation(a: &[&str], b: &[&str]) -> bool {
        if a.len() != b.len() {
            return false;
        }

        // Try each rotation
        let n = a.len();
        for i in 0..n {
            let mut matches = true;
            for j in 0..n {
                if a[j] != b[(i + j) % n] {
                    matches = false;
                    break;
                }
            }
            if matches {
                return true;
            }
        }

        false
    }
}
