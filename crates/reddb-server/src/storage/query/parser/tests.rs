//! Parser tests

use super::*;
use crate::storage::engine::vector_metadata::MetadataValue;
use crate::storage::query::ast::{
    CompareOp, DistanceMetric, EdgeDirection, FieldRef, Filter, FusionStrategy, JoinType,
    MetadataFilter, Projection, QueueCommand, TableQuery, TreeCommand, TreePosition, VectorSource,
};

/// Test helper that returns the legacy `QueryExpr` shape. The
/// top-level `parser::parse` returns `QueryWithCte` since #40; these
/// tests pre-date that change and don't exercise CTEs, so unwrap the
/// inner query for assertion ergonomics. CTE-specific tests should
/// call [`super::parse`] directly and inspect `with_clause`.
fn parse(input: &str) -> Result<QueryExpr, error::ParseError> {
    super::parse(input).map(|q| q.query)
}

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
fn test_parse_arithmetic_projection_sub() {
    // Regression: Fase 1.3 projection Pratt referenced Token::Minus
    // but the lexer emits Token::Dash. Subtraction silently fell
    // through to parse_field_ref and errored. Ensure `a - b` parses
    // into the nested Function("SUB", [col(a), col(b)]) form.
    let query = parse("SELECT a - b FROM t").unwrap();
    let QueryExpr::Table(tq) = query else {
        panic!("Expected TableQuery");
    };
    assert_eq!(tq.columns.len(), 1);
    let Projection::Function(name, args) = &tq.columns[0] else {
        panic!("expected arithmetic Function, got {:?}", tq.columns[0]);
    };
    assert_eq!(name, "SUB");
    assert_eq!(args.len(), 2);
}

#[test]
fn test_parse_arithmetic_projection_chain() {
    // `a - b - c` is left-associative → SUB(SUB(a,b), c).
    let query = parse("SELECT a - b - c FROM t").unwrap();
    let QueryExpr::Table(tq) = query else {
        panic!("Expected TableQuery");
    };
    let Projection::Function(name, args) = &tq.columns[0] else {
        panic!("expected Function");
    };
    assert_eq!(name, "SUB");
    // lhs should itself be a SUB
    assert!(matches!(&args[0], Projection::Function(n, _) if n == "SUB"));
}

#[test]
fn test_parse_cast_column_to_text() {
    let query = parse("SELECT CAST(age AS TEXT) FROM users").unwrap();
    let QueryExpr::Table(tq) = query else {
        panic!("Expected TableQuery");
    };
    assert_eq!(tq.columns.len(), 1);
    let Projection::Function(name, args) = &tq.columns[0] else {
        panic!("Expected Function, got {:?}", tq.columns[0]);
    };
    assert_eq!(name, "CAST");
    assert_eq!(args.len(), 2);
    assert!(
        matches!(&args[0], Projection::Column(c) if c == "age")
            || matches!(&args[0], Projection::Field(f, _) if matches!(f, FieldRef::TableColumn { column, .. } if column == "age"))
    );
    assert!(
        matches!(&args[1], Projection::Column(c) if c == "TYPE:TEXT")
            || matches!(&args[1], Projection::Field(f, _) if matches!(f, FieldRef::TableColumn { column, .. } if column == "TYPE:TEXT"))
    );
}

#[test]
fn test_parse_cast_with_alias() {
    let query = parse("SELECT CAST(score AS INT) AS score_int FROM matches").unwrap();
    let QueryExpr::Table(tq) = query else {
        panic!("Expected TableQuery");
    };
    let Projection::Function(name, args) = &tq.columns[0] else {
        panic!("Expected Function");
    };
    assert_eq!(name, "CAST:score_int");
    // Pratt path emits TYPE:INTEGER (DataType display); legacy path emits TYPE:INT (raw SQL name)
    assert!(
        matches!(&args[1], Projection::Column(c) if c == "TYPE:INT" || c == "TYPE:INTEGER"),
        "unexpected type arg: {:?}",
        &args[1]
    );
}

#[test]
fn test_parse_cast_literal_integer() {
    let query = parse("SELECT CAST(42 AS TEXT) FROM users").unwrap();
    let QueryExpr::Table(tq) = query else {
        panic!("Expected TableQuery");
    };
    let Projection::Function(name, args) = &tq.columns[0] else {
        panic!("Expected Function");
    };
    assert_eq!(name, "CAST");
    assert!(matches!(&args[0], Projection::Column(c) if c == "LIT:42"));
    assert!(matches!(&args[1], Projection::Column(c) if c == "TYPE:TEXT"));
}

#[test]
fn test_parse_money_scalar_function() {
    let query = parse("SELECT MONEY('BTC 0.125') FROM wallets").unwrap();
    let QueryExpr::Table(tq) = query else {
        panic!("Expected TableQuery");
    };
    let Projection::Function(name, args) = &tq.columns[0] else {
        panic!("Expected Function");
    };
    assert_eq!(name, "MONEY");
    assert_eq!(args.len(), 1);
}

#[test]
fn test_parse_between_with_column_bounds() {
    // BETWEEN where both bounds are columns decomposes into
    // AND(CompareFields(target, >=, low), CompareFields(target, <=, high)).
    let query = parse("SELECT * FROM sensors WHERE temp BETWEEN min_temp AND max_temp").unwrap();
    let QueryExpr::Table(tq) = query else {
        panic!("Expected TableQuery");
    };
    let Some(Filter::And(left, right)) = tq.filter else {
        panic!("Expected AND of CompareFields, got {:?}", tq.filter);
    };
    let Filter::CompareFields {
        op: op_lo,
        right: lo,
        ..
    } = &*left
    else {
        panic!("Expected CompareFields on lower bound");
    };
    let Filter::CompareFields {
        op: op_hi,
        right: hi,
        ..
    } = &*right
    else {
        panic!("Expected CompareFields on upper bound");
    };
    assert_eq!(*op_lo, CompareOp::Ge);
    assert_eq!(*op_hi, CompareOp::Le);
    let FieldRef::TableColumn { column: lo_col, .. } = lo else {
        panic!("lower bound not a column ref");
    };
    let FieldRef::TableColumn { column: hi_col, .. } = hi else {
        panic!("upper bound not a column ref");
    };
    assert_eq!(lo_col, "min_temp");
    assert_eq!(hi_col, "max_temp");
}

#[test]
fn test_parse_between_literal_bounds_preserved() {
    // Literal bounds still emit the classic Filter::Between form so
    // existing planner / executor paths are untouched.
    let query = parse("SELECT * FROM sensors WHERE temp BETWEEN 10 AND 20").unwrap();
    let QueryExpr::Table(tq) = query else {
        panic!("Expected TableQuery");
    };
    assert!(
        matches!(tq.filter, Some(Filter::Between { .. })),
        "literal BETWEEN must stay on the classic variant"
    );
}

#[test]
fn test_parse_between_mixed_bounds() {
    // Mixed: literal low + column high. Decomposes to
    // AND(Compare(field >= lit), CompareFields(field <= col)).
    let query = parse("SELECT * FROM sensors WHERE temp BETWEEN 0 AND max_temp").unwrap();
    let QueryExpr::Table(tq) = query else {
        panic!("Expected TableQuery");
    };
    let Some(Filter::And(left, right)) = tq.filter else {
        panic!("Expected AND for mixed bounds");
    };
    assert!(matches!(
        &*left,
        Filter::Compare {
            op: CompareOp::Ge,
            ..
        }
    ));
    assert!(matches!(
        &*right,
        Filter::CompareFields {
            op: CompareOp::Le,
            ..
        }
    ));
}

#[test]
fn test_parse_compare_rhs_bare_identifier_uses_compare_fields() {
    let query = parse("SELECT * FROM sensors WHERE temp = max_temp").unwrap();
    let QueryExpr::Table(tq) = query else {
        panic!("Expected TableQuery");
    };
    let Some(Filter::CompareFields { op, right, .. }) = tq.filter else {
        panic!("Expected CompareFields, got {:?}", tq.filter);
    };
    assert_eq!(op, CompareOp::Eq);
    let FieldRef::TableColumn { table, column } = right else {
        panic!("Expected table column rhs");
    };
    assert!(table.is_empty());
    assert_eq!(column, "max_temp");
}

#[test]
fn test_parse_compare_rhs_column_expression_stays_compare_expr() {
    let query = parse("SELECT * FROM sensors WHERE temp = max_temp + 1").unwrap();
    let QueryExpr::Table(tq) = query else {
        panic!("Expected TableQuery");
    };
    assert!(
        matches!(tq.filter, Some(Filter::CompareExpr { .. })),
        "column arithmetic rhs must stay on CompareExpr"
    );
}

#[test]
fn test_parse_cast_lowercase_keyword() {
    // keyword should be case-insensitive
    let query = parse("SELECT cast(age as float) FROM users").unwrap();
    let QueryExpr::Table(tq) = query else {
        panic!("Expected TableQuery");
    };
    let Projection::Function(name, args) = &tq.columns[0] else {
        panic!("Expected Function");
    };
    assert_eq!(name, "CAST");
    assert!(matches!(&args[1], Projection::Column(c) if c == "TYPE:FLOAT"));
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
        assert!(!tq.order_by[0].ascending);
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
fn test_parse_select_all_keyword_with_where() {
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
        assert_eq!(gq.pattern.nodes[0].node_label.as_deref(), Some("host"));
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
            gq.pattern.edges[0].edge_label.as_deref(),
            Some("has_service")
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
        assert_eq!(pq.via[0], "auth_access");
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
fn test_parse_join_with_all_keyword() {
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
fn test_parse_table_join_variants_and_from_subquery() {
    let query =
        parse("FROM users u RIGHT OUTER JOIN orders o ON u.id = o.user_id RETURN u.id, o.id")
            .unwrap();
    let QueryExpr::Join(join) = query else {
        panic!("Expected JoinQuery");
    };
    assert_eq!(join.join_type, JoinType::RightOuter);
    assert_eq!(join.return_.len(), 2);

    let query =
        parse("FROM users u FULL OUTER JOIN profiles p ON u.id = p.user_id WHERE p.id IS NOT NULL")
            .unwrap();
    let QueryExpr::Join(join) = query else {
        panic!("Expected JoinQuery");
    };
    assert_eq!(join.join_type, JoinType::FullOuter);
    assert!(join.filter.is_some());

    let query = parse("FROM users u CROSS JOIN regions r LIMIT 5 OFFSET 2").unwrap();
    let QueryExpr::Join(join) = query else {
        panic!("Expected JoinQuery");
    };
    assert_eq!(join.join_type, JoinType::Cross);
    assert_eq!(join.limit, Some(5));
    assert_eq!(join.offset, Some(2));

    let query = parse("FROM (SELECT id FROM users WHERE active = true) AS u RETURN u.id").unwrap();
    let QueryExpr::Table(table) = query else {
        panic!("Expected TableQuery");
    };
    assert_eq!(table.table, "__subq_u");
    assert_eq!(table.alias.as_deref(), Some("u"));
    assert_eq!(table.columns.len(), 1);
    assert!(table.source.is_some());

    assert!(parse("FROM (MATCH (n) RETURN n) AS g").is_err());
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
fn test_parse_vector_query_with_subquery() {
    let query = parse(
        "VECTOR SEARCH embeddings \
         SIMILAR TO (VECTOR SEARCH seeds SIMILAR TO [1.0, 0.0] LIMIT 1) \
         LIMIT 3",
    )
    .unwrap();

    if let QueryExpr::Vector(vq) = query {
        assert_eq!(vq.collection, "embeddings");
        assert_eq!(vq.k, 3);
        match vq.query_vector {
            VectorSource::Subquery(expr) => match *expr {
                QueryExpr::Vector(inner) => {
                    assert_eq!(inner.collection, "seeds");
                    assert_eq!(inner.k, 1);
                    match inner.query_vector {
                        VectorSource::Literal(vector) => assert_eq!(vector, vec![1.0, 0.0]),
                        other => panic!("expected literal inner vector, got {other:?}"),
                    }
                }
                other => panic!("expected inner vector query, got {other:?}"),
            },
            other => panic!("expected subquery source, got {other:?}"),
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
        assert!(iq.returning.is_none());
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
fn test_parse_insert_returning_star() {
    let query =
        parse("INSERT INTO hosts (ip, hostname) VALUES ('10.0.0.1', 'web01') RETURNING *").unwrap();
    if let QueryExpr::Insert(iq) = query {
        let items = iq.returning.as_ref().expect("RETURNING parsed");
        assert_eq!(items.len(), 1);
        assert!(matches!(
            items[0],
            crate::storage::query::ast::ReturningItem::All
        ));
    } else {
        panic!("Expected InsertQuery");
    }
}

#[test]
fn test_parse_insert_returning_columns() {
    let query = parse(
        "INSERT INTO hosts (ip, hostname) VALUES ('10.0.0.1', 'web01') RETURNING ip, hostname",
    )
    .unwrap();
    if let QueryExpr::Insert(iq) = query {
        let items = iq.returning.as_ref().expect("RETURNING parsed");
        assert_eq!(items.len(), 2);
        match &items[0] {
            crate::storage::query::ast::ReturningItem::Column(c) => assert_eq!(c, "ip"),
            other => panic!("expected column, got {other:?}"),
        }
        match &items[1] {
            crate::storage::query::ast::ReturningItem::Column(c) => assert_eq!(c, "hostname"),
            other => panic!("expected column, got {other:?}"),
        }
    } else {
        panic!("Expected InsertQuery");
    }
}

#[test]
fn test_parse_insert_bare_returning_errors() {
    let err = parse("INSERT INTO hosts (ip) VALUES ('10.0.0.1') RETURNING");
    assert!(err.is_err(), "bare RETURNING must require * or column list");
}

#[test]
fn test_parse_update_returning_star() {
    let query = parse("UPDATE hosts SET hostname = 'x' WHERE ip = '10.0.0.1' RETURNING *").unwrap();
    if let QueryExpr::Update(uq) = query {
        let items = uq.returning.as_ref().expect("RETURNING parsed");
        assert_eq!(items.len(), 1);
        assert!(matches!(
            items[0],
            crate::storage::query::ast::ReturningItem::All
        ));
    } else {
        panic!("Expected UpdateQuery");
    }
}

#[test]
fn test_parse_update_returning_columns() {
    let query =
        parse("UPDATE hosts SET hostname = 'x' WHERE id = 1 RETURNING id, hostname").unwrap();
    if let QueryExpr::Update(uq) = query {
        let items = uq.returning.as_ref().expect("RETURNING parsed");
        assert_eq!(items.len(), 2);
    } else {
        panic!("Expected UpdateQuery");
    }
}

#[test]
fn test_parse_delete_returning_star() {
    let query = parse("DELETE FROM hosts WHERE id = 1 RETURNING *").unwrap();
    if let QueryExpr::Delete(dq) = query {
        let items = dq.returning.as_ref().expect("RETURNING parsed");
        assert_eq!(items.len(), 1);
        assert!(matches!(
            items[0],
            crate::storage::query::ast::ReturningItem::All
        ));
    } else {
        panic!("Expected DeleteQuery");
    }
}

#[test]
fn test_parse_delete_returning_columns() {
    let query = parse("DELETE FROM hosts WHERE id = 1 RETURNING id, hostname").unwrap();
    if let QueryExpr::Delete(dq) = query {
        let items = dq.returning.as_ref().expect("RETURNING parsed");
        assert_eq!(items.len(), 2);
    } else {
        panic!("Expected DeleteQuery");
    }
}

#[test]
fn test_parse_top_level_kv_put_get_delete() {
    let query = parse("PUT session = 'abc' EXPIRE 2 m IF NOT EXISTS").unwrap();
    if let QueryExpr::Kv(crate::storage::query::ast::KvQuery::Put {
        key,
        value,
        ttl_ms,
        if_not_exists,
    }) = query
    {
        assert_eq!(key, "session");
        assert_eq!(value, crate::storage::schema::Value::text("abc"));
        assert_eq!(ttl_ms, Some(120_000));
        assert!(if_not_exists);
    } else {
        panic!("Expected Kv Put");
    }

    let query = parse("GET sessions.42").unwrap();
    if let QueryExpr::Kv(crate::storage::query::ast::KvQuery::Get { key }) = query {
        assert_eq!(key, "sessions.42");
    } else {
        panic!("Expected Kv Get");
    }

    let query = parse("DELETE sessions.42").unwrap();
    if let QueryExpr::Kv(crate::storage::query::ast::KvQuery::Delete { key }) = query {
        assert_eq!(key, "sessions.42");
    } else {
        panic!("Expected Kv Delete");
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
fn test_parse_insert_with_password_literal_constructor() {
    let query =
        parse("INSERT INTO accounts (username, pw) VALUES ('alice', PASSWORD('MyP@ss123'))")
            .unwrap();
    if let QueryExpr::Insert(iq) = query {
        assert_eq!(
            iq.values[0][1],
            crate::storage::schema::Value::Password("@@plain@@MyP@ss123".to_string())
        );
    } else {
        panic!("Expected InsertQuery");
    }
}

#[test]
fn test_parse_insert_with_secret_literal_constructor() {
    let query =
        parse("INSERT INTO creds (name, token) VALUES ('stripe', SECRET('sk_live_abc'))").unwrap();
    if let QueryExpr::Insert(iq) = query {
        assert_eq!(
            iq.values[0][1],
            crate::storage::schema::Value::Secret(b"@@plain@@sk_live_abc".to_vec())
        );
    } else {
        panic!("Expected InsertQuery");
    }
}

#[test]
fn test_parse_dml_extended_literals_auto_embed_and_ask_forms() {
    use crate::storage::query::ast::{Expr, ReturningItem};
    use crate::storage::schema::Value;

    let query = parse(
        "INSERT INTO docs (id, body) VALUES (1, 'hello') WITH AUTO EMBED (body) USING ollama MODEL 'nomic-embed-text'",
    )
    .unwrap();
    let QueryExpr::Insert(insert) = query else {
        panic!("Expected InsertQuery");
    };
    let auto_embed = insert.auto_embed.expect("auto embed config");
    assert_eq!(auto_embed.fields, vec!["body"]);
    assert_eq!(auto_embed.provider, "ollama");
    assert_eq!(auto_embed.model.as_deref(), Some("nomic-embed-text"));

    let query = parse("INSERT INTO docs (body) VALUES ('hello') WITH AUTO EMBED (body)").unwrap();
    let QueryExpr::Insert(insert) = query else {
        panic!("Expected InsertQuery");
    };
    let auto_embed = insert.auto_embed.expect("auto embed config");
    assert_eq!(auto_embed.provider, "openai");
    assert_eq!(auto_embed.model, None);

    let query = parse(
        "INSERT INTO payloads (tags, body) VALUES (['a', true, null], {'kind' = 'event', count: 2})",
    )
    .unwrap();
    let QueryExpr::Insert(insert) = query else {
        panic!("Expected InsertQuery");
    };
    assert!(matches!(insert.values[0][0], Value::Json(_)));
    assert!(matches!(insert.values[0][1], Value::Json(_)));

    let query =
        parse("INSERT INTO cache (key) VALUES ('k') WITH EXPIRES AT '1735689600000'").unwrap();
    let QueryExpr::Insert(insert) = query else {
        panic!("Expected InsertQuery");
    };
    assert_eq!(insert.expires_at_ms, Some(1_735_689_600_000));

    let query = parse(
        "INSERT INTO events (name) VALUES ('login') WITH TTL 42 WITH METADATA (empty = [], mixed = ['a', true, null, 3, 1.5], nested = {kind: 'event', attrs: {ok: true}}, raw = {\"x\":1})",
    )
    .unwrap();
    let QueryExpr::Insert(insert) = query else {
        panic!("Expected InsertQuery");
    };
    assert_eq!(insert.ttl_ms, Some(42_000));
    assert_eq!(insert.with_metadata.len(), 4);
    assert!(insert
        .with_metadata
        .iter()
        .all(|(_, value)| matches!(value, Value::Json(_))));

    let query =
        parse("UPDATE counters SET value = value + 1 WHERE id = 7 LIMIT 50 RETURNING id").unwrap();
    let QueryExpr::Update(update) = query else {
        panic!("Expected UpdateQuery");
    };
    assert_eq!(update.limit, Some(50));
    assert_eq!(update.assignment_exprs.len(), 1);
    assert!(update.assignments.is_empty());
    assert!(matches!(
        update.returning.as_deref(),
        Some([ReturningItem::Column(col)]) if col == "id"
    ));
    assert!(matches!(
        update.assignment_exprs[0].1,
        Expr::BinaryOp { .. } | Expr::FunctionCall { .. }
    ));

    let query =
        parse("ASK 'why did login fail?' USING openai MODEL 'gpt-4.1-mini' DEPTH 3 LIMIT 25 COLLECTION events")
            .unwrap();
    assert!(matches!(
        query,
        QueryExpr::Ask(ask)
            if ask.question == "why did login fail?"
                && ask.provider.as_deref() == Some("openai")
                && ask.model.as_deref() == Some("gpt-4.1-mini")
                && ask.depth == Some(3)
                && ask.limit == Some(25)
                && ask.collection.as_deref() == Some("events")
    ));

    for sql in [
        "INSERT INTO docs (body) VALUES ('x') WITH UNKNOWN",
        "INSERT INTO docs (body) VALUES ('x') WITH TTL 1 parsec",
        "INSERT INTO docs (body) VALUES ('x') WITH EXPIRES AT 'tomorrow'",
        "INSERT INTO docs (body) VALUES ('x') WITH EXPIRES AT true",
        "INSERT INTO docs (body) VALUES ('x') WITH METADATA (source)",
        "INSERT INTO docs (body) VALUES ('x') WITH AUTO EMBED ()",
        "INSERT INTO docs (body) VALUES ('x') WITH AUTO EMBED (body) USING",
        "ASK",
        "ASK 'q' MODEL gpt4",
        "ASK 'q' DEPTH",
        "ASK 'q' COLLECTION",
    ] {
        assert!(parse(sql).is_err(), "{sql}");
    }
}

#[test]
fn test_parse_dml_literal_value_array_and_object_branches() {
    use crate::storage::schema::Value;

    let mut parser = Parser::new("[1, 2.5]").unwrap();
    let value = parser.parse_literal_value().unwrap();
    assert!(matches!(value, Value::Vector(values) if values == vec![1.0, 2.5]));

    let mut parser = Parser::new("[]").unwrap();
    let value = parser.parse_literal_value().unwrap();
    let Value::Json(bytes) = value else {
        panic!("Expected Value::Json");
    };
    let parsed: crate::json::Value = crate::json::from_slice(&bytes).unwrap();
    assert!(parsed.as_array().is_some_and(|items| items.is_empty()));

    let mut parser = Parser::new("[PASSWORD('pw')]").unwrap();
    let value = parser.parse_literal_value().unwrap();
    let Value::Json(bytes) = value else {
        panic!("Expected Value::Json");
    };
    let parsed: crate::json::Value = crate::json::from_slice(&bytes).unwrap();
    assert_eq!(
        parsed.as_array().and_then(|items| items.first()),
        Some(&crate::json::Value::Null)
    );

    let mut parser = Parser::new(
        "{'quoted': 'text', ident = 1, type: true, nested: {ok: false}, raw: {\"x\":1}, secret: SECRET('s')}",
    )
    .unwrap();
    let value = parser.parse_literal_value().unwrap();
    let Value::Json(bytes) = value else {
        panic!("Expected Value::Json");
    };
    let parsed: crate::json::Value = crate::json::from_slice(&bytes).unwrap();
    assert_eq!(parsed.get("quoted").and_then(|v| v.as_str()), Some("text"));
    assert_eq!(parsed.get("ident").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(parsed.get("type").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        parsed
            .get("nested")
            .and_then(|v| v.get("ok"))
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        parsed
            .get("raw")
            .and_then(|v| v.get("x"))
            .and_then(|v| v.as_f64()),
        Some(1.0)
    );
    assert_eq!(parsed.get("secret"), Some(&crate::json::Value::Null));
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

#[test]
fn test_parse_set_secret() {
    let query = parse("SET SECRET mycompany.stripe.key = 'sk_live'").unwrap();
    if let QueryExpr::SetSecret { key, value } = query {
        assert_eq!(key, "mycompany.stripe.key");
        assert_eq!(value, crate::storage::schema::Value::Text("sk_live".into()));
    } else {
        panic!("Expected SetSecret");
    }
}

#[test]
fn test_parse_delete_secret() {
    let query = parse("DELETE SECRET mycompany.stripe.key").unwrap();
    if let QueryExpr::DeleteSecret { key } = query {
        assert_eq!(key, "mycompany.stripe.key");
    } else {
        panic!("Expected DeleteSecret");
    }
}

#[test]
fn test_parse_show_secret_prefix() {
    let query = parse("SHOW SECRET mycompany.stripe").unwrap();
    if let QueryExpr::ShowSecrets { prefix } = query {
        assert_eq!(prefix.as_deref(), Some("mycompany.stripe"));
    } else {
        panic!("Expected ShowSecrets");
    }
}

#[test]
fn test_parse_show_secrets_all() {
    let query = parse("SHOW SECRETS").unwrap();
    if let QueryExpr::ShowSecrets { prefix } = query {
        assert!(prefix.is_none());
    } else {
        panic!("Expected ShowSecrets");
    }
}

#[test]
fn test_parse_dollar_secret_reference_projection() {
    let query = parse("SELECT $secret.mycompany.stripe.key AS secret_value FROM tokens").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "tokens");
        assert_eq!(tq.columns.len(), 1);
        match &tq.columns[0] {
            Projection::Function(name, args) => {
                assert_eq!(name, "__SECRET_REF:secret_value");
                assert_eq!(args.len(), 1);
            }
            other => panic!("Expected secret projection function, got {other:?}"),
        }
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_dollar_secret_reference_filter() {
    let query = parse("SELECT id FROM tokens WHERE token = $red.secret.tokens.active").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "tokens");
        assert!(
            matches!(tq.filter, Some(Filter::CompareExpr { .. })),
            "secret reference RHS must parse as CompareExpr"
        );
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_dollar_config_reference_projection() {
    let query = parse("SELECT $red.config.ai.provider AS provider FROM tokens").unwrap();
    if let QueryExpr::Table(tq) = query {
        match &tq.columns[0] {
            Projection::Function(name, args) => {
                assert_eq!(name, "CONFIG:provider");
                assert_eq!(args.len(), 1);
            }
            other => panic!("Expected config projection function, got {other:?}"),
        }
    } else {
        panic!("Expected TableQuery");
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

#[test]
fn test_parse_ddl_table_options_types_and_alter_forms() {
    use crate::storage::query::ast::{AlterOperation, ExplainFormat, PartitionKind};

    let mut legacy_parser = Parser::new(
        "CREATE TABLE legacy (id INT, body TEXT) WITH CONTEXT INDEX ON (body) WITH TIMESTAMPS = true WITH TTL 2 h",
    )
    .unwrap();
    let legacy = legacy_parser.parse_create_table_query().unwrap();
    let QueryExpr::CreateTable(table) = legacy else {
        panic!("Expected CreateTableQuery");
    };
    assert_eq!(table.name, "legacy");
    assert_eq!(table.context_index_fields, vec!["body"]);
    assert!(table.context_index_enabled);
    assert!(table.timestamps);
    assert_eq!(table.default_ttl_ms, Some(7_200_000));

    let mut legacy_parser = Parser::new("DROP TABLE IF EXISTS legacy").unwrap();
    let legacy = legacy_parser.parse_drop_table_query().unwrap();
    assert!(matches!(
        legacy,
        QueryExpr::DropTable(drop) if drop.name == "legacy" && drop.if_exists
    ));

    let query = parse(
        "CREATE TABLE IF NOT EXISTS audit (\
         id DECIMAL(10) PRIMARY KEY, \
         tags ARRAY(TEXT), \
         active BOOLEAN DEFAULT = true, \
         score FLOAT DEFAULT = 1.5, \
         deleted_at TEXT DEFAULT = null\
         ) WITH (tenant_by = 'tenant_id', append_only = true, timestamps = true, context_index = false)",
    )
    .unwrap();
    let QueryExpr::CreateTable(table) = query else {
        panic!("Expected CreateTableQuery");
    };
    assert_eq!(table.name, "audit");
    assert!(table.if_not_exists);
    assert_eq!(table.tenant_by.as_deref(), Some("tenant_id"));
    assert!(table.append_only);
    assert!(table.timestamps);
    assert!(!table.context_index_enabled);
    assert_eq!(table.columns[0].decimal_precision, Some(10));
    assert_eq!(table.columns[1].array_element.as_deref(), Some("TEXT"));
    assert_eq!(table.columns[2].default.as_deref(), Some("true"));
    assert_eq!(table.columns[3].default.as_deref(), Some("1.5"));
    assert_eq!(table.columns[4].default.as_deref(), Some("null"));

    let query =
        parse("CREATE TABLE events (id INT) PARTITION BY HASH (id) APPEND ONLY TENANT BY (metadata.tenant)")
            .unwrap();
    let QueryExpr::CreateTable(table) = query else {
        panic!("Expected CreateTableQuery");
    };
    assert_eq!(
        table.partition_by.expect("partition").kind,
        PartitionKind::Hash
    );
    assert!(table.append_only);
    assert_eq!(table.tenant_by.as_deref(), Some("METADATA.tenant"));

    let query =
        parse("EXPLAIN ALTER FOR CREATE TABLE users (id INT, email TEXT NOT NULL) FORMAT JSON")
            .unwrap();
    assert!(matches!(
        query,
        QueryExpr::ExplainAlter(explain) if matches!(explain.format, ExplainFormat::Json)
            && explain.target.name == "users"
    ));

    let query = parse(
        "ALTER TABLE users ADD nickname TEXT DEFAULT = 'n/a', DROP old_name, RENAME nickname TO display_name",
    )
    .unwrap();
    let QueryExpr::AlterTable(alter) = query else {
        panic!("Expected AlterTableQuery");
    };
    assert_eq!(alter.operations.len(), 3);
    assert!(matches!(alter.operations[0], AlterOperation::AddColumn(_)));
    assert!(matches!(alter.operations[1], AlterOperation::DropColumn(_)));
    assert!(matches!(
        alter.operations[2],
        AlterOperation::RenameColumn { .. }
    ));

    let query = parse("ALTER TABLE users ENABLE TENANCY ON (metadata.tenant)").unwrap();
    assert!(matches!(
        query,
        QueryExpr::AlterTable(alter)
            if matches!(&alter.operations[0], AlterOperation::EnableTenancy { column } if column == "METADATA.tenant")
    ));

    let query = parse("ALTER TABLE users DISABLE TENANCY").unwrap();
    assert!(matches!(
        query,
        QueryExpr::AlterTable(alter)
            if matches!(alter.operations[0], AlterOperation::DisableTenancy)
    ));

    let query = parse("ALTER TABLE users SET APPEND_ONLY = false").unwrap();
    assert!(matches!(
        query,
        QueryExpr::AlterTable(alter)
            if matches!(alter.operations[0], AlterOperation::SetAppendOnly(false))
    ));

    let query = parse("ALTER TABLE users SET VERSIONED = true").unwrap();
    assert!(matches!(
        query,
        QueryExpr::AlterTable(alter)
            if matches!(alter.operations[0], AlterOperation::SetVersioned(true))
    ));

    for sql in [
        "CREATE TABLE bad (id INT NOT UNIQUE)",
        "CREATE TABLE bad (id INT DEFAULT = {})",
        "CREATE TABLE bad (id INT) WITH (tenant_by = 42)",
        "CREATE TABLE bad (id INT) WITH (append_only = maybe)",
        "CREATE TABLE bad (id INT) WITH unknown = true",
        "CREATE TABLE bad (id INT) WITH TTL 0.1 ms",
        "CREATE TABLE bad (id INT) WITH TTL 1 parsec",
        "CREATE TABLE bad (id INT) WITH TTL -1 s",
        "CREATE TABLE bad (id INT) PARTITION BY RANDOM (id)",
        "CREATE TABLE bad (id INT) APPEND SOMETIMES",
        "CREATE TABLE bad (id INT) TENANT id",
        "EXPLAIN ALTER FOR CREATE TABLE bad (id INT) FORMAT YAML",
        "ALTER TABLE users ENABLE TENANCY id",
        "ALTER TABLE users SET OTHER = true",
        "ALTER TABLE users UNKNOWN action",
    ] {
        assert!(parse(sql).is_err(), "{sql}");
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
fn test_parse_graph_shortest_path_astar() {
    let query = parse("GRAPH SHORTEST_PATH 'a' TO 'b' ALGORITHM astar").unwrap();
    if let QueryExpr::GraphCommand(crate::storage::query::ast::GraphCommand::ShortestPath {
        algorithm,
        ..
    }) = query
    {
        assert_eq!(algorithm, "astar");
    } else {
        panic!("Expected GraphCommand::ShortestPath");
    }
}

#[test]
fn test_parse_graph_shortest_path_bellman_ford() {
    let query = parse("GRAPH SHORTEST_PATH 'a' TO 'b' ALGORITHM bellman_ford").unwrap();
    if let QueryExpr::GraphCommand(crate::storage::query::ast::GraphCommand::ShortestPath {
        algorithm,
        ..
    }) = query
    {
        assert_eq!(algorithm, "bellman_ford");
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

#[test]
fn test_parse_graph_properties() {
    let query = parse("GRAPH PROPERTIES").unwrap();
    assert!(matches!(
        query,
        QueryExpr::GraphCommand(crate::storage::query::ast::GraphCommand::Properties)
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
fn test_parse_group_by_time_bucket() {
    let query = parse(
        "SELECT time_bucket(5m) AS bucket, avg(value) AS avg_value FROM cpu_metrics GROUP BY time_bucket(5m)",
    )
    .unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "cpu_metrics");
        assert_eq!(tq.group_by, vec!["TIME_BUCKET(5m)".to_string()]);
        assert_eq!(tq.columns.len(), 2);
        assert_eq!(
            tq.columns[0],
            Projection::Function(
                "TIME_BUCKET:bucket".to_string(),
                vec![Projection::Column("LIT:5m".to_string())]
            )
        );
        // Parser may emit Column("value") or Field({ column: "value" }) depending on version.
        let avg_col = match &tq.columns[1] {
            Projection::Function(name, args) if name == "AVG:avg_value" && args.len() == 1 => {
                &args[0]
            }
            other => panic!("Expected AVG:avg_value function, got {:?}", other),
        };
        assert!(
            matches!(avg_col, Projection::Column(c) if c == "value")
                || matches!(avg_col, Projection::Field(f, _) if matches!(f, FieldRef::TableColumn { column, .. } if column == "value")),
            "unexpected avg arg: {:?}",
            avg_col
        );
    } else {
        panic!("Expected TableQuery");
    }
}

#[test]
fn test_parse_keyword_field_reference_in_where() {
    let query = parse("SELECT metric FROM cpu_metrics WHERE metric = 'cpu.usage'").unwrap();
    if let QueryExpr::Table(tq) = query {
        assert_eq!(tq.table, "cpu_metrics");
        match tq.filter {
            Some(Filter::Compare { field, .. }) => match field {
                FieldRef::TableColumn { table, column } => {
                    assert!(table.is_empty());
                    assert_eq!(column, "metric");
                }
                other => panic!("expected table-column field ref, got {other:?}"),
            },
            other => panic!("expected compare filter, got {other:?}"),
        }
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
        assert!(ts.downsample_policies.is_empty());
    } else {
        panic!("Expected CreateTimeSeriesQuery");
    }
}

#[test]
fn test_parse_create_timeseries_with_downsample() {
    let query =
        parse("CREATE TIMESERIES cpu_metrics RETENTION 90 d DOWNSAMPLE 1h:5m:avg, 1d:1h:max")
            .unwrap();
    if let QueryExpr::CreateTimeSeries(ts) = query {
        assert_eq!(
            ts.downsample_policies,
            vec!["1h:5m:avg".to_string(), "1d:1h:max".to_string()]
        );
    } else {
        panic!("Expected CreateTimeSeriesQuery");
    }
}

#[test]
fn test_parse_timeseries_duration_downsample_and_hypertable_forms() {
    let query =
        parse("CREATE TIMESERIES IF NOT EXISTS metrics CHUNK_SIZE 2048 RETENTION 5 min").unwrap();
    let QueryExpr::CreateTimeSeries(ts) = query else {
        panic!("Expected CreateTimeSeriesQuery");
    };
    assert_eq!(ts.name, "metrics");
    assert!(ts.if_not_exists);
    assert_eq!(ts.chunk_size, Some(2048));
    assert_eq!(ts.retention_ms, Some(5 * 60_000));

    let query = parse("CREATE TIMESERIES metrics DOWNSAMPLE 1.5h:raw, 10m:1m:sum").unwrap();
    let QueryExpr::CreateTimeSeries(ts) = query else {
        panic!("Expected CreateTimeSeriesQuery");
    };
    assert_eq!(
        ts.downsample_policies,
        vec!["1.5h:raw:avg".to_string(), "10m:1m:sum".to_string()]
    );

    let query = parse(
        "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1d' TTL '2h' RETENTION 7 day",
    )
    .unwrap();
    let QueryExpr::CreateTimeSeries(ts) = query else {
        panic!("Expected CreateTimeSeriesQuery");
    };
    let hypertable = ts.hypertable.expect("hypertable ddl");
    assert_eq!(hypertable.time_column, "ts");
    assert_eq!(hypertable.chunk_interval_ns, 86_400_000_000_000);
    assert_eq!(hypertable.default_ttl_ns, Some(7_200_000_000_000));
    assert_eq!(ts.retention_ms, Some(7 * 86_400_000));

    let query = parse("DROP HYPERTABLE IF EXISTS metrics").unwrap();
    assert!(matches!(
        query,
        QueryExpr::DropTimeSeries(ts) if ts.name == "metrics" && ts.if_exists
    ));

    for sql in [
        "CREATE TIMESERIES metrics RETENTION 1 fortnights",
        "CREATE TIMESERIES metrics RETENTION 1 MAX",
        "CREATE TIMESERIES metrics RETENTION 1 AVG",
        "CREATE TIMESERIES metrics DOWNSAMPLE raw:",
        "CREATE HYPERTABLE metrics CHUNK_INTERVAL '1d'",
        "CREATE HYPERTABLE metrics TIME_COLUMN ts",
        "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL 42",
        "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL 'nope'",
    ] {
        assert!(parse(sql).is_err(), "{sql}");
    }
}

#[test]
fn test_parse_create_queue() {
    let query = parse("CREATE QUEUE tasks MAX_SIZE 1000 PRIORITY").unwrap();
    if let QueryExpr::CreateQueue(q) = query {
        assert_eq!(q.name, "tasks");
        assert_eq!(q.max_size, Some(1000));
        assert!(q.priority);
        assert_eq!(q.max_attempts, 3);
        assert_eq!(q.dlq, None);
    } else {
        panic!("Expected CreateQueueQuery");
    }
}

#[test]
fn test_parse_create_queue_with_dlq_and_attempts() {
    let query = parse("CREATE QUEUE tasks WITH DLQ failed_tasks MAX_ATTEMPTS 5").unwrap();
    if let QueryExpr::CreateQueue(q) = query {
        assert_eq!(q.name, "tasks");
        assert_eq!(q.dlq.as_deref(), Some("failed_tasks"));
        assert_eq!(q.max_attempts, 5);
    } else {
        panic!("Expected CreateQueueQuery");
    }
}

#[test]
fn test_parse_queue_control_and_group_command_forms() {
    use crate::storage::query::ast::QueueSide;

    let query = parse("CREATE QUEUE IF NOT EXISTS tasks WITH TTL 2.5 s").unwrap();
    assert!(matches!(
        query,
        QueryExpr::CreateQueue(q)
            if q.name == "tasks" && q.if_not_exists && q.ttl_ms == Some(2_500)
    ));

    for (sql, expected_ttl_ms) in [
        ("CREATE QUEUE q_ms WITH TTL 10 ms", 10),
        ("CREATE QUEUE q_min WITH TTL 2 mins", 120_000),
        ("CREATE QUEUE q_hr WITH TTL 3 hrs", 10_800_000),
        ("CREATE QUEUE q_day WITH TTL 1 day", 86_400_000),
        ("CREATE QUEUE q_default WITH TTL 4", 4_000),
    ] {
        let query = parse(sql).unwrap();
        assert!(matches!(
            query,
            QueryExpr::CreateQueue(q) if q.ttl_ms == Some(expected_ttl_ms)
        ));
    }

    let query = parse("CREATE QUEUE retries MAXATTEMPTS 0 MAXSIZE 10").unwrap();
    assert!(matches!(
        query,
        QueryExpr::CreateQueue(q)
            if q.max_attempts == 1 && q.max_size == Some(10)
    ));

    let query = parse("QUEUE PEEK tasks 5").unwrap();
    assert!(matches!(
        query,
        QueryExpr::QueueCommand(QueueCommand::Peek { queue, count })
            if queue == "tasks" && count == 5
    ));

    let query = parse("QUEUE PEEK tasks").unwrap();
    assert!(matches!(
        query,
        QueryExpr::QueueCommand(QueueCommand::Peek { queue, count })
            if queue == "tasks" && count == 1
    ));

    let query = parse("QUEUE LEN tasks").unwrap();
    assert!(matches!(
        query,
        QueryExpr::QueueCommand(QueueCommand::Len { queue }) if queue == "tasks"
    ));

    let query = parse("QUEUE PURGE tasks").unwrap();
    assert!(matches!(
        query,
        QueryExpr::QueueCommand(QueueCommand::Purge { queue }) if queue == "tasks"
    ));

    let query = parse("QUEUE GROUP CREATE tasks workers").unwrap();
    assert!(matches!(
        query,
        QueryExpr::QueueCommand(QueueCommand::GroupCreate { queue, group })
            if queue == "tasks" && group == "workers"
    ));

    let query = parse("QUEUE READ tasks GROUP workers CONSUMER worker1 COUNT 10").unwrap();
    assert!(matches!(
        query,
        QueryExpr::QueueCommand(QueueCommand::GroupRead {
            queue,
            group,
            consumer,
            count,
        }) if queue == "tasks" && group == "workers" && consumer == "worker1" && count == 10
    ));

    let query = parse("QUEUE ACK tasks GROUP workers 'msg-1'").unwrap();
    assert!(matches!(
        query,
        QueryExpr::QueueCommand(QueueCommand::Ack {
            queue,
            group,
            message_id,
        }) if queue == "tasks" && group == "workers" && message_id == "msg-1"
    ));

    let query = parse("QUEUE NACK tasks GROUP workers 'msg-2'").unwrap();
    assert!(matches!(
        query,
        QueryExpr::QueueCommand(QueueCommand::Nack {
            queue,
            group,
            message_id,
        }) if queue == "tasks" && group == "workers" && message_id == "msg-2"
    ));

    let query = parse("QUEUE RPUSH tasks 'right' PRIORITY 9").unwrap();
    assert!(matches!(
        query,
        QueryExpr::QueueCommand(QueueCommand::Push {
            queue,
            side: QueueSide::Right,
            priority: Some(9),
            ..
        }) if queue == "tasks"
    ));

    let query = parse("QUEUE POP tasks COUNT 3").unwrap();
    assert!(matches!(
        query,
        QueryExpr::QueueCommand(QueueCommand::Pop {
            queue,
            side: QueueSide::Left,
            count: 3,
        }) if queue == "tasks"
    ));

    let query = parse("DROP QUEUE tasks").unwrap();
    assert!(matches!(
        query,
        QueryExpr::DropQueue(q) if q.name == "tasks" && !q.if_exists
    ));

    for sql in [
        "CREATE QUEUE q WITH TTL 5 fortnights",
        "QUEUE READ tasks workers CONSUMER worker1",
        "QUEUE CLAIM tasks GROUP workers CONSUMER worker1",
        "QUEUE CLAIM tasks GROUP workers MIN_IDLE 1",
        "QUEUE ACK tasks GROUP workers",
        "QUEUE UNKNOWN tasks",
    ] {
        assert!(parse(sql).is_err(), "{sql}");
    }
}

#[test]
fn test_parse_queue_push() {
    let query = parse("QUEUE PUSH tasks 'hello world'").unwrap();
    if let QueryExpr::QueueCommand(QueueCommand::Push { queue, value, .. }) = query {
        assert_eq!(queue, "tasks");
        assert_eq!(
            value,
            crate::storage::schema::Value::text("hello world".to_string())
        );
    } else {
        panic!("Expected QueueCommand::Push");
    }
}

#[test]
fn test_parse_queue_push_inline_json_literal() {
    let query = parse("QUEUE PUSH tasks {job: 'hello', retries: 3}").unwrap();
    if let QueryExpr::QueueCommand(QueueCommand::Push { queue, value, .. }) = query {
        assert_eq!(queue, "tasks");
        match value {
            crate::storage::schema::Value::Json(bytes) => {
                let json: crate::json::Value =
                    crate::json::from_slice(&bytes).expect("queue payload json should decode");
                assert_eq!(
                    json.get("job").and_then(crate::json::Value::as_str),
                    Some("hello")
                );
                assert_eq!(
                    json.get("retries").and_then(crate::json::Value::as_i64),
                    Some(3)
                );
            }
            other => panic!("Expected JSON queue payload, got {other:?}"),
        }
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
fn test_parse_queue_alias_sides() {
    let lpush = parse("QUEUE LPUSH tasks 'left'").unwrap();
    if let QueryExpr::QueueCommand(QueueCommand::Push { side, .. }) = lpush {
        assert_eq!(side, crate::storage::query::ast::QueueSide::Left);
    } else {
        panic!("Expected QueueCommand::Push");
    }

    let rpush = parse("QUEUE RPUSH tasks 'right'").unwrap();
    if let QueryExpr::QueueCommand(QueueCommand::Push { side, .. }) = rpush {
        assert_eq!(side, crate::storage::query::ast::QueueSide::Right);
    } else {
        panic!("Expected QueueCommand::Push");
    }

    let lpop = parse("QUEUE LPOP tasks").unwrap();
    if let QueryExpr::QueueCommand(QueueCommand::Pop { side, .. }) = lpop {
        assert_eq!(side, crate::storage::query::ast::QueueSide::Left);
    } else {
        panic!("Expected QueueCommand::Pop");
    }

    let rpop = parse("QUEUE RPOP tasks").unwrap();
    if let QueryExpr::QueueCommand(QueueCommand::Pop { side, .. }) = rpop {
        assert_eq!(side, crate::storage::query::ast::QueueSide::Right);
    } else {
        panic!("Expected QueueCommand::Pop");
    }
}

#[test]
fn test_parse_queue_pending() {
    let query = parse("QUEUE PENDING tasks GROUP workers").unwrap();
    if let QueryExpr::QueueCommand(QueueCommand::Pending { queue, group }) = query {
        assert_eq!(queue, "tasks");
        assert_eq!(group, "workers");
    } else {
        panic!("Expected QueueCommand::Pending");
    }
}

#[test]
fn test_parse_queue_claim() {
    let query = parse("QUEUE CLAIM tasks GROUP workers CONSUMER worker2 MIN_IDLE 60000").unwrap();
    if let QueryExpr::QueueCommand(QueueCommand::Claim {
        queue,
        group,
        consumer,
        min_idle_ms,
    }) = query
    {
        assert_eq!(queue, "tasks");
        assert_eq!(group, "workers");
        assert_eq!(consumer, "worker2");
        assert_eq!(min_idle_ms, 60000);
    } else {
        panic!("Expected QueueCommand::Claim");
    }
}

#[test]
fn test_parse_create_tree() {
    let query = parse(
        "CREATE TREE org IN forest ROOT LABEL company TYPE root PROPERTIES {name: 'Acme'} MAX_CHILDREN 3",
    )
    .unwrap();
    if let QueryExpr::CreateTree(tree) = query {
        assert_eq!(tree.name, "org");
        assert_eq!(tree.collection, "forest");
        assert_eq!(tree.root.label, "company");
        assert_eq!(tree.root.node_type.as_deref(), Some("root"));
        assert_eq!(tree.default_max_children, 3);
        assert_eq!(tree.root.properties.len(), 1);
        assert_eq!(tree.root.properties[0].0, "name");
    } else {
        panic!("Expected CreateTreeQuery");
    }
}

#[test]
fn test_parse_tree_insert() {
    let query = parse(
        "TREE INSERT INTO forest.org PARENT 42 LABEL team TYPE branch PROPERTIES {name: 'A'} METADATA {owner: 'ops'} MAX_CHILDREN 5 POSITION FIRST",
    )
    .unwrap();
    if let QueryExpr::TreeCommand(TreeCommand::Insert {
        collection,
        tree_name,
        parent_id,
        node,
        position,
    }) = query
    {
        assert_eq!(collection, "forest");
        assert_eq!(tree_name, "org");
        assert_eq!(parent_id, 42);
        assert_eq!(node.label, "team");
        assert_eq!(node.node_type.as_deref(), Some("branch"));
        assert_eq!(node.max_children, Some(5));
        assert_eq!(node.properties.len(), 1);
        assert_eq!(node.metadata.len(), 1);
        assert_eq!(position, TreePosition::First);
    } else {
        panic!("Expected TreeCommand::Insert");
    }
}

#[test]
fn test_parse_tree_rebalance_dry_run() {
    let query = parse("TREE REBALANCE forest.org DRY RUN").unwrap();
    if let QueryExpr::TreeCommand(TreeCommand::Rebalance {
        collection,
        tree_name,
        dry_run,
    }) = query
    {
        assert_eq!(collection, "forest");
        assert_eq!(tree_name, "org");
        assert!(dry_run);
    } else {
        panic!("Expected TreeCommand::Rebalance");
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

#[test]
fn test_parse_auth_grant_revoke_and_alter_user_forms() {
    use crate::storage::query::ast::{AlterUserAttribute, GrantObjectKind, GrantPrincipalRef};

    let query =
        parse("GRANT ALL PRIVILEGES ON DATABASE maindb TO PUBLIC WITH GRANT OPTION").unwrap();
    let QueryExpr::Grant(grant) = query else {
        panic!("Expected Grant");
    };
    assert!(grant.all);
    assert_eq!(grant.actions, vec!["ALL"]);
    assert_eq!(grant.object_kind, GrantObjectKind::Database);
    assert_eq!(grant.objects[0].name, "maindb");
    assert!(matches!(grant.principals[0], GrantPrincipalRef::Public));
    assert!(grant.with_grant_option);

    let query =
        parse("GRANT SELECT, UPDATE (name, email) ON TABLE public.users TO tenant1.alice").unwrap();
    let QueryExpr::Grant(grant) = query else {
        panic!("Expected Grant");
    };
    assert_eq!(grant.actions, vec!["SELECT", "UPDATE"]);
    assert_eq!(
        grant.columns.as_deref(),
        Some(&["name".to_string(), "email".to_string()][..])
    );
    assert_eq!(grant.object_kind, GrantObjectKind::Table);
    assert_eq!(grant.objects[0].schema.as_deref(), Some("public"));
    assert_eq!(grant.objects[0].name, "users");
    assert!(matches!(
        &grant.principals[0],
        GrantPrincipalRef::User { tenant: Some(t), name } if t == "tenant1" && name == "alice"
    ));

    let query = parse("REVOKE GRANT OPTION FOR USAGE ON SCHEMA analytics FROM analysts").unwrap();
    let QueryExpr::Revoke(revoke) = query else {
        panic!("Expected Revoke");
    };
    assert!(revoke.grant_option_for);
    assert_eq!(revoke.actions, vec!["USAGE"]);
    assert_eq!(revoke.object_kind, GrantObjectKind::Schema);
    assert!(matches!(
        &revoke.principals[0],
        GrantPrincipalRef::User { tenant: None, name } if name == "analysts"
    ));

    let query = parse(
        "ALTER USER tenant1.alice VALID UNTIL '2030-01-01' CONNECTION LIMIT 5 DISABLE SET search_path = 'public,analytics' ADD GROUP analysts DROP GROUP temp PASSWORD 'newpw'",
    )
    .unwrap();
    let QueryExpr::AlterUser(stmt) = query else {
        panic!("Expected AlterUser");
    };
    assert_eq!(stmt.tenant.as_deref(), Some("tenant1"));
    assert_eq!(stmt.username, "alice");
    assert!(
        matches!(stmt.attributes[0], AlterUserAttribute::ValidUntil(ref s) if s == "2030-01-01")
    );
    assert!(matches!(
        stmt.attributes[1],
        AlterUserAttribute::ConnectionLimit(5)
    ));
    assert!(matches!(stmt.attributes[2], AlterUserAttribute::Disable));
    assert!(
        matches!(stmt.attributes[3], AlterUserAttribute::SetSearchPath(ref s) if s == "public,analytics")
    );
    assert!(matches!(stmt.attributes[4], AlterUserAttribute::AddGroup(ref g) if g == "analysts"));
    assert!(matches!(stmt.attributes[5], AlterUserAttribute::DropGroup(ref g) if g == "temp"));
    assert!(matches!(stmt.attributes[6], AlterUserAttribute::Password(ref p) if p == "newpw"));

    let query = parse(
        "GRANT INSERT, DELETE, TRUNCATE, REFERENCES, EXECUTE ON FUNCTION public.recalc TO GROUP analysts, bob",
    )
    .unwrap();
    let QueryExpr::Grant(grant) = query else {
        panic!("Expected Grant");
    };
    assert_eq!(
        grant.actions,
        vec!["INSERT", "DELETE", "TRUNCATE", "REFERENCES", "EXECUTE"]
    );
    assert_eq!(grant.object_kind, GrantObjectKind::Function);
    assert_eq!(grant.objects[0].schema.as_deref(), Some("public"));
    assert!(matches!(
        &grant.principals[0],
        GrantPrincipalRef::Group(group) if group == "analysts"
    ));
    assert!(matches!(
        &grant.principals[1],
        GrantPrincipalRef::User { tenant: None, name } if name == "bob"
    ));

    let query = parse("REVOKE ALL ON public.users, audit.events FROM GROUP analysts").unwrap();
    let QueryExpr::Revoke(revoke) = query else {
        panic!("Expected Revoke");
    };
    assert!(revoke.all);
    assert_eq!(revoke.object_kind, GrantObjectKind::Table);
    assert_eq!(revoke.objects.len(), 2);
    assert!(matches!(
        &revoke.principals[0],
        GrantPrincipalRef::Group(group) if group == "analysts"
    ));

    let query = parse("ALTER USER bob ENABLE SET search_path TO 'public'").unwrap();
    let QueryExpr::AlterUser(stmt) = query else {
        panic!("Expected AlterUser");
    };
    assert_eq!(stmt.tenant, None);
    assert_eq!(stmt.username, "bob");
    assert!(matches!(stmt.attributes[0], AlterUserAttribute::Enable));
    assert!(
        matches!(stmt.attributes[1], AlterUserAttribute::SetSearchPath(ref s) if s == "public")
    );
}

#[test]
fn test_parse_iam_policy_forms() {
    use crate::storage::query::ast::PolicyPrincipalRef;

    let query = parse("CREATE POLICY 'readonly' AS '{\"Statement\":[]}'").unwrap();
    assert!(matches!(
        query,
        QueryExpr::CreateIamPolicy { ref id, ref json }
            if id == "readonly" && json == "{\"Statement\":[]}"
    ));

    let query = parse("DROP POLICY 'readonly'").unwrap();
    assert!(matches!(query, QueryExpr::DropIamPolicy { ref id } if id == "readonly"));

    let query = parse("ATTACH POLICY 'readonly' TO USER tenant1.alice").unwrap();
    assert!(matches!(
        query,
        QueryExpr::AttachPolicy {
            ref policy_id,
            principal: PolicyPrincipalRef::User(ref user),
        } if policy_id == "readonly"
            && user.tenant.as_deref() == Some("tenant1")
            && user.username == "alice"
    ));

    let query = parse("DETACH POLICY 'readonly' FROM GROUP analysts").unwrap();
    assert!(matches!(
        query,
        QueryExpr::DetachPolicy {
            ref policy_id,
            principal: PolicyPrincipalRef::Group(ref group),
        } if policy_id == "readonly" && group == "analysts"
    ));

    let query = parse("SHOW POLICIES FOR GROUP analysts").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ShowPolicies {
            filter: Some(PolicyPrincipalRef::Group(ref group)),
        } if group == "analysts"
    ));

    let query =
        parse("SHOW EFFECTIVE PERMISSIONS FOR tenant1.alice ON table:public.orders").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ShowEffectivePermissions {
            ref user,
            resource: Some(ref resource),
        } if user.tenant.as_deref() == Some("tenant1")
            && user.username == "alice"
            && resource.kind == "table"
            && resource.name == "public.orders"
    ));

    let query = parse("SIMULATE tenant1.alice ACTION SELECT ON 'table:public.orders'").unwrap();
    assert!(matches!(
        query,
        QueryExpr::SimulatePolicy {
            ref user,
            ref action,
            ref resource,
        } if user.tenant.as_deref() == Some("tenant1")
            && user.username == "alice"
            && action == "select"
            && resource.kind == "table"
            && resource.name == "public.orders"
    ));

    let query = parse("SHOW POLICIES").unwrap();
    assert!(matches!(query, QueryExpr::ShowPolicies { filter: None }));

    let query = parse("SHOW POLICIES FOR USER alice").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ShowPolicies {
            filter: Some(PolicyPrincipalRef::User(ref user)),
        } if user.tenant.is_none() && user.username == "alice"
    ));

    let query = parse("SHOW EFFECTIVE PERMISSIONS FOR alice").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ShowEffectivePermissions {
            ref user,
            resource: None,
        } if user.tenant.is_none() && user.username == "alice"
    ));

    let query = parse("SIMULATE alice ACTION 'iam:PassRole' ON TABLE:public.orders").unwrap();
    assert!(matches!(
        query,
        QueryExpr::SimulatePolicy {
            ref user,
            ref action,
            ref resource,
        } if user.tenant.is_none()
            && user.username == "alice"
            && action == "iam:PassRole"
            && resource.kind == "table"
            && resource.name == "public.orders"
    ));

    for sql in [
        "ATTACH ROLE 'readonly' TO USER alice",
        "DETACH POLICY 'readonly' TO USER alice",
        "SHOW EFFECTIVE ACTIONS FOR alice",
        "SIMULATE alice ON SELECT ON table:public.orders",
        "SIMULATE alice ACTION SELECT ON 'missing-colon'",
    ] {
        assert!(parse(sql).is_err(), "{sql}");
    }
}

#[test]
fn test_parse_auth_error_forms() {
    for sql in [
        "GRANT UNKNOWN ON TABLE users TO alice",
        "GRANT SELECT users TO alice",
        "GRANT SELECT ON TABLE users TO GROUP",
        "GRANT SELECT ON TABLE users TO alice WITH OPTION",
        "GRANT SELECT ON TABLE users TO alice WITH GRANT",
        "REVOKE GRANT SELECT ON TABLE users FROM alice",
        "REVOKE GRANT OPTION SELECT ON TABLE users FROM alice",
        "ALTER USER alice",
        "ALTER USER alice VALID '2030-01-01'",
        "ALTER USER alice CONNECTION 5",
        "ALTER USER alice SET timezone = 'UTC'",
        "ALTER USER alice SET search_path 'public'",
        "ALTER USER alice ADD ROLE analysts",
        "ALTER USER alice DROP ROLE analysts",
        "ATTACH POLICY 'readonly' TO ROLE analysts",
        "SIMULATE alice ACTION SELECT ON table public.orders",
        "SIMULATE alice ACTION 42 ON table:public.orders",
    ] {
        assert!(parse(sql).is_err(), "{sql}");
    }
}

#[test]
fn test_parse_migration_forms() {
    use crate::storage::query::ast::ApplyMigrationTarget;

    let query = parse(
        "CREATE MIGRATION m2 DEPENDS ON m0, m1 BATCH 500 ROWS NO ROLLBACK AS CREATE TABLE accounts (id INTEGER)",
    )
    .unwrap();
    let QueryExpr::CreateMigration(migration) = query else {
        panic!("Expected CreateMigration");
    };
    assert_eq!(migration.name, "m2");
    assert_eq!(migration.depends_on, vec!["m0", "m1"]);
    assert_eq!(migration.batch_size, Some(500));
    assert!(migration.no_rollback);
    assert_eq!(migration.body, "CREATE TABLE accounts ( id INTEGER )");

    let query = parse("APPLY MIGRATION * FOR TENANT tenant1").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ApplyMigration(apply)
            if matches!(apply.target, ApplyMigrationTarget::All)
                && apply.for_tenant.as_deref() == Some("tenant1")
    ));

    let query = parse("ROLLBACK MIGRATION m2").unwrap();
    assert!(matches!(query, QueryExpr::RollbackMigration(r) if r.name == "m2"));

    let query = parse("EXPLAIN MIGRATION m2").unwrap();
    assert!(matches!(query, QueryExpr::ExplainMigration(e) if e.name == "m2"));

    let query = parse("APPLY MIGRATION m2 FOR TENANT 'tenant-string'").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ApplyMigration(apply)
            if matches!(apply.target, ApplyMigrationTarget::Named(ref name) if name == "m2")
                && apply.for_tenant.as_deref() == Some("tenant-string")
    ));

    let query = parse("CREATE MIGRATION m3 CREATE INDEX idx ON accounts (id)").unwrap();
    assert!(matches!(
        query,
        QueryExpr::CreateMigration(migration)
            if migration.name == "m3"
                && migration.depends_on.is_empty()
                && migration.batch_size.is_none()
                && !migration.no_rollback
                && migration.body == "CREATE INDEX idx ON accounts ( id )"
    ));

    for sql in [
        "APPLY m2",
        "APPLY MIGRATION * FOR m2",
        "APPLY MIGRATION * FOR TENANT 42",
        "CREATE MIGRATION m4 DEPENDS m3 AS SELECT 1",
    ] {
        assert!(parse(sql).is_err(), "{sql}");
    }
}

#[test]
fn test_parse_probabilistic_command_matrix() {
    use crate::storage::query::ast::ProbabilisticCommand;

    let query = parse("CREATE HLL IF NOT EXISTS visitors").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::CreateHll {
            ref name,
            if_not_exists: true,
        }) if name == "visitors"
    ));

    let query = parse("HLL COUNT visitors signups").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::HllCount { ref names })
            if names == &vec!["visitors".to_string(), "signups".to_string()]
    ));

    let query = parse("HLL MERGE all_visitors visitors signups").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::HllMerge {
            ref dest,
            ref sources,
        }) if dest == "all_visitors"
            && sources == &vec!["visitors".to_string(), "signups".to_string()]
    ));

    let query = parse("HLL INFO visitors").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::HllInfo { ref name })
            if name == "visitors"
    ));

    let query = parse("DROP HLL IF EXISTS visitors").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::DropHll {
            ref name,
            if_exists: true,
        }) if name == "visitors"
    ));

    let query = parse("CREATE SKETCH IF NOT EXISTS freqs WIDTH 2048 DEPTH 7").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::CreateSketch {
            ref name,
            width: 2048,
            depth: 7,
            if_not_exists: true,
        }) if name == "freqs"
    ));

    let query = parse("SKETCH ADD freqs 'red' 3").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::SketchAdd {
            ref name,
            ref element,
            count: 3,
        }) if name == "freqs" && element == "red"
    ));

    let query = parse("SKETCH ADD freqs 'blue'").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::SketchAdd {
            ref name,
            ref element,
            count: 1,
        }) if name == "freqs" && element == "blue"
    ));

    let query = parse("SKETCH COUNT freqs 'red'").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::SketchCount {
            ref name,
            ref element,
        }) if name == "freqs" && element == "red"
    ));

    let query = parse("SKETCH MERGE total freqs other_freqs").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::SketchMerge {
            ref dest,
            ref sources,
        }) if dest == "total" && sources == &vec!["freqs".to_string(), "other_freqs".to_string()]
    ));

    let query = parse("SKETCH INFO freqs").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::SketchInfo { ref name })
            if name == "freqs"
    ));

    let query = parse("DROP SKETCH IF EXISTS freqs").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::DropSketch {
            ref name,
            if_exists: true,
        }) if name == "freqs"
    ));

    let query = parse("CREATE FILTER IF NOT EXISTS seen CAPACITY 4096").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::CreateFilter {
            ref name,
            capacity: 4096,
            if_not_exists: true,
        }) if name == "seen"
    ));

    let query = parse("FILTER ADD seen 'a'").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::FilterAdd {
            ref name,
            ref element,
        }) if name == "seen" && element == "a"
    ));

    let query = parse("FILTER CHECK seen 'a'").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::FilterCheck {
            ref name,
            ref element,
        }) if name == "seen" && element == "a"
    ));

    let query = parse("FILTER DELETE seen 'a'").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::FilterDelete {
            ref name,
            ref element,
        }) if name == "seen" && element == "a"
    ));

    let query = parse("FILTER COUNT seen").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::FilterCount { ref name })
            if name == "seen"
    ));

    let query = parse("FILTER INFO seen").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::FilterInfo { ref name })
            if name == "seen"
    ));

    let query = parse("DROP FILTER IF EXISTS seen").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::DropFilter {
            ref name,
            if_exists: true,
        }) if name == "seen"
    ));

    let query = parse("CREATE HLL visitors2").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::CreateHll {
            ref name,
            if_not_exists: false,
        }) if name == "visitors2"
    ));

    let query = parse("CREATE SKETCH freqs2 DEPTH 3 WIDTH 512").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::CreateSketch {
            ref name,
            width: 512,
            depth: 3,
            if_not_exists: false,
        }) if name == "freqs2"
    ));

    let query = parse("CREATE FILTER seen2").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::CreateFilter {
            ref name,
            capacity: 100_000,
            if_not_exists: false,
        }) if name == "seen2"
    ));

    let query = parse("DROP FILTER seen2").unwrap();
    assert!(matches!(
        query,
        QueryExpr::ProbabilisticCommand(ProbabilisticCommand::DropFilter {
            ref name,
            if_exists: false,
        }) if name == "seen2"
    ));

    for sql in [
        "HLL DESCRIBE visitors",
        "SKETCH DESCRIBE freqs",
        "FILTER DESCRIBE seen",
        "CREATE BLOOM seen",
        "DROP BLOOM seen",
    ] {
        assert!(parse(sql).is_err(), "{sql}");
    }
}

#[test]
fn test_parse_transaction_control() {
    use crate::storage::query::ast::TxnControl;

    // BEGIN (bare)
    assert!(matches!(
        parse("BEGIN").unwrap(),
        QueryExpr::TransactionControl(TxnControl::Begin)
    ));
    // BEGIN WORK
    assert!(matches!(
        parse("BEGIN WORK").unwrap(),
        QueryExpr::TransactionControl(TxnControl::Begin)
    ));
    // BEGIN TRANSACTION
    assert!(matches!(
        parse("BEGIN TRANSACTION").unwrap(),
        QueryExpr::TransactionControl(TxnControl::Begin)
    ));
    // START TRANSACTION
    assert!(matches!(
        parse("START TRANSACTION").unwrap(),
        QueryExpr::TransactionControl(TxnControl::Begin)
    ));

    // COMMIT + COMMIT WORK + COMMIT TRANSACTION
    for s in ["COMMIT", "COMMIT WORK", "COMMIT TRANSACTION"] {
        assert!(
            matches!(
                parse(s).unwrap(),
                QueryExpr::TransactionControl(TxnControl::Commit)
            ),
            "failed for {s}"
        );
    }

    // ROLLBACK + ROLLBACK WORK + ROLLBACK TRANSACTION
    for s in ["ROLLBACK", "ROLLBACK WORK", "ROLLBACK TRANSACTION"] {
        assert!(
            matches!(
                parse(s).unwrap(),
                QueryExpr::TransactionControl(TxnControl::Rollback)
            ),
            "failed for {s}"
        );
    }

    // SAVEPOINT name
    if let QueryExpr::TransactionControl(TxnControl::Savepoint(name)) =
        parse("SAVEPOINT sp1").unwrap()
    {
        assert_eq!(name, "sp1");
    } else {
        panic!("Expected Savepoint");
    }

    // RELEASE SAVEPOINT name
    if let QueryExpr::TransactionControl(TxnControl::ReleaseSavepoint(name)) =
        parse("RELEASE SAVEPOINT sp1").unwrap()
    {
        assert_eq!(name, "sp1");
    } else {
        panic!("Expected ReleaseSavepoint");
    }
    // RELEASE name (without SAVEPOINT keyword — PG accepts both)
    if let QueryExpr::TransactionControl(TxnControl::ReleaseSavepoint(name)) =
        parse("RELEASE sp2").unwrap()
    {
        assert_eq!(name, "sp2");
    } else {
        panic!("Expected ReleaseSavepoint");
    }

    // ROLLBACK TO SAVEPOINT name
    if let QueryExpr::TransactionControl(TxnControl::RollbackToSavepoint(name)) =
        parse("ROLLBACK TO SAVEPOINT sp1").unwrap()
    {
        assert_eq!(name, "sp1");
    } else {
        panic!("Expected RollbackToSavepoint");
    }
    // ROLLBACK TO name (without SAVEPOINT keyword)
    if let QueryExpr::TransactionControl(TxnControl::RollbackToSavepoint(name)) =
        parse("ROLLBACK TO sp3").unwrap()
    {
        assert_eq!(name, "sp3");
    } else {
        panic!("Expected RollbackToSavepoint");
    }
}

#[test]
fn test_parse_maintenance_commands() {
    use crate::storage::query::ast::MaintenanceCommand as Mc;

    // VACUUM (no target)
    if let QueryExpr::MaintenanceCommand(Mc::Vacuum { target, full }) = parse("VACUUM").unwrap() {
        assert_eq!(target, None);
        assert!(!full);
    } else {
        panic!("Expected Vacuum");
    }

    // VACUUM users (table target)
    if let QueryExpr::MaintenanceCommand(Mc::Vacuum { target, full }) =
        parse("VACUUM users").unwrap()
    {
        assert_eq!(target, Some("users".to_string()));
        assert!(!full);
    } else {
        panic!("Expected Vacuum");
    }

    // VACUUM FULL
    if let QueryExpr::MaintenanceCommand(Mc::Vacuum { target, full }) =
        parse("VACUUM FULL").unwrap()
    {
        assert_eq!(target, None);
        assert!(full);
    } else {
        panic!("Expected Vacuum FULL");
    }

    // VACUUM FULL users
    if let QueryExpr::MaintenanceCommand(Mc::Vacuum { target, full }) =
        parse("VACUUM FULL users").unwrap()
    {
        assert_eq!(target, Some("users".to_string()));
        assert!(full);
    } else {
        panic!("Expected Vacuum FULL users");
    }

    // ANALYZE (no target)
    if let QueryExpr::MaintenanceCommand(Mc::Analyze { target }) = parse("ANALYZE").unwrap() {
        assert_eq!(target, None);
    } else {
        panic!("Expected Analyze");
    }

    // ANALYZE users
    if let QueryExpr::MaintenanceCommand(Mc::Analyze { target }) = parse("ANALYZE users").unwrap() {
        assert_eq!(target, Some("users".to_string()));
    } else {
        panic!("Expected Analyze users");
    }
}

#[test]
fn test_parse_schema_and_sequence_ddl() {
    // CREATE SCHEMA
    if let QueryExpr::CreateSchema(q) = parse("CREATE SCHEMA app").unwrap() {
        assert_eq!(q.name, "app");
        assert!(!q.if_not_exists);
    } else {
        panic!("Expected CreateSchema");
    }
    if let QueryExpr::CreateSchema(q) = parse("CREATE SCHEMA IF NOT EXISTS app").unwrap() {
        assert_eq!(q.name, "app");
        assert!(q.if_not_exists);
    } else {
        panic!("Expected CreateSchema IF NOT EXISTS");
    }

    // DROP SCHEMA
    if let QueryExpr::DropSchema(q) = parse("DROP SCHEMA app").unwrap() {
        assert_eq!(q.name, "app");
        assert!(!q.if_exists);
        assert!(!q.cascade);
    } else {
        panic!("Expected DropSchema");
    }
    if let QueryExpr::DropSchema(q) = parse("DROP SCHEMA IF EXISTS app CASCADE").unwrap() {
        assert_eq!(q.name, "app");
        assert!(q.if_exists);
        assert!(q.cascade);
    } else {
        panic!("Expected DropSchema IF EXISTS CASCADE");
    }

    // CREATE SEQUENCE — bare
    if let QueryExpr::CreateSequence(q) = parse("CREATE SEQUENCE s1").unwrap() {
        assert_eq!(q.name, "s1");
        assert_eq!(q.start, 1);
        assert_eq!(q.increment, 1);
        assert!(!q.if_not_exists);
    } else {
        panic!("Expected CreateSequence");
    }

    // CREATE SEQUENCE with START and INCREMENT
    if let QueryExpr::CreateSequence(q) =
        parse("CREATE SEQUENCE s1 START WITH 100 INCREMENT BY 5").unwrap()
    {
        assert_eq!(q.name, "s1");
        assert_eq!(q.start, 100);
        assert_eq!(q.increment, 5);
    } else {
        panic!("Expected CreateSequence with START/INCREMENT");
    }

    // Order agnostic (INCREMENT before START)
    if let QueryExpr::CreateSequence(q) = parse("CREATE SEQUENCE s1 INCREMENT 3 START 10").unwrap()
    {
        assert_eq!(q.start, 10);
        assert_eq!(q.increment, 3);
    } else {
        panic!("Expected CreateSequence reversed order");
    }

    // IF NOT EXISTS
    if let QueryExpr::CreateSequence(q) = parse("CREATE SEQUENCE IF NOT EXISTS s1").unwrap() {
        assert!(q.if_not_exists);
    } else {
        panic!("Expected CreateSequence IF NOT EXISTS");
    }

    // DROP SEQUENCE
    if let QueryExpr::DropSequence(q) = parse("DROP SEQUENCE s1").unwrap() {
        assert_eq!(q.name, "s1");
        assert!(!q.if_exists);
    } else {
        panic!("Expected DropSequence");
    }
    if let QueryExpr::DropSequence(q) = parse("DROP SEQUENCE IF EXISTS s1").unwrap() {
        assert!(q.if_exists);
    } else {
        panic!("Expected DropSequence IF EXISTS");
    }
}

#[test]
fn test_parse_copy_from_csv() {
    // Basic COPY: no options.
    if let QueryExpr::CopyFrom(q) = parse("COPY users FROM '/tmp/u.csv'").unwrap() {
        assert_eq!(q.table, "users");
        assert_eq!(q.path, "/tmp/u.csv");
        assert!(!q.has_header);
        assert_eq!(q.delimiter, None);
    } else {
        panic!("Expected CopyFrom");
    }

    // Short form with HEADER + DELIMITER outside WITH.
    if let QueryExpr::CopyFrom(q) =
        parse("COPY users FROM '/tmp/u.csv' DELIMITER ';' HEADER").unwrap()
    {
        assert_eq!(q.table, "users");
        assert_eq!(q.delimiter, Some(';'));
        assert!(q.has_header);
    } else {
        panic!("Expected CopyFrom with short options");
    }

    // PG-style WITH block.
    if let QueryExpr::CopyFrom(q) =
        parse("COPY users FROM '/tmp/u.csv' WITH (FORMAT = csv, HEADER = true, DELIMITER = ',')")
            .unwrap()
    {
        assert_eq!(q.delimiter, Some(','));
        assert!(q.has_header);
    } else {
        panic!("Expected CopyFrom with WITH block");
    }
}

#[test]
fn test_parse_view_ddl() {
    // CREATE VIEW
    if let QueryExpr::CreateView(q) =
        parse("CREATE VIEW active_users AS SELECT * FROM users").unwrap()
    {
        assert_eq!(q.name, "active_users");
        assert!(!q.materialized);
        assert!(!q.if_not_exists);
        assert!(!q.or_replace);
        // Body must be a Table query pointing at `users`.
        if let QueryExpr::Table(tq) = *q.query {
            assert_eq!(tq.table, "users");
        } else {
            panic!("Expected Table body");
        }
    } else {
        panic!("Expected CreateView");
    }

    // CREATE OR REPLACE VIEW
    if let QueryExpr::CreateView(q) = parse("CREATE OR REPLACE VIEW v AS SELECT id FROM t").unwrap()
    {
        assert!(q.or_replace);
        assert!(!q.materialized);
    } else {
        panic!("Expected CreateView OR REPLACE");
    }

    // CREATE MATERIALIZED VIEW IF NOT EXISTS
    if let QueryExpr::CreateView(q) =
        parse("CREATE MATERIALIZED VIEW IF NOT EXISTS mv AS SELECT id FROM t").unwrap()
    {
        assert!(q.materialized);
        assert!(q.if_not_exists);
    } else {
        panic!("Expected CreateView MATERIALIZED IF NOT EXISTS");
    }

    // DROP VIEW
    if let QueryExpr::DropView(q) = parse("DROP VIEW v").unwrap() {
        assert_eq!(q.name, "v");
        assert!(!q.materialized);
        assert!(!q.if_exists);
    } else {
        panic!("Expected DropView");
    }

    // DROP MATERIALIZED VIEW IF EXISTS
    if let QueryExpr::DropView(q) = parse("DROP MATERIALIZED VIEW IF EXISTS mv").unwrap() {
        assert!(q.materialized);
        assert!(q.if_exists);
    } else {
        panic!("Expected DropView MATERIALIZED IF EXISTS");
    }

    // REFRESH MATERIALIZED VIEW
    if let QueryExpr::RefreshMaterializedView(q) = parse("REFRESH MATERIALIZED VIEW mv").unwrap() {
        assert_eq!(q.name, "mv");
    } else {
        panic!("Expected RefreshMaterializedView");
    }
}

#[test]
fn test_parse_partitioning_ddl() {
    use crate::storage::query::ast::{AlterOperation, PartitionKind};

    // CREATE TABLE with PARTITION BY RANGE
    if let QueryExpr::CreateTable(t) =
        parse("CREATE TABLE events (id INT, ts INT) PARTITION BY RANGE (ts)").unwrap()
    {
        assert_eq!(t.name, "events");
        let spec = t.partition_by.expect("partition_by should be set");
        assert_eq!(spec.kind, PartitionKind::Range);
        assert_eq!(spec.column, "ts");
    } else {
        panic!("Expected CreateTable with PARTITION BY RANGE");
    }

    // CREATE TABLE with PARTITION BY LIST
    if let QueryExpr::CreateTable(t) =
        parse("CREATE TABLE logs (region TEXT) PARTITION BY LIST (region)").unwrap()
    {
        let spec = t.partition_by.unwrap();
        assert_eq!(spec.kind, PartitionKind::List);
        assert_eq!(spec.column, "region");
    } else {
        panic!("Expected LIST partition");
    }

    // CREATE TABLE with PARTITION BY HASH
    if let QueryExpr::CreateTable(t) =
        parse("CREATE TABLE shards (uid INT) PARTITION BY HASH (uid)").unwrap()
    {
        let spec = t.partition_by.unwrap();
        assert_eq!(spec.kind, PartitionKind::Hash);
    } else {
        panic!("Expected HASH partition");
    }

    // ALTER TABLE ... ATTACH PARTITION
    if let QueryExpr::AlterTable(q) =
        parse("ALTER TABLE events ATTACH PARTITION events_2024 FOR VALUES FROM (2024) TO (2025)")
            .unwrap()
    {
        assert_eq!(q.name, "events");
        match &q.operations[0] {
            AlterOperation::AttachPartition { child, bound } => {
                assert_eq!(child, "events_2024");
                assert!(bound.contains("FROM"));
                assert!(bound.contains("TO"));
            }
            other => panic!("Expected AttachPartition, got {:?}", other),
        }
    } else {
        panic!("Expected AlterTable");
    }

    // ALTER TABLE ... DETACH PARTITION
    if let QueryExpr::AlterTable(q) =
        parse("ALTER TABLE events DETACH PARTITION events_2024").unwrap()
    {
        match &q.operations[0] {
            AlterOperation::DetachPartition { child } => {
                assert_eq!(child, "events_2024");
            }
            other => panic!("Expected DetachPartition, got {:?}", other),
        }
    } else {
        panic!("Expected AlterTable");
    }
}

#[test]
fn test_parse_row_level_security_ddl() {
    use crate::storage::query::ast::{AlterOperation, PolicyAction};

    // CREATE POLICY
    if let QueryExpr::CreatePolicy(q) =
        parse("CREATE POLICY owner_only ON users USING (owner_id = 1)").unwrap()
    {
        assert_eq!(q.name, "owner_only");
        assert_eq!(q.table, "users");
        assert_eq!(q.action, None);
        assert_eq!(q.role, None);
    } else {
        panic!("Expected CreatePolicy");
    }

    // CREATE POLICY with action + role
    if let QueryExpr::CreatePolicy(q) =
        parse("CREATE POLICY readonly ON t FOR SELECT TO analytics USING (public = 1)").unwrap()
    {
        assert_eq!(q.action, Some(PolicyAction::Select));
        assert_eq!(q.role.as_deref(), Some("analytics"));
    } else {
        panic!("Expected CreatePolicy with action + role");
    }

    // DROP POLICY
    if let QueryExpr::DropPolicy(q) = parse("DROP POLICY owner_only ON users").unwrap() {
        assert_eq!(q.name, "owner_only");
        assert_eq!(q.table, "users");
        assert!(!q.if_exists);
    } else {
        panic!("Expected DropPolicy");
    }

    // DROP POLICY IF EXISTS
    if let QueryExpr::DropPolicy(q) = parse("DROP POLICY IF EXISTS p ON t").unwrap() {
        assert!(q.if_exists);
    } else {
        panic!("Expected DropPolicy IF EXISTS");
    }

    // ALTER TABLE ENABLE ROW LEVEL SECURITY
    if let QueryExpr::AlterTable(q) = parse("ALTER TABLE users ENABLE ROW LEVEL SECURITY").unwrap()
    {
        assert!(matches!(
            q.operations[0],
            AlterOperation::EnableRowLevelSecurity
        ));
    } else {
        panic!("Expected ENABLE ROW LEVEL SECURITY");
    }

    // ALTER TABLE DISABLE ROW LEVEL SECURITY
    if let QueryExpr::AlterTable(q) = parse("ALTER TABLE users DISABLE ROW LEVEL SECURITY").unwrap()
    {
        assert!(matches!(
            q.operations[0],
            AlterOperation::DisableRowLevelSecurity
        ));
    } else {
        panic!("Expected DISABLE ROW LEVEL SECURITY");
    }
}

#[test]
fn test_parse_fdw_ddl() {
    // CREATE SERVER
    if let QueryExpr::CreateServer(q) =
        parse("CREATE SERVER mycsv FOREIGN DATA WRAPPER csv OPTIONS (base_path '/data')").unwrap()
    {
        assert_eq!(q.name, "mycsv");
        assert_eq!(q.wrapper, "csv");
        assert_eq!(q.options.len(), 1);
        assert_eq!(q.options[0].0, "base_path");
        assert_eq!(q.options[0].1, "/data");
    } else {
        panic!("Expected CreateServer");
    }

    // CREATE SERVER with IF NOT EXISTS + multiple options
    if let QueryExpr::CreateServer(q) =
        parse("CREATE SERVER IF NOT EXISTS s2 FOREIGN DATA WRAPPER csv OPTIONS (a 'x', b 'y')")
            .unwrap()
    {
        assert!(q.if_not_exists);
        assert_eq!(q.options.len(), 2);
    } else {
        panic!("Expected CreateServer IF NOT EXISTS");
    }

    // DROP SERVER
    if let QueryExpr::DropServer(q) = parse("DROP SERVER mycsv").unwrap() {
        assert_eq!(q.name, "mycsv");
        assert!(!q.if_exists);
        assert!(!q.cascade);
    } else {
        panic!("Expected DropServer");
    }

    // DROP SERVER IF EXISTS ... CASCADE
    if let QueryExpr::DropServer(q) = parse("DROP SERVER IF EXISTS mycsv CASCADE").unwrap() {
        assert!(q.if_exists);
        assert!(q.cascade);
    } else {
        panic!("Expected DropServer IF EXISTS CASCADE");
    }

    // CREATE FOREIGN TABLE
    if let QueryExpr::CreateForeignTable(q) = parse(
        "CREATE FOREIGN TABLE users (id INT, name TEXT) SERVER mycsv OPTIONS (path 'users.csv')",
    )
    .unwrap()
    {
        assert_eq!(q.name, "users");
        assert_eq!(q.server, "mycsv");
        assert_eq!(q.columns.len(), 2);
        assert_eq!(q.columns[0].name, "id");
        assert_eq!(q.columns[1].name, "name");
        assert_eq!(q.options.len(), 1);
        assert_eq!(q.options[0].0, "path");
    } else {
        panic!("Expected CreateForeignTable");
    }

    // DROP FOREIGN TABLE
    if let QueryExpr::DropForeignTable(q) = parse("DROP FOREIGN TABLE IF EXISTS users").unwrap() {
        assert_eq!(q.name, "users");
        assert!(q.if_exists);
    } else {
        panic!("Expected DropForeignTable");
    }
}

// ─────────────────────────────────────────────────────────────────────
// CTE / WITH clause parsing — issue #40.
//
// Top-level `parser::parse` now returns `QueryWithCte`. A leading
// `WITH` is consumed as a CTE prelude; everything else returns the
// `simple` shape with `with_clause: None`. Execution-side wiring is
// out of scope here (#41) — these tests only assert that the parser
// produces the right AST.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn cte_no_with_clause_produces_simple_shape() {
    let parsed = super::parse("SELECT * FROM users").unwrap();
    assert!(parsed.with_clause.is_none());
    assert!(matches!(parsed.query, QueryExpr::Table(_)));
}

#[test]
fn cte_single_with_single_reference() {
    let parsed = super::parse(
        "WITH active AS (SELECT * FROM users WHERE status = 'active') \
         SELECT count(*) FROM active",
    )
    .unwrap();
    let with = parsed
        .with_clause
        .as_ref()
        .expect("with_clause should be Some");
    assert_eq!(with.ctes.len(), 1);
    assert_eq!(with.ctes[0].name, "active");
    assert!(with.ctes[0].columns.is_empty());
    assert!(!with.has_recursive);
    assert!(matches!(parsed.query, QueryExpr::Table(_)));
}

#[test]
fn cte_chained_definitions() {
    let parsed =
        super::parse("WITH a AS (SELECT * FROM t1), b AS (SELECT * FROM a) SELECT * FROM b")
            .unwrap();
    let with = parsed.with_clause.expect("with_clause should be Some");
    assert_eq!(with.ctes.len(), 2);
    assert_eq!(with.ctes[0].name, "a");
    assert_eq!(with.ctes[1].name, "b");
}

#[test]
fn cte_with_column_aliases() {
    let parsed =
        super::parse("WITH x(a, b) AS (SELECT id, name FROM users) SELECT * FROM x").unwrap();
    let with = parsed.with_clause.expect("with_clause should be Some");
    assert_eq!(with.ctes.len(), 1);
    assert_eq!(with.ctes[0].name, "x");
    assert_eq!(with.ctes[0].columns, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn cte_recursive_marker_propagates() {
    let parsed =
        super::parse("WITH RECURSIVE walk AS (SELECT * FROM nodes) SELECT * FROM walk").unwrap();
    let with = parsed.with_clause.expect("with_clause should be Some");
    assert!(with.has_recursive);
    assert!(with.ctes[0].recursive);
}

// --- Issue #87: hardening / DoS-limit tests --------------------

#[test]
fn dos_limit_input_too_large_is_structured() {
    use super::limits::ParserLimits;
    let limits = ParserLimits {
        max_input_bytes: 16,
        ..ParserLimits::default()
    };
    // The input is well over 16 bytes — must refuse before tokenizing.
    let err = super::Parser::with_limits("SELECT * FROM users WHERE x = 1", limits)
        .err()
        .expect("oversized input must error");
    assert!(matches!(
        err.kind,
        super::ParseErrorKind::InputTooLarge {
            limit_name: "max_input_bytes",
            value: 16,
        }
    ));
}

#[test]
fn dos_limit_identifier_too_long_is_structured() {
    use super::limits::ParserLimits;
    let limits = ParserLimits {
        max_identifier_chars: 8,
        ..ParserLimits::default()
    };
    // `userstable_long_identifier_name` is well over 8 chars; SELECT/FROM
    // are 6 and 4 chars respectively, both ≤ 8.
    let err = super::Parser::with_limits("SELECT * FROM userstable_long_identifier_name", limits)
        .and_then(|mut p| p.parse())
        .err()
        .expect("oversized ident must error");
    assert!(
        matches!(
            err.kind,
            super::ParseErrorKind::IdentifierTooLong {
                limit_name: "max_identifier_chars",
                ..
            }
        ),
        "kind: {:?}",
        err.kind
    );
}

#[test]
fn dos_limit_recursion_depth_is_structured() {
    use super::limits::ParserLimits;
    let limits = ParserLimits {
        max_depth: 8,
        ..ParserLimits::default()
    };
    // 50 nested parens = recursion depth 50, well past the cap of 8.
    let nested = format!("SELECT {}1{} FROM t", "(".repeat(50), ")".repeat(50));
    let mut parser = super::Parser::with_limits(&nested, limits).expect("ctor ok");
    let err = parser.parse().err().expect("deep nesting must error");
    assert!(matches!(
        err.kind,
        super::ParseErrorKind::DepthLimit {
            limit_name: "max_depth",
            value: 8,
        }
    ));
}

#[test]
fn dos_limit_no_panic_on_pathological_input() {
    // Adversarial inputs return Err but never panic / OOM.
    let mut adversarial = "(".repeat(2_000);
    adversarial.push_str("SELECT 1");
    adversarial.push_str(&")".repeat(2_000));
    let _ = super::parse(&adversarial); // must return Err, not panic.

    let big = "x".repeat(10_000); // exceeds default 256 ident cap
    let _ = super::parse(&format!("SELECT {} FROM t", big));

    let huge = "a".repeat(2 * 1024 * 1024); // 2 MiB, exceeds 1 MiB cap
    let err = super::parse(&huge).err().expect("must reject");
    assert!(matches!(
        err.kind,
        super::ParseErrorKind::InputTooLarge { .. }
    ));
}

#[test]
fn dos_limit_chained_not_in_where_does_not_overflow_stack() {
    // Regression test for issue #91. Before the fix,
    // `parse_not_expr` recursed into itself for `NOT NOT NOT … x`
    // without entering the depth counter — only the leaf reached
    // `parse_expr_prec`'s `enter_depth()`, so a 10k-NOT payload
    // overflowed the Rust stack BEFORE `max_depth=128` could fire.
    //
    // We run on a generous-stack thread so that, in the unfixed
    // build, the test reliably reports the overflow as a panic
    // rather than aborting the whole test runner. With the fix,
    // the depth guard fires structurally and the inner thread
    // returns a `DepthLimit` error.
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut sql = String::from("SELECT * FROM t WHERE ");
            for _ in 0..10_000 {
                sql.push_str("NOT ");
            }
            sql.push_str("x");
            let err = super::parse(&sql).err().expect("must error, not panic");
            assert!(
                matches!(
                    err.kind,
                    super::ParseErrorKind::DepthLimit {
                        limit_name: "max_depth",
                        ..
                    }
                ),
                "expected DepthLimit, got: {:?}",
                err.kind
            );
        })
        .expect("spawn deep-NOT thread");
    handle.join().expect("deep-NOT thread must not panic");
}

// ---------------------------------------------------------------------------
// Documented raw JSON literal forms (audit: cover every example we ship in
// docs and landing page so the docs cannot lie). Each test parses the exact
// string a user would copy from the doc and asserts the resulting AST.
// ---------------------------------------------------------------------------

#[test]
fn doc_form_queue_push_raw_json_literal() {
    // docs/data-models/queues.md:38, README.md:68
    let q = parse(r#"QUEUE PUSH tasks {"job":"process","id":123}"#).unwrap();
    let QueryExpr::QueueCommand(QueueCommand::Push { queue, value, .. }) = q else {
        panic!("expected Push");
    };
    assert_eq!(queue, "tasks");
    let crate::storage::schema::Value::Json(bytes) = value else {
        panic!("expected Value::Json, got {value:?}");
    };
    let v: crate::json::Value = crate::json::from_slice(&bytes).unwrap();
    assert_eq!(v.get("job").and_then(|x| x.as_str()), Some("process"));
    assert_eq!(v.get("id").and_then(|x| x.as_i64()), Some(123));
}

#[test]
fn doc_form_queue_rpush_raw_json_literal() {
    // docs/data-models/queues.md:39
    let q = parse(r#"QUEUE RPUSH tasks {"job":"process","id":456}"#).unwrap();
    assert!(matches!(
        q,
        QueryExpr::QueueCommand(QueueCommand::Push { .. })
    ));
}

#[test]
fn doc_form_queue_lpush_raw_json_literal() {
    // docs/data-models/queues.md:42
    let q = parse(r#"QUEUE LPUSH tasks {"urgent":true}"#).unwrap();
    assert!(matches!(
        q,
        QueryExpr::QueueCommand(QueueCommand::Push { .. })
    ));
}

#[test]
fn doc_form_queue_push_raw_json_with_priority_suffix() {
    // docs/data-models/queues.md:45
    let q = parse(r#"QUEUE PUSH urgent_tasks {"job":"deploy"} PRIORITY 10"#).unwrap();
    let QueryExpr::QueueCommand(QueueCommand::Push {
        priority, value, ..
    }) = q
    else {
        panic!("expected Push");
    };
    assert_eq!(priority, Some(10));
    assert!(matches!(value, crate::storage::schema::Value::Json(_)));
}

#[test]
fn doc_form_insert_document_with_raw_json_literal_in_values() {
    // README.md:44, landing +page.svelte:54/150/492
    let q = parse(r#"INSERT INTO logs DOCUMENT (body) VALUES ({"level":"warn","ip":"10.0.0.1"})"#)
        .unwrap();
    // We don't pin the exact AST shape — just prove it parses to *some* insert.
    match q {
        QueryExpr::Insert(_) => {}
        other => panic!(
            "expected Insert/InsertDocument, got: {:?}",
            std::mem::discriminant(&other)
        ),
    }
}

#[test]
fn doc_form_timeseries_insert_with_raw_json_tags_3_tuple() {
    // landing +page.svelte:59/497, data-types.ts:604, README.md:57
    let q = parse(
        r#"INSERT INTO cpu_metrics (metric, value, tags) VALUES ('cpu.idle', 95.2, {"host":"srv1"})"#,
    )
    .unwrap();
    assert!(matches!(q, QueryExpr::Insert(_)));
}

#[test]
fn doc_form_timeseries_insert_with_raw_json_tags_4_tuple() {
    // docs/data-models/timeseries.md:62
    let q = parse(
        r#"INSERT INTO cpu_metrics (metric, value, tags, timestamp) VALUES ('cpu.idle', 94.8, {"host":"srv1"}, 1704067200000000000)"#,
    )
    .unwrap();
    assert!(matches!(q, QueryExpr::Insert(_)));
}

#[test]
fn doc_form_timeseries_multi_row_insert_with_raw_json_tags() {
    // docs/data-models/timeseries.md:69 — multi-row VALUES with raw JSON
    let q = parse(
        r#"INSERT INTO cpu_metrics (metric, value, tags) VALUES ('cpu.idle', 95.2, {"host":"srv1"}), ('cpu.busy', 4.8, {"host":"srv1"})"#,
    )
    .unwrap();
    assert!(matches!(q, QueryExpr::Insert(_)));
}
