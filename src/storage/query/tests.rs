use super::*;
use crate::storage::schema::Value;

#[cfg(test)]
mod query_tests {
    use super::*;

    #[test]
    fn test_table_query_builder() {
        let query = QueryExpr::table("hosts")
            .alias("h")
            .select("ip")
            .select("hostname")
            .filter(Filter::compare(
                FieldRef::column("h", "os"),
                CompareOp::Eq,
                Value::Text("Linux".to_string()),
            ))
            .limit(100)
            .build();

        if let QueryExpr::Table(tq) = query {
            assert_eq!(tq.table, "hosts");
            assert_eq!(tq.alias, Some("h".to_string()));
            assert_eq!(tq.columns.len(), 2);
            assert_eq!(tq.limit, Some(100));
        } else {
            panic!("Expected TableQuery");
        }
    }

    #[test]
    fn test_graph_query_builder() {
        let query = QueryExpr::graph()
            .node(NodePattern::new("h").of_type(GraphNodeType::Host))
            .node(NodePattern::new("s").of_type(GraphNodeType::Service))
            .edge(EdgePattern::new("h", "s").of_type(GraphEdgeType::HasService))
            .return_field(FieldRef::node_id("h"))
            .build();

        if let QueryExpr::Graph(gq) = query {
            assert_eq!(gq.pattern.nodes.len(), 2);
            assert_eq!(gq.pattern.edges.len(), 1);
            assert_eq!(gq.return_.len(), 1);
        } else {
            panic!("Expected GraphQuery");
        }
    }

    #[test]
    fn test_path_query_builder() {
        let query = QueryExpr::path(
            NodeSelector::by_id("host:192.168.1.1"),
            NodeSelector::by_id("host:10.0.0.1"),
        )
        .via(GraphEdgeType::AuthAccess)
        .via(GraphEdgeType::ConnectsTo)
        .max_length(5)
        .build();

        if let QueryExpr::Path(pq) = query {
            assert_eq!(pq.via.len(), 2);
            assert_eq!(pq.max_length, 5);
        } else {
            panic!("Expected PathQuery");
        }
    }

    #[test]
    fn test_join_query_builder() {
        let query = QueryExpr::table("hosts")
            .alias("h")
            .select("ip")
            .join_graph(
                GraphPattern::new()
                    .node(NodePattern::new("n").of_type(GraphNodeType::Host))
                    .edge(EdgePattern::new("n", "v").of_type(GraphEdgeType::AffectedBy)),
                JoinCondition::new(
                    FieldRef::column("h", "ip"),
                    FieldRef::node_prop("n", "label"),
                ),
            )
            .filter(Filter::compare(
                FieldRef::column("h", "severity"),
                CompareOp::Gt,
                Value::Integer(5),
            ))
            .order_by(OrderByClause {
                field: FieldRef::column("h", "ip"),
                ascending: true,
                nulls_first: false,
            })
            .limit(25)
            .return_field(FieldRef::column("h", "ip"))
            .build();

        if let QueryExpr::Join(jq) = query {
            assert!(matches!(*jq.left, QueryExpr::Table(_)));
            assert!(matches!(*jq.right, QueryExpr::Graph(_)));
            assert!(jq.filter.is_some());
            assert_eq!(jq.order_by.len(), 1);
            assert_eq!(jq.limit, Some(25));
            assert_eq!(jq.return_.len(), 1);
        } else {
            panic!("Expected JoinQuery");
        }
    }

    #[test]
    fn test_table_join_query_builder() {
        let query = QueryExpr::table("hosts")
            .alias("h")
            .join_table(
                "services",
                JoinCondition::new(
                    FieldRef::column("h", "id"),
                    FieldRef::column("services", "host_id"),
                ),
            )
            .return_field(FieldRef::column("h", "id"))
            .build();

        if let QueryExpr::Join(jq) = query {
            assert!(matches!(*jq.left, QueryExpr::Table(_)));
            assert!(matches!(*jq.right, QueryExpr::Table(_)));
            assert_eq!(jq.return_.len(), 1);
        } else {
            panic!("Expected JoinQuery");
        }
    }

    #[test]
    fn test_vector_join_query_builder() {
        let vector = VectorQuery::new("embeddings", VectorSource::Literal(vec![0.1, 0.2]));
        let query = QueryExpr::table("docs")
            .alias("d")
            .join_vector(
                vector,
                JoinCondition::new(
                    FieldRef::column("d", "id"),
                    FieldRef::column("", "entity_id"),
                ),
            )
            .right_alias("sim")
            .return_field(FieldRef::column("d", "id"))
            .build();

        if let QueryExpr::Join(jq) = query {
            assert!(matches!(*jq.left, QueryExpr::Table(_)));
            match jq.right.as_ref() {
                QueryExpr::Vector(vq) => assert_eq!(vq.alias.as_deref(), Some("sim")),
                _ => panic!("Expected VectorQuery"),
            }
            assert_eq!(jq.return_.len(), 1);
        } else {
            panic!("Expected JoinQuery");
        }
    }

    #[test]
    fn test_path_join_query_builder() {
        let path = PathQuery::new(NodeSelector::by_id("host:a"), NodeSelector::by_id("host:b"))
            .via(GraphEdgeType::ConnectsTo);
        let query = QueryExpr::table("hosts")
            .alias("h")
            .join_path(
                path,
                JoinCondition::new(
                    FieldRef::column("h", "id"),
                    FieldRef::column("path", "entity_id"),
                ),
            )
            .right_alias("p")
            .return_field(FieldRef::column("h", "id"))
            .build();

        if let QueryExpr::Join(jq) = query {
            assert!(matches!(*jq.left, QueryExpr::Table(_)));
            match jq.right.as_ref() {
                QueryExpr::Path(pq) => assert_eq!(pq.alias.as_deref(), Some("p")),
                _ => panic!("Expected PathQuery"),
            }
            assert_eq!(jq.return_.len(), 1);
        } else {
            panic!("Expected JoinQuery");
        }
    }

    #[test]
    fn test_hybrid_join_query_builder() {
        let hybrid = HybridQuery::new(
            QueryExpr::table("hosts").build(),
            VectorQuery::new("embeddings", VectorSource::Literal(vec![0.1, 0.2])),
        );
        let query = QueryExpr::table("docs")
            .alias("d")
            .join_hybrid(
                hybrid,
                JoinCondition::new(
                    FieldRef::column("d", "id"),
                    FieldRef::column("", "entity_id"),
                ),
            )
            .right_alias("hy")
            .return_field(FieldRef::column("d", "id"))
            .build();

        if let QueryExpr::Join(jq) = query {
            assert!(matches!(*jq.left, QueryExpr::Table(_)));
            match jq.right.as_ref() {
                QueryExpr::Hybrid(hq) => assert_eq!(hq.alias.as_deref(), Some("hy")),
                _ => panic!("Expected HybridQuery"),
            }
            assert_eq!(jq.return_.len(), 1);
        } else {
            panic!("Expected JoinQuery");
        }
    }

    #[test]
    fn test_cte_builder() {
        // Build a query with a non-recursive CTE
        let inner_query = QueryExpr::table("hosts")
            .filter(Filter::compare(
                FieldRef::column("", "os"),
                CompareOp::Eq,
                Value::Text("Linux".to_string()),
            ))
            .build();

        let main_query = QueryExpr::table("linux_hosts").select("ip").build();

        let query_with_cte = CteQueryBuilder::new()
            .cte("linux_hosts", inner_query)
            .build(main_query);

        assert!(query_with_cte.with_clause.is_some());
        let with_clause = query_with_cte.with_clause.unwrap();
        assert_eq!(with_clause.ctes.len(), 1);
        assert_eq!(with_clause.ctes[0].name, "linux_hosts");
        assert!(!with_clause.ctes[0].recursive);
        assert!(!with_clause.has_recursive);
    }

    #[test]
    fn test_recursive_cte() {
        // Build a recursive CTE for hierarchical data
        let base_query = QueryExpr::table("hosts")
            .filter(Filter::compare(
                FieldRef::column("", "ip"),
                CompareOp::Eq,
                Value::Text("192.168.1.1".to_string()),
            ))
            .build();

        let main_query = QueryExpr::table("reachable").select("ip").build();

        let query_with_cte = CteQueryBuilder::new()
            .recursive_cte("reachable", base_query)
            .build(main_query);

        assert!(query_with_cte.with_clause.is_some());
        let with_clause = query_with_cte.with_clause.unwrap();
        assert!(with_clause.has_recursive);
        assert!(with_clause.ctes[0].recursive);
    }

    #[test]
    fn test_cte_with_columns() {
        let inner = QueryExpr::table("hosts").build();
        let main = QueryExpr::table("h").build();

        let cte =
            CteDefinition::new("h", inner).with_columns(vec!["id".to_string(), "name".to_string()]);

        assert_eq!(cte.columns.len(), 2);
        assert_eq!(cte.columns[0], "id");
        assert_eq!(cte.columns[1], "name");

        let query = QueryWithCte::with_ctes(WithClause::new().add(cte), main);
        assert!(query.with_clause.is_some());
    }
}
