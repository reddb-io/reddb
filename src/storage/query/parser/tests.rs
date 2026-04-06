//! Parser tests

use super::*;
use crate::storage::engine::graph_store::{GraphEdgeType, GraphNodeType};
use crate::storage::engine::vector_metadata::MetadataValue;
use crate::storage::query::ast::{
    DistanceMetric, EdgeDirection, Filter, FusionStrategy, JoinType, MetadataFilter, Projection,
    TableQuery, VectorSource,
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
fn test_parse_select_with_where() {
    let query = parse("SELECT ip FROM hosts WHERE os = 'Linux'").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert!(tq.filter.is_some());
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
        parse("FROM hosts h JOIN GRAPH (n:Host)-[:AFFECTED_BY]->(v) ON h.ip = n.id").unwrap();
    if let QueryExpr::Join(jq) = query {
        assert!(matches!(*jq.left, QueryExpr::Table(_)));
        assert!(matches!(*jq.right, QueryExpr::Graph(_)));
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
