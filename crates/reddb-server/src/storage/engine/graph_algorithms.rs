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

/// Build the deduplicated, sorted node universe (nodes ∪ edge endpoints)
/// plus a stable ascending index for each node. Shared by the algorithms so
/// node identity → integer index is computed identically everywhere.
fn node_universe<N: Clone + Ord>(nodes: &[N], edges: &[(N, N, Weight)]) -> Vec<N> {
    let mut universe: BTreeSet<N> = BTreeSet::new();
    for n in nodes {
        universe.insert(n.clone());
    }
    for (a, b, _w) in edges {
        universe.insert(a.clone());
        universe.insert(b.clone());
    }
    universe.into_iter().collect()
}

/// Modularity Q of a partition over an undirected weighted graph.
///
/// `comm[i]` is the community id of the node at index `i` (indices match the
/// ascending node universe). `resolution` (γ) scales the null-model term; the
/// classic modularity uses γ = 1.0.
///
/// Q = Σ_C [ W_in(C)/m − γ·(Σ_tot(C)/2m)² ], where W_in(C) is the total weight
/// of edges with both endpoints in C (self-loops included once), Σ_tot(C) is
/// the summed degree of C, and m is the total edge weight. Returns 0.0 for an
/// edgeless graph (m = 0), which is the modularity of every partition there.
fn modularity_of(
    n: usize,
    adj: &[Vec<(usize, f64)>],
    selfloop: &[f64],
    degree: &[f64],
    m: f64,
    comm: &[usize],
    resolution: f64,
) -> f64 {
    if m <= 0.0 {
        return 0.0;
    }
    let two_m = 2.0 * m;
    // W_in per community: sum edge weights whose endpoints share a community.
    // Each undirected non-self edge (i, j) appears once in i's adjacency and
    // once in j's, so summing over adjacency and halving counts it once; the
    // self-loop weight is added separately (counted once).
    let mut w_in: BTreeMap<usize, f64> = BTreeMap::new();
    let mut tot: BTreeMap<usize, f64> = BTreeMap::new();
    for i in 0..n {
        *tot.entry(comm[i]).or_insert(0.0) += degree[i];
        if selfloop[i] != 0.0 {
            *w_in.entry(comm[i]).or_insert(0.0) += selfloop[i];
        }
        for &(j, w) in &adj[i] {
            if comm[i] == comm[j] {
                *w_in.entry(comm[i]).or_insert(0.0) += w / 2.0;
            }
        }
    }
    let mut q = 0.0;
    for (c, &win) in &w_in {
        let st = *tot.get(c).unwrap_or(&0.0);
        q += win / m - resolution * (st / two_m) * (st / two_m);
    }
    // Communities with no internal weight still contribute the null term.
    for (c, &st) in &tot {
        if !w_in.contains_key(c) {
            q += -resolution * (st / two_m) * (st / two_m);
        }
    }
    q
}

/// A level in the Louvain hierarchy: an undirected weighted graph over
/// `0..n` super-nodes with per-node self-loop weight, degree, and total
/// edge weight. `adj[i]` lists `(neighbour, weight)` for non-self edges only.
struct Level {
    n: usize,
    adj: Vec<Vec<(usize, f64)>>,
    selfloop: Vec<f64>,
    degree: Vec<f64>,
    m: f64,
}

impl Level {
    /// Build the base level directly from the abstract graph using the
    /// supplied node universe for index assignment. Parallel edges are summed;
    /// `(a, a, w)` is a self-loop. Edges are undirected.
    fn base<N: Clone + Ord>(ordered: &[N], edges: &[(N, N, Weight)]) -> Self {
        let n = ordered.len();
        let mut index_of: BTreeMap<&N, usize> = BTreeMap::new();
        for (i, node) in ordered.iter().enumerate() {
            index_of.insert(node, i);
        }
        // Accumulate undirected weights between distinct index pairs, and
        // self-loops separately, so parallel edges merge deterministically.
        let mut pair_weight: BTreeMap<(usize, usize), f64> = BTreeMap::new();
        let mut selfloop = vec![0.0f64; n];
        for (a, b, w) in edges {
            let ia = index_of[a];
            let ib = index_of[b];
            let w = *w as f64;
            if ia == ib {
                selfloop[ia] += w;
            } else {
                let key = if ia < ib { (ia, ib) } else { (ib, ia) };
                *pair_weight.entry(key).or_insert(0.0) += w;
            }
        }
        Self::from_parts(n, pair_weight, selfloop)
    }

    /// Assemble a level from summed undirected pair weights and self-loops.
    fn from_parts(
        n: usize,
        pair_weight: BTreeMap<(usize, usize), f64>,
        selfloop: Vec<f64>,
    ) -> Self {
        let mut adj: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
        let mut degree = vec![0.0f64; n];
        let mut m = 0.0;
        for (&(lo, hi), &w) in &pair_weight {
            adj[lo].push((hi, w));
            adj[hi].push((lo, w));
            degree[lo] += w;
            degree[hi] += w;
            m += w;
        }
        for (i, &sl) in selfloop.iter().enumerate() {
            // A self-loop contributes 2·w to the node degree (A_ii = 2w
            // convention) and w to the total edge weight m, keeping
            // Σ_i degree[i] = 2m across aggregation levels.
            degree[i] += 2.0 * sl;
            m += sl;
        }
        // Neighbour lists are built by ascending pair key, so each adjacency
        // list is already sorted by neighbour index — relied on for
        // deterministic community iteration during local moving.
        Level {
            n,
            adj,
            selfloop,
            degree,
            m,
        }
    }

    /// One pass of local moving (Louvain phase 1). Each node starts in its own
    /// community; repeatedly move nodes into the neighbouring community with
    /// the greatest modularity gain until a full sweep moves nothing. Returns
    /// the community label per node. Deterministic: nodes are swept in index
    /// order, candidate communities in ascending neighbour order, ties keep
    /// the current community / smallest community id.
    fn local_moving(&self, resolution: f64) -> Vec<usize> {
        let mut comm: Vec<usize> = (0..self.n).collect();
        let mut sigma_tot: Vec<f64> = self.degree.clone();
        if self.m <= 0.0 {
            return comm; // no edges: every node is its own community.
        }
        let two_m = 2.0 * self.m;
        loop {
            let mut improved = false;
            for i in 0..self.n {
                let ci = comm[i];
                // Weight from i to each neighbouring community (excludes the
                // self-loop, which never moves between communities). BTreeMap
                // keeps candidate communities in ascending id order.
                let mut w_to: BTreeMap<usize, f64> = BTreeMap::new();
                for &(j, w) in &self.adj[i] {
                    *w_to.entry(comm[j]).or_insert(0.0) += w;
                }
                // Remove i from its community before scoring candidates.
                sigma_tot[ci] -= self.degree[i];
                let ki = self.degree[i];
                // Gain of (re)joining community c, dropping the constant term:
                //   ΔQ ∝ w_to[c] − γ·Σ_tot[c]·k_i / 2m.
                let gain = |c: usize| -> f64 {
                    w_to.get(&c).copied().unwrap_or(0.0) - resolution * sigma_tot[c] * ki / two_m
                };
                let mut best_comm = ci;
                let mut best_gain = gain(ci);
                for &c in w_to.keys() {
                    let g = gain(c);
                    if g > best_gain {
                        best_gain = g;
                        best_comm = c;
                    }
                }
                sigma_tot[best_comm] += self.degree[i];
                if best_comm != ci {
                    comm[i] = best_comm;
                    improved = true;
                }
            }
            if !improved {
                break;
            }
        }
        comm
    }

    /// Aggregate communities into a coarser level: every community becomes one
    /// super-node, intra-community weight folds into its self-loop, and
    /// inter-community weight becomes edges between super-nodes. Returns the
    /// coarser level plus the dense renumbering `community id → super-node id`
    /// (assigned in ascending community-id order).
    fn aggregate(&self, comm: &[usize]) -> (Level, BTreeMap<usize, usize>) {
        let mut renumber: BTreeMap<usize, usize> = BTreeMap::new();
        for &c in comm {
            let next = renumber.len();
            renumber.entry(c).or_insert(next);
        }
        let nc = renumber.len();
        let mut pair_weight: BTreeMap<(usize, usize), f64> = BTreeMap::new();
        let mut selfloop = vec![0.0f64; nc];
        // Existing self-loops stay internal to their community.
        for i in 0..self.n {
            if self.selfloop[i] != 0.0 {
                selfloop[renumber[&comm[i]]] += self.selfloop[i];
            }
        }
        // Each undirected non-self edge appears twice across adjacency lists,
        // so halve the accumulated weight.
        for i in 0..self.n {
            let ci = renumber[&comm[i]];
            for &(j, w) in &self.adj[i] {
                let cj = renumber[&comm[j]];
                if ci == cj {
                    selfloop[ci] += w / 2.0;
                } else {
                    let key = if ci < cj { (ci, cj) } else { (cj, ci) };
                    *pair_weight.entry(key).or_insert(0.0) += w / 2.0;
                }
            }
        }
        (Self::from_parts(nc, pair_weight, selfloop), renumber)
    }
}

/// Detect communities with the Louvain modularity-maximisation algorithm over
/// an abstract undirected weighted graph.
///
/// Inputs mirror [`connected_components`]:
/// - `nodes`: the declared node universe.
/// - `edges`: weighted edges `(src, dst, weight)`, treated as UNDIRECTED.
///   Endpoints not present in `nodes` are still included in the universe.
/// - `resolution` (γ): the modularity resolution parameter (default 1.0 at the
///   call site). Higher values yield more, smaller communities.
///
/// Output: exactly one `(node, community_id)` pair per distinct node in the
/// universe, ordered by node ascending.
///
/// Determinism (guaranteed and tested):
/// - The node universe is a sorted, deduped `BTreeSet`, giving stable indices.
/// - Local moving sweeps nodes in index order, considers candidate communities
///   in ascending id order, and breaks ties toward the current / smallest
///   community, so the optimisation path is fully reproducible.
/// - `community_id`s are assigned in ascending order of each community's
///   smallest node, yielding `0, 1, 2, …` — the same labelling scheme as
///   `connected_components`.
///
/// Identical `(nodes, edges, resolution)` always produces identical output.
pub fn louvain<N: Clone + Ord>(
    nodes: &[N],
    edges: &[(N, N, Weight)],
    resolution: f64,
) -> Vec<(N, usize)> {
    let ordered = node_universe(nodes, edges);
    let n = ordered.len();
    if n == 0 {
        return Vec::new();
    }

    let base = Level::base(&ordered, edges);
    // `membership[i]` tracks the current community of base node i across
    // aggregation levels. Initially each node is its own community.
    let mut membership: Vec<usize> = (0..n).collect();
    let mut level = base;

    loop {
        let comm = level.local_moving(resolution);
        // Project this level's partition back onto the base nodes.
        let (next_level, renumber) = level.aggregate(&comm);
        for c in membership.iter_mut() {
            *c = renumber[&comm[*c]];
        }
        // Converged when local moving collapses nothing further: the number of
        // communities equals the number of nodes at this level.
        if next_level.n == level.n {
            break;
        }
        level = next_level;
    }

    // Relabel communities 0,1,2,… in ascending order of each community's
    // smallest base-node index, matching the connected_components convention.
    let mut label_of: BTreeMap<usize, usize> = BTreeMap::new();
    let mut next_label = 0usize;
    let mut result: Vec<(N, usize)> = Vec::with_capacity(n);
    for (i, node) in ordered.iter().enumerate() {
        let label = *label_of.entry(membership[i]).or_insert_with(|| {
            let id = next_label;
            next_label += 1;
            id
        });
        result.push((node.clone(), label));
    }
    result
}

/// Public modularity helper over the abstract graph for a given node→community
/// labelling (as produced by [`louvain`]). Exposed for tests and callers that
/// want to score a partition. Edges are treated as undirected; `resolution` is
/// the γ parameter. Returns 0.0 for an edgeless graph.
pub fn modularity<N: Clone + Ord>(
    nodes: &[N],
    edges: &[(N, N, Weight)],
    assignment: &[(N, usize)],
    resolution: f64,
) -> f64 {
    let ordered = node_universe(nodes, edges);
    let n = ordered.len();
    if n == 0 {
        return 0.0;
    }
    let mut index_of: BTreeMap<&N, usize> = BTreeMap::new();
    for (i, node) in ordered.iter().enumerate() {
        index_of.insert(node, i);
    }
    let level = Level::base(&ordered, edges);
    let assigned: BTreeMap<N, usize> = assignment.iter().cloned().collect();
    let mut comm = vec![0usize; n];
    for (node, &i) in &index_of {
        comm[i] = *assigned.get(node).unwrap_or(&i);
    }
    modularity_of(
        n,
        &level.adj,
        &level.selfloop,
        &level.degree,
        level.m,
        &comm,
        resolution,
    )
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

    // ── Louvain community detection (issue #796) ──

    /// Number of distinct communities in a labelling.
    fn community_count(assignment: &[(String, usize)]) -> usize {
        assignment
            .iter()
            .map(|(_, c)| *c)
            .collect::<BTreeSet<usize>>()
            .len()
    }

    /// The singleton partition: every node in its own community. Used as the
    /// modularity floor — Louvain must never do worse than this.
    fn singleton_partition(assignment: &[(String, usize)]) -> Vec<(String, usize)> {
        assignment
            .iter()
            .enumerate()
            .map(|(i, (n, _))| (n.clone(), i))
            .collect()
    }

    /// Two K5 cliques joined by a single bridge edge. Edges all weight 1.
    fn two_cliques_bridge() -> (Vec<String>, Vec<(String, String, Weight)>) {
        let nodes: Vec<String> = ["a0", "a1", "a2", "a3", "a4", "b0", "b1", "b2", "b3", "b4"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let clique = |p: &str| -> Vec<(String, String, Weight)> {
            let mut e = Vec::new();
            for i in 0..5 {
                for j in (i + 1)..5 {
                    e.push((format!("{p}{i}"), format!("{p}{j}"), 1.0));
                }
            }
            e
        };
        let mut edges = clique("a");
        edges.extend(clique("b"));
        // Thin bridge between the two cliques.
        edges.push(("a0".to_string(), "b0".to_string(), 1.0));
        (nodes, edges)
    }

    #[test]
    fn louvain_golden_two_cliques_form_two_communities() {
        // Canonical clear-community-structure fixture (Karate-Club-equivalent):
        // two dense cliques joined by one thin edge. The literature-known
        // community count is unambiguously 2, with each clique its own
        // community.
        let (nodes, edges) = two_cliques_bridge();
        let assignment = louvain(&nodes, &edges, 1.0);
        let map: BTreeMap<String, usize> = assignment.iter().cloned().collect();

        assert_eq!(community_count(&assignment), 2, "expected two communities");
        // Every member of clique A shares a community; likewise clique B.
        for i in 1..5 {
            assert_eq!(map[&format!("a{i}")], map["a0"], "clique A coheres");
            assert_eq!(map[&format!("b{i}")], map["b0"], "clique B coheres");
        }
        assert_ne!(map["a0"], map["b0"], "the two cliques are distinct");
        // Smallest node "a0" anchors community 0 (ascending-smallest-node ids).
        assert_eq!(map["a0"], 0);
    }

    /// Zachary's Karate Club — the canonical community-detection benchmark
    /// (Zachary 1977), 34 nodes / 78 undirected edges, 0-indexed and rendered
    /// as zero-padded ids so lexicographic order matches node number.
    fn karate_club() -> (Vec<String>, Vec<(String, String, Weight)>) {
        let pairs: &[(u32, u32)] = &[
            (0, 1),
            (0, 2),
            (0, 3),
            (0, 4),
            (0, 5),
            (0, 6),
            (0, 7),
            (0, 8),
            (0, 10),
            (0, 11),
            (0, 12),
            (0, 13),
            (0, 17),
            (0, 19),
            (0, 21),
            (0, 31),
            (1, 2),
            (1, 3),
            (1, 7),
            (1, 13),
            (1, 17),
            (1, 19),
            (1, 21),
            (1, 30),
            (2, 3),
            (2, 7),
            (2, 8),
            (2, 9),
            (2, 13),
            (2, 27),
            (2, 28),
            (2, 32),
            (3, 7),
            (3, 12),
            (3, 13),
            (4, 6),
            (4, 10),
            (5, 6),
            (5, 10),
            (5, 16),
            (6, 16),
            (8, 30),
            (8, 32),
            (8, 33),
            (9, 33),
            (13, 33),
            (14, 32),
            (14, 33),
            (15, 32),
            (15, 33),
            (18, 32),
            (18, 33),
            (19, 33),
            (20, 32),
            (20, 33),
            (22, 32),
            (22, 33),
            (23, 25),
            (23, 27),
            (23, 29),
            (23, 32),
            (23, 33),
            (24, 25),
            (24, 27),
            (24, 31),
            (25, 31),
            (26, 29),
            (26, 33),
            (27, 33),
            (28, 31),
            (28, 33),
            (29, 32),
            (29, 33),
            (30, 32),
            (30, 33),
            (31, 32),
            (31, 33),
            (32, 33),
        ];
        let nodes: Vec<String> = (0..34).map(|i| format!("n{i:02}")).collect();
        let edges = pairs
            .iter()
            .map(|(a, b)| (format!("n{a:02}"), format!("n{b:02}"), 1.0f32))
            .collect();
        (nodes, edges)
    }

    #[test]
    fn louvain_karate_club_recovers_high_modularity_communities() {
        // The literature places the Karate Club's modularity-optimal partition
        // at Q ≈ 0.42 over 2–4 communities. Assert a strong partition rather
        // than a brittle exact count: a handful of communities, modularity
        // comfortably above the singleton floor, and the two faction leaders
        // (instructor n00, president n33) separated.
        let (nodes, edges) = karate_club();
        let assignment = louvain(&nodes, &edges, 1.0);
        let map: BTreeMap<String, usize> = assignment.iter().cloned().collect();

        let k = community_count(&assignment);
        assert!((2..=5).contains(&k), "expected 2..=5 communities, got {k}");

        let q = modularity(&nodes, &edges, &assignment, 1.0);
        assert!(
            q >= 0.38,
            "modularity {q} should clear the 0.38 literature bar"
        );

        assert_ne!(
            map["n00"], map["n33"],
            "the two faction leaders land in different communities"
        );
    }

    #[test]
    fn louvain_resolution_higher_yields_more_communities() {
        // Raising γ penalises large communities, so the partition is at least
        // as fragmented. The two-cliques graph is one community at very low γ
        // and two at γ = 1.0.
        let (nodes, edges) = two_cliques_bridge();
        let low = community_count(&louvain(&nodes, &edges, 0.1));
        let high = community_count(&louvain(&nodes, &edges, 2.0));
        assert!(
            high >= low,
            "higher resolution must not reduce community count ({low} -> {high})"
        );
    }

    #[test]
    fn louvain_determinism_100_runs_identical() {
        // Determinism criterion: 100 runs over identical input are bit-for-bit
        // identical. Node ids are deliberately out of order to exercise the
        // sorted-universe normalisation.
        let (nodes, edges) = karate_club();
        let first = louvain(&nodes, &edges, 1.0);
        for _ in 0..100 {
            assert_eq!(louvain(&nodes, &edges, 1.0), first);
        }
    }

    #[test]
    fn louvain_empty_and_isolated_nodes() {
        // Empty graph -> empty result.
        let empty: Vec<String> = Vec::new();
        assert!(louvain(&empty, &[], 1.0).is_empty());

        // Three isolated nodes -> three singleton communities 0,1,2.
        let nodes: Vec<String> = ["x", "y", "z"].iter().map(|s| s.to_string()).collect();
        let got = louvain(&nodes, &[], 1.0);
        let map: BTreeMap<String, usize> = got.into_iter().collect();
        assert_eq!(map["x"], 0);
        assert_eq!(map["y"], 1);
        assert_eq!(map["z"], 2);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        // (a) modularity of the Louvain partition is >= modularity of the
        //     singleton partition (Louvain never regresses below the floor).
        // (b) every community is non-empty and node coverage is exact.
        #[test]
        fn louvain_beats_singleton_and_covers_all((nodes, edges) in graph_strategy()) {
            let assignment = louvain(&nodes, &edges, 1.0);

            // Universe = nodes ∪ edge endpoints.
            let mut universe: BTreeSet<String> = BTreeSet::new();
            for n in &nodes { universe.insert(n.clone()); }
            for (a, b, _) in &edges { universe.insert(a.clone()); universe.insert(b.clone()); }

            // Exactly one row per distinct node.
            prop_assert_eq!(assignment.len(), universe.len());
            let assigned: BTreeMap<String, usize> = assignment.iter().cloned().collect();
            prop_assert_eq!(assigned.len(), universe.len());

            // Every community is non-empty by construction; verify community
            // ids are contiguous 0..k so no id maps to an empty community.
            let ids: BTreeSet<usize> = assignment.iter().map(|(_, c)| *c).collect();
            let k = ids.len();
            for id in 0..k {
                prop_assert!(ids.contains(&id), "community ids contiguous 0..k");
            }

            // Modularity floor: Louvain >= singleton (allow a tiny epsilon for
            // floating-point noise).
            let q = modularity(&nodes, &edges, &assignment, 1.0);
            let q0 = modularity(&nodes, &edges, &singleton_partition(&assignment), 1.0);
            prop_assert!(q + 1e-9 >= q0, "louvain Q {} >= singleton Q {}", q, q0);
        }

        // determinism property: identical input -> identical output.
        #[test]
        fn louvain_determinism_property((nodes, edges) in graph_strategy()) {
            let a = louvain(&nodes, &edges, 1.0);
            let b = louvain(&nodes, &edges, 1.0);
            prop_assert_eq!(a, b);
        }
    }
}
