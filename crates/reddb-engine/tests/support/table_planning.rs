//! The canonical native table planner runs unchanged on native and WASM hosts.
use reddb_engine::query::{
    logical_plan::{AccessPathDecision, CanonicalLogicalNode, CardinalityEstimate},
    planning::{build_table_plan_with_host, helpers::filter_summary, TablePlanningHost},
};
use reddb_rql::ast::{CompareOp, FieldRef, Filter, Projection, TableQuery};
use reddb_types::Value;
use std::cell::RefCell;

pub const CASES: &[(&str, fn())] = &[
    (
        "table_planning_order_costs_and_metadata",
        order_costs_and_metadata,
    ),
    (
        "table_planning_document_and_universal_routes",
        document_and_universal_routes,
    ),
    (
        "table_planning_effective_sql_and_zero_bounds",
        effective_sql_and_zero_bounds,
    ),
    (
        "table_planning_bounded_metadata_and_secrets",
        bounded_metadata_and_secrets,
    ),
];

#[derive(Default)]
struct Host {
    events: RefCell<Vec<String>>,
    document: bool,
}
impl TablePlanningHost for Host {
    fn access_path(&self, _: &TableQuery) -> AccessPathDecision {
        self.events.borrow_mut().push("access".into());
        AccessPathDecision {
            path: "index_seek",
            index_hint: Some("orders_by_total".into()),
            reason: "fixture index is operational".into(),
            warning: Some("other index is disabled".into()),
        }
    }
    fn scan_cardinality(&self, _: &TableQuery, access: &AccessPathDecision) -> CardinalityEstimate {
        self.events
            .borrow_mut()
            .push(format!("scan:{}", access.path));
        CardinalityEstimate {
            rows: 100.0,
            selectivity: 1.0,
            confidence: 0.85,
        }
    }
    fn uses_document_path_filter(&self, _: &TableQuery) -> bool {
        self.events.borrow_mut().push("document_filter".into());
        self.document
    }
    fn filter_cardinality(&self, _: &TableQuery, document_path: bool) -> CardinalityEstimate {
        self.events
            .borrow_mut()
            .push(format!("filtered:{document_path}"));
        CardinalityEstimate {
            rows: 10.0,
            selectivity: 0.1,
            confidence: 0.75,
        }
    }
    fn is_document_projection(&self, _: &TableQuery) -> bool {
        self.events.borrow_mut().push("document_projection".into());
        self.document
    }
}

pub fn nodes(mut root: &CanonicalLogicalNode) -> Vec<&CanonicalLogicalNode> {
    let mut nodes = vec![root];
    while let Some(child) = root.children.first() {
        assert_eq!(root.children.len(), 1, "table plan must be unary");
        root = child;
        nodes.push(root);
    }
    nodes
}
fn operators(root: &CanonicalLogicalNode) -> Vec<&str> {
    nodes(root)
        .into_iter()
        .map(|node| node.operator.as_str())
        .collect()
}
fn query(sql: &str) -> TableQuery {
    super::record_filters::table_query(sql)
}

fn order_costs_and_metadata() {
    let host = Host::default();
    let query = query("SELECT o.rid AS identity, total FROM orders AS o WHERE total >= 10 ORDER BY total DESC LIMIT 2 OFFSET 1");
    let plan = build_table_plan_with_host(&host, &query);
    assert_eq!(
        operators(&plan),
        [
            "projection",
            "limit",
            "offset",
            "sort",
            "filter",
            "index_seek"
        ]
    );
    assert_eq!(
        *host.events.borrow(),
        [
            "access",
            "scan:index_seek",
            "document_filter",
            "filtered:false",
            "document_projection"
        ]
    );
    let nodes = nodes(&plan);
    assert_eq!(
        nodes
            .iter()
            .map(|node| node.estimated_rows)
            .collect::<Vec<_>>(),
        [2.0, 2.0, 9.0, 10.0, 10.0, 100.0]
    );
    assert_eq!(
        nodes
            .iter()
            .map(|node| node.operator_cost)
            .collect::<Vec<_>>(),
        [0.24, 0.16, 0.45, 8.0, 2.5, 35.0]
    );
    assert_eq!(plan.estimated_selectivity, 0.1);
    assert_eq!(plan.estimated_confidence, 0.75);
    assert_eq!(nodes[4].details["predicate"], "total >= 10");
    assert_eq!(nodes[3].details["keys"], "total desc nulls_first");
    let scan = nodes[5];
    assert_eq!(scan.source.as_deref(), Some("orders"));
    assert_eq!(scan.details["access_path"], "index_seek");
    assert_eq!(
        scan.details["access_path_reason"],
        "fixture index is operational"
    );
    assert_eq!(scan.details["lifecycle_warning"], "other index is disabled");
    assert_eq!(scan.details["index_hint"], "orders_by_total");
    assert_eq!(scan.details["projection_count"], "2");
    assert_eq!(scan.details["universal"], "false");
    assert_eq!(scan.details["filter"], "true");
    assert_eq!(scan.details["order_by"], "1");
    assert_eq!(scan.details["offset"], "1");
    assert_eq!(scan.details["limit"], "2");
    assert!(nodes[..5].iter().all(|node| node.source.is_none()));
}

fn document_and_universal_routes() {
    let host = Host {
        document: true,
        ..Default::default()
    };
    let doc = query("SELECT body.total FROM orders WHERE body.total >= 10 ORDER BY body.total");
    assert_eq!(
        operators(&build_table_plan_with_host(&host, &doc)),
        [
            "document_projection",
            "document_sort",
            "document_path_filter",
            "index_seek"
        ]
    );
    let host = Host::default();
    let alias = query("SELECT o.total FROM orders AS o ORDER BY o.total");
    assert_eq!(
        operators(&build_table_plan_with_host(&host, &alias)),
        ["projection", "sort", "index_seek"]
    );
    // Universal selection must not consult a table-only index, and pagination
    // without ORDER BY keeps the native deterministic ranked ordering.
    let mut universal = query("SELECT rid FROM orders WHERE total >= 10 LIMIT 2 OFFSET 1");
    for source in [
        "any",
        "_any",
        "all",
        "entity",
        "universal",
        "mixed",
        "*",
        "ANY",
    ] {
        universal.table = source.into();
        let host = Host::default();
        let plan = build_table_plan_with_host(&host, &universal);
        assert_eq!(
            operators(&plan),
            [
                "entity_projection",
                "entity_limit",
                "entity_offset",
                "entity_sort",
                "entity_filter",
                "entity_scan"
            ]
        );
        assert_eq!(
            nodes(&plan)[3].details["keys"],
            "_score desc, _entity_id asc"
        );
        assert_eq!(nodes(&plan)[5].details["universal"], "true");
        assert_eq!(
            *host.events.borrow(),
            [
                "scan:entity_scan",
                "document_filter",
                "filtered:false",
                "document_projection"
            ]
        );
    }
}

fn effective_sql_and_zero_bounds() {
    let host = Host::default();
    let simple = build_table_plan_with_host(&host, &query("SELECT * FROM orders"));
    assert_eq!(operators(&simple), ["index_seek"]);
    assert_eq!(*host.events.borrow(), ["access", "scan:index_seek"]);
    assert_eq!(simple.details["limit"], "none");
    assert_eq!(simple.details["offset"], "none");
    let mut typed = query("SELECT total FROM orders WHERE total >= 10 LIMIT 0 OFFSET 0");
    typed.columns = vec![Projection::All];
    typed.filter = None;
    let plan = build_table_plan_with_host(&Host::default(), &typed);
    assert_eq!(
        operators(&plan),
        ["projection", "limit", "offset", "filter", "index_seek"]
    );
    assert_eq!(plan.estimated_rows, 0.0);
    assert_eq!(plan.details["columns"], "total");
    let scan = nodes(&plan).pop().expect("scan");
    assert_eq!(scan.details["filter"], "true");
    assert_eq!(scan.details["limit"], "0");
    assert_eq!(scan.details["offset"], "0");
}

fn bounded_metadata_and_secrets() {
    let field = FieldRef::TableColumn {
        table: "orders".into(),
        column: "secret".into(),
    };
    let secret = Filter::Compare {
        field: field.clone(),
        op: CompareOp::Eq,
        value: Value::Secret(vec![1, 2, 3]),
    };
    assert_eq!(filter_summary(&secret), "orders.secret = secret(3 bytes)");
    let mut filter = Filter::IsNull(field);
    for _ in 0..reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
        filter = Filter::Not(Box::new(filter));
    }
    let at_limit = filter_summary(&filter);
    assert!(!at_limit.contains("<maximum filter depth>"));
    let over_limit = filter_summary(&Filter::Not(Box::new(filter)));
    assert!(over_limit.contains("<maximum filter depth>"));
    assert!(
        over_limit.len() < 1000,
        "metadata expansion is depth bounded"
    );
}
