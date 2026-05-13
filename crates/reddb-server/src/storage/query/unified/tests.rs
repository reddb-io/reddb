use super::*;
use crate::storage::engine::{GraphStore, GraphTableIndex, StoredNode};
use crate::storage::query::ast::*;
use crate::storage::query::test_support::{add_node_or_panic, unified_query_graph};
use crate::storage::schema::Value;
use std::collections::HashMap;
use std::sync::Arc;

fn create_test_graph() -> Arc<GraphStore> {
    unified_query_graph()
}

fn create_test_index() -> Arc<GraphTableIndex> {
    Arc::new(GraphTableIndex::new())
}

#[test]
fn test_simple_graph_query() {
    let graph = create_test_graph();
    let index = create_test_index();
    let executor = UnifiedExecutor::new(graph, index);

    let query = QueryExpr::graph()
        .node(super::super::ast::NodePattern::new("h").of_label("host"))
        .return_field(FieldRef::node_id("h"))
        .build();

    let result = executor.execute(&query).unwrap();
    assert_eq!(result.records.len(), 2); // Two hosts
}

#[test]
fn test_graph_query_with_edge() {
    let graph = create_test_graph();
    let index = create_test_index();
    let executor = UnifiedExecutor::new(graph, index);

    let query = QueryExpr::graph()
        .node(super::super::ast::NodePattern::new("h").of_label("host"))
        .node(super::super::ast::NodePattern::new("s").of_label("service"))
        .edge(super::super::ast::EdgePattern::new("h", "s").of_label("has_service"))
        .return_field(FieldRef::node_id("h"))
        .return_field(FieldRef::node_id("s"))
        .build();

    let result = executor.execute(&query).unwrap();
    assert_eq!(result.records.len(), 2); // Two service connections from host 192.168.1.1
}

#[test]
fn test_path_query() {
    let graph = create_test_graph();
    let index = create_test_index();
    let executor = UnifiedExecutor::new(graph, index);

    let query = QueryExpr::path(
        NodeSelector::by_id("host:192.168.1.1"),
        NodeSelector::by_id("host:192.168.1.2"),
    )
    .via_label("connects_to")
    .max_length(5)
    .build();

    let result = executor.execute(&query).unwrap();
    assert_eq!(result.records.len(), 1); // One path
    assert_eq!(result.records[0].paths[0].nodes.len(), 2); // Two nodes in path
}

#[test]
fn test_unified_result() {
    let mut result = UnifiedResult::with_columns(vec!["ip".to_string(), "port".to_string()]);

    let mut record = UnifiedRecord::new();
    record.set("ip", Value::text("192.168.1.1".to_string()));
    record.set("port", Value::Integer(22));

    result.push(record);

    assert_eq!(result.len(), 1);
    assert_eq!(
        result.records[0].get("ip"),
        Some(&Value::text("192.168.1.1".to_string()))
    );
}

#[test]
fn test_matched_node() {
    let node = StoredNode {
        id: "host:1".to_string(),
        label: "192.168.1.1".to_string(),
        node_type: "host".to_string(),
        label_id: crate::storage::engine::graph_store::LabelId::new(1),
        flags: 0,
        out_edge_count: 0,
        in_edge_count: 0,
        page_id: 0,
        slot: 0,
        table_ref: None,
        vector_ref: None,
    };

    let matched = MatchedNode::from_stored(&node);
    assert_eq!(matched.id, "host:1");
    assert_eq!(matched.label, "192.168.1.1");
    assert_eq!(matched.node_label, "host");
}

#[test]
fn test_graph_query_filter_custom_node_property() {
    let graph = GraphStore::new();
    add_node_or_panic(&graph, "host:1", "host-1", "host");
    add_node_or_panic(&graph, "host:2", "host-2", "host");

    let mut node_properties = HashMap::new();
    node_properties.insert(
        "host:1".to_string(),
        HashMap::from([("os".to_string(), Value::text("linux".to_string()))]),
    );

    let graph = Arc::new(graph);
    let index = create_test_index();
    let executor = UnifiedExecutor::new_with_node_properties(graph, index, node_properties);

    let query = QueryExpr::graph()
        .node(
            super::super::ast::NodePattern::new("h")
                .of_label("host")
                .with_property("os", CompareOp::Eq, Value::text("linux".to_string())),
        )
        .return_field(FieldRef::node_prop("h", "os"))
        .build();

    let result = executor.execute(&query).unwrap();
    assert_eq!(result.records.len(), 1);
    assert_eq!(
        result.records[0].get("h.os"),
        Some(&Value::text("linux".to_string()))
    );
}

#[test]
fn test_graph_path() {
    let path = GraphPath::start("node:1");
    assert!(path.is_empty());
    assert_eq!(path.nodes.len(), 1);

    let edge = MatchedEdge {
        from: "node:1".to_string(),
        to: "node:2".to_string(),
        edge_label: "connects_to".to_string(),
        weight: 1.5,
    };

    let extended = path.extend(edge, "node:2");
    assert_eq!(extended.len(), 1);
    assert_eq!(extended.nodes.len(), 2);
    assert!((extended.total_weight - 1.5).abs() < f32::EPSILON);
}

#[test]
fn test_matched_edge_from_tuple() {
    let edge = MatchedEdge::from_tuple("node:1", "has_service", "node:2", 0.5);
    assert_eq!(edge.from, "node:1");
    assert_eq!(edge.to, "node:2");
    assert_eq!(edge.edge_label, "has_service");
    assert!((edge.weight - 0.5).abs() < f32::EPSILON);
}

#[test]
fn test_unified_record_operations() {
    let mut record = UnifiedRecord::new();

    // Test set and get
    record.set("name", Value::text("test".to_string()));
    assert_eq!(record.get("name"), Some(&Value::text("test".to_string())));
    assert_eq!(record.get("missing"), None);

    // Test set_node and get_node
    let node = MatchedNode {
        id: "n1".to_string(),
        label: "Node 1".to_string(),
        node_label: "host".to_string(),
        properties: HashMap::new(),
    };
    record.set_node("h", node.clone());
    assert_eq!(record.get_node("h").unwrap().id, "n1");
    assert!(record.get_node("missing").is_none());

    // Test set_edge
    let edge = MatchedEdge::from_tuple("a", "connects_to", "b", 1.0);
    record.set_edge("e", edge);
    assert!(record.edges.contains_key("e"));
}

#[test]
fn test_unified_result_empty() {
    let result = UnifiedResult::empty();
    assert!(result.is_empty());
    assert_eq!(result.len(), 0);
    assert!(result.columns.is_empty());
}

#[test]
fn test_path_query_no_path() {
    let graph = GraphStore::new();
    // Create disconnected nodes
    let _ = graph.add_node_with_label("a", "Node A", "host");
    let _ = graph.add_node_with_label("b", "Node B", "host");
    // No edge between them

    let graph = Arc::new(graph);
    let index = create_test_index();
    let executor = UnifiedExecutor::new(graph, index);

    let query = QueryExpr::path(NodeSelector::by_id("a"), NodeSelector::by_id("b"))
        .max_length(5)
        .build();

    let result = executor.execute(&query).unwrap();
    assert!(result.is_empty()); // No path exists
}

#[test]
fn test_table_query_empty() {
    let graph = create_test_graph();
    let index = create_test_index();
    let executor = UnifiedExecutor::new(graph, index);

    // Table queries return empty without table storage
    let query = QueryExpr::table("hosts").build();
    let result = executor.execute(&query).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_query_stats_tracking() {
    let graph = create_test_graph();
    let index = create_test_index();
    let executor = UnifiedExecutor::new(graph, index);

    let query = QueryExpr::graph()
        .node(super::super::ast::NodePattern::new("h").of_label("host"))
        .return_field(FieldRef::node_id("h"))
        .build();

    let result = executor.execute(&query).unwrap();
    // Stats should track scanned nodes
    assert!(result.stats.nodes_scanned > 0);
}

#[test]
fn test_path_max_length_limit() {
    let graph = GraphStore::new();
    // Create a chain: a -> b -> c -> d
    let _ = graph.add_node_with_label("a", "A", "host");
    let _ = graph.add_node_with_label("b", "B", "host");
    let _ = graph.add_node_with_label("c", "C", "host");
    let _ = graph.add_node_with_label("d", "D", "host");
    let _ = graph.add_edge_with_label("a", "b", "connects_to", 1.0);
    let _ = graph.add_edge_with_label("b", "c", "connects_to", 1.0);
    let _ = graph.add_edge_with_label("c", "d", "connects_to", 1.0);

    let graph = Arc::new(graph);
    let index = create_test_index();
    let executor = UnifiedExecutor::new(graph, index);

    // max_length=2 should not reach d (3 hops away)
    let query = QueryExpr::path(NodeSelector::by_id("a"), NodeSelector::by_id("d"))
        .max_length(2)
        .build();

    let result = executor.execute(&query).unwrap();
    assert!(result.is_empty());

    // max_length=3 should reach d
    let query = QueryExpr::path(NodeSelector::by_id("a"), NodeSelector::by_id("d"))
        .max_length(3)
        .build();

    let result = executor.execute(&query).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result.records[0].paths[0].len(), 3); // 3 edges
}

#[test]
fn test_graph_query_empty_pattern() {
    let graph = create_test_graph();
    let index = create_test_index();
    let executor = UnifiedExecutor::new(graph, index);

    // Empty pattern should return empty result
    let query = QueryExpr::Graph(GraphQuery {
        alias: None,
        pattern: GraphPattern {
            nodes: vec![],
            edges: vec![],
        },
        filter: None,
        return_: vec![],
        limit: None,
    });

    let result = executor.execute(&query).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_execution_error_display() {
    let err = ExecutionError::new("Test error");
    assert_eq!(format!("{}", err), "Execution error: Test error");
}

#[test]
fn test_graph_path_multi_hop() {
    let path = GraphPath::start("a");

    let edge1 = MatchedEdge::from_tuple("a", "connects_to", "b", 1.0);
    let path = path.extend(edge1, "b");

    let edge2 = MatchedEdge::from_tuple("b", "connects_to", "c", 2.0);
    let path = path.extend(edge2, "c");

    assert_eq!(path.len(), 2);
    assert_eq!(path.nodes.len(), 3);
    assert_eq!(path.nodes, vec!["a", "b", "c"]);
    assert!((path.total_weight - 3.0).abs() < f32::EPSILON);
}

#[test]
fn test_node_selector_by_type() {
    let graph = GraphStore::new();
    let _ = graph.add_node_with_label("host:1", "Host 1", "host");
    let _ = graph.add_node_with_label("host:2", "Host 2", "host");
    let _ = graph.add_node_with_label("svc:1", "Service 1", "service");

    let graph = Arc::new(graph);
    let index = create_test_index();
    let executor = UnifiedExecutor::new(graph, index);

    // Path from any host to any service
    let query = QueryExpr::path(
        NodeSelector::by_label("host"),
        NodeSelector::by_label("service"),
    )
    .max_length(1)
    .build();

    // No edges, so no paths
    let result = executor.execute(&query).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_join_query_execution() {
    let graph = create_test_graph();
    let index = create_test_index();
    let executor = UnifiedExecutor::new(graph, index);

    // Join two graph queries
    let left = QueryExpr::graph()
        .node(super::super::ast::NodePattern::new("h").of_label("host"))
        .return_field(FieldRef::node_id("h"))
        .build();

    let right = QueryExpr::graph()
        .node(super::super::ast::NodePattern::new("s").of_label("service"))
        .return_field(FieldRef::node_id("s"))
        .build();

    // Create join - both return results but no matching condition
    let join = QueryExpr::Join(JoinQuery {
        left: Box::new(left),
        right: Box::new(right),
        join_type: JoinType::Inner,
        on: super::super::ast::JoinCondition {
            left_field: FieldRef::node_prop("h", "id"),
            right_field: FieldRef::node_prop("s", "id"),
        },
        filter: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        return_items: Vec::new(),
        return_: Vec::new(),
    });

    let result = executor.execute(&join).unwrap();
    // No matches because host ids != service ids
    assert!(result.is_empty());
}
