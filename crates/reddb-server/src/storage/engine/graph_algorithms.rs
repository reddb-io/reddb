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

/// Single-source single-target shortest path over an abstract undirected
/// weighted graph.
///
/// Inputs mirror [`connected_components`] / [`louvain`]:
/// - `nodes`: the declared node universe.
/// - `edges`: weighted edges `(src, dst, weight)`, treated as UNDIRECTED.
///   Endpoints not present in `nodes` are still included in the universe.
///   Parallel edges between the same pair collapse to their MINIMUM weight;
///   self-loops are ignored (they never shorten a path). Edge weights are
///   assumed NON-NEGATIVE (the storage layer's `f32` weights are); the
///   shortest-path / triangle-inequality guarantees below hold under that
///   assumption.
/// - `src`, `dst`: the path endpoints. Either absent from the universe yields
///   `None`.
/// - `max_hops`: an optional cap on the number of EDGES in the path. `None`
///   means unbounded. The result is the minimum-weight path using at most
///   `max_hops` edges; if no such path exists, `None`.
///
/// Output: `Some(path)` where `path` is the ordered sequence of
/// `(node, cumulative_weight)` from `src` to `dst` (the first entry is
/// `(src, 0.0)`, the hop index is the position in the vector, and the last
/// entry's weight is the total path weight). `None` when `dst` is unreachable
/// from `src` (within the hop budget) — the executor maps this to an EMPTY
/// result set, never an error. A zero-length path (`src == dst`) returns the
/// single entry `(src, 0.0)`.
///
/// Algorithm: a hop-limited Bellman-Ford relaxation. Each round relaxes every
/// (undirected) edge against the previous round's distances, so after `k`
/// rounds `dist[v]` is the shortest distance to `v` using at most `k` edges.
/// With `max_hops = None` we run `n - 1` rounds, which suffices for the true
/// shortest path because — with non-negative weights — a shortest path is
/// simple and therefore uses at most `n - 1` edges. O(rounds · E).
///
/// Determinism (guaranteed and tested): the node universe is a sorted, deduped
/// `BTreeSet` giving stable indices; relaxation sweeps nodes in index order and
/// neighbours in ascending index order; ties keep the lower-index predecessor.
/// Identical `(nodes, edges, src, dst, max_hops)` always produces identical
/// output. On an undirected graph the distance is symmetric:
/// `shortest_path(.., a, b, ..)` and `shortest_path(.., b, a, ..)` have equal
/// total weight.
pub fn shortest_path<N: Clone + Ord>(
    nodes: &[N],
    edges: &[(N, N, Weight)],
    src: &N,
    dst: &N,
    max_hops: Option<usize>,
) -> Option<Vec<(N, f64)>> {
    let ordered = node_universe(nodes, edges);
    let n = ordered.len();
    if n == 0 {
        return None;
    }
    let mut index_of: BTreeMap<&N, usize> = BTreeMap::new();
    for (i, node) in ordered.iter().enumerate() {
        index_of.insert(node, i);
    }
    let si = match index_of.get(src) {
        Some(&i) => i,
        None => return None,
    };
    let di = match index_of.get(dst) {
        Some(&i) => i,
        None => return None,
    };

    // Undirected adjacency keeping the minimum weight between each pair, so
    // parallel edges collapse deterministically and self-loops are dropped.
    let mut adj: Vec<BTreeMap<usize, f64>> = vec![BTreeMap::new(); n];
    for (a, b, w) in edges {
        let ia = index_of[a];
        let ib = index_of[b];
        if ia == ib {
            continue; // self-loop: never part of a shortest simple path.
        }
        let w = *w as f64;
        let e = adj[ia].entry(ib).or_insert(f64::INFINITY);
        if w < *e {
            *e = w;
        }
        let e = adj[ib].entry(ia).or_insert(f64::INFINITY);
        if w < *e {
            *e = w;
        }
    }

    // Hop-limited Bellman-Ford. `rounds` bounds the path length in edges:
    // `max_hops` when provided, else `n - 1` (enough for any simple path).
    let rounds = max_hops.unwrap_or(n.saturating_sub(1));
    let mut dist = vec![f64::INFINITY; n];
    dist[si] = 0.0;
    let mut pred: Vec<Option<usize>> = vec![None; n];
    for _ in 0..rounds {
        // Relax against the previous round's distances (the clone) so each
        // round extends every path by at most one edge.
        let mut next = dist.clone();
        let mut changed = false;
        for u in 0..n {
            if !dist[u].is_finite() {
                continue;
            }
            for (&v, &w) in &adj[u] {
                let cand = dist[u] + w;
                if cand < next[v] {
                    next[v] = cand;
                    pred[v] = Some(u);
                    changed = true;
                }
            }
        }
        dist = next;
        if !changed {
            break; // converged early — fewer hops than the budget sufficed.
        }
    }

    if !dist[di].is_finite() {
        return None; // unreachable within the hop budget.
    }

    // Reconstruct the path dst -> src via predecessors. The cycle guard is
    // defensive: with non-negative weights each predecessor step is to a
    // strictly-or-equally smaller distance and the chain terminates at src.
    let mut path_idx = Vec::new();
    let mut seen = BTreeSet::new();
    let mut cur = di;
    loop {
        if !seen.insert(cur) {
            return None; // unexpected cycle — bail rather than loop forever.
        }
        path_idx.push(cur);
        if cur == si {
            break;
        }
        match pred[cur] {
            Some(p) => cur = p,
            None => return None, // no predecessor yet not src: unreachable.
        }
    }
    path_idx.reverse();

    // Cumulative weight along the reconstructed path (per-hop running sum of
    // the min pair weights), so the final entry equals `dist[dst]`.
    let mut result = Vec::with_capacity(path_idx.len());
    let mut cum = 0.0;
    for (i, &idx) in path_idx.iter().enumerate() {
        if i > 0 {
            let prev = path_idx[i - 1];
            cum += adj[prev][&idx];
        }
        result.push((ordered[idx].clone(), cum));
    }
    Some(result)
}

/// Build an undirected adjacency list indexed by the ascending node universe.
///
/// `adj[i]` lists each neighbour index of node `i`, sorted ascending and
/// deduplicated (parallel edges collapse; self-loops are dropped — they never
/// affect betweenness shortest paths and would distort degree counts). Edge
/// weights are ignored: the centrality family below treats the graph as
/// unweighted and undirected, matching `connected_components`. Returns the node
/// universe alongside the adjacency so callers share one index assignment.
fn undirected_adjacency<N: Clone + Ord>(
    nodes: &[N],
    edges: &[(N, N, Weight)],
) -> (Vec<N>, Vec<Vec<usize>>) {
    let ordered = node_universe(nodes, edges);
    let n = ordered.len();
    let mut index_of: BTreeMap<&N, usize> = BTreeMap::new();
    for (i, node) in ordered.iter().enumerate() {
        index_of.insert(node, i);
    }
    // BTreeSet per node keeps neighbours ascending and deduplicated, so BFS /
    // matrix sweeps visit them in a fully reproducible order.
    let mut sets: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
    for (a, b, _w) in edges {
        let ia = index_of[a];
        let ib = index_of[b];
        if ia == ib {
            continue; // self-loop: irrelevant to undirected centrality.
        }
        sets[ia].insert(ib);
        sets[ib].insert(ia);
    }
    let adj: Vec<Vec<usize>> = sets.into_iter().map(|s| s.into_iter().collect()).collect();
    (ordered, adj)
}

/// Betweenness centrality over an abstract undirected, unweighted graph using
/// Brandes' algorithm.
///
/// Inputs mirror [`connected_components`]: `nodes` declares the universe and
/// `edges` are treated as UNDIRECTED (weights ignored; endpoints absent from
/// `nodes` still join the universe). The score of node `v` is the number of
/// shortest paths between all other unordered pairs `{s, t}` that pass through
/// `v`. Each unordered pair is counted once (the raw directed accumulation is
/// halved), so an isolated node and both endpoints of a bridge score `0.0`.
///
/// Output: one `(node, score)` pair per distinct node, ordered by node
/// ascending.
///
/// Determinism (guaranteed and tested): the node universe is a sorted, deduped
/// `BTreeSet`; BFS explores neighbours in ascending index order; the
/// accumulation visits vertices in reverse-BFS-distance order with stable
/// per-level ordering. Identical input always produces identical output.
pub fn betweenness<N: Clone + Ord>(nodes: &[N], edges: &[(N, N, Weight)]) -> Vec<(N, f64)> {
    let (ordered, adj) = undirected_adjacency(nodes, edges);
    let n = ordered.len();
    let mut cb = vec![0.0f64; n];

    for s in 0..n {
        // Single-source shortest-path counting (unweighted -> BFS).
        let mut stack: Vec<usize> = Vec::with_capacity(n);
        let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut sigma = vec![0.0f64; n]; // # shortest s->v paths.
        let mut dist = vec![-1i64; n];
        sigma[s] = 1.0;
        dist[s] = 0;
        let mut queue: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
        queue.push_back(s);
        while let Some(v) = queue.pop_front() {
            stack.push(v);
            for &w in &adj[v] {
                // First time we reach w: record its distance and enqueue.
                if dist[w] < 0 {
                    dist[w] = dist[v] + 1;
                    queue.push_back(w);
                }
                // w found on a shortest path via v.
                if dist[w] == dist[v] + 1 {
                    sigma[w] += sigma[v];
                    preds[w].push(v);
                }
            }
        }
        // Back-propagation of dependencies (Brandes accumulation).
        let mut delta = vec![0.0f64; n];
        while let Some(w) = stack.pop() {
            for &v in &preds[w] {
                delta[v] += (sigma[v] / sigma[w]) * (1.0 + delta[w]);
            }
            if w != s {
                cb[w] += delta[w];
            }
        }
    }

    // Undirected graphs count every unordered pair twice in the directed
    // accumulation above, so halve to recover the conventional score.
    let mut result: Vec<(N, f64)> = Vec::with_capacity(n);
    for (i, node) in ordered.iter().enumerate() {
        result.push((node.clone(), cb[i] / 2.0));
    }
    result
}

/// Eigenvector centrality over an abstract undirected, unweighted graph via
/// power iteration on the adjacency matrix.
///
/// `max_iterations` caps the power-iteration sweeps; `tolerance` is the L1
/// convergence threshold on successive (normalised) iterates. The returned
/// vector is L2-normalised, so every score lies in `[0, 1]` and the
/// Perron–Frobenius principal eigenvector is non-negative. An isolated node
/// (no incident edges) scores `0.0` whenever the graph has any edges; an
/// edgeless graph has no well-defined dominant eigenvector, so every node gets
/// the uniform value `1/√n` (still L2-normalised).
///
/// Output: one `(node, score)` pair per distinct node, ordered by node
/// ascending. Deterministic: fixed uniform start vector, ascending sweep order,
/// sign fixed non-negative by construction.
pub fn eigenvector<N: Clone + Ord>(
    nodes: &[N],
    edges: &[(N, N, Weight)],
    max_iterations: usize,
    tolerance: f64,
) -> Vec<(N, f64)> {
    let (ordered, adj) = undirected_adjacency(nodes, edges);
    let n = ordered.len();
    if n == 0 {
        return Vec::new();
    }
    let has_edges = adj.iter().any(|row| !row.is_empty());
    let mut x = vec![1.0f64 / (n as f64).sqrt(); n];
    if !has_edges {
        // No dominant eigenvector: fall back to the uniform unit vector.
        return ordered.into_iter().zip(x).collect();
    }

    for _ in 0..max_iterations {
        // y = (I + A)·x. The identity shift keeps the same principal
        // eigenvector as A while making every eigenvalue positive, so power
        // iteration converges on BIPARTITE graphs too (plain A·x oscillates
        // there because its spectrum is symmetric about zero). Connected nodes
        // grow by the dominant factor each sweep while isolated nodes grow by
        // 1, so after normalisation isolated nodes still decay to 0.
        let mut y = vec![0.0f64; n];
        for i in 0..n {
            let mut acc = x[i];
            for &j in &adj[i] {
                acc += x[j];
            }
            y[i] = acc;
        }
        // Normalise to unit L2 norm; non-negative start keeps the sign fixed.
        let norm: f64 = y.iter().map(|v| v * v).sum::<f64>().sqrt();
        if norm == 0.0 {
            break;
        }
        for v in y.iter_mut() {
            *v /= norm;
        }
        let diff: f64 = x.iter().zip(&y).map(|(a, b)| (a - b).abs()).sum();
        x = y;
        if diff < tolerance {
            break;
        }
    }

    ordered.into_iter().zip(x).collect()
}

/// PageRank over an abstract undirected, unweighted graph.
///
/// Edges are treated as UNDIRECTED — each edge contributes an out-link in both
/// directions. `damping` is the classic teleport factor (0.85 at the call
/// site); `max_iterations` caps the sweeps. Rank mass from dangling nodes
/// (degree 0) is redistributed uniformly each iteration, so the returned scores
/// always sum to `1.0`. Every score is strictly positive (≥ the teleport floor
/// `(1−damping)/n`), so an isolated node receives exactly that boundary share.
///
/// Output: one `(node, score)` pair per distinct node, ordered by node
/// ascending. Deterministic: fixed uniform start, ascending sweep order, fixed
/// iteration cap. Identical input always produces identical output.
pub fn pagerank<N: Clone + Ord>(
    nodes: &[N],
    edges: &[(N, N, Weight)],
    damping: f64,
    max_iterations: usize,
) -> Vec<(N, f64)> {
    let (ordered, adj) = undirected_adjacency(nodes, edges);
    let n = ordered.len();
    if n == 0 {
        return Vec::new();
    }
    let nf = n as f64;
    let degree: Vec<f64> = adj.iter().map(|row| row.len() as f64).collect();
    let mut rank = vec![1.0f64 / nf; n];
    let teleport = (1.0 - damping) / nf;

    for _ in 0..max_iterations {
        // Mass held by dangling (degree-0) nodes is spread uniformly.
        let dangling: f64 = (0..n).filter(|&i| degree[i] == 0.0).map(|i| rank[i]).sum();
        let mut next = vec![teleport + damping * dangling / nf; n];
        for i in 0..n {
            if degree[i] == 0.0 {
                continue;
            }
            let share = damping * rank[i] / degree[i];
            for &j in &adj[i] {
                next[j] += share;
            }
        }
        rank = next;
    }

    // Renormalise to defend against floating-point drift so scores sum to 1.0.
    let total: f64 = rank.iter().sum();
    if total > 0.0 {
        for r in rank.iter_mut() {
            *r /= total;
        }
    }
    ordered.into_iter().zip(rank).collect()
}

/// Deterministic 2D spectral layout over an abstract undirected, unweighted
/// graph. Returns one `(node, (x, y))` pair per distinct node with both
/// coordinates min–max normalised to `[0, 1]`.
///
/// The coordinates are the two smallest non-trivial eigenvectors of the graph
/// Laplacian `L = D − A` (the Fiedler vector and the next one) — the classic
/// spectral-drawing layout: graph-adjacent nodes land near each other and
/// disconnected components separate cleanly, so a client force-directed layout
/// seeded from these hints converges faster and to a stable arrangement. The
/// hint is purely advisory; callers may seed their own layout with it or ignore
/// it entirely.
///
/// Method: shifted, deflated power iteration. We iterate on `M = cI − L` (whose
/// DOMINANT eigenvectors are `L`'s SMALLEST), deflating the trivial constant
/// mode (`L`-eigenvalue 0) and then each found coordinate via Gram–Schmidt, so
/// successive sweeps recover the next Laplacian eigenvector. `max_iterations`
/// caps the sweeps per coordinate; `tolerance` is the L1 convergence threshold
/// on successive normalised iterates.
///
/// Determinism (guaranteed and tested): the node universe is a sorted, deduped
/// `BTreeSet`; the start vectors are fixed index-polynomial seeds (no random
/// initialisation); sweeps run in ascending index order with a fixed cap. The
/// eigenvector sign — mathematically arbitrary — is pinned by the fixed seed,
/// so identical input always produces identical coordinates. An isolated node
/// or an edgeless / single-node graph still yields finite, deterministic hints
/// (a degenerate coordinate collapses to `0.5`).
pub fn spectral_embedding<N: Clone + Ord>(
    nodes: &[N],
    edges: &[(N, N, Weight)],
    max_iterations: usize,
    tolerance: f64,
) -> Vec<(N, (f64, f64))> {
    let (ordered, adj) = undirected_adjacency(nodes, edges);
    let n = ordered.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        // A single point has no spread; place it at the centre.
        return ordered.into_iter().map(|node| (node, (0.5, 0.5))).collect();
    }

    let degree: Vec<f64> = adj.iter().map(|row| row.len() as f64).collect();
    // The largest Laplacian eigenvalue is bounded by 2·max_degree (Gershgorin),
    // so shifting by c just above that makes M = cI − L positive definite and
    // its dominant eigenvectors L's smallest. +1 guarantees a strict spectral
    // gap and a valid shift even for an edgeless graph (max_degree = 0).
    let max_degree = degree.iter().cloned().fold(0.0_f64, f64::max);
    let c = 2.0 * max_degree + 1.0;

    // (M·v)[i] = (c − deg[i])·v[i] + Σ_{j ~ i} v[j].
    let apply_m = |v: &[f64]| -> Vec<f64> {
        let mut out = vec![0.0f64; n];
        for i in 0..n {
            let mut acc = (c - degree[i]) * v[i];
            for &j in &adj[i] {
                acc += v[j];
            }
            out[i] = acc;
        }
        out
    };

    // v0: the constant (L-eigenvalue-0) mode we deflate away so the layout
    // captures structure rather than the trivial all-equal solution. Unit norm
    // keeps the Gram–Schmidt basis orthonormal.
    let inv_sqrt_n = 1.0 / (n as f64).sqrt();
    let v0 = vec![inv_sqrt_n; n];

    // Distinct non-constant seeds for the two coordinates. Their differing
    // index-polynomial shapes keep a component along the next eigenvector after
    // deflation, and being fixed they make the result fully reproducible.
    let seed_x: Vec<f64> = (0..n).map(|i| (i as f64) + 1.0).collect();
    let seed_y: Vec<f64> = (0..n).map(|i| ((i as f64) + 1.0).powi(2)).collect();

    let mut basis: Vec<Vec<f64>> = vec![v0];
    let vx = dominant_orthogonal(&apply_m, &seed_x, &basis, max_iterations, tolerance);
    basis.push(vx.clone());
    let vy = dominant_orthogonal(&apply_m, &seed_y, &basis, max_iterations, tolerance);

    let x = normalize_unit_interval(&vx);
    let y = normalize_unit_interval(&vy);
    ordered
        .into_iter()
        .enumerate()
        .map(|(i, node)| (node, (x[i], y[i])))
        .collect()
}

/// L2 norm of a vector.
fn l2_norm(v: &[f64]) -> f64 {
    v.iter().map(|x| x * x).sum::<f64>().sqrt()
}

/// Subtract the projection of `v` onto each (orthonormal) basis vector,
/// leaving `v` in the orthogonal complement of the span. Assumes `basis`
/// vectors are unit-norm and mutually orthogonal.
fn project_out(v: &mut [f64], basis: &[Vec<f64>]) {
    for b in basis {
        let dot: f64 = v.iter().zip(b).map(|(x, y)| x * y).sum();
        for (x, &bi) in v.iter_mut().zip(b) {
            *x -= dot * bi;
        }
    }
}

/// Deflated power iteration: the dominant eigenvector of `apply` within the
/// orthogonal complement of `basis`, started from `seed`. Re-orthogonalises
/// against `basis` every sweep so the iterate cannot drift back toward an
/// already-found mode. Returns a unit (or zero, if `seed` lies entirely in the
/// span of `basis`) vector.
fn dominant_orthogonal(
    apply: &impl Fn(&[f64]) -> Vec<f64>,
    seed: &[f64],
    basis: &[Vec<f64>],
    max_iterations: usize,
    tolerance: f64,
) -> Vec<f64> {
    let mut v = seed.to_vec();
    project_out(&mut v, basis);
    let norm = l2_norm(&v);
    if norm == 0.0 {
        return v; // seed fully explained by the existing basis.
    }
    for x in v.iter_mut() {
        *x /= norm;
    }
    for _ in 0..max_iterations {
        let mut next = apply(&v);
        project_out(&mut next, basis);
        let norm = l2_norm(&next);
        if norm == 0.0 {
            break;
        }
        for x in next.iter_mut() {
            *x /= norm;
        }
        let diff: f64 = v.iter().zip(&next).map(|(a, b)| (a - b).abs()).sum();
        v = next;
        if diff < tolerance {
            break;
        }
    }
    v
}

/// Min–max normalise a vector into `[0, 1]`. A degenerate (zero-span or
/// non-finite) vector collapses to the centre `0.5` so a hint is always finite.
fn normalize_unit_interval(v: &[f64]) -> Vec<f64> {
    let min = v.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let span = max - min;
    if !span.is_finite() || span <= 0.0 {
        return vec![0.5; v.len()];
    }
    v.iter().map(|x| (x - min) / span).collect()
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

    // ── shortest_path (issue #798) ──

    /// Total weight of a path (the last entry's cumulative weight), or `None`
    /// when no path exists.
    fn path_weight(path: &Option<Vec<(String, f64)>>) -> Option<f64> {
        path.as_ref()
            .map(|p| p.last().map(|(_, w)| *w).unwrap_or(0.0))
    }

    #[test]
    fn shortest_path_golden_known_path() {
        // Diamond: a-b (1), a-c (4), b-c (1), c-d (1), b-d (5).
        // a -> d shortest is a-b-c-d with weight 3 (beats a-c-d=5, a-b-d=6).
        let nodes: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        let edges = vec![
            ("a".to_string(), "b".to_string(), 1.0f32),
            ("a".to_string(), "c".to_string(), 4.0),
            ("b".to_string(), "c".to_string(), 1.0),
            ("c".to_string(), "d".to_string(), 1.0),
            ("b".to_string(), "d".to_string(), 5.0),
        ];
        let path = shortest_path(&nodes, &edges, &"a".to_string(), &"d".to_string(), None)
            .expect("a reaches d");
        let route: Vec<String> = path.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(route, vec!["a", "b", "c", "d"], "min-weight route");
        // Hop ordering: first entry is the source at weight 0, cumulative weight
        // is monotonically non-decreasing, last entry is the total.
        assert_eq!(path[0], ("a".to_string(), 0.0));
        assert_eq!(path[1].1, 1.0); // a-b
        assert_eq!(path[2].1, 2.0); // +b-c
        assert_eq!(path[3].1, 3.0); // +c-d (total)
    }

    #[test]
    fn shortest_path_self_is_zero_length() {
        let nodes: Vec<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        let edges = vec![("a".to_string(), "b".to_string(), 1.0f32)];
        let path = shortest_path(&nodes, &edges, &"a".to_string(), &"a".to_string(), None)
            .expect("self path exists");
        assert_eq!(path, vec![("a".to_string(), 0.0)]);
    }

    #[test]
    fn shortest_path_unreachable_is_none() {
        // Two disconnected edges: {a-b} and {c-d}. a cannot reach d.
        let nodes: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        let edges = vec![
            ("a".to_string(), "b".to_string(), 1.0f32),
            ("c".to_string(), "d".to_string(), 1.0),
        ];
        assert!(shortest_path(&nodes, &edges, &"a".to_string(), &"d".to_string(), None).is_none());
    }

    #[test]
    fn shortest_path_missing_endpoint_is_none() {
        let nodes: Vec<String> = vec!["a".to_string()];
        let edges: Vec<(String, String, Weight)> = vec![];
        // dst not in the universe.
        assert!(shortest_path(&nodes, &edges, &"a".to_string(), &"z".to_string(), None).is_none());
    }

    #[test]
    fn shortest_path_max_hops_caps_edge_count() {
        // Path a-b-c-d needs 3 edges. A direct shortcut a-d (weight 10) needs 1.
        let nodes: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        let edges = vec![
            ("a".to_string(), "b".to_string(), 1.0f32),
            ("b".to_string(), "c".to_string(), 1.0),
            ("c".to_string(), "d".to_string(), 1.0),
            ("a".to_string(), "d".to_string(), 10.0),
        ];
        // Unbounded: the cheap 3-hop route wins (weight 3).
        let unbounded = shortest_path(&nodes, &edges, &"a".to_string(), &"d".to_string(), None);
        assert_eq!(path_weight(&unbounded), Some(3.0));
        // max_hops = 1: only the direct shortcut fits the budget (weight 10).
        let capped = shortest_path(&nodes, &edges, &"a".to_string(), &"d".to_string(), Some(1));
        assert_eq!(path_weight(&capped), Some(10.0));
        // max_hops = 0: no edges allowed, so distinct endpoints are unreachable.
        assert!(
            shortest_path(&nodes, &edges, &"a".to_string(), &"d".to_string(), Some(0)).is_none()
        );
    }

    // ── Centrality family: betweenness / eigenvector / pagerank (issue #797) ──

    /// Default power-iteration cap and tolerance for eigenvector tests.
    const EIG_ITERS: usize = 200;
    const EIG_TOL: f64 = 1e-10;
    /// Classic PageRank damping and a generous iteration cap for convergence.
    const PR_DAMPING: f64 = 0.85;
    const PR_ITERS: usize = 200;

    fn map_of(rows: Vec<(String, f64)>) -> BTreeMap<String, f64> {
        rows.into_iter().collect()
    }

    /// An undirected star K1,3: centre "c" joined to leaves "l1","l2","l3".
    fn star() -> (Vec<String>, Vec<(String, String, Weight)>) {
        let nodes: Vec<String> = ["c", "l1", "l2", "l3"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let edges = vec![w("c", "l1"), w("c", "l2"), w("c", "l3")];
        (nodes, edges)
    }

    /// An undirected path P3: a - b - c.
    fn path3() -> (Vec<String>, Vec<(String, String, Weight)>) {
        let nodes: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let edges = vec![w("a", "b"), w("b", "c")];
        (nodes, edges)
    }

    /// A triangle K3 over a,b,c — fully symmetric.
    fn triangle() -> (Vec<String>, Vec<(String, String, Weight)>) {
        let nodes: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let edges = vec![w("a", "b"), w("b", "c"), w("c", "a")];
        (nodes, edges)
    }

    // ── betweenness golden + properties ──

    #[test]
    fn betweenness_golden_star_centre_carries_all_pairs() {
        // In K1,3 every shortest path between two leaves passes through the
        // centre. There are C(3,2) = 3 leaf pairs, so centre = 3.0 and each
        // leaf = 0.0 (no pair routes through a leaf).
        let (nodes, edges) = star();
        let got = map_of(betweenness(&nodes, &edges));
        assert!(
            (got["c"] - 3.0).abs() < 1e-9,
            "centre = 3.0, got {}",
            got["c"]
        );
        for leaf in ["l1", "l2", "l3"] {
            assert!(got[leaf].abs() < 1e-9, "{leaf} = 0.0, got {}", got[leaf]);
        }
    }

    #[test]
    fn betweenness_golden_path_middle_is_one() {
        // On a - b - c only the pair {a, c} routes through b, so b = 1.0 and
        // the two endpoints score 0.0.
        let (nodes, edges) = path3();
        let got = map_of(betweenness(&nodes, &edges));
        assert!(
            (got["b"] - 1.0).abs() < 1e-9,
            "middle = 1.0, got {}",
            got["b"]
        );
        assert!(got["a"].abs() < 1e-9);
        assert!(got["c"].abs() < 1e-9);
    }

    #[test]
    fn betweenness_isolated_node_scores_zero() {
        let nodes: Vec<String> = ["a", "b", "iso"].iter().map(|s| s.to_string()).collect();
        let edges = vec![w("a", "b")];
        let got = map_of(betweenness(&nodes, &edges));
        assert!(got["iso"].abs() < 1e-9, "isolated node scores 0.0");
        // A single edge contributes no intermediary, so its endpoints are 0 too.
        assert!(got["a"].abs() < 1e-9);
        assert!(got["b"].abs() < 1e-9);
    }

    #[test]
    fn betweenness_empty_graph_is_empty() {
        let empty: Vec<String> = Vec::new();
        assert!(betweenness(&empty, &[]).is_empty());
    }

    // ── eigenvector golden + properties ──

    #[test]
    fn eigenvector_golden_path3_known_vector() {
        // The path P3 adjacency has principal eigenvector (1, √2, 1); L2
        // normalised that is (0.5, 1/√2, 0.5).
        let (nodes, edges) = path3();
        let got = map_of(eigenvector(&nodes, &edges, EIG_ITERS, EIG_TOL));
        let inv_sqrt2 = 1.0 / 2.0_f64.sqrt();
        assert!(
            (got["b"] - inv_sqrt2).abs() < 1e-6,
            "centre {} ~ {inv_sqrt2}",
            got["b"]
        );
        assert!(
            (got["a"] - 0.5).abs() < 1e-6,
            "endpoint a {} ~ 0.5",
            got["a"]
        );
        assert!(
            (got["c"] - 0.5).abs() < 1e-6,
            "endpoint c {} ~ 0.5",
            got["c"]
        );
        // L2-normalised: Σ x² = 1.
        let sumsq: f64 = got.values().map(|v| v * v).sum();
        assert!((sumsq - 1.0).abs() < 1e-6, "unit L2 norm, got {sumsq}");
    }

    #[test]
    fn eigenvector_golden_triangle_all_equal() {
        // K3 is vertex-transitive, so all three scores equal 1/√3.
        let (nodes, edges) = triangle();
        let got = map_of(eigenvector(&nodes, &edges, EIG_ITERS, EIG_TOL));
        let expect = 1.0 / 3.0_f64.sqrt();
        for k in ["a", "b", "c"] {
            assert!((got[k] - expect).abs() < 1e-6, "{k} = 1/√3, got {}", got[k]);
        }
    }

    #[test]
    fn eigenvector_isolated_node_scores_zero() {
        let nodes: Vec<String> = ["a", "b", "iso"].iter().map(|s| s.to_string()).collect();
        let edges = vec![w("a", "b")];
        let got = map_of(eigenvector(&nodes, &edges, EIG_ITERS, EIG_TOL));
        assert!(
            got["iso"].abs() < 1e-9,
            "isolated node scores 0.0, got {}",
            got["iso"]
        );
    }

    #[test]
    fn shortest_path_parallel_edges_take_minimum() {
        // Two parallel a-b edges (weight 5 and weight 2): the min wins.
        let nodes: Vec<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        let edges = vec![
            ("a".to_string(), "b".to_string(), 5.0f32),
            ("a".to_string(), "b".to_string(), 2.0),
        ];
        let path = shortest_path(&nodes, &edges, &"a".to_string(), &"b".to_string(), None);
        assert_eq!(path_weight(&path), Some(2.0));
    }

    /// A small id universe plus random POSITIVE-weight edges between those ids.
    /// Weights are integers in 1..=9 (as f32) so distance comparisons are exact
    /// and the non-negative-weight precondition holds.
    fn weighted_graph_strategy(
    ) -> impl Strategy<Value = (Vec<String>, Vec<(String, String, Weight)>)> {
        (2usize..7usize).prop_flat_map(|n| {
            let nodes: Vec<String> = (0..n).map(|i| format!("n{i:02}")).collect();
            let ids = nodes.clone();
            let edge = (0..n, 0..n, 1u32..10u32)
                .prop_map(move |(a, b, w)| (format!("n{a:02}"), format!("n{b:02}"), w as f32));
            let edges = prop::collection::vec(edge, 0..14);
            (Just(ids), edges)
        })
    }

    #[test]
    fn eigenvector_edgeless_graph_is_uniform_unit() {
        let nodes: Vec<String> = ["x", "y", "z", "q"].iter().map(|s| s.to_string()).collect();
        let got = map_of(eigenvector(&nodes, &[], EIG_ITERS, EIG_TOL));
        let expect = 1.0 / 4.0_f64.sqrt();
        for k in ["x", "y", "z", "q"] {
            assert!((got[k] - expect).abs() < 1e-9, "uniform 1/√n");
        }
    }

    // ── pagerank golden + properties ──

    #[test]
    fn pagerank_golden_triangle_all_equal() {
        // K3 is symmetric, so PageRank is uniform 1/3 regardless of damping.
        let (nodes, edges) = triangle();
        let got = map_of(pagerank(&nodes, &edges, PR_DAMPING, PR_ITERS));
        for k in ["a", "b", "c"] {
            assert!(
                (got[k] - 1.0 / 3.0).abs() < 1e-9,
                "{k} = 1/3, got {}",
                got[k]
            );
        }
        let sum: f64 = got.values().sum();
        assert!((sum - 1.0).abs() < 1e-9, "sums to 1");
    }

    #[test]
    fn pagerank_golden_star_centre_ranks_highest() {
        // In K1,3 the centre collects rank from all three leaves and outranks
        // every (symmetric) leaf; the leaves share one rank.
        let (nodes, edges) = star();
        let got = map_of(pagerank(&nodes, &edges, PR_DAMPING, PR_ITERS));
        for leaf in ["l1", "l2", "l3"] {
            assert!(got["c"] > got[leaf], "centre outranks {leaf}");
            assert!((got[leaf] - got["l1"]).abs() < 1e-9, "leaves share rank");
        }
        let sum: f64 = got.values().sum();
        assert!((sum - 1.0).abs() < 1e-9, "sums to 1");
    }

    #[test]
    fn pagerank_isolated_node_gets_teleport_floor() {
        let nodes: Vec<String> = ["a", "b", "iso"].iter().map(|s| s.to_string()).collect();
        let edges = vec![w("a", "b")];
        let got = map_of(pagerank(&nodes, &edges, PR_DAMPING, PR_ITERS));
        let sum: f64 = got.values().sum();
        assert!((sum - 1.0).abs() < 1e-9, "sums to 1");
        // Every node clears the teleport floor (1-d)/n.
        let floor = (1.0 - PR_DAMPING) / 3.0;
        for v in got.values() {
            assert!(*v + 1e-12 >= floor, "score {v} >= floor {floor}");
        }
        // The isolated node ranks below both connected nodes.
        assert!(got["iso"] < got["a"]);
        assert!(got["iso"] < got["b"]);
    }

    #[test]
    fn centrality_empty_graph_all_empty() {
        let empty: Vec<String> = Vec::new();
        assert!(eigenvector(&empty, &[], EIG_ITERS, EIG_TOL).is_empty());
        assert!(pagerank(&empty, &[], PR_DAMPING, PR_ITERS).is_empty());
    }

    // ── spectral_embedding (issue #804) ──

    const SPECTRAL_ITERS: usize = 200;
    const SPECTRAL_TOL: f64 = 1e-9;

    fn hint_map(rows: Vec<(String, (f64, f64))>) -> BTreeMap<String, (f64, f64)> {
        rows.into_iter().collect()
    }

    #[test]
    fn spectral_embedding_coords_in_unit_interval() {
        // Two triangles bridged: every coordinate must land in [0, 1].
        let (nodes, edges) = two_cliques_bridge();
        let rows = spectral_embedding(&nodes, &edges, SPECTRAL_ITERS, SPECTRAL_TOL);
        for (node, (x, y)) in &rows {
            assert!(
                (0.0..=1.0).contains(x) && (0.0..=1.0).contains(y),
                "node {node} hint ({x}, {y}) must be normalised to [0, 1]²"
            );
        }
        // Min–max normalisation pins the extremes of the spread to 0 and 1.
        let xs: Vec<f64> = rows.iter().map(|(_, (x, _))| *x).collect();
        let min_x = xs.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_x = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        assert!(min_x.abs() < 1e-9, "x min anchored at 0, got {min_x}");
        assert!(
            (max_x - 1.0).abs() < 1e-9,
            "x max anchored at 1, got {max_x}"
        );
    }

    #[test]
    fn spectral_embedding_determinism_100_runs_identical() {
        // AC: identical input must produce identical hints (no random init).
        let (nodes, edges) = karate_club();
        let first = spectral_embedding(&nodes, &edges, SPECTRAL_ITERS, SPECTRAL_TOL);
        for _ in 0..100 {
            assert_eq!(
                spectral_embedding(&nodes, &edges, SPECTRAL_ITERS, SPECTRAL_TOL),
                first,
                "spectral embedding must be bit-for-bit reproducible"
            );
        }
    }

    #[test]
    fn spectral_embedding_node_order_independent() {
        // Shuffling the input node/edge order must not change the per-node hint:
        // the embedding sorts the universe internally before laying out.
        let (nodes, edges) = two_cliques_bridge();
        let mut shuffled_nodes = nodes.clone();
        shuffled_nodes.reverse();
        let mut shuffled_edges = edges.clone();
        shuffled_edges.reverse();

        let a = hint_map(spectral_embedding(
            &nodes,
            &edges,
            SPECTRAL_ITERS,
            SPECTRAL_TOL,
        ));
        let b = hint_map(spectral_embedding(
            &shuffled_nodes,
            &shuffled_edges,
            SPECTRAL_ITERS,
            SPECTRAL_TOL,
        ));
        assert_eq!(a, b, "hint is keyed on node identity, not input order");
    }

    #[test]
    fn spectral_embedding_separates_disconnected_components() {
        // Two disjoint triangles: the Fiedler coordinate should place the two
        // components in distinct regions of the x axis (graph-distance shows up
        // as layout distance), so the means differ noticeably.
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
        let map = hint_map(spectral_embedding(
            &nodes,
            &edges,
            SPECTRAL_ITERS,
            SPECTRAL_TOL,
        ));
        let left = (map["a"].0 + map["b"].0 + map["c"].0) / 3.0;
        let right = (map["d"].0 + map["e"].0 + map["f"].0) / 3.0;
        assert!(
            (left - right).abs() > 0.25,
            "disconnected components should separate on the layout (Δx = {})",
            (left - right).abs()
        );
    }

    #[test]
    fn spectral_embedding_degenerate_graphs() {
        // Empty graph -> empty result.
        let empty: Vec<String> = Vec::new();
        assert!(spectral_embedding(&empty, &[], SPECTRAL_ITERS, SPECTRAL_TOL).is_empty());

        // Single node -> centred, finite hint.
        let one = vec!["solo".to_string()];
        let rows = spectral_embedding(&one, &[], SPECTRAL_ITERS, SPECTRAL_TOL);
        assert_eq!(rows, vec![("solo".to_string(), (0.5, 0.5))]);

        // Edgeless multi-node graph -> still finite and in range.
        let nodes: Vec<String> = ["x", "y", "z"].iter().map(|s| s.to_string()).collect();
        for (_, (x, y)) in spectral_embedding(&nodes, &[], SPECTRAL_ITERS, SPECTRAL_TOL) {
            assert!(x.is_finite() && y.is_finite(), "edgeless hint stays finite");
            assert!((0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y));
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        // (a) symmetric distance on undirected graphs:
        //     weight(src -> dst) == weight(dst -> src), and reachability agrees.
        #[test]
        fn shortest_path_is_symmetric((nodes, edges) in weighted_graph_strategy()) {
            let universe = node_universe(&nodes, &edges);
            for a in &universe {
                for b in &universe {
                    let fwd = shortest_path(&nodes, &edges, a, b, None);
                    let rev = shortest_path(&nodes, &edges, b, a, None);
                    prop_assert_eq!(
                        path_weight(&fwd), path_weight(&rev),
                        "distance {} <-> {} must be symmetric", a, b
                    );
                }
            }
        }

        // (b) triangle inequality: for reachable a,b,c,
        //     dist(a, c) <= dist(a, b) + dist(b, c).
        #[test]
        fn shortest_path_triangle_inequality((nodes, edges) in weighted_graph_strategy()) {
            let universe = node_universe(&nodes, &edges);
            for a in &universe {
                for b in &universe {
                    for c in &universe {
                        let ab = path_weight(&shortest_path(&nodes, &edges, a, b, None));
                        let bc = path_weight(&shortest_path(&nodes, &edges, b, c, None));
                        let ac = path_weight(&shortest_path(&nodes, &edges, a, c, None));
                        if let (Some(ab), Some(bc), Some(ac)) = (ab, bc, ac) {
                            prop_assert!(
                                ac <= ab + bc + 1e-9,
                                "triangle: d({},{})={} > d({},{})={} + d({},{})={}",
                                a, c, ac, a, b, ab, b, c, bc
                            );
                        }
                    }
                }
            }
        }

        // (c) unreachable pairs across distinct connected components yield None.
        //     Build two disjoint cliques and assert no cross-component path.
        #[test]
        fn shortest_path_unreachable_across_components(
            (left, right) in (1usize..4usize, 1usize..4usize)
        ) {
            // Left clique uses ids l0.., right clique uses r0.. — disjoint id
            // spaces guarantee no shared node and thus no connecting edge.
            let mut nodes = Vec::new();
            let mut edges: Vec<(String, String, Weight)> = Vec::new();
            for i in 0..left { nodes.push(format!("l{i}")); }
            for i in 0..right { nodes.push(format!("r{i}")); }
            for i in 0..left {
                for j in (i + 1)..left {
                    edges.push((format!("l{i}"), format!("l{j}"), 1.0));
                }
            }
            for i in 0..right {
                for j in (i + 1)..right {
                    edges.push((format!("r{i}"), format!("r{j}"), 1.0));
                }
            }
            // Every left node is unreachable from every right node.
            prop_assert!(
                shortest_path(&nodes, &edges, &"l0".to_string(), &"r0".to_string(), None).is_none()
            );
            prop_assert!(
                shortest_path(&nodes, &edges, &"r0".to_string(), &"l0".to_string(), None).is_none()
            );
        }

        // determinism: identical input -> identical output.
        #[test]
        fn shortest_path_determinism((nodes, edges) in weighted_graph_strategy()) {
            let universe = node_universe(&nodes, &edges);
            if universe.len() >= 2 {
                let a = &universe[0];
                let b = &universe[universe.len() - 1];
                let p1 = shortest_path(&nodes, &edges, a, b, None);
                let p2 = shortest_path(&nodes, &edges, a, b, None);
                prop_assert_eq!(p1, p2);
            }
        }

        // betweenness: one row per node; all scores >= 0; isolated nodes (no
        // incident edge) score exactly 0; deterministic across runs.
        #[test]
        fn betweenness_range_isolation_and_determinism((nodes, edges) in graph_strategy()) {
            let rows = betweenness(&nodes, &edges);

            let mut universe: BTreeSet<String> = BTreeSet::new();
            for n in &nodes { universe.insert(n.clone()); }
            for (a, b, _) in &edges { universe.insert(a.clone()); universe.insert(b.clone()); }
            prop_assert_eq!(rows.len(), universe.len());

            // Degrees (undirected, self-loops excluded).
            let mut deg: BTreeMap<String, usize> = BTreeMap::new();
            for n in &universe { deg.insert(n.clone(), 0); }
            for (a, b, _) in &edges {
                if a != b {
                    *deg.get_mut(a).unwrap() += 1;
                    *deg.get_mut(b).unwrap() += 1;
                }
            }
            for (node, score) in &rows {
                prop_assert!(*score >= -1e-9, "score {} >= 0", score);
                if deg[node] == 0 {
                    prop_assert!(score.abs() < 1e-9, "isolated {} scores 0", node);
                }
            }

            prop_assert_eq!(betweenness(&nodes, &edges), rows);
        }

        // eigenvector: L2-normalised (Σx² ≈ 1), every score in [0, 1], and an
        // isolated node scores 0 whenever the graph has any edge. Deterministic.
        #[test]
        fn eigenvector_unit_norm_range_and_isolation((nodes, edges) in graph_strategy()) {
            let rows = eigenvector(&nodes, &edges, EIG_ITERS, EIG_TOL);
            prop_assume!(!rows.is_empty());

            let sumsq: f64 = rows.iter().map(|(_, v)| v * v).sum();
            prop_assert!((sumsq - 1.0).abs() < 1e-6, "unit L2 norm, got {}", sumsq);

            let mut deg: BTreeMap<String, usize> = BTreeMap::new();
            for (n, _) in &rows { deg.insert(n.clone(), 0); }
            for (a, b, _) in &edges {
                if a != b {
                    *deg.get_mut(a).unwrap() += 1;
                    *deg.get_mut(b).unwrap() += 1;
                }
            }
            let has_edges = deg.values().any(|d| *d > 0);
            for (node, score) in &rows {
                prop_assert!(*score >= -1e-9 && *score <= 1.0 + 1e-9, "score {} in [0,1]", score);
                if has_edges && deg[node] == 0 {
                    prop_assert!(score.abs() < 1e-6, "isolated {} ~ 0", node);
                }
            }

            prop_assert_eq!(eigenvector(&nodes, &edges, EIG_ITERS, EIG_TOL), rows);
        }

        // pagerank: scores sum to ~1, every score >= the teleport floor
        // (1-d)/n (so strictly positive), and the result is deterministic.
        #[test]
        fn pagerank_sums_to_one_floor_and_determinism((nodes, edges) in graph_strategy()) {
            let rows = pagerank(&nodes, &edges, PR_DAMPING, PR_ITERS);
            prop_assume!(!rows.is_empty());

            let sum: f64 = rows.iter().map(|(_, v)| v).sum();
            prop_assert!((sum - 1.0).abs() < 1e-9, "PageRank sums to 1, got {}", sum);

            let floor = (1.0 - PR_DAMPING) / rows.len() as f64;
            for (_, score) in &rows {
                prop_assert!(*score + 1e-12 >= floor, "score {} >= floor {}", score, floor);
            }

            prop_assert_eq!(pagerank(&nodes, &edges, PR_DAMPING, PR_ITERS), rows);
        }

        // spectral_embedding: one hint per node, every coordinate finite and in
        // [0, 1], and the result is deterministic across runs.
        #[test]
        fn spectral_embedding_range_coverage_and_determinism((nodes, edges) in graph_strategy()) {
            let rows = spectral_embedding(&nodes, &edges, SPECTRAL_ITERS, SPECTRAL_TOL);

            let mut universe: BTreeSet<String> = BTreeSet::new();
            for n in &nodes { universe.insert(n.clone()); }
            for (a, b, _) in &edges { universe.insert(a.clone()); universe.insert(b.clone()); }
            prop_assert_eq!(rows.len(), universe.len());

            for (node, (x, y)) in &rows {
                prop_assert!(x.is_finite() && y.is_finite(), "node {} hint is finite", node);
                prop_assert!(
                    *x >= -1e-9 && *x <= 1.0 + 1e-9 && *y >= -1e-9 && *y <= 1.0 + 1e-9,
                    "node {} hint ({}, {}) in [0,1]^2", node, x, y
                );
            }

            prop_assert_eq!(spectral_embedding(&nodes, &edges, SPECTRAL_ITERS, SPECTRAL_TOL), rows);
        }
    }
}
