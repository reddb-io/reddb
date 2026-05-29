//! Pure graph algorithms operating on abstract node/edge inputs.
//!
//! This module is deliberately self-contained: it has NO dependency on
//! catalog, storage, cluster, or `GraphStore` types. Callers (e.g. the query
//! executor) materialize a node-id list plus weighted edges and call into
//! these pure functions. This keeps the algorithms easy to test and reuse,
//! and separate from the storage-coupled
//! `crate::storage::engine::algorithms` helpers.

use std::collections::{BTreeMap, BTreeSet};

/// Edge weight type. The storage layer uses `f32` for edge weight, so we
/// mirror that here. Weight is accepted for API compatibility; connectivity
/// treats edges as unweighted/undirected.
pub type Weight = f32;

/// Compute connected components over an abstract graph.
///
/// Inputs:
/// - `nodes`: the declared node universe.
/// - `edges`: weighted edges `(src, dst, weight)`. Edges are treated as
///   UNDIRECTED for connectivity. Edge endpoints not present in `nodes` are
///   still included in the universe (`nodes ∪ all edge endpoints`).
///
/// Output: exactly one `(node, island_id)` pair per distinct node in the
/// universe.
///
/// Determinism (guaranteed and tested):
/// - The id universe is built from a `BTreeSet`, so it is sorted and deduped.
/// - Union-find unions toward the smaller index, so each component's
///   representative is its smallest node.
/// - `island_id`s are assigned in ascending order of each component's
///   smallest node, yielding `0, 1, 2, ...`.
/// - Output is ordered by node ascending.
///
/// Identical input always produces identical output.
pub fn connected_components<N: Clone + Ord>(
    nodes: &[N],
    edges: &[(N, N, Weight)],
) -> Vec<(N, usize)> {
    // Build the deduplicated, sorted node universe (nodes ∪ edge endpoints).
    let mut universe: BTreeSet<N> = BTreeSet::new();
    for n in nodes {
        universe.insert(n.clone());
    }
    for (a, b, _w) in edges {
        universe.insert(a.clone());
        universe.insert(b.clone());
    }

    // Stable index for each node (ascending order via BTreeSet iteration).
    let ordered: Vec<N> = universe.into_iter().collect();
    let mut index_of: BTreeMap<&N, usize> = BTreeMap::new();
    for (i, n) in ordered.iter().enumerate() {
        index_of.insert(n, i);
    }

    // Union-find parent array. Representative is always the smaller index,
    // i.e. the component's smallest node.
    let mut parent: Vec<usize> = (0..ordered.len()).collect();

    fn find(parent: &mut [usize], mut x: usize) -> usize {
        // Path compression.
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }

    for (a, b, _w) in edges {
        let ia = index_of[a];
        let ib = index_of[b];
        let ra = find(&mut parent, ia);
        let rb = find(&mut parent, ib);
        if ra != rb {
            // Union toward the smaller index so the representative is the
            // component's smallest node.
            let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
            parent[hi] = lo;
        }
    }

    // Assign island ids by ascending order of each component's representative
    // (which is its smallest node). Iterating nodes ascending and assigning a
    // fresh id the first time a representative is seen yields 0,1,2,... in
    // ascending-smallest-node order.
    let mut island_of_root: BTreeMap<usize, usize> = BTreeMap::new();
    let mut next_island: usize = 0;
    let mut result: Vec<(N, usize)> = Vec::with_capacity(ordered.len());
    for (i, n) in ordered.iter().enumerate() {
        let root = find(&mut parent, i);
        let island = *island_of_root.entry(root).or_insert_with(|| {
            let id = next_island;
            next_island += 1;
            id
        });
        result.push((n.clone(), island));
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::{BTreeMap, BTreeSet, VecDeque};

    fn w(a: &str, b: &str) -> (String, String, Weight) {
        (a.to_string(), b.to_string(), 1.0)
    }

    #[test]
    fn golden_two_disjoint_triangles() {
        // Triangle {a,b,c} and triangle {d,e,f}, fully disconnected.
        let nodes: Vec<String> = ["a", "b", "c", "d", "e", "f"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let edges = vec![
            w("a", "b"),
            w("b", "c"),
            w("c", "a"),
            w("d", "e"),
            w("e", "f"),
            w("f", "d"),
        ];
        let got = connected_components(&nodes, &edges);
        let map: BTreeMap<String, usize> = got.into_iter().collect();
        // Smallest node "a" -> island 0; smallest node "d" -> island 1.
        assert_eq!(map["a"], 0);
        assert_eq!(map["b"], 0);
        assert_eq!(map["c"], 0);
        assert_eq!(map["d"], 1);
        assert_eq!(map["e"], 1);
        assert_eq!(map["f"], 1);
    }

    #[test]
    fn dumbbell_is_one_component() {
        // Two triangles joined by a bridge edge c-d => one island.
        let nodes: Vec<String> = ["a", "b", "c", "d", "e", "f"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let edges = vec![
            w("a", "b"),
            w("b", "c"),
            w("c", "a"),
            w("d", "e"),
            w("e", "f"),
            w("f", "d"),
            w("c", "d"),
        ];
        let got = connected_components(&nodes, &edges);
        let islands: BTreeSet<usize> = got.iter().map(|(_, i)| *i).collect();
        assert_eq!(islands.len(), 1);
        assert!(got.iter().all(|(_, i)| *i == 0));
    }

    #[test]
    fn isolated_nodes_each_their_own_island() {
        let nodes: Vec<String> = ["x", "y", "z"].iter().map(|s| s.to_string()).collect();
        let edges: Vec<(String, String, Weight)> = vec![];
        let got = connected_components(&nodes, &edges);
        let map: BTreeMap<String, usize> = got.into_iter().collect();
        assert_eq!(map["x"], 0);
        assert_eq!(map["y"], 1);
        assert_eq!(map["z"], 2);
    }

    #[test]
    fn edge_endpoints_not_in_node_list_are_included() {
        // Edge references "g" which is not declared in nodes.
        let nodes: Vec<String> = vec!["a".to_string()];
        let edges = vec![w("a", "g")];
        let got = connected_components(&nodes, &edges);
        assert_eq!(got.len(), 2);
        let map: BTreeMap<String, usize> = got.into_iter().collect();
        assert_eq!(map["a"], map["g"]);
        assert_eq!(map["a"], 0);
    }

    #[test]
    fn determinism_repeated_runs_identical() {
        let nodes: Vec<String> = ["n3", "n1", "n2", "n4"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let edges = vec![w("n1", "n2"), w("n3", "n4")];
        let a = connected_components(&nodes, &edges);
        let b = connected_components(&nodes, &edges);
        assert_eq!(a, b);
        // Output ordered by node ascending.
        let ordered: Vec<String> = a.iter().map(|(n, _)| n.clone()).collect();
        let mut sorted = ordered.clone();
        sorted.sort();
        assert_eq!(ordered, sorted);
    }

    // Strategy: a small id universe plus random edges between those ids.
    fn graph_strategy() -> impl Strategy<Value = (Vec<String>, Vec<(String, String, Weight)>)> {
        (1usize..8usize).prop_flat_map(|n| {
            let nodes: Vec<String> = (0..n).map(|i| format!("n{i:02}")).collect();
            let ids = nodes.clone();
            let edge = (0..n, 0..n)
                .prop_map(move |(a, b)| (format!("n{a:02}"), format!("n{b:02}"), 1.0f32));
            let edges = prop::collection::vec(edge, 0..16);
            (Just(ids), edges)
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        // (a) every node in exactly one component; union of components == total
        //     distinct node count. (b) no node assigned two island ids.
        #[test]
        fn every_node_exactly_one_island((nodes, edges) in graph_strategy()) {
            let result = connected_components(&nodes, &edges);
            // Universe = nodes ∪ edge endpoints.
            let mut universe: BTreeSet<String> = BTreeSet::new();
            for n in &nodes { universe.insert(n.clone()); }
            for (a, b, _) in &edges { universe.insert(a.clone()); universe.insert(b.clone()); }

            // One row per distinct node, and each node appears exactly once.
            prop_assert_eq!(result.len(), universe.len());
            let assigned: BTreeMap<String, usize> = result.iter().cloned().collect();
            prop_assert_eq!(assigned.len(), universe.len());
            for n in &universe {
                prop_assert!(assigned.contains_key(n));
            }
        }

        // (c) each component is internally connected: a BFS over the undirected
        //     adjacency from any member reaches exactly that island's members.
        #[test]
        fn islands_are_connected((nodes, edges) in graph_strategy()) {
            let result = connected_components(&nodes, &edges);
            let assigned: BTreeMap<String, usize> = result.iter().cloned().collect();

            // Build undirected adjacency.
            let mut adj: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            for n in assigned.keys() {
                adj.entry(n.clone()).or_default();
            }
            for (a, b, _) in &edges {
                adj.entry(a.clone()).or_default().insert(b.clone());
                adj.entry(b.clone()).or_default().insert(a.clone());
            }

            // Group nodes by island.
            let mut by_island: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
            for (n, isl) in &assigned {
                by_island.entry(*isl).or_default().insert(n.clone());
            }

            for members in by_island.values() {
                // BFS from the first member; reachable set must equal members.
                let start = members.iter().next().unwrap().clone();
                let mut seen: BTreeSet<String> = BTreeSet::new();
                let mut q: VecDeque<String> = VecDeque::new();
                q.push_back(start.clone());
                seen.insert(start);
                while let Some(cur) = q.pop_front() {
                    if let Some(ns) = adj.get(&cur) {
                        for nb in ns {
                            if seen.insert(nb.clone()) {
                                q.push_back(nb.clone());
                            }
                        }
                    }
                }
                prop_assert_eq!(&seen, members);
            }

            // island ids are contiguous 0..k.
            let k = by_island.len();
            for id in 0..k {
                prop_assert!(by_island.contains_key(&id));
            }
        }

        // determinism property: identical input -> identical output.
        #[test]
        fn determinism_property((nodes, edges) in graph_strategy()) {
            let a = connected_components(&nodes, &edges);
            let b = connected_components(&nodes, &edges);
            prop_assert_eq!(a, b);
        }
    }
}
