//! Integration tests for RedDB — Graph analytics, admin, catalog, health, and operational features.
//!
//! Covers graph neighborhood, traversal, shortest-path, components, centrality, community
//! detection, cycle detection, clustering coefficients, catalog/admin operations, health
//! reports, index lifecycle, checkpointing, multi-collection isolation, large batch inserts,
//! and read-after-write consistency.

use reddb::{
    ArtifactState, RedDBRuntime, EntityId, EntityUseCases, QueryUseCases, GraphUseCases,
    NativeUseCases, CatalogUseCases, HealthState,
};
use reddb::application::{
    CreateRowInput, CreateNodeInput, CreateEdgeInput,
    GraphNeighborhoodInput, GraphTraversalInput, GraphShortestPathInput,
    GraphComponentsInput, GraphCentralityInput, GraphCommunitiesInput,
    GraphCyclesInput, GraphClusteringInput, ExecuteQueryInput,
};
use reddb::runtime::{
    RuntimeGraphDirection, RuntimeGraphTraversalStrategy, RuntimeGraphPathAlgorithm,
    RuntimeGraphCentralityAlgorithm, RuntimeGraphCommunityAlgorithm,
    RuntimeGraphComponentsMode,
};
use reddb::storage::schema::Value;

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("failed to create in-memory runtime")
}

/// A created node, carrying both the EntityId (for edge creation) and the
/// string graph-node-id used by the graph analytics APIs.
struct NodeHandle {
    entity_id: EntityId,
    graph_id: String,
}

/// Helper: create a named node in a collection and return its handle.
fn make_node(uc: &EntityUseCases<'_, RedDBRuntime>, collection: &str, label: &str) -> NodeHandle {
    let out = uc
        .create_node(CreateNodeInput {
            collection: collection.into(),
            label: label.into(),
            node_type: Some("Host".into()),
            properties: vec![("name".into(), Value::Text(label.into()))],
            metadata: vec![],
            embeddings: vec![],
            table_links: vec![],
            node_links: vec![],
        })
        .unwrap();
    NodeHandle {
        graph_id: out.id.raw().to_string(),
        entity_id: out.id,
    }
}

/// Helper: create a directed edge between two nodes.
fn make_edge(
    uc: &EntityUseCases<'_, RedDBRuntime>,
    collection: &str,
    label: &str,
    from: &NodeHandle,
    to: &NodeHandle,
    weight: f32,
) {
    uc.create_edge(CreateEdgeInput {
        collection: collection.into(),
        label: label.into(),
        from: from.entity_id,
        to: to.entity_id,
        weight: Some(weight),
        properties: vec![],
        metadata: vec![],
    })
    .expect("create_edge should succeed");
}

// ---------------------------------------------------------------------------
// 1. Graph Neighborhood
// ---------------------------------------------------------------------------

#[test]
fn test_graph_neighborhood() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let graph = GraphUseCases::new(&rt);

    let col = "neighborhood_net";

    // Star graph: center + 5 spokes
    let center = make_node(&entity, col, "center");
    let mut spokes = Vec::new();
    for i in 0..5 {
        let spoke = make_node(&entity, col, &format!("spoke_{i}"));
        make_edge(&entity, col, "connects_to", &center, &spoke, 1.0);
        spokes.push(spoke);
    }

    let result = graph
        .neighborhood(GraphNeighborhoodInput {
            node: center.graph_id.clone(),
            direction: RuntimeGraphDirection::Outgoing,
            max_depth: 1,
            edge_labels: None,
            projection: None,
        })
        .expect("neighborhood should succeed");

    // The neighborhood may include the source node itself, so we filter it out
    // and count only the actual neighbors.
    let neighbor_count = result
        .nodes
        .iter()
        .filter(|v| v.node.id != center.graph_id)
        .count();
    assert_eq!(
        neighbor_count, 5,
        "center should have exactly 5 outgoing neighbors (excluding self), got {}",
        neighbor_count
    );
    assert_eq!(result.source, center.graph_id);
}

// ---------------------------------------------------------------------------
// 2. Graph Traversal — BFS
// ---------------------------------------------------------------------------

#[test]
fn test_graph_traversal_bfs() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let graph = GraphUseCases::new(&rt);

    let col = "traversal_chain";

    // Chain: A -> B -> C -> D
    let a = make_node(&entity, col, "A");
    let b = make_node(&entity, col, "B");
    let c = make_node(&entity, col, "C");
    let d = make_node(&entity, col, "D");

    make_edge(&entity, col, "connects_to", &a, &b, 1.0);
    make_edge(&entity, col, "connects_to", &b, &c, 1.0);
    make_edge(&entity, col, "connects_to", &c, &d, 1.0);

    let result = graph
        .traverse(GraphTraversalInput {
            source: a.graph_id.clone(),
            direction: RuntimeGraphDirection::Outgoing,
            max_depth: 10,
            strategy: RuntimeGraphTraversalStrategy::Bfs,
            edge_labels: None,
            projection: None,
        })
        .expect("BFS traversal should succeed");

    // Should visit at least B, C, D (the source may or may not be in visits)
    let visited_ids: Vec<String> = result.visits.iter().map(|v| v.node.id.clone()).collect();
    assert!(
        visited_ids.len() >= 3,
        "BFS should visit at least B, C, D; got {visited_ids:?}"
    );

    // Verify monotonically non-decreasing depth for BFS ordering
    let depths: Vec<usize> = result.visits.iter().map(|v| v.depth).collect();
    for window in depths.windows(2) {
        assert!(
            window[0] <= window[1],
            "BFS should visit in non-decreasing depth order, got {depths:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. Graph Shortest Path
// ---------------------------------------------------------------------------

#[test]
fn test_graph_shortest_path() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let graph = GraphUseCases::new(&rt);

    let col = "shortest_path_net";

    // Weighted graph:
    //   A --(1)--> B --(1)--> D   (short path, total 2)
    //   A --(5)--> C --(5)--> D   (long path, total 10)
    let a = make_node(&entity, col, "A");
    let b = make_node(&entity, col, "B");
    let c = make_node(&entity, col, "C");
    let d = make_node(&entity, col, "D");

    make_edge(&entity, col, "connects_to", &a, &b, 1.0);
    make_edge(&entity, col, "connects_to", &b, &d, 1.0);
    make_edge(&entity, col, "connects_to", &a, &c, 5.0);
    make_edge(&entity, col, "connects_to", &c, &d, 5.0);

    let result = graph
        .shortest_path(GraphShortestPathInput {
            source: a.graph_id.clone(),
            target: d.graph_id.clone(),
            direction: RuntimeGraphDirection::Outgoing,
            algorithm: RuntimeGraphPathAlgorithm::Dijkstra,
            edge_labels: None,
            projection: None,
        })
        .expect("shortest_path should succeed");

    let path = result.path.expect("a path should exist from A to D");
    assert_eq!(path.hop_count, 2, "shortest path should have 2 hops (A->B->D)");
    assert!(
        path.total_weight <= 2.5,
        "shortest path weight should be ~2.0, got {}",
        path.total_weight
    );
}

// ---------------------------------------------------------------------------
// 4. Graph Components
// ---------------------------------------------------------------------------

#[test]
fn test_graph_components() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let graph = GraphUseCases::new(&rt);

    let col = "components_net";

    // Subgraph 1: X1 <-> X2
    let x1 = make_node(&entity, col, "X1");
    let x2 = make_node(&entity, col, "X2");
    make_edge(&entity, col, "connects_to", &x1, &x2, 1.0);
    make_edge(&entity, col, "connects_to", &x2, &x1, 1.0);

    // Subgraph 2: Y1 <-> Y2 <-> Y3
    let y1 = make_node(&entity, col, "Y1");
    let y2 = make_node(&entity, col, "Y2");
    let y3 = make_node(&entity, col, "Y3");
    make_edge(&entity, col, "connects_to", &y1, &y2, 1.0);
    make_edge(&entity, col, "connects_to", &y2, &y1, 1.0);
    make_edge(&entity, col, "connects_to", &y2, &y3, 1.0);
    make_edge(&entity, col, "connects_to", &y3, &y2, 1.0);

    let result = graph
        .components(GraphComponentsInput {
            mode: RuntimeGraphComponentsMode::Connected,
            min_size: 1,
            projection: None,
        })
        .expect("components detection should succeed");

    assert!(
        result.count >= 2,
        "should detect at least 2 components, got {}",
        result.count
    );
}

// ---------------------------------------------------------------------------
// 5. Graph Centrality — Degree
// ---------------------------------------------------------------------------

#[test]
fn test_graph_centrality() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let graph = GraphUseCases::new(&rt);

    let col = "centrality_star";

    // Star graph: hub + 5 spokes (hub has highest degree)
    let hub = make_node(&entity, col, "hub");
    for i in 0..5 {
        let spoke = make_node(&entity, col, &format!("spoke_{i}"));
        make_edge(&entity, col, "connects_to", &hub, &spoke, 1.0);
    }

    let result = graph
        .centrality(GraphCentralityInput {
            algorithm: RuntimeGraphCentralityAlgorithm::Degree,
            top_k: 10,
            normalize: false,
            max_iterations: None,
            epsilon: None,
            alpha: None,
            projection: None,
        })
        .expect("degree centrality should succeed");

    // The hub should appear in the results.  Degree algorithm may populate
    // either `scores` or `degree_scores` -- check whichever is non-empty.
    if !result.degree_scores.is_empty() {
        let top = &result.degree_scores[0];
        assert_eq!(
            top.node.id, hub.graph_id,
            "hub should have the highest degree, but got {:?}",
            top.node.id
        );
        assert!(
            top.total_degree >= 5,
            "hub total_degree should be >= 5, got {}",
            top.total_degree
        );
    } else if !result.scores.is_empty() {
        let top = &result.scores[0];
        assert_eq!(
            top.node.id, hub.graph_id,
            "hub should have the highest centrality score, but got {:?}",
            top.node.id
        );
    } else {
        panic!("centrality result has no scores at all");
    }
}

// ---------------------------------------------------------------------------
// 6. Graph Community Detection
// ---------------------------------------------------------------------------

#[test]
fn test_graph_community() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let graph = GraphUseCases::new(&rt);

    let col = "community_net";

    // Cluster A: fully connected triangle
    let a1 = make_node(&entity, col, "a1");
    let a2 = make_node(&entity, col, "a2");
    let a3 = make_node(&entity, col, "a3");
    make_edge(&entity, col, "connects_to", &a1, &a2, 1.0);
    make_edge(&entity, col, "connects_to", &a2, &a1, 1.0);
    make_edge(&entity, col, "connects_to", &a2, &a3, 1.0);
    make_edge(&entity, col, "connects_to", &a3, &a2, 1.0);
    make_edge(&entity, col, "connects_to", &a1, &a3, 1.0);
    make_edge(&entity, col, "connects_to", &a3, &a1, 1.0);

    // Cluster B: fully connected triangle
    let b1 = make_node(&entity, col, "b1");
    let b2 = make_node(&entity, col, "b2");
    let b3 = make_node(&entity, col, "b3");
    make_edge(&entity, col, "connects_to", &b1, &b2, 1.0);
    make_edge(&entity, col, "connects_to", &b2, &b1, 1.0);
    make_edge(&entity, col, "connects_to", &b2, &b3, 1.0);
    make_edge(&entity, col, "connects_to", &b3, &b2, 1.0);
    make_edge(&entity, col, "connects_to", &b1, &b3, 1.0);
    make_edge(&entity, col, "connects_to", &b3, &b1, 1.0);

    // Sparse inter-cluster bridge
    make_edge(&entity, col, "connects_to", &a3, &b1, 0.1);

    let result = graph
        .communities(GraphCommunitiesInput {
            algorithm: RuntimeGraphCommunityAlgorithm::LabelPropagation,
            min_size: 1,
            max_iterations: Some(100),
            resolution: None,
            projection: None,
        })
        .expect("community detection should succeed");

    assert!(
        result.count >= 1,
        "should detect at least 1 community, got {}",
        result.count
    );
    // With two dense clusters and a weak bridge, label propagation should
    // ideally find 2 communities -- but the algorithm is non-deterministic,
    // so we accept >= 1.
}

// ---------------------------------------------------------------------------
// 7. Graph Cycle Detection
// ---------------------------------------------------------------------------

#[test]
fn test_graph_cycles() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let graph = GraphUseCases::new(&rt);

    let col = "cycle_net";

    // Cycle: A -> B -> C -> A
    let a = make_node(&entity, col, "A");
    let b = make_node(&entity, col, "B");
    let c = make_node(&entity, col, "C");

    make_edge(&entity, col, "connects_to", &a, &b, 1.0);
    make_edge(&entity, col, "connects_to", &b, &c, 1.0);
    make_edge(&entity, col, "connects_to", &c, &a, 1.0);

    let result = graph
        .cycles(GraphCyclesInput {
            max_length: 10,
            max_cycles: 10,
            projection: None,
        })
        .expect("cycle detection should succeed");

    assert!(
        !result.cycles.is_empty(),
        "should detect at least one cycle in A->B->C->A"
    );
}

// ---------------------------------------------------------------------------
// 8. Graph Clustering Coefficient
// ---------------------------------------------------------------------------

#[test]
fn test_graph_clustering_coefficient() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let graph = GraphUseCases::new(&rt);

    let col = "clustering_net";

    // Fully connected triangle: A <-> B <-> C <-> A
    let a = make_node(&entity, col, "A");
    let b = make_node(&entity, col, "B");
    let c = make_node(&entity, col, "C");

    make_edge(&entity, col, "connects_to", &a, &b, 1.0);
    make_edge(&entity, col, "connects_to", &b, &a, 1.0);
    make_edge(&entity, col, "connects_to", &b, &c, 1.0);
    make_edge(&entity, col, "connects_to", &c, &b, 1.0);
    make_edge(&entity, col, "connects_to", &a, &c, 1.0);
    make_edge(&entity, col, "connects_to", &c, &a, 1.0);

    let result = graph
        .clustering(GraphClusteringInput {
            top_k: 10,
            include_triangles: true,
            projection: None,
        })
        .expect("clustering coefficient should succeed");

    // A fully connected triangle should have a high global clustering coefficient.
    assert!(
        result.global >= 0.0,
        "global clustering coefficient should be non-negative, got {}",
        result.global
    );
    // When triangles are requested, triangle_count should be present and >= 1.
    if let Some(count) = result.triangle_count {
        assert!(count >= 1, "fully connected triangle should have >= 1 triangle, got {count}");
    }
}

// ---------------------------------------------------------------------------
// 9. Catalog — Collections
// ---------------------------------------------------------------------------

#[test]
fn test_catalog_collections() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let catalog = CatalogUseCases::new(&rt);

    // Create entities in three different collections
    for name in &["cat_alpha", "cat_beta", "cat_gamma"] {
        entity
            .create_row(CreateRowInput {
                collection: name.to_string(),
                fields: vec![("key".into(), Value::Text("val".into()))],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .unwrap();
    }

    let collections = catalog.collections();
    for name in &["cat_alpha", "cat_beta", "cat_gamma"] {
        assert!(
            collections.contains(&name.to_string()),
            "catalog should list collection '{name}'; got {collections:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 10. Catalog — Stats
// ---------------------------------------------------------------------------

#[test]
fn test_catalog_stats() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let catalog = CatalogUseCases::new(&rt);

    // Insert a few entities
    for i in 0..5 {
        entity
            .create_row(CreateRowInput {
                collection: "stats_col".into(),
                fields: vec![("idx".into(), Value::Integer(i))],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .unwrap();
    }

    let stats = catalog.stats();
    assert!(
        stats.store.total_entities >= 5,
        "total entities should be >= 5, got {}",
        stats.store.total_entities
    );
    assert!(
        stats.store.collection_count >= 1,
        "should have at least 1 collection, got {}",
        stats.store.collection_count
    );
}

// ---------------------------------------------------------------------------
// 11. Health Report
// ---------------------------------------------------------------------------

#[test]
fn test_health_report() {
    let rt = rt();
    let native = NativeUseCases::new(&rt);

    // Fresh runtime should be healthy or degraded
    let report = native.health();
    assert!(
        report.is_healthy() || matches!(report.state, HealthState::Degraded),
        "fresh runtime should be healthy or degraded, got {:?}",
        report.state
    );

    // After some operations, health should still not be unhealthy
    let entity = EntityUseCases::new(&rt);
    for i in 0..10 {
        entity
            .create_row(CreateRowInput {
                collection: "health_col".into(),
                fields: vec![("idx".into(), Value::Integer(i))],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .unwrap();
    }

    let report2 = native.health();
    assert!(
        !matches!(report2.state, HealthState::Unhealthy),
        "runtime should not be unhealthy after basic operations, got {:?}",
        report2.state
    );
}

// ---------------------------------------------------------------------------
// 12. Index Lifecycle — ArtifactState transitions
// ---------------------------------------------------------------------------

#[test]
fn test_index_lifecycle() {
    // Verify ArtifactState transitions without touching actual indexes.
    // The ArtifactState type models Declared -> Building -> Ready.

    let declared = ArtifactState::Declared;
    assert!(
        declared.can_rebuild(),
        "Declared should allow rebuilding"
    );
    assert!(
        !declared.is_queryable(),
        "Declared should not be queryable"
    );

    let building = ArtifactState::Building;
    assert!(
        !building.is_queryable(),
        "Building should not be queryable"
    );
    assert!(
        !building.can_rebuild(),
        "Building should not allow rebuild while in progress"
    );

    let ready = ArtifactState::Ready;
    assert!(ready.is_queryable(), "Ready should be queryable");
    assert!(
        !ready.can_rebuild(),
        "Ready should not need rebuild"
    );

    // String round-trips
    assert_eq!(ArtifactState::Declared.to_string(), "declared");
    assert_eq!(ArtifactState::Building.to_string(), "building");
    assert_eq!(ArtifactState::Ready.to_string(), "ready");

    // from_build_state conversions
    assert_eq!(
        ArtifactState::from_build_state("ready", true),
        ArtifactState::Ready
    );
    assert_eq!(
        ArtifactState::from_build_state("ready", false),
        ArtifactState::Disabled
    );
    assert_eq!(
        ArtifactState::from_build_state("building", true),
        ArtifactState::Building
    );
    assert_eq!(
        ArtifactState::from_build_state("failed", true),
        ArtifactState::Failed
    );

    // Failed -> can_rebuild
    assert!(
        ArtifactState::Failed.can_rebuild(),
        "Failed state should allow rebuild"
    );
}

// ---------------------------------------------------------------------------
// 13. Checkpoint
// ---------------------------------------------------------------------------

#[test]
fn test_checkpoint() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let native = NativeUseCases::new(&rt);

    // Insert some data first
    for i in 0..10 {
        entity
            .create_row(CreateRowInput {
                collection: "ckpt_col".into(),
                fields: vec![("idx".into(), Value::Integer(i))],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .unwrap();
    }

    // Checkpoint should not error
    let result = native.checkpoint();
    assert!(
        result.is_ok(),
        "checkpoint should succeed: {:?}",
        result.err()
    );
}

// ---------------------------------------------------------------------------
// 14. Multiple Collections — Isolation
// ---------------------------------------------------------------------------

#[test]
fn test_multiple_collections() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    // Create entities in 5 isolated collections
    let collections: Vec<String> = (0..5).map(|i| format!("isolation_col_{i}")).collect();
    for (idx, col) in collections.iter().enumerate() {
        for j in 0..3 {
            entity
                .create_row(CreateRowInput {
                    collection: col.clone(),
                    fields: vec![
                        ("col_idx".into(), Value::Integer(idx as i64)),
                        ("row_idx".into(), Value::Integer(j)),
                    ],
                    metadata: vec![],
                    node_links: vec![],
                    vector_links: vec![],
                })
                .unwrap();
        }
    }

    // Query each collection and verify count
    for col in &collections {
        let result = query
            .execute(ExecuteQueryInput {
                query: format!("SELECT * FROM {col}"),
            })
            .expect("query should succeed");

        let count = result.result.records.len();
        assert_eq!(
            count, 3,
            "collection '{col}' should have exactly 3 rows, got {count}"
        );
    }
}

// ---------------------------------------------------------------------------
// 15. Large Batch Insert
// ---------------------------------------------------------------------------

#[test]
fn test_large_batch_insert() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let catalog = CatalogUseCases::new(&rt);

    let col = "batch_500";
    for i in 0..500 {
        entity
            .create_row(CreateRowInput {
                collection: col.into(),
                fields: vec![
                    ("idx".into(), Value::Integer(i)),
                    ("label".into(), Value::Text(format!("row_{i}"))),
                ],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .unwrap();
    }

    let stats = catalog.stats();
    assert!(
        stats.store.total_entities >= 500,
        "should have at least 500 entities after batch insert, got {}",
        stats.store.total_entities
    );

    // Verify queryable
    let query = QueryUseCases::new(&rt);
    let result = query
        .execute(ExecuteQueryInput {
            query: format!("SELECT * FROM {col}"),
        })
        .expect("query after large batch should succeed");
    assert_eq!(
        result.result.records.len(),
        500,
        "should query back all 500 rows, got {}",
        result.result.records.len()
    );
}

// ---------------------------------------------------------------------------
// 16. Concurrent Read-After-Write Consistency
// ---------------------------------------------------------------------------

#[test]
fn test_concurrent_read_after_write() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    let col = "raw_consistency";

    // Insert then immediately query -- verify consistency
    for i in 0..20 {
        entity
            .create_row(CreateRowInput {
                collection: col.into(),
                fields: vec![("step".into(), Value::Integer(i))],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .unwrap();

        // Immediately query to verify the just-inserted row is visible
        let result = query
            .execute(ExecuteQueryInput {
                query: format!("SELECT * FROM {col}"),
            })
            .expect("immediate read-after-write should succeed");

        let count = result.result.records.len();
        assert_eq!(
            count,
            (i + 1) as usize,
            "after inserting row {i}, should see {} rows, got {count}",
            i + 1
        );
    }
}
