//! Parser tests

use super::*;
use crate::storage::engine::graph_store::{GraphEdgeType, GraphNodeType};
use crate::storage::engine::vector_metadata::MetadataValue;
use crate::storage::query::ast::{
    DistanceMetric, EdgeDirection, FieldRef, Filter, FusionStrategy, JoinType, MetadataFilter,
    Projection, QueueCommand, TableQuery, VectorSource,
};

#[test]
fn test_parse_simple_select() {
    let query = parse("SELECT ip, hostname FROM hosts").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "hosts");
        assert_eq!(tq.columns.len(), 2);
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_select_star() {
    let query = parse("SELECT * FROM hosts").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "hosts");
        assert!(tq.columns.is_empty()); // * means all
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_select_star_from_asterisk_defaults_to_any() {
    let query = parse("SELECT * FROM *").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "*");
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_select_without_from_defaults_to_any() {
    let query = parse("SELECT *").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "any");
        assert_eq!(tq.columns.len(), 0);
        assert!(tq.alias.is_none());
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_select_without_from_with_trailing_identifier_errors() {
    let err = parse("SELECT * docs").unwrap_err();
    assert!(matches!(err.to_string(), s if s.contains("Unexpected token after query")));
}

#[test]
fn test_parse_select_with_alias() {
    let query = parse("SELECT h.ip FROM hosts h").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "hosts");
        assert_eq!(tq.alias, Some("h".to_string()));
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_select_any_with_alias() {
    let query = parse("SELECT * FROM ANY u").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "ANY");
        assert_eq!(tq.alias, Some("u".to_string()));
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_select_entity_with_alias() {
    let query = parse("SELECT * FROM entity e").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "entity");
        assert_eq!(tq.alias, Some("e".to_string()));
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_select_with_where() {
    let query = parse("SELECT ip FROM hosts WHERE os = 'Linux'").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert!(tq.filter.is_some());
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_select_any_with_where() {
    let query = parse("SELECT * FROM ANY WHERE _entity_type = 'graph_node'").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "ANY");
        assert!(tq.filter.is_some());
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_select_any_with_alias_where_order_limit() {
    let query = parse("SELECT u._entity_type FROM ANY u WHERE u._entity_type = 'vector' ORDER BY u._score DESC LIMIT 10").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "ANY");
        assert_eq!(tq.alias.as_deref(), Some("u"));
        assert!(tq.filter.is_some());
        assert_eq!(tq.order_by.len(), 1);
        assert_eq!(tq.order_by[0].ascending, false);
        assert_eq!(tq.limit, Some(10));
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_select_any_document_path_projection() {
    let query = parse("SELECT payload.name FROM ANY").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "ANY");
        match &tq.columns[0] {
            Projection::Field(FieldRef::TableColumn { table, column }, _) => {
                assert_eq!(table, "payload");
                assert_eq!(column, "name");
            }
            other => panic!("Expected field projection, got {other:?}"),
        }
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_select_any_nested_document_path_projection() {
    let query = parse("SELECT payload.owner.name FROM ANY").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "ANY");
        match &tq.columns[0] {
            Projection::Field(FieldRef::TableColumn { table, column }, _) => {
                assert_eq!(table, "payload");
                assert_eq!(column, "owner.name");
            }
            other => panic!("Expected field projection, got {other:?}"),
        }
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_select_alt_any_with_where() {
    let query = parse("SELECT * FROM _any WHERE _entity_type = 'vector'").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "_any");
        assert!(tq.alias.is_none());
        assert!(tq.filter.is_some());
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_select_all_with_where() {
    let query = parse("SELECT * FROM all WHERE _entity_type = 'vector'").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "all");
        assert!(tq.alias.is_none());
        assert!(tq.filter.is_some());
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_select_ALL_keyword_with_where() {
    let query = parse("SELECT * FROM ALL WHERE _entity_type = 'vector'").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "all");
        assert!(tq.alias.is_none());
        assert!(tq.filter.is_some());
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_select_any_with_capabilities_in() {
    let query =
        parse("SELECT * FROM ANY WHERE _capabilities IN ('vector', 'graph_node', 'document')")
            .unwrap();
    if let QueryExpr::Table(tq) = query {
        match tq.filter {
            Some(Filter::In { .. }) => {}
            _ => panic!("Expected IN filter"),
        }
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_select_universal_with_where() {
    let query = parse("SELECT * FROM UNIVERSAL WHERE _entity_type = 'vector'").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "UNIVERSAL");
        assert!(tq.filter.is_some());
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_select_mixed_with_where() {
    let query = parse("SELECT * FROM mixed WHERE _entity_type = 'document'").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "mixed");
        assert!(tq.filter.is_some());
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_select_entity_with_where() {
    let query = parse("SELECT * FROM ENTITY WHERE _entity_type = 'document'").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "ENTITY");
        assert!(tq.alias.is_none());
        assert!(tq.filter.is_some());
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_select_entity_lowercase_without_from_alias() {
    let query = parse("SELECT * FROM entity e").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "entity");
        assert_eq!(tq.alias.as_deref(), Some("e"));
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_select_table_alias_nested_document_path_projection() {
    let query = parse("SELECT d.payload.owner.name FROM docs AS d").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "docs");
        assert_eq!(tq.alias.as_deref(), Some("d"));
        match &tq.columns[0] {
            Projection::Field(FieldRef::TableColumn { table, column }, _) => {
                assert_eq!(table, "d");
                assert_eq!(column, "payload.owner.name");
            }
            other => panic!("Expected field projection, got {other:?}"),
        }
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_select_with_order_limit() {
    let query = parse("SELECT ip FROM hosts ORDER BY created_at DESC LIMIT 10").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.order_by.len(), 1);
        assert!(!tq.order_by[0].ascending);
        assert_eq!(tq.limit, Some(10));
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_where_and_or() {
    let query = parse("SELECT * FROM hosts WHERE os = 'Linux' AND status = 'active'").unwrap();
    if let QueryExpr::Table(tq) = query {
        match tq.filter {
            Some(Filter::And(_, _)) => {}
            _ => panic!("Expected AND filter"),
        }
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_where_between() {
    let query = parse("SELECT * FROM hosts WHERE port BETWEEN 80 AND 443").unwrap();
    if let QueryExpr::Table(tq) = query {
        match tq.filter {
            Some(Filter::Between { .. }) => {}
            _ => panic!("Expected BETWEEN filter"),
        }
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_where_in() {
    let query = parse("SELECT * FROM hosts WHERE os IN ('Linux', 'Windows')").unwrap();
    if let QueryExpr::Table(tq) = query {
        match tq.filter {
            Some(Filter::In { values, .. }) => {
                assert_eq!(values.len(), 2);
            }
            _ => panic!("Expected IN filter"),
        }
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_where_like() {
    let query = parse("SELECT * FROM hosts WHERE hostname LIKE '%server%'").unwrap();
    if let QueryExpr::Table(tq) = query {
        match tq.filter {
            Some(Filter::Like { .. }) => {}
            _ => panic!("Expected LIKE filter"),
        }
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_simple_match() {
    let query = parse("MATCH (h:Host) RETURN h").unwrap();
    if let QueryExpr::Graph(gq) = query {
        assert_eq!(gq.pattern.nodes.len(), 1);
        assert_eq!(gq.pattern.nodes[0].alias, "h");
        assert_eq!(gq.pattern.nodes[0].node_type, Some(GraphNodeType::Host));
    } else {
        panic!("Expected GraphQuery");
    }
}

#[test]
fn test_parse_match_with_edge() {
    let query = parse("MATCH (h:Host)-[:HAS_SERVICE]->(s:Service) RETURN h, s").unwrap();
    if let QueryExpr::Graph(gq) = query {
        assert_eq!(gq.pattern.nodes.len(), 2);
        assert_eq!(gq.pattern.edges.len(), 1);
        assert_eq!(
            gq.pattern.edges[0].edge_type,
            Some(GraphEdgeType::HasService)
        );
        assert_eq!(gq.pattern.edges[0].direction, EdgeDirection::Outgoing);
    } else {
        panic!("Expected GraphQuery");
    }
}

#[test]
fn test_parse_match_variable_length() {
    let query = parse("MATCH (a)-[*1..5]->(b) RETURN a, b").unwrap();
    if let QueryExpr::Graph(gq) = query {
        assert_eq!(gq.pattern.edges[0].min_hops, 1);
        assert_eq!(gq.pattern.edges[0].max_hops, 5);
    } else {
        panic!("Expected GraphQuery");
    }
}

#[test]
fn test_parse_match_incoming_edge() {
    let query = parse("MATCH (a)<-[:CONTAINS]-(b) RETURN a, b").unwrap();
    if let QueryExpr::Graph(gq) = query {
        assert_eq!(gq.pattern.edges[0].direction, EdgeDirection::Incoming);
    } else {
        panic!("Expected GraphQuery");
    }
}

#[test]
fn test_parse_path_query() {
    let query =
        parse("PATH FROM host('192.168.1.1') TO host('10.0.0.1') VIA [:AUTH_ACCESS]").unwrap();
    if let QueryExpr::Path(pq) = query {
        assert_eq!(pq.via.len(), 1);
        assert_eq!(pq.via[0], GraphEdgeType::AuthAccess);
    } else {
        panic!("Expected PathQuery");
    }
}

#[test]
fn test_parse_join_query() {
    let query =
        parse("FROM hosts h JOIN GRAPH (n:Host)-[:AFFECTED_BY]->(v) AS g ON h.ip = n.id").unwrap();
    if let QueryExpr::Join(jq) = query {
        assert!(matches!(*jq.left, QueryExpr::Table(_)));
        match jq.right.as_ref() {
            QueryExpr::Graph(gq) => assert_eq!(gq.alias.as_deref(), Some("g")),
            _ => panic!("Expected GraphQuery"),
        }
    } else {
        panic!("Expected JoinQuery");
    }
}

#[test]
fn test_parse_join_with_universal_left_alias() {
    let query =
        parse("FROM ANY a JOIN GRAPH (n:Host)-[:AFFECTED_BY]->(v) ON a._entity_id = n.id").unwrap();
    if let QueryExpr::Join(jq) = query {
        match &*jq.left {
            QueryExpr::Table(tq) => assert_eq!(tq.alias.as_deref(), Some("a")),
            _ => panic!("Expected left table"),
        }
        assert!(matches!(*jq.right, QueryExpr::Graph(_)));
    } else {
        panic!("Expected JoinQuery");
    }
}

#[test]
fn test_parse_join_with_universal_right_alias() {
    let query =
        parse("FROM hosts h JOIN ANY a ON h.id = a._entity_id RETURN h.id, a._score").unwrap();
    if let QueryExpr::Join(jq) = query {
        match &*jq.right {
            QueryExpr::Table(tq) => {
                assert_eq!(tq.table, "ANY");
                assert_eq!(tq.alias.as_deref(), Some("a"));
            }
            _ => panic!("Expected right table"),
        }
    } else {
        panic!("Expected JoinQuery");
    }
}

#[test]
fn test_parse_join_with_ALL_keyword() {
    let query = parse(
        "FROM docs d JOIN ALL a ON d.id = a._entity_id WHERE a._entity_type = 'vector' LIMIT 2",
    )
    .unwrap();
    if let QueryExpr::Join(jq) = query {
        match &*jq.right {
            QueryExpr::Table(tq) => {
                assert_eq!(tq.table, "all");
                assert_eq!(tq.alias.as_deref(), Some("a"));
            }
            _ => panic!("Expected right table"),
        }
        assert!(jq.filter.is_some());
    } else {
        panic!("Expected JoinQuery");
    }
}

#[test]
fn test_parse_left_join() {
    let query = parse("FROM hosts h LEFT JOIN GRAPH (n:Host) ON h.ip = n.id").unwrap();
    if let QueryExpr::Join(jq) = query {
        assert_eq!(jq.join_type, JoinType::LeftOuter);
    } else {
        panic!("Expected JoinQuery");
    }
}

#[test]
fn test_parse_join_vector_query() {
    let query = parse(
        "FROM docs d JOIN VECTOR SEARCH embeddings SIMILAR TO [0.1, 0.2] LIMIT 5 AS sim ON d.id = sim.entity_id RETURN d.id, sim.score",
    )
    .unwrap();
    if let QueryExpr::Join(jq) = query {
        assert!(matches!(*jq.left, QueryExpr::Table(_)));
        match jq.right.as_ref() {
            QueryExpr::Vector(vq) => assert_eq!(vq.alias.as_deref(), Some("sim")),
            _ => panic!("Expected VectorQuery"),
        }
        assert_eq!(jq.limit, None);
        assert_eq!(jq.return_.len(), 2);
    } else {
        panic!("Expected JoinQuery");
    }
}

#[test]
fn test_parse_join_vector_query_with_implicit_alias() {
    let query = parse(
        "FROM docs d JOIN VECTOR SEARCH embeddings SIMILAR TO [0.1, 0.2] LIMIT 5 sim ON d.id = sim.entity_id RETURN d.id, sim.score",
    )
    .unwrap();
    if let QueryExpr::Join(jq) = query {
        match jq.right.as_ref() {
            QueryExpr::Vector(vq) => assert_eq!(vq.alias.as_deref(), Some("sim")),
            _ => panic!("Expected VectorQuery"),
        }
    } else {
        panic!("Expected JoinQuery");
    }
}

#[test]
fn test_parse_join_path_query() {
    let query = parse(
        "FROM hosts h JOIN PATH FROM host('host:a') TO host('host:b') LIMIT 4 AS p ON h.id = p.entity_id WHERE h.status = 'active' RETURN h.id, p.entity_id",
    )
    .unwrap();
    if let QueryExpr::Join(jq) = query {
        assert!(matches!(*jq.left, QueryExpr::Table(_)));
        match jq.right.as_ref() {
            QueryExpr::Path(pq) => assert_eq!(pq.alias.as_deref(), Some("p")),
            _ => panic!("Expected PathQuery"),
        }
        assert!(jq.filter.is_some());
        assert_eq!(jq.return_.len(), 2);
    } else {
        panic!("Expected JoinQuery");
    }
}

#[test]
fn test_parse_join_path_query_with_implicit_alias() {
    let query = parse(
        "FROM hosts h JOIN PATH FROM host('host:a') TO host('host:b') LIMIT 4 p ON h.id = p.entity_id RETURN h.id, p.entity_id",
    )
    .unwrap();
    if let QueryExpr::Join(jq) = query {
        match jq.right.as_ref() {
            QueryExpr::Path(pq) => assert_eq!(pq.alias.as_deref(), Some("p")),
            _ => panic!("Expected PathQuery"),
        }
    } else {
        panic!("Expected JoinQuery");
    }
}

#[test]
fn test_parse_join_hybrid_query() {
    let query = parse(
        "FROM docs d JOIN HYBRID SELECT * FROM hosts VECTOR SEARCH embeddings SIMILAR TO [0.1, 0.2] FUSION RERANK AS hy ON d.id = hy.entity_id RETURN d.id, hy.score",
    )
    .unwrap();
    if let QueryExpr::Join(jq) = query {
        assert!(matches!(*jq.left, QueryExpr::Table(_)));
        match jq.right.as_ref() {
            QueryExpr::Hybrid(hq) => assert_eq!(hq.alias.as_deref(), Some("hy")),
            _ => panic!("Expected HybridQuery"),
        }
        assert_eq!(jq.return_.len(), 2);
    } else {
        panic!("Expected JoinQuery");
    }
}

#[test]
fn test_parse_join_hybrid_query_with_implicit_alias() {
    let query = parse(
        "FROM docs d JOIN HYBRID SELECT * FROM hosts VECTOR SEARCH embeddings SIMILAR TO [0.1, 0.2] FUSION RERANK h ON d.id = h.entity_id RETURN d.id, h.score",
    )
    .unwrap();
    if let QueryExpr::Join(jq) = query {
        match jq.right.as_ref() {
            QueryExpr::Hybrid(hq) => assert_eq!(hq.alias.as_deref(), Some("h")),
            _ => panic!("Expected HybridQuery"),
        }
    } else {
        panic!("Expected JoinQuery");
    }
}

// ========================================================================
// Vector Query Tests
// ========================================================================

#[test]
fn test_parse_simple_vector_query() {
    let query = parse("VECTOR SEARCH embeddings SIMILAR TO [0.1, 0.2, 0.3] LIMIT 10").unwrap();
    if let QueryExpr::Vector(vq) = query {
        assert_eq!(vq.collection, "embeddings");
        assert_eq!(vq.k, 10);
        match vq.query_vector {
            VectorSource::Literal(v) => {
                assert_eq!(v.len(), 3);
                assert!((v[0] - 0.1).abs() < 0.001);
            }
            _ => panic!("Expected literal vector"),
        }
    } else {
        panic!("Expected VectorQuery");
    }
}

#[test]
fn test_parse_vector_query_with_text() {
    let query =
        parse("VECTOR SEARCH cve_embeddings SIMILAR TO 'remote code execution' LIMIT 5").unwrap();
    if let QueryExpr::Vector(vq) = query {
        assert_eq!(vq.collection, "cve_embeddings");
        assert_eq!(vq.k, 5);
        match vq.query_vector {
            VectorSource::Text(t) => assert_eq!(t, "remote code execution"),
            _ => panic!("Expected text source"),
        }
    } else {
        panic!("Expected VectorQuery");
    }
}

#[test]
fn test_parse_vector_query_with_filter() {
    let query =
        parse("VECTOR SEARCH docs SIMILAR TO [0.5, 0.5] WHERE source = 'nmap' LIMIT 20").unwrap();
    if let QueryExpr::Vector(vq) = query {
        assert_eq!(vq.collection, "docs");
        assert!(vq.filter.is_some());
        match vq.filter.unwrap() {
            MetadataFilter::Eq(field, value) => {
                assert_eq!(field, "source");
                assert_eq!(value, MetadataValue::String("nmap".to_string()));
            }
            _ => panic!("Expected Eq filter"),
        }
    } else {
        panic!("Expected VectorQuery");
    }
}

#[test]
fn test_parse_vector_query_with_metric() {
    // Use "embeddings" instead of "vectors" since "VECTORS" is a reserved keyword
    let query =
        parse("VECTOR SEARCH embeddings SIMILAR TO [1.0, 0.0] METRIC COSINE LIMIT 5").unwrap();
    if let QueryExpr::Vector(vq) = query {
        assert_eq!(vq.metric, Some(DistanceMetric::Cosine));
    } else {
        panic!("Expected VectorQuery");
    }
}

#[test]
fn test_parse_vector_query_full() {
    let query = parse(
        "VECTOR SEARCH knowledge SIMILAR TO 'vulnerability scan' \
         WHERE severity >= 7 AND type = 'CVE' \
         METRIC L2 THRESHOLD 0.5 INCLUDE VECTORS INCLUDE METADATA LIMIT 100",
    )
    .unwrap();
    if let QueryExpr::Vector(vq) = query {
        assert_eq!(vq.collection, "knowledge");
        assert_eq!(vq.k, 100);
        assert!(vq.filter.is_some());
        assert_eq!(vq.metric, Some(DistanceMetric::L2));
        assert_eq!(vq.threshold, Some(0.5));
        assert!(vq.include_vectors);
        assert!(vq.include_metadata);
    } else {
        panic!("Expected VectorQuery");
    }
}

#[test]
fn test_parse_hybrid_query() {
    let query = parse(
        "HYBRID \
         SELECT * FROM hosts WHERE os = 'Linux' \
         VECTOR SEARCH embeddings SIMILAR TO [0.1, 0.2] \
         FUSION RERANK(0.7) LIMIT 50",
    )
    .unwrap();
    if let QueryExpr::Hybrid(hq) = query {
        assert!(matches!(*hq.structured, QueryExpr::Table(_)));
        assert_eq!(hq.vector.collection, "embeddings");
        assert!(
            matches!(hq.fusion, FusionStrategy::Rerank { weight } if (weight - 0.7).abs() < 0.01)
        );
        assert_eq!(hq.limit, Some(50));
    } else {
        panic!("Expected HybridQuery");
    }
}

#[test]
fn test_parse_hybrid_with_graph() {
    let query = parse(
        "HYBRID \
         MATCH (h:Host)-[:HAS_SERVICE]->(s:Service) RETURN h, s \
         VECTOR SEARCH service_vectors SIMILAR TO 'ssh vulnerable' \
         FUSION RRF(60) LIMIT 20",
    )
    .unwrap();
    if let QueryExpr::Hybrid(hq) = query {
        assert!(matches!(*hq.structured, QueryExpr::Graph(_)));
        assert_eq!(hq.vector.collection, "service_vectors");
        assert!(matches!(hq.fusion, FusionStrategy::RRF { k: 60 }));
    } else {
        panic!("Expected HybridQuery");
    }
}

#[test]
fn test_parse_fusion_strategies() {
    // RERANK
    let query =
        parse("HYBRID FROM hosts VECTOR SEARCH e SIMILAR TO [0.1] FUSION RERANK LIMIT 10").unwrap();
    if let QueryExpr::Hybrid(hq) = query {
        assert!(matches!(hq.fusion, FusionStrategy::Rerank { .. }));
    }

    // RRF
    let query = parse("HYBRID FROM hosts VECTOR SEARCH e SIMILAR TO [0.1] FUSION RRF(30) LIMIT 10")
        .unwrap();
    if let QueryExpr::Hybrid(hq) = query {
        assert!(matches!(hq.fusion, FusionStrategy::RRF { k: 30 }));
    }

    // INTERSECTION
    let query =
        parse("HYBRID FROM hosts VECTOR SEARCH e SIMILAR TO [0.1] FUSION INTERSECTION LIMIT 10")
            .unwrap();
    if let QueryExpr::Hybrid(hq) = query {
        assert!(matches!(hq.fusion, FusionStrategy::Intersection));
    }

    // UNION
    let query =
        parse("HYBRID FROM hosts VECTOR SEARCH e SIMILAR TO [0.1] FUSION UNION(0.3, 0.7) LIMIT 10")
            .unwrap();
    if let QueryExpr::Hybrid(hq) = query {
        if let FusionStrategy::Union {
            structured_weight,
            vector_weight,
        } = hq.fusion
        {
            assert!((structured_weight - 0.3).abs() < 0.01);
            assert!((vector_weight - 0.7).abs() < 0.01);
        } else {
            panic!("Expected Union fusion");
        }
    }
}

// ========================================================================
// DML Tests: INSERT, UPDATE, DELETE
// ========================================================================

#[test]
fn test_parse_insert_single_row() {
    let query = parse("INSERT INTO hosts (ip, hostname) VALUES ('10.0.0.1', 'web01')").unwrap();
    if let QueryExpr::Insert(iq) = query {
        assert_eq!(iq.table, "hosts");
        assert_eq!(iq.columns, vec!["ip", "hostname"]);
        assert_eq!(iq.values.len(), 1);
        assert_eq!(iq.values[0].len(), 2);
        assert!(!iq.returning);
    } else {
        panic!("Expected InsertQuery");
    }
}

#[test]
fn test_parse_insert_multi_row() {
    let query = parse(
        "INSERT INTO hosts (ip, port) VALUES ('10.0.0.1', 22), ('10.0.0.2', 80), ('10.0.0.3', 443)",
    )
    .unwrap();
    if let QueryExpr::Insert(iq) = query {
        assert_eq!(iq.table, "hosts");
        assert_eq!(iq.columns, vec!["ip", "port"]);
        assert_eq!(iq.values.len(), 3);
        assert_eq!(iq.values[0].len(), 2);
        assert_eq!(iq.values[1].len(), 2);
        assert_eq!(iq.values[2].len(), 2);
    } else {
        panic!("Expected InsertQuery");
    }
}

#[test]
fn test_parse_insert_with_returning() {
    let query =
        parse("INSERT INTO hosts (ip, hostname) VALUES ('10.0.0.1', 'web01') RETURNING").unwrap();
    if let QueryExpr::Insert(iq) = query {
        assert!(iq.returning);
    } else {
        panic!("Expected InsertQuery");
    }
}

#[test]
fn test_parse_insert_mixed_types() {
    let query =
        parse("INSERT INTO metrics (name, value, active) VALUES ('cpu', 3.14, true)").unwrap();
    if let QueryExpr::Insert(iq) = query {
        assert_eq!(iq.values[0].len(), 3);
        assert!(matches!(
            iq.values[0][0],
            crate::storage::schema::Value::Text(_)
        ));
        assert!(matches!(
            iq.values[0][1],
            crate::storage::schema::Value::Float(_)
        ));
        assert!(matches!(
            iq.values[0][2],
            crate::storage::schema::Value::Boolean(true)
        ));
    } else {
        panic!("Expected InsertQuery");
    }
}

#[test]
fn test_parse_update_with_where() {
    let query = parse("UPDATE hosts SET hostname = 'new-name' WHERE ip = '10.0.0.1'").unwrap();
    if let QueryExpr::Update(uq) = query {
        assert_eq!(uq.table, "hosts");
        assert_eq!(uq.assignments.len(), 1);
        assert_eq!(uq.assignments[0].0, "hostname");
        assert!(uq.filter.is_some());
    } else {
        panic!("Expected UpdateQuery");
    }
}

#[test]
fn test_parse_update_no_where() {
    let query = parse("UPDATE hosts SET status = 'inactive'").unwrap();
    if let QueryExpr::Update(uq) = query {
        assert_eq!(uq.table, "hosts");
        assert_eq!(uq.assignments.len(), 1);
        assert!(uq.filter.is_none());
    } else {
        panic!("Expected UpdateQuery");
    }
}

#[test]
fn test_parse_update_multiple_assignments() {
    let query =
        parse("UPDATE hosts SET hostname = 'web01', port = 8080, active = true WHERE id = 1")
            .unwrap();
    if let QueryExpr::Update(uq) = query {
        assert_eq!(uq.assignments.len(), 3);
        assert_eq!(uq.assignments[0].0, "hostname");
        assert_eq!(uq.assignments[1].0, "port");
        assert_eq!(uq.assignments[2].0, "active");
        assert!(uq.filter.is_some());
    } else {
        panic!("Expected UpdateQuery");
    }
}

#[test]
fn test_parse_delete_with_where() {
    let query = parse("DELETE FROM hosts WHERE status = 'inactive'").unwrap();
    if let QueryExpr::Delete(dq) = query {
        assert_eq!(dq.table, "hosts");
        assert!(dq.filter.is_some());
    } else {
        panic!("Expected DeleteQuery");
    }
}

#[test]
fn test_parse_delete_no_where() {
    let query = parse("DELETE FROM hosts").unwrap();
    if let QueryExpr::Delete(dq) = query {
        assert_eq!(dq.table, "hosts");
        assert!(dq.filter.is_none());
    } else {
        panic!("Expected DeleteQuery");
    }
}

// ========================================================================
// DDL Tests: CREATE TABLE, DROP TABLE, ALTER TABLE
// ========================================================================

#[test]
fn test_parse_create_table_simple() {
    let query = parse("CREATE TABLE hosts (ip TEXT, hostname TEXT, port INTEGER)").unwrap();
    if let QueryExpr::CreateTable(ct) = query {
        assert_eq!(ct.name, "hosts");
        assert_eq!(ct.columns.len(), 3);
        assert!(!ct.if_not_exists);
        assert_eq!(ct.default_ttl_ms, None);
        assert_eq!(ct.columns[0].name, "ip");
        assert_eq!(ct.columns[0].data_type, "TEXT");
        assert_eq!(ct.columns[1].name, "hostname");
        assert_eq!(ct.columns[2].name, "port");
        assert_eq!(ct.columns[2].data_type, "INTEGER");
    } else {
        panic!("Expected CreateTableQuery");
    }
}

#[test]
fn test_parse_create_table_full() {
    let query = parse(
        "CREATE TABLE IF NOT EXISTS users (\
         id INTEGER PRIMARY KEY, \
         email TEXT NOT NULL UNIQUE, \
         name TEXT DEFAULT = 'unknown', \
         bio TEXT COMPRESS:3\
         )",
    )
    .unwrap();
    if let QueryExpr::CreateTable(ct) = query {
        assert_eq!(ct.name, "users");
        assert!(ct.if_not_exists);
        assert_eq!(ct.default_ttl_ms, None);
        assert_eq!(ct.columns.len(), 4);

        // id column
        assert_eq!(ct.columns[0].name, "id");
        assert_eq!(ct.columns[0].data_type, "INTEGER");
        assert!(ct.columns[0].primary_key);

        // email column
        assert_eq!(ct.columns[1].name, "email");
        assert_eq!(ct.columns[1].data_type, "TEXT");
        assert!(ct.columns[1].not_null);
        assert!(ct.columns[1].unique);

        // name column
        assert_eq!(ct.columns[2].name, "name");
        assert_eq!(ct.columns[2].default, Some("unknown".to_string()));

        // bio column
        assert_eq!(ct.columns[3].name, "bio");
        assert_eq!(ct.columns[3].compress, Some(3));
    } else {
        panic!("Expected CreateTableQuery");
    }
}

#[test]
fn test_parse_create_table_with_enum() {
    let query =
        parse("CREATE TABLE statuses (status ENUM('active','inactive','pending'))").unwrap();
    if let QueryExpr::CreateTable(ct) = query {
        assert_eq!(
            ct.columns[0].data_type,
            "ENUM('active','inactive','pending')"
        );
        assert_eq!(ct.default_ttl_ms, None);
    } else {
        panic!("Expected CreateTableQuery");
    }
}

#[test]
fn test_parse_create_table_with_ttl_clause() {
    let query = parse("CREATE TABLE sessions (token TEXT, user_id TEXT) WITH TTL 60s").unwrap();
    if let QueryExpr::CreateTable(ct) = query {
        assert_eq!(ct.name, "sessions");
        assert_eq!(ct.default_ttl_ms, Some(60_000));
        assert_eq!(ct.columns.len(), 2);
    } else {
        panic!("Expected CreateTableQuery");
    }
}

#[test]
fn test_parse_drop_table() {
    let query = parse("DROP TABLE hosts").unwrap();
    if let QueryExpr::DropTable(dt) = query {
        assert_eq!(dt.name, "hosts");
        assert!(!dt.if_exists);
    } else {
        panic!("Expected DropTableQuery");
    }
}

#[test]
fn test_parse_drop_table_if_exists() {
    let query = parse("DROP TABLE IF EXISTS hosts").unwrap();
    if let QueryExpr::DropTable(dt) = query {
        assert_eq!(dt.name, "hosts");
        assert!(dt.if_exists);
    } else {
        panic!("Expected DropTableQuery");
    }
}

#[test]
fn test_parse_alter_table_add_column() {
    let query = parse("ALTER TABLE hosts ADD COLUMN status TEXT NOT NULL").unwrap();
    if let QueryExpr::AlterTable(at) = query {
        assert_eq!(at.name, "hosts");
        assert_eq!(at.operations.len(), 1);
        match &at.operations[0] {
            crate::storage::query::ast::AlterOperation::AddColumn(col) => {
                assert_eq!(col.name, "status");
                assert_eq!(col.data_type, "TEXT");
                assert!(col.not_null);
            }
            _ => panic!("Expected AddColumn"),
        }
    } else {
        panic!("Expected AlterTableQuery");
    }
}

#[test]
fn test_parse_alter_table_drop_column() {
    let query = parse("ALTER TABLE hosts DROP COLUMN old_field").unwrap();
    if let QueryExpr::AlterTable(at) = query {
        assert_eq!(at.name, "hosts");
        assert_eq!(at.operations.len(), 1);
        match &at.operations[0] {
            crate::storage::query::ast::AlterOperation::DropColumn(name) => {
                assert_eq!(name, "old_field");
            }
            _ => panic!("Expected DropColumn"),
        }
    } else {
        panic!("Expected AlterTableQuery");
    }
}

#[test]
fn test_parse_alter_table_rename_column() {
    let query = parse("ALTER TABLE hosts RENAME COLUMN old_name TO new_name").unwrap();
    if let QueryExpr::AlterTable(at) = query {
        assert_eq!(at.name, "hosts");
        assert_eq!(at.operations.len(), 1);
        match &at.operations[0] {
            crate::storage::query::ast::AlterOperation::RenameColumn { from, to } => {
                assert_eq!(from, "old_name");
                assert_eq!(to, "new_name");
            }
            _ => panic!("Expected RenameColumn"),
        }
    } else {
        panic!("Expected AlterTableQuery");
    }
}

// ========================================================================
// INSERT with entity types: NODE, EDGE, VECTOR, DOCUMENT, KV
// ========================================================================

#[test]
fn test_parse_insert_row_default() {
    let query = parse("INSERT INTO hosts (ip, port) VALUES ('10.0.0.1', 22)").unwrap();
    if let QueryExpr::Insert(ins) = query {
        assert_eq!(ins.table, "hosts");
        assert_eq!(
            ins.entity_type,
            crate::storage::query::ast::InsertEntityType::Row
        );
        assert_eq!(ins.columns, vec!["ip", "port"]);
        assert_eq!(ins.values.len(), 1);
    } else {
        panic!("Expected InsertQuery");
    }
}

#[test]
fn test_parse_insert_node() {
    let query = parse(
        "INSERT INTO network NODE (label, node_type, ip) VALUES ('router', 'device', '10.0.0.1')",
    )
    .unwrap();
    if let QueryExpr::Insert(ins) = query {
        assert_eq!(ins.table, "network");
        assert_eq!(
            ins.entity_type,
            crate::storage::query::ast::InsertEntityType::Node
        );
        assert_eq!(ins.columns, vec!["label", "node_type", "ip"]);
        assert_eq!(ins.values.len(), 1);
        assert_eq!(ins.values[0].len(), 3);
    } else {
        panic!("Expected InsertQuery with Node entity type");
    }
}

#[test]
fn test_parse_insert_edge() {
    let query =
        parse("INSERT INTO network EDGE (label, from, to, weight) VALUES ('connects', 1, 2, 0.5)")
            .unwrap();
    if let QueryExpr::Insert(ins) = query {
        assert_eq!(ins.table, "network");
        assert_eq!(
            ins.entity_type,
            crate::storage::query::ast::InsertEntityType::Edge
        );
        // Keywords as column names are returned uppercase
        assert_eq!(ins.columns.len(), 4);
        assert!(ins.columns[0].eq_ignore_ascii_case("label"));
        assert!(ins.columns[1].eq_ignore_ascii_case("from"));
        assert!(ins.columns[2].eq_ignore_ascii_case("to"));
        assert!(ins.columns[3].eq_ignore_ascii_case("weight"));
    } else {
        panic!("Expected InsertQuery with Edge entity type");
    }
}

#[test]
fn test_parse_insert_vector() {
    let query = parse(
        "INSERT INTO embeddings VECTOR (dense, content) VALUES ([0.1, 0.2, 0.3], 'hello world')",
    )
    .unwrap();
    if let QueryExpr::Insert(ins) = query {
        assert_eq!(ins.table, "embeddings");
        assert_eq!(
            ins.entity_type,
            crate::storage::query::ast::InsertEntityType::Vector
        );
        assert_eq!(ins.columns, vec!["dense", "content"]);
        assert_eq!(ins.values.len(), 1);
        // The vector literal should be parsed as Value::Vector
        match &ins.values[0][0] {
            crate::storage::schema::Value::Vector(v) => {
                assert_eq!(v.len(), 3);
                assert!((v[0] - 0.1).abs() < 0.01);
            }
            other => panic!("Expected Vector value, got {other:?}"),
        }
    } else {
        panic!("Expected InsertQuery with Vector entity type");
    }
}

#[test]
fn test_parse_insert_document() {
    let query =
        parse(r#"INSERT INTO docs DOCUMENT (body) VALUES ('{"name":"test","value":42}')"#).unwrap();
    if let QueryExpr::Insert(ins) = query {
        assert_eq!(ins.table, "docs");
        assert_eq!(
            ins.entity_type,
            crate::storage::query::ast::InsertEntityType::Document
        );
        assert_eq!(ins.columns, vec!["body"]);
    } else {
        panic!("Expected InsertQuery with Document entity type");
    }
}

#[test]
fn test_parse_insert_kv() {
    let query =
        parse("INSERT INTO cache KV (key, value) VALUES ('session:123', 'token-abc')").unwrap();
    if let QueryExpr::Insert(ins) = query {
        assert_eq!(ins.table, "cache");
        assert_eq!(
            ins.entity_type,
            crate::storage::query::ast::InsertEntityType::Kv
        );
        assert_eq!(ins.columns.len(), 2);
        assert!(ins.columns[0].eq_ignore_ascii_case("key"));
        assert!(ins.columns[1].eq_ignore_ascii_case("value"));
    } else {
        panic!("Expected InsertQuery with Kv entity type");
    }
}

#[test]
fn test_parse_insert_vector_array_literal() {
    // Test array literal parsing in VALUES
    let query = parse("INSERT INTO emb VECTOR (dense) VALUES ([1, 2, 3])").unwrap();
    if let QueryExpr::Insert(ins) = query {
        match &ins.values[0][0] {
            crate::storage::schema::Value::Vector(v) => {
                assert_eq!(v, &[1.0, 2.0, 3.0]);
            }
            other => panic!("Expected Vector value, got {other:?}"),
        }
    } else {
        panic!("Expected InsertQuery");
    }
}

// ========================================================================
// GRAPH Command Tests
// ========================================================================

#[test]
fn test_parse_graph_neighborhood_defaults() {
    let query = parse("GRAPH NEIGHBORHOOD 'node_1'").unwrap();
    if let QueryExpr::GraphCommand(crate::storage::query::ast::GraphCommand::Neighborhood {
        source,
        depth,
        direction,
    }) = query
    {
        assert_eq!(source, "node_1");
        assert_eq!(depth, 3);
        assert_eq!(direction, "outgoing");
    } else {
        panic!("Expected GraphCommand::Neighborhood");
    }
}

#[test]
fn test_parse_graph_neighborhood_with_options() {
    let query = parse("GRAPH NEIGHBORHOOD 'node_a' DEPTH 5 DIRECTION both").unwrap();
    if let QueryExpr::GraphCommand(crate::storage::query::ast::GraphCommand::Neighborhood {
        source,
        depth,
        direction,
    }) = query
    {
        assert_eq!(source, "node_a");
        assert_eq!(depth, 5);
        assert!(direction.eq_ignore_ascii_case("both"));
    } else {
        panic!("Expected GraphCommand::Neighborhood");
    }
}

#[test]
fn test_parse_graph_shortest_path() {
    let query = parse("GRAPH SHORTEST_PATH 'a' TO 'b' ALGORITHM dijkstra").unwrap();
    if let QueryExpr::GraphCommand(crate::storage::query::ast::GraphCommand::ShortestPath {
        source,
        target,
        algorithm,
        direction,
    }) = query
    {
        assert_eq!(source, "a");
        assert_eq!(target, "b");
        assert_eq!(algorithm, "dijkstra");
        assert_eq!(direction, "outgoing");
    } else {
        panic!("Expected GraphCommand::ShortestPath");
    }
}

#[test]
fn test_parse_graph_traverse() {
    let query = parse("GRAPH TRAVERSE 'root' STRATEGY dfs DEPTH 10 DIRECTION incoming").unwrap();
    if let QueryExpr::GraphCommand(crate::storage::query::ast::GraphCommand::Traverse {
        source,
        strategy,
        depth,
        direction,
    }) = query
    {
        assert_eq!(source, "root");
        assert_eq!(strategy, "dfs");
        assert_eq!(depth, 10);
        assert!(direction.eq_ignore_ascii_case("incoming"));
    } else {
        panic!("Expected GraphCommand::Traverse");
    }
}

#[test]
fn test_parse_graph_centrality() {
    let query = parse("GRAPH CENTRALITY ALGORITHM pagerank").unwrap();
    if let QueryExpr::GraphCommand(crate::storage::query::ast::GraphCommand::Centrality {
        algorithm,
    }) = query
    {
        assert_eq!(algorithm, "pagerank");
    } else {
        panic!("Expected GraphCommand::Centrality");
    }
}

#[test]
fn test_parse_graph_centrality_default() {
    let query = parse("GRAPH CENTRALITY").unwrap();
    if let QueryExpr::GraphCommand(crate::storage::query::ast::GraphCommand::Centrality {
        algorithm,
    }) = query
    {
        assert_eq!(algorithm, "degree");
    } else {
        panic!("Expected GraphCommand::Centrality with default algorithm");
    }
}

#[test]
fn test_parse_graph_community() {
    let query = parse("GRAPH COMMUNITY ALGORITHM louvain MAX_ITERATIONS 50").unwrap();
    if let QueryExpr::GraphCommand(crate::storage::query::ast::GraphCommand::Community {
        algorithm,
        max_iterations,
    }) = query
    {
        assert_eq!(algorithm, "louvain");
        assert_eq!(max_iterations, 50);
    } else {
        panic!("Expected GraphCommand::Community");
    }
}

#[test]
fn test_parse_graph_components() {
    let query = parse("GRAPH COMPONENTS MODE strong").unwrap();
    if let QueryExpr::GraphCommand(crate::storage::query::ast::GraphCommand::Components { mode }) =
        query
    {
        assert_eq!(mode, "strong");
    } else {
        panic!("Expected GraphCommand::Components");
    }
}

#[test]
fn test_parse_graph_cycles() {
    let query = parse("GRAPH CYCLES MAX_LENGTH 5").unwrap();
    if let QueryExpr::GraphCommand(crate::storage::query::ast::GraphCommand::Cycles {
        max_length,
    }) = query
    {
        assert_eq!(max_length, 5);
    } else {
        panic!("Expected GraphCommand::Cycles");
    }
}

#[test]
fn test_parse_graph_clustering() {
    let query = parse("GRAPH CLUSTERING").unwrap();
    assert!(matches!(
        query,
        QueryExpr::GraphCommand(crate::storage::query::ast::GraphCommand::Clustering)
    ));
}

#[test]
fn test_parse_graph_topological_sort() {
    let query = parse("GRAPH TOPOLOGICAL_SORT").unwrap();
    assert!(matches!(
        query,
        QueryExpr::GraphCommand(crate::storage::query::ast::GraphCommand::TopologicalSort)
    ));
}

// ========================================================================
// SEARCH Command Tests
// ========================================================================

#[test]
fn test_parse_search_similar() {
    let query = parse("SEARCH SIMILAR [0.1, 0.2, 0.3] COLLECTION embeddings LIMIT 5 MIN_SCORE 0.8")
        .unwrap();
    if let QueryExpr::SearchCommand(crate::storage::query::ast::SearchCommand::Similar {
        vector,
        collection,
        limit,
        min_score,
        text: _,
        provider: _,
    }) = query
    {
        assert_eq!(vector.len(), 3);
        assert!((vector[0] - 0.1).abs() < 0.01);
        assert_eq!(collection, "embeddings");
        assert_eq!(limit, 5);
        assert!((min_score - 0.8).abs() < 0.01);
    } else {
        panic!("Expected SearchCommand::Similar");
    }
}

#[test]
fn test_parse_search_similar_defaults() {
    let query = parse("SEARCH SIMILAR [1, 2, 3] COLLECTION vecs").unwrap();
    if let QueryExpr::SearchCommand(crate::storage::query::ast::SearchCommand::Similar {
        limit,
        min_score,
        ..
    }) = query
    {
        assert_eq!(limit, 10);
        assert!((min_score).abs() < 0.01);
    } else {
        panic!("Expected SearchCommand::Similar");
    }
}

#[test]
fn test_parse_search_text() {
    let query =
        parse("SEARCH TEXT 'find all vulnerabilities' COLLECTION hosts LIMIT 20 FUZZY").unwrap();
    if let QueryExpr::SearchCommand(crate::storage::query::ast::SearchCommand::Text {
        query: q,
        collection,
        limit,
        fuzzy,
    }) = query
    {
        assert_eq!(q, "find all vulnerabilities");
        assert_eq!(collection, Some("hosts".to_string()));
        assert_eq!(limit, 20);
        assert!(fuzzy);
    } else {
        panic!("Expected SearchCommand::Text");
    }
}

#[test]
fn test_parse_search_text_minimal() {
    let query = parse("SEARCH TEXT 'hello world'").unwrap();
    if let QueryExpr::SearchCommand(crate::storage::query::ast::SearchCommand::Text {
        query: q,
        collection,
        limit,
        fuzzy,
    }) = query
    {
        assert_eq!(q, "hello world");
        assert_eq!(collection, None);
        assert_eq!(limit, 10);
        assert!(!fuzzy);
    } else {
        panic!("Expected SearchCommand::Text");
    }
}

#[test]
fn test_parse_search_hybrid() {
    let query =
        parse("SEARCH HYBRID SIMILAR [0.1, 0.2] TEXT 'query string' COLLECTION data LIMIT 15")
            .unwrap();
    if let QueryExpr::SearchCommand(crate::storage::query::ast::SearchCommand::Hybrid {
        vector,
        query: q,
        collection,
        limit,
    }) = query
    {
        assert_eq!(vector.unwrap().len(), 2);
        assert_eq!(q.unwrap(), "query string");
        assert_eq!(collection, "data");
        assert_eq!(limit, 15);
    } else {
        panic!("Expected SearchCommand::Hybrid");
    }
}

#[test]
fn test_parse_search_hybrid_text_only() {
    let query = parse("SEARCH HYBRID TEXT 'query' COLLECTION data").unwrap();
    if let QueryExpr::SearchCommand(crate::storage::query::ast::SearchCommand::Hybrid {
        vector,
        query: q,
        ..
    }) = query
    {
        assert!(vector.is_none());
        assert_eq!(q.unwrap(), "query");
    } else {
        panic!("Expected SearchCommand::Hybrid");
    }
}

#[test]
fn test_parse_search_hybrid_vector_only() {
    let query = parse("SEARCH HYBRID SIMILAR [1, 2, 3] COLLECTION data").unwrap();
    if let QueryExpr::SearchCommand(crate::storage::query::ast::SearchCommand::Hybrid {
        vector,
        query: q,
        ..
    }) = query
    {
        assert!(vector.is_some());
        assert!(q.is_none());
    } else {
        panic!("Expected SearchCommand::Hybrid");
    }
}

#[test]
fn test_parse_search_hybrid_requires_input() {
    // Must have at least SIMILAR or TEXT
    let result = parse("SEARCH HYBRID COLLECTION data");
    assert!(result.is_err());
}

#[test]
fn test_parse_search_multimodal() {
    let query =
        parse("SEARCH MULTIMODAL 'CPF: 000.000.000-00' COLLECTION people LIMIT 20").unwrap();
    if let QueryExpr::SearchCommand(crate::storage::query::ast::SearchCommand::Multimodal {
        query,
        collection,
        limit,
    }) = query
    {
        assert_eq!(query, "CPF: 000.000.000-00");
        assert_eq!(collection, Some("people".to_string()));
        assert_eq!(limit, 20);
    } else {
        panic!("Expected SearchCommand::Multimodal");
    }
}

#[test]
fn test_parse_search_multimodal_defaults() {
    let query = parse("SEARCH MULTIMODAL 'user:123'").unwrap();
    if let QueryExpr::SearchCommand(crate::storage::query::ast::SearchCommand::Multimodal {
        query,
        collection,
        limit,
    }) = query
    {
        assert_eq!(query, "user:123");
        assert_eq!(collection, None);
        assert_eq!(limit, 25);
    } else {
        panic!("Expected SearchCommand::Multimodal");
    }
}

#[test]
fn test_parse_search_index_defaults() {
    let query = parse("SEARCH INDEX cpf VALUE '000.000.000-00'").unwrap();
    if let QueryExpr::SearchCommand(crate::storage::query::ast::SearchCommand::Index {
        index,
        value,
        collection,
        limit,
        exact,
    }) = query
    {
        assert_eq!(index, "cpf");
        assert_eq!(value, "000.000.000-00");
        assert_eq!(collection, None);
        assert_eq!(limit, 25);
        assert!(exact);
    } else {
        panic!("Expected SearchCommand::Index");
    }
}

#[test]
fn test_parse_search_index_with_collection_limit_fuzzy() {
    let query =
        parse("SEARCH INDEX cpf VALUE '000.000.000-00' COLLECTION people LIMIT 20 FUZZY").unwrap();
    if let QueryExpr::SearchCommand(crate::storage::query::ast::SearchCommand::Index {
        index,
        value,
        collection,
        limit,
        exact,
    }) = query
    {
        assert_eq!(index, "cpf");
        assert_eq!(value, "000.000.000-00");
        assert_eq!(collection, Some("people".to_string()));
        assert_eq!(limit, 20);
        assert!(!exact);
    } else {
        panic!("Expected SearchCommand::Index");
    }
}

#[test]
fn test_parse_search_context_defaults() {
    let query = parse("SEARCH CONTEXT '000.000.000-00'").unwrap();
    if let QueryExpr::SearchCommand(crate::storage::query::ast::SearchCommand::Context {
        query: q,
        field,
        collection,
        limit,
        depth,
    }) = query
    {
        assert_eq!(q, "000.000.000-00");
        assert_eq!(field, None);
        assert_eq!(collection, None);
        assert_eq!(limit, 25);
        assert_eq!(depth, 1);
    } else {
        panic!("Expected SearchCommand::Context");
    }
}

#[test]
fn test_parse_search_context_with_field_collection_limit_depth() {
    let query =
        parse("SEARCH CONTEXT '000.000.000-00' FIELD cpf COLLECTION customers LIMIT 50 DEPTH 2")
            .unwrap();
    if let QueryExpr::SearchCommand(crate::storage::query::ast::SearchCommand::Context {
        query: q,
        field,
        collection,
        limit,
        depth,
    }) = query
    {
        assert_eq!(q, "000.000.000-00");
        assert_eq!(field, Some("cpf".to_string()));
        assert_eq!(collection, Some("customers".to_string()));
        assert_eq!(limit, 50);
        assert_eq!(depth, 2);
    } else {
        panic!("Expected SearchCommand::Context");
    }
}

#[test]
fn test_parse_group_by() {
    let query = parse("SELECT status FROM users GROUP BY status").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "users");
        assert_eq!(tq.group_by, vec!["status".to_string()]);
        assert!(tq.having.is_none());
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_group_by_multiple_fields() {
    let query = parse("SELECT dept, role FROM employees GROUP BY dept, role").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "employees");
        assert_eq!(tq.group_by, vec!["dept".to_string(), "role".to_string()]);
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_group_by_with_having() {
    let query =
        parse("SELECT dept FROM employees GROUP BY dept HAVING dept > 5 ORDER BY dept").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "employees");
        assert_eq!(tq.group_by, vec!["dept".to_string()]);
        assert!(tq.having.is_some());
        assert!(!tq.order_by.is_empty());
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_group_by_with_limit() {
    let query = parse("SELECT * FROM logs GROUP BY level LIMIT 10").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "logs");
        assert_eq!(tq.group_by, vec!["level".to_string()]);
        assert_eq!(tq.limit, Some(10));
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_insert_with_ttl() {
    let query = parse("INSERT INTO sessions (token) VALUES ('abc') WITH TTL 60 s").unwrap();
    if let QueryExpr::Insert(iq) = query {
        assert_eq!(iq.table, "sessions");
        assert_eq!(iq.ttl_ms, Some(60_000));
        assert!(iq.with_metadata.is_empty());
    } else {
        panic!("Expected InsertQuery");
    }
}

#[test]
fn test_parse_insert_with_ttl_hours() {
    let query = parse("INSERT INTO cache (key, value) VALUES ('k', 'v') WITH TTL 24 h").unwrap();
    if let QueryExpr::Insert(iq) = query {
        assert_eq!(iq.ttl_ms, Some(24 * 3_600_000));
    } else {
        panic!("Expected InsertQuery");
    }
}

#[test]
fn test_parse_insert_with_metadata() {
    let query = parse(
        "INSERT INTO events (name) VALUES ('login') WITH METADATA (priority = 'high', level = 3)",
    )
    .unwrap();
    if let QueryExpr::Insert(iq) = query {
        assert_eq!(iq.table, "events");
        assert_eq!(iq.with_metadata.len(), 2);
        assert_eq!(iq.with_metadata[0].0, "priority");
        assert_eq!(iq.with_metadata[1].0, "level");
    } else {
        panic!("Expected InsertQuery");
    }
}

#[test]
fn test_parse_insert_with_ttl_and_metadata() {
    let query = parse(
        "INSERT INTO sessions (token) VALUES ('abc') WITH TTL 1 h WITH METADATA (source = 'web')",
    )
    .unwrap();
    if let QueryExpr::Insert(iq) = query {
        assert_eq!(iq.ttl_ms, Some(3_600_000));
        assert_eq!(iq.with_metadata.len(), 1);
        assert_eq!(iq.with_metadata[0].0, "source");
    } else {
        panic!("Expected InsertQuery");
    }
}

#[test]
fn test_parse_insert_with_expires_at() {
    let query =
        parse("INSERT INTO events (name) VALUES ('launch') WITH EXPIRES AT 1735689600000").unwrap();
    if let QueryExpr::Insert(iq) = query {
        assert_eq!(iq.expires_at_ms, Some(1735689600000));
        assert_eq!(iq.ttl_ms, None);
    } else {
        panic!("Expected InsertQuery");
    }
}

#[test]
fn test_parse_update_with_ttl() {
    let query = parse("UPDATE sessions SET active = true WHERE id = 1 WITH TTL 2 h").unwrap();
    if let QueryExpr::Update(uq) = query {
        assert_eq!(uq.table, "sessions");
        assert_eq!(uq.ttl_ms, Some(7_200_000));
        assert!(uq.filter.is_some());
    } else {
        panic!("Expected UpdateQuery");
    }
}

#[test]
fn test_parse_update_with_metadata() {
    let query =
        parse("UPDATE users SET name = 'Alice' WHERE id = 1 WITH METADATA (role = 'admin')")
            .unwrap();
    if let QueryExpr::Update(uq) = query {
        assert_eq!(uq.with_metadata.len(), 1);
        assert_eq!(uq.with_metadata[0].0, "role");
    } else {
        panic!("Expected UpdateQuery");
    }
}

#[test]
fn test_parse_update_with_expires_at() {
    let query =
        parse("UPDATE cache SET value = 'x' WHERE name = 'k' WITH EXPIRES AT 1735689600000")
            .unwrap();
    if let QueryExpr::Update(uq) = query {
        assert_eq!(uq.expires_at_ms, Some(1735689600000));
    } else {
        panic!("Expected UpdateQuery");
    }
}

#[test]
fn test_parse_select_with_expand_graph() {
    let query =
        parse("SELECT * FROM customers WHERE cpf = '081' WITH EXPAND GRAPH DEPTH 2").unwrap();
    if let QueryExpr::Table(tq) = query {
        let expand = tq.expand.unwrap();
        assert!(expand.graph);
        assert_eq!(expand.graph_depth, 2);
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_select_with_expand_cross_refs() {
    let query = parse("SELECT * FROM ANY WHERE name = 'Alice' WITH EXPAND CROSS_REFS").unwrap();
    if let QueryExpr::Table(tq) = query {
        let expand = tq.expand.unwrap();
        assert!(expand.cross_refs);
        assert!(!expand.graph);
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_select_with_expand_all() {
    let query = parse("SELECT * FROM hosts WITH EXPAND ALL").unwrap();
    if let QueryExpr::Table(tq) = query {
        let expand = tq.expand.unwrap();
        assert!(expand.graph);
        assert!(expand.cross_refs);
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_create_table_with_context_index() {
    let query = parse(
        "CREATE TABLE customers (name TEXT, cpf TEXT, email TEXT) WITH CONTEXT INDEX ON (cpf, email)",
    )
    .unwrap();
    if let QueryExpr::CreateTable(ct) = query {
        assert_eq!(ct.name, "customers");
        assert_eq!(
            ct.context_index_fields,
            vec!["cpf".to_string(), "email".to_string()]
        );
    } else {
        panic!("Expected CreateTableQuery");
    }
}

#[test]
fn test_parse_create_table_with_ttl_and_context_index() {
    let query =
        parse("CREATE TABLE sessions (token TEXT) WITH TTL 24 h WITH CONTEXT INDEX ON (token)")
            .unwrap();
    if let QueryExpr::CreateTable(ct) = query {
        assert_eq!(ct.default_ttl_ms, Some(86_400_000));
        assert_eq!(ct.context_index_fields, vec!["token".to_string()]);
    } else {
        panic!("Expected CreateTableQuery");
    }
}

// ========================================================================
// JSON inline literal tests
// ========================================================================

#[test]
fn test_parse_insert_with_inline_json_object() {
    let query = parse("INSERT INTO logs (data) VALUES ({level: 'info', msg: 'hello'})").unwrap();
    if let QueryExpr::Insert(iq) = query {
        assert_eq!(iq.table, "logs");
        assert_eq!(iq.values.len(), 1);
        // The JSON value should be Value::Json(...)
        match &iq.values[0][0] {
            crate::storage::schema::Value::Json(bytes) => {
                let parsed: crate::json::Value = crate::json::from_slice(bytes).unwrap();
                assert_eq!(parsed.get("level").and_then(|v| v.as_str()), Some("info"));
                assert_eq!(parsed.get("msg").and_then(|v| v.as_str()), Some("hello"));
            }
            other => panic!("Expected Value::Json, got {:?}", other),
        }
    } else {
        panic!("Expected InsertQuery");
    }
}

#[test]
fn test_parse_insert_with_nested_json() {
    let query =
        parse("INSERT INTO events (payload) VALUES ({type: 'click', meta: {x: 100, y: 200}})")
            .unwrap();
    if let QueryExpr::Insert(iq) = query {
        match &iq.values[0][0] {
            crate::storage::schema::Value::Json(bytes) => {
                let parsed: crate::json::Value = crate::json::from_slice(bytes).unwrap();
                assert_eq!(parsed.get("type").and_then(|v| v.as_str()), Some("click"));
                let meta = parsed.get("meta").unwrap();
                assert_eq!(meta.get("x").and_then(|v| v.as_f64()), Some(100.0));
                assert_eq!(meta.get("y").and_then(|v| v.as_f64()), Some(200.0));
            }
            other => panic!("Expected Value::Json, got {:?}", other),
        }
    } else {
        panic!("Expected InsertQuery");
    }
}

#[test]
fn test_parse_insert_json_with_colon_separator() {
    // JSON-style with colons (standard JSON syntax)
    let query =
        parse(r#"INSERT INTO logs (data) VALUES ({"host": "srv1", "port": 8080})"#).unwrap();
    if let QueryExpr::Insert(iq) = query {
        match &iq.values[0][0] {
            crate::storage::schema::Value::Json(bytes) => {
                let parsed: crate::json::Value = crate::json::from_slice(bytes).unwrap();
                assert_eq!(parsed.get("host").and_then(|v| v.as_str()), Some("srv1"));
                assert_eq!(parsed.get("port").and_then(|v| v.as_f64()), Some(8080.0));
            }
            other => panic!("Expected Value::Json, got {:?}", other),
        }
    } else {
        panic!("Expected InsertQuery");
    }
}

#[test]
fn test_parse_create_timeseries() {
    let query = parse("CREATE TIMESERIES cpu_metrics RETENTION 90 d").unwrap();
    if let QueryExpr::CreateTimeSeries(ts) = query {
        assert_eq!(ts.name, "cpu_metrics");
        assert_eq!(ts.retention_ms, Some(90 * 86_400_000));
    } else {
        panic!("Expected CreateTimeSeriesQuery");
    }
}

#[test]
fn test_parse_create_queue() {
    let query = parse("CREATE QUEUE tasks MAX_SIZE 1000 PRIORITY").unwrap();
    if let QueryExpr::CreateQueue(q) = query {
        assert_eq!(q.name, "tasks");
        assert_eq!(q.max_size, Some(1000));
        assert!(q.priority);
    } else {
        panic!("Expected CreateQueueQuery");
    }
}

#[test]
fn test_parse_queue_push() {
    let query = parse("QUEUE PUSH tasks 'hello world'").unwrap();
    if let QueryExpr::QueueCommand(QueueCommand::Push { queue, value, .. }) = query {
        assert_eq!(queue, "tasks");
        assert_eq!(value, "hello world");
    } else {
        panic!("Expected QueueCommand::Push");
    }
}

#[test]
fn test_parse_queue_pop() {
    let query = parse("QUEUE POP tasks").unwrap();
    if let QueryExpr::QueueCommand(QueueCommand::Pop { queue, count, .. }) = query {
        assert_eq!(queue, "tasks");
        assert_eq!(count, 1);
    } else {
        panic!("Expected QueueCommand::Pop");
    }
}

#[test]
fn test_parse_create_index_hash() {
    let query = parse("CREATE INDEX idx_email ON users (email) USING HASH").unwrap();
    if let QueryExpr::CreateIndex(ci) = query {
        assert_eq!(ci.name, "idx_email");
        assert_eq!(ci.table, "users");
        assert_eq!(ci.columns, vec!["email"]);
        assert_eq!(ci.method, crate::storage::query::ast::IndexMethod::Hash);
        assert!(!ci.unique);
    } else {
        panic!("Expected CreateIndexQuery");
    }
}

#[test]
fn test_parse_create_unique_index() {
    let query = parse("CREATE UNIQUE INDEX idx_pk ON orders (id) USING HASH").unwrap();
    if let QueryExpr::CreateIndex(ci) = query {
        assert!(ci.unique);
        assert_eq!(ci.method, crate::storage::query::ast::IndexMethod::Hash);
    } else {
        panic!("Expected CreateIndexQuery");
    }
}

#[test]
fn test_parse_search_spatial_radius() {
    let query = parse(
        "SEARCH SPATIAL RADIUS 48.8566 2.3522 10.0 COLLECTION sites COLUMN location LIMIT 50",
    )
    .unwrap();
    if let QueryExpr::SearchCommand(crate::storage::query::ast::SearchCommand::SpatialRadius {
        center_lat,
        center_lon,
        radius_km,
        collection,
        limit,
        ..
    }) = query
    {
        assert!((center_lat - 48.8566).abs() < 0.001);
        assert!((center_lon - 2.3522).abs() < 0.001);
        assert!((radius_km - 10.0).abs() < 0.001);
        assert_eq!(collection, "sites");
        assert_eq!(limit, 50);
    } else {
        panic!("Expected SearchCommand::SpatialRadius");
    }
}

#[test]
fn test_parse_hll_commands() {
    let query = parse("CREATE HLL visitors").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(
            crate::storage::query::ast::ProbabilisticCommand::CreateHll { .. }
        )
    ));

    let query = parse("HLL ADD visitors 'user1' 'user2'").unwrap();
    if let QueryExpr::ProbabilisticCommand(
        crate::storage::query::ast::ProbabilisticCommand::HllAdd { name, elements },
    ) = query
    {
        assert_eq!(name, "visitors");
        assert_eq!(elements, vec!["user1", "user2"]);
    } else {
        panic!("Expected HllAdd");
    }

    let query = parse("HLL COUNT visitors").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(
            crate::storage::query::ast::ProbabilisticCommand::HllCount { .. }
        )
    ));
}
