//! End-to-end test for the centrality-family table-valued functions (#797):
//! `betweenness(<graph>)`, `eigenvector(<graph> [, max_iterations, tolerance])`,
//! and `pagerank(<graph> [, damping, max_iterations])`.
//!
//! Builds one graph collection with a clear hub-and-spoke shape, runs each TVF
//! through the full SQL → parser → executor → row-projection path, and asserts
//! every TVF returns `(node_id, score)` rows that rank the hub above the leaves,
//! deterministically across reruns. Also exercises the named-argument forms.
//!
//! Known v0 limitation (shared with `components`/`louvain`, #795/#796): the
//! `<collection>` argument is NOT resolved — the TVF runs over the WHOLE graph
//! store. This test therefore places the whole structure in one collection.

mod support;

use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use std::collections::BTreeMap;
use support::PersistentDbPath;

/// Run a centrality TVF and return a map of node_id -> score, asserting the
/// `(node_id, score)` shape and TVF routing.
fn run_centrality(rt: &RedDBRuntime, sql: &str) -> BTreeMap<String, f64> {
    let result = rt.execute_query(sql).expect("centrality query");
    assert_eq!(
        result.engine, "runtime-graph-tvf",
        "centrality TVF must route through the TVF executor"
    );
    let unified = result.result;
    assert_eq!(
        unified.columns,
        vec!["node_id".to_string(), "score".to_string()],
        "columns must be node_id, score"
    );

    let mut map = BTreeMap::new();
    for rec in &unified.records {
        let node = match rec.get("node_id").expect("node_id present") {
            Value::Text(s) => s.to_string(),
            other => panic!("node_id should be Text, got {other:?}"),
        };
        let score = match rec.get("score").expect("score present") {
            Value::Float(f) => *f,
            other => panic!("score should be Float, got {other:?}"),
        };
        map.insert(node, score);
    }
    map
}

/// A hub-and-spoke graph: hub joined to three leaves, plus one extra edge
/// between two leaves. The hub is unambiguously the most central node under all
/// three measures, giving a sensible, distinct ranking to assert against.
fn build_graph(rt: &RedDBRuntime) -> (String, Vec<String>) {
    let hub = make_node(rt, "g", "hub");
    let leaves: Vec<String> = (1..=3)
        .map(|n| make_node(rt, "g", &format!("leaf{n}")))
        .collect();
    for leaf in &leaves {
        make_edge(rt, "g", &hub, leaf);
    }
    // One leaf-to-leaf edge breaks symmetry between the leaves.
    make_edge(rt, "g", &leaves[0], &leaves[1]);
    (hub, leaves)
}

#[test]
fn betweenness_tvf_ranks_hub_highest() {
    let db = PersistentDbPath::new("issue_797_betweenness_tvf");
    let rt = db.open_runtime();
    let (hub, leaves) = build_graph(&rt);

    let map = run_centrality(&rt, "SELECT * FROM betweenness(g)");
    assert_eq!(map.len(), 4, "all nodes scored: {map:?}");
    for leaf in &leaves {
        assert!(
            map[&hub] >= map[leaf],
            "hub betweenness {} >= leaf {} {}",
            map[&hub],
            leaf,
            map[leaf]
        );
    }
    // The hub bridges leaf3 to the rest, so it must carry strictly positive
    // betweenness while leaf3 (a pure spoke) carries none.
    assert!(map[&hub] > 0.0, "hub has positive betweenness");
    assert!(
        map[&leaves[2]].abs() < 1e-9,
        "pure-spoke leaf3 has zero betweenness"
    );

    // Determinism across reruns.
    assert_eq!(map, run_centrality(&rt, "SELECT * FROM betweenness(g)"));
}

#[test]
fn eigenvector_tvf_ranks_hub_highest_and_is_normalised() {
    let db = PersistentDbPath::new("issue_797_eigenvector_tvf");
    let rt = db.open_runtime();
    let (hub, leaves) = build_graph(&rt);

    // Exercise the named-argument form end to end.
    let map = run_centrality(
        &rt,
        "SELECT * FROM eigenvector(g, max_iterations => 200, tolerance => 0.0000001)",
    );
    assert_eq!(map.len(), 4);
    for leaf in &leaves {
        assert!(map[&hub] >= map[leaf], "hub eigenvector >= leaf {leaf}");
    }
    // L2-normalised: Σ score² ≈ 1.
    let sumsq: f64 = map.values().map(|v| v * v).sum();
    assert!((sumsq - 1.0).abs() < 1e-6, "unit L2 norm, got {sumsq}");

    assert_eq!(
        map,
        run_centrality(
            &rt,
            "SELECT * FROM eigenvector(g, max_iterations => 200, tolerance => 0.0000001)"
        )
    );
}

#[test]
fn pagerank_tvf_ranks_hub_highest_and_sums_to_one() {
    let db = PersistentDbPath::new("issue_797_pagerank_tvf");
    let rt = db.open_runtime();
    let (hub, leaves) = build_graph(&rt);

    // Bare form first.
    let map = run_centrality(&rt, "SELECT * FROM pagerank(g)");
    assert_eq!(map.len(), 4);
    for leaf in &leaves {
        assert!(map[&hub] >= map[leaf], "hub pagerank >= leaf {leaf}");
    }
    let sum: f64 = map.values().sum();
    assert!((sum - 1.0).abs() < 1e-9, "PageRank sums to 1, got {sum}");

    // Named-argument form also routes and sums to 1.
    let map2 = run_centrality(
        &rt,
        "SELECT * FROM pagerank(g, damping => 0.85, max_iterations => 100)",
    );
    let sum2: f64 = map2.values().sum();
    assert!((sum2 - 1.0).abs() < 1e-9);
    assert!(map2[&hub] >= map2[&leaves[2]]);
}

#[test]
fn centrality_family_produces_distinct_sensible_rankings() {
    // All three TVFs over the SAME collection agree the hub is most central,
    // yet each is a distinct measure (the score vectors differ).
    let db = PersistentDbPath::new("issue_797_centrality_family");
    let rt = db.open_runtime();
    let (hub, _leaves) = build_graph(&rt);

    let bet = run_centrality(&rt, "SELECT * FROM betweenness(g)");
    let eig = run_centrality(&rt, "SELECT * FROM eigenvector(g)");
    let pr = run_centrality(&rt, "SELECT * FROM pagerank(g)");

    // Each ranks the hub top.
    let top = |m: &BTreeMap<String, f64>| -> String {
        m.iter()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(k, _)| k.clone())
            .unwrap()
    };
    assert_eq!(top(&bet), hub);
    assert_eq!(top(&eig), hub);
    assert_eq!(top(&pr), hub);

    // The three measures are genuinely different — their raw score vectors are
    // not identical (betweenness counts paths, the others are probabilities).
    assert_ne!(
        bet.values().cloned().collect::<Vec<_>>(),
        pr.values().cloned().collect::<Vec<_>>(),
        "betweenness and pagerank are distinct measures"
    );
    assert_ne!(
        eig.values().cloned().collect::<Vec<_>>(),
        pr.values().cloned().collect::<Vec<_>>(),
        "eigenvector and pagerank are distinct measures"
    );
}

/// Insert a graph node via SQL and return its assigned graph node id.
fn make_node(rt: &RedDBRuntime, collection: &str, label: &str) -> String {
    rt.execute_query(&format!(
        "INSERT INTO {collection} NODE (label, node_type) VALUES ('{label}', 'Host')"
    ))
    .unwrap_or_else(|err| panic!("insert node {label}: {err:?}"));
    graph_node_id(rt, collection, label)
}

/// Insert a directed edge between two graph nodes via SQL.
fn make_edge(rt: &RedDBRuntime, collection: &str, from: &str, to: &str) {
    rt.execute_query(&format!(
        "INSERT INTO {collection} EDGE (label, from, to, weight) VALUES ('connects', {from}, {to}, 1.0)"
    ))
    .unwrap_or_else(|err| panic!("insert edge {from}->{to}: {err:?}"));
}

/// Resolve a graph node's storage id by its label.
fn graph_node_id(rt: &RedDBRuntime, collection: &str, label: &str) -> String {
    use reddb::storage::EntityKind;
    rt.db()
        .store()
        .get_collection(collection)
        .unwrap_or_else(|| panic!("collection '{collection}' should exist"))
        .query_all(|_| true)
        .into_iter()
        .find_map(|entity| match &entity.kind {
            EntityKind::GraphNode(node) if node.label == label => Some(entity.id.raw().to_string()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("graph node '{label}' not found in '{collection}'"))
}
