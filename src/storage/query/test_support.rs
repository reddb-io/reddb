use std::sync::Arc;

use crate::storage::engine::{GraphEdgeType, GraphNodeType, GraphStore};

pub(crate) fn add_node_or_panic(
    graph: &GraphStore,
    id: &str,
    label: &str,
    node_type: GraphNodeType,
) {
    graph
        .add_node(id, label, node_type)
        .unwrap_or_else(|err| panic!("failed to add test graph node {id}: {err}"));
}

pub(crate) fn add_edge_or_panic(
    graph: &GraphStore,
    from: &str,
    to: &str,
    edge_type: GraphEdgeType,
    weight: f32,
) {
    graph
        .add_edge(from, to, edge_type, weight)
        .unwrap_or_else(|err| panic!("failed to add test graph edge {from}->{to}: {err}"));
}

pub(crate) fn service_graph() -> Arc<GraphStore> {
    let graph = GraphStore::new();

    add_node_or_panic(&graph, "host:10.0.0.1", "webserver", GraphNodeType::Host);
    add_node_or_panic(&graph, "host:10.0.0.2", "database", GraphNodeType::Host);
    add_node_or_panic(&graph, "svc:ssh", "SSH", GraphNodeType::Service);
    add_node_or_panic(&graph, "svc:http", "HTTP", GraphNodeType::Service);

    add_edge_or_panic(
        &graph,
        "host:10.0.0.1",
        "svc:ssh",
        GraphEdgeType::HasService,
        1.0,
    );
    add_edge_or_panic(
        &graph,
        "host:10.0.0.1",
        "svc:http",
        GraphEdgeType::HasService,
        1.0,
    );
    add_edge_or_panic(
        &graph,
        "host:10.0.0.1",
        "host:10.0.0.2",
        GraphEdgeType::ConnectsTo,
        1.0,
    );
    add_edge_or_panic(
        &graph,
        "host:10.0.0.2",
        "svc:ssh",
        GraphEdgeType::HasService,
        1.0,
    );

    Arc::new(graph)
}

pub(crate) fn service_graph_with_user() -> Arc<GraphStore> {
    let graph = service_graph();

    add_node_or_panic(&graph, "user:admin", "admin", GraphNodeType::User);
    add_edge_or_panic(
        &graph,
        "host:10.0.0.1",
        "user:admin",
        GraphEdgeType::HasUser,
        1.0,
    );

    graph
}

pub(crate) fn unified_query_graph() -> Arc<GraphStore> {
    let graph = GraphStore::new();

    add_node_or_panic(
        &graph,
        "host:192.168.1.1",
        "192.168.1.1",
        GraphNodeType::Host,
    );
    add_node_or_panic(
        &graph,
        "host:192.168.1.2",
        "192.168.1.2",
        GraphNodeType::Host,
    );
    add_node_or_panic(&graph, "svc:ssh:22", "SSH", GraphNodeType::Service);
    add_node_or_panic(&graph, "svc:http:80", "HTTP", GraphNodeType::Service);

    add_edge_or_panic(
        &graph,
        "host:192.168.1.1",
        "svc:ssh:22",
        GraphEdgeType::HasService,
        1.0,
    );
    add_edge_or_panic(
        &graph,
        "host:192.168.1.1",
        "svc:http:80",
        GraphEdgeType::HasService,
        1.0,
    );
    add_edge_or_panic(
        &graph,
        "host:192.168.1.1",
        "host:192.168.1.2",
        GraphEdgeType::ConnectsTo,
        1.0,
    );

    Arc::new(graph)
}
