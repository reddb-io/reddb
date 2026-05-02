//! Dependency graph resolution for native migrations.
//!
//! Pure logic — no runtime, no I/O. Input is a list of migration names and
//! their dependency edges; output is a topologically-sorted application order
//! or a `CycleError` naming the involved migrations.

use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, PartialEq, Eq)]
pub struct CycleError {
    pub involved: Vec<String>,
}

impl std::fmt::Display for CycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "dependency cycle detected among: {}", self.involved.join(", "))
    }
}

/// Sort `migrations` in topological order given `edges` (migration → depends_on).
///
/// Returns `Err(CycleError)` if a cycle exists.
/// Migrations not present in `edges` are treated as having no dependencies.
pub fn topological_sort(
    migrations: &[String],
    edges: &[(String, String)],
) -> Result<Vec<String>, CycleError> {
    // Build adjacency: node → nodes that depend on it (reverse for Kahn).
    // in_degree: node → count of dependencies not yet satisfied.
    let node_set: HashSet<&str> = migrations.iter().map(|s| s.as_str()).collect();

    let mut in_degree: HashMap<&str, usize> = migrations
        .iter()
        .map(|s| (s.as_str(), 0usize))
        .collect();

    // dependents[dep] = list of migrations that depend on dep
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();

    for (migration, dep) in edges {
        // Only consider edges where both nodes are in our migration set.
        if !node_set.contains(migration.as_str()) || !node_set.contains(dep.as_str()) {
            continue;
        }
        *in_degree.entry(migration.as_str()).or_insert(0) += 1;
        dependents
            .entry(dep.as_str())
            .or_default()
            .push(migration.as_str());
    }

    // Kahn's algorithm
    let mut queue: VecDeque<&str> = in_degree
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(&name, _)| name)
        .collect();

    // Deterministic order within same in-degree level
    let mut queue_vec: Vec<&str> = queue.drain(..).collect();
    queue_vec.sort();
    queue.extend(queue_vec);

    let mut sorted: Vec<String> = Vec::with_capacity(migrations.len());

    while let Some(node) = queue.pop_front() {
        sorted.push(node.to_string());
        if let Some(deps) = dependents.get(node) {
            let mut next: Vec<&str> = Vec::new();
            for &dependent in deps {
                let deg = in_degree.entry(dependent).or_insert(0);
                *deg = deg.saturating_sub(1);
                if *deg == 0 {
                    next.push(dependent);
                }
            }
            next.sort();
            queue.extend(next);
        }
    }

    if sorted.len() != migrations.len() {
        // Cycle: find involved nodes (those still with in_degree > 0)
        let mut involved: Vec<String> = in_degree
            .iter()
            .filter(|(_, &deg)| deg > 0)
            .map(|(&name, _)| name.to_string())
            .collect();
        involved.sort();
        return Err(CycleError { involved });
    }

    Ok(sorted)
}

/// Check whether adding edge `from → to` would create a cycle in the existing
/// graph. Returns the cycle path if detected.
///
/// Uses DFS reachability: a cycle exists iff `from` is reachable from `to`
/// through existing edges.
pub fn would_create_cycle(
    existing_edges: &[(String, String)],
    from: &str,
    to: &str,
) -> bool {
    // Build adjacency: node → its dependencies
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for (m, dep) in existing_edges {
        adj.entry(m.as_str()).or_default().push(dep.as_str());
    }

    // Check if `from` is reachable from `to` (meaning adding from→to creates a cycle)
    let mut visited: HashSet<&str> = HashSet::new();
    let mut stack = vec![to];
    while let Some(node) = stack.pop() {
        if node == from {
            return true;
        }
        if visited.insert(node) {
            if let Some(deps) = adj.get(node) {
                stack.extend(deps.iter().copied());
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn sorts_linear_chain() {
        let migrations = vec![s("c"), s("b"), s("a")];
        let edges = vec![(s("b"), s("a")), (s("c"), s("b"))];
        let result = topological_sort(&migrations, &edges).unwrap();
        assert_eq!(result, vec!["a", "b", "c"]);
    }

    #[test]
    fn sorts_independent_nodes_alphabetically() {
        let migrations = vec![s("z"), s("a"), s("m")];
        let edges = vec![];
        let result = topological_sort(&migrations, &edges).unwrap();
        assert_eq!(result, vec!["a", "m", "z"]);
    }

    #[test]
    fn detects_cycle() {
        let migrations = vec![s("a"), s("b"), s("c")];
        let edges = vec![(s("a"), s("b")), (s("b"), s("c")), (s("c"), s("a"))];
        let err = topological_sort(&migrations, &edges).unwrap_err();
        assert!(err.involved.contains(&s("a")));
        assert!(err.involved.contains(&s("b")));
        assert!(err.involved.contains(&s("c")));
    }

    #[test]
    fn detects_self_cycle() {
        let migrations = vec![s("a")];
        let edges = vec![(s("a"), s("a"))];
        let err = topological_sort(&migrations, &edges).unwrap_err();
        assert!(err.involved.contains(&s("a")));
    }

    #[test]
    fn would_create_cycle_detects_indirect() {
        // a → b, b → c. Adding c → a would create a cycle.
        let edges = vec![(s("a"), s("b")), (s("b"), s("c"))];
        assert!(would_create_cycle(&edges, "c", "a"));
    }

    #[test]
    fn would_create_cycle_allows_valid_edge() {
        let edges = vec![(s("b"), s("a"))];
        assert!(!would_create_cycle(&edges, "c", "b"));
    }

    #[test]
    fn multi_root_dag() {
        // a, b independent; c depends on both; d depends on c
        let migrations = vec![s("a"), s("b"), s("c"), s("d")];
        let edges = vec![(s("c"), s("a")), (s("c"), s("b")), (s("d"), s("c"))];
        let result = topological_sort(&migrations, &edges).unwrap();
        // a and b must come before c; c before d
        let pos = |name: &str| result.iter().position(|x| x == name).unwrap();
        assert!(pos("a") < pos("c"));
        assert!(pos("b") < pos("c"));
        assert!(pos("c") < pos("d"));
    }
}
