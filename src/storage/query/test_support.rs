use std::sync::Arc;

use crate::storage::engine::GraphStore;

pub(crate) fn add_node_or_panic(graph: &GraphStore, id: &str, label: &str, category: &str) {
    graph
        .add_node_with_label(id, label, category)
        .unwrap_or_else(|err| panic!("failed to add test graph node {id}: {err}"));
}

pub(crate) fn add_edge_or_panic(
    graph: &GraphStore,
    from: &str,
    to: &str,
    edge_label: &str,
    weight: f32,
) {
    graph
        .add_edge_with_label(from, to, edge_label, weight)
        .unwrap_or_else(|err| panic!("failed to add test graph edge {from}->{to}: {err}"));
}

pub(crate) fn service_graph() -> Arc<GraphStore> {
    let graph = GraphStore::new();

    add_node_or_panic(&graph, "host:10.0.0.1", "webserver", "host");
    add_node_or_panic(&graph, "host:10.0.0.2", "database", "host");
    add_node_or_panic(&graph, "svc:ssh", "SSH", "service");
    add_node_or_panic(&graph, "svc:http", "HTTP", "service");

    add_edge_or_panic(&graph, "host:10.0.0.1", "svc:ssh", "has_service", 1.0);
    add_edge_or_panic(&graph, "host:10.0.0.1", "svc:http", "has_service", 1.0);
    add_edge_or_panic(
        &graph,
        "host:10.0.0.1",
        "host:10.0.0.2",
        "connects_to",
        1.0,
    );
    add_edge_or_panic(&graph, "host:10.0.0.2", "svc:ssh", "has_service", 1.0);

    Arc::new(graph)
}

pub(crate) fn service_graph_with_user() -> Arc<GraphStore> {
    let graph = service_graph();

    add_node_or_panic(&graph, "user:admin", "admin", "user");
    add_edge_or_panic(&graph, "host:10.0.0.1", "user:admin", "has_user", 1.0);

    graph
}

pub(crate) fn unified_query_graph() -> Arc<GraphStore> {
    let graph = GraphStore::new();

    add_node_or_panic(&graph, "host:192.168.1.1", "192.168.1.1", "host");
    add_node_or_panic(&graph, "host:192.168.1.2", "192.168.1.2", "host");
    add_node_or_panic(&graph, "svc:ssh:22", "SSH", "service");
    add_node_or_panic(&graph, "svc:http:80", "HTTP", "service");

    add_edge_or_panic(
        &graph,
        "host:192.168.1.1",
        "svc:ssh:22",
        "has_service",
        1.0,
    );
    add_edge_or_panic(
        &graph,
        "host:192.168.1.1",
        "svc:http:80",
        "has_service",
        1.0,
    );
    add_edge_or_panic(
        &graph,
        "host:192.168.1.1",
        "host:192.168.1.2",
        "connects_to",
        1.0,
    );

    Arc::new(graph)
}
