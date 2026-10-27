//! Canonical read-plan sequencing, errors and bounds in native and WASM.
use reddb_engine::query::{
    logical_plan::CanonicalLogicalNode,
    runtime::table_plan::{execute_table_plan_with_host, table_scan_cap, TablePlanHost},
    unified::UnifiedRecord,
};
use reddb_rql::ast::TableQuery;
use reddb_types::Value;
use std::cell::RefCell;

pub const CASES: &[(&str, fn())] = &[
    (
        "table_plan_callback_order_and_aliases",
        callback_order_and_aliases,
    ),
    ("table_plan_fast_lookup_and_errors", fast_lookup_and_errors),
    ("table_plan_wide_bounds_and_topk", wide_bounds_and_topk),
    ("table_plan_depth_and_first_child", depth_and_first_child),
];

fn node(operator: &str, children: Vec<CanonicalLogicalNode>) -> CanonicalLogicalNode {
    CanonicalLogicalNode {
        operator: operator.into(),
        children,
        ..Default::default()
    }
}
fn chain(operators: &[&str]) -> CanonicalLogicalNode {
    let mut plan = node(operators[0], vec![]);
    for operator in &operators[1..] {
        plan = node(operator, vec![plan]);
    }
    plan
}
fn query() -> TableQuery {
    super::record_filters::table_query("SELECT * FROM orders")
}
fn records() -> Vec<UnifiedRecord> {
    [1, 2, 3]
        .into_iter()
        .map(|id| {
            let mut record = UnifiedRecord::new();
            record.set("rid", Value::Integer(id));
            record
        })
        .collect()
}
fn ids(records: &[UnifiedRecord]) -> Vec<i64> {
    records
        .iter()
        .map(|record| match record.get("rid") {
            Some(Value::Integer(id)) => *id,
            other => panic!("invalid fixture identity {other:?}"),
        })
        .collect()
}

#[derive(Default)]
struct Host {
    events: RefCell<Vec<String>>,
    fast: bool,
    failure: Option<&'static str>,
}
impl Host {
    fn event(&self, event: &str) -> Result<(), String> {
        self.events.borrow_mut().push(event.into());
        if self.failure == Some(event) {
            Err(format!("host {event} failed"))
        } else {
            Ok(())
        }
    }
}
impl TablePlanHost for Host {
    type Error = String;
    fn scan(&self, node: &CanonicalLogicalNode) -> Result<Vec<UnifiedRecord>, String> {
        self.event(&format!("scan:{}", node.operator))?;
        Ok(records())
    }
    fn try_filter_lookup(&self) -> Result<Option<Vec<UnifiedRecord>>, String> {
        self.event("lookup")?;
        Ok(self.fast.then(records))
    }
    fn filter_records(
        &self,
        records: Vec<UnifiedRecord>,
        document_gate: bool,
    ) -> Result<Vec<UnifiedRecord>, String> {
        self.event(if document_gate {
            "document_filter"
        } else {
            "filter"
        })?;
        Ok(records)
    }
    fn sort_records(
        &self,
        node: &CanonicalLogicalNode,
        records: &mut Vec<UnifiedRecord>,
    ) -> Result<(), String> {
        self.event(&format!("sort:{}", node.operator))?;
        records.reverse();
        Ok(())
    }
    fn rank_records(&self, records: &mut Vec<UnifiedRecord>) {
        self.events.borrow_mut().push("rank".into());
        records.reverse();
    }
    fn project_records(
        &self,
        node: &CanonicalLogicalNode,
        records: Vec<UnifiedRecord>,
    ) -> Result<Vec<UnifiedRecord>, String> {
        self.event(&format!("project:{}", node.operator))?;
        Ok(records)
    }
    fn query_error(&self, message: String) -> String {
        message
    }
}

fn callback_order_and_aliases() {
    for (scan, filter, sort, offset, limit, projection) in [
        (
            "table_scan",
            "filter",
            "sort",
            "offset",
            "limit",
            "projection",
        ),
        (
            "entity_scan",
            "entity_filter",
            "entity_sort",
            "entity_offset",
            "entity_limit",
            "entity_projection",
        ),
        (
            "document_path_index_seek",
            "document_path_filter",
            "document_sort",
            "offset",
            "limit",
            "document_projection",
        ),
    ] {
        let plan = chain(&[scan, filter, sort, offset, limit, projection]);
        let mut query = query();
        query.offset = Some(1);
        query.limit = Some(1);
        let host = Host::default();
        assert_eq!(
            ids(&execute_table_plan_with_host(&host, &plan, &query).expect("plan")),
            [2]
        );
        let mut expected = vec![];
        if filter != "document_path_filter" {
            expected.push("lookup".to_string());
        }
        expected.extend([
            format!("scan:{scan}"),
            if filter == "document_path_filter" {
                "document_filter".into()
            } else {
                "filter".into()
            },
            format!("sort:{sort}"),
            format!("project:{projection}"),
        ]);
        assert_eq!(*host.events.borrow(), expected);
    }
    let host = Host::default();
    let mut query = query();
    query.limit = Some(0);
    assert!(execute_table_plan_with_host(
        &host,
        &chain(&["index_seek", "limit", "projection"]),
        &query
    )
    .expect("zero limit")
    .is_empty());
    assert_eq!(
        *host.events.borrow(),
        ["scan:index_seek", "project:projection"],
        "empty results still run the host projection/window boundary"
    );
}

fn fast_lookup_and_errors() {
    let host = Host {
        fast: true,
        ..Default::default()
    };
    assert_eq!(
        ids(
            &execute_table_plan_with_host(&host, &node("filter", vec![]), &query())
                .expect("lookup bypasses child")
        ),
        [1, 2, 3]
    );
    assert_eq!(*host.events.borrow(), ["lookup"]);
    let host = Host::default();
    assert_eq!(
        execute_table_plan_with_host(&host, &node("limit", vec![]), &query())
            .expect_err("missing child"),
        "canonical table operator limit is missing its child plan"
    );
    assert!(host.events.borrow().is_empty());
    assert_eq!(
        execute_table_plan_with_host(&host, &node("unsupported", vec![]), &query())
            .expect_err("unsupported"),
        "unsupported canonical table operator unsupported"
    );
    for (failure, expected) in [
        ("lookup", vec!["lookup"]),
        ("scan:table_scan", vec!["lookup", "scan:table_scan"]),
        ("filter", vec!["lookup", "scan:table_scan", "filter"]),
        (
            "sort:sort",
            vec!["lookup", "scan:table_scan", "filter", "sort:sort"],
        ),
        (
            "project:projection",
            vec![
                "lookup",
                "scan:table_scan",
                "filter",
                "sort:sort",
                "project:projection",
            ],
        ),
    ] {
        let host = Host {
            failure: Some(failure),
            ..Default::default()
        };
        assert_eq!(
            execute_table_plan_with_host(
                &host,
                &chain(&["table_scan", "filter", "sort", "projection"]),
                &query()
            )
            .expect_err("host error"),
            format!("host {failure} failed")
        );
        assert_eq!(*host.events.borrow(), expected);
    }
}

fn wide_bounds_and_topk() {
    let host = Host::default();
    let mut query = query();
    for offset in [4_294_967_296, u64::MAX] {
        query.offset = Some(offset);
        assert!(
            execute_table_plan_with_host(&host, &chain(&["table_scan", "offset"]), &query)
                .expect("wide offset")
                .is_empty()
        );
    }
    query.offset = None;
    query.limit = Some(4_294_967_296);
    assert_eq!(
        ids(
            &execute_table_plan_with_host(&host, &chain(&["table_scan", "limit"]), &query)
                .expect("wide limit")
        ),
        [1, 2, 3]
    );
    query.limit = Some(1);
    let mut topk = chain(&["entity_scan", "entity_search", "entity_topk"]);
    topk.details.insert("k".into(), "4294967296".into());
    assert_eq!(
        ids(&execute_table_plan_with_host(&host, &topk, &query).expect("wide k")),
        [3, 2, 1],
        "wide k must not fall back to the smaller query limit on wasm32"
    );
    topk.details.insert("k".into(), "invalid".into());
    assert_eq!(
        ids(&execute_table_plan_with_host(&host, &topk, &query).expect("fallback k")),
        [3]
    );
    query.offset = Some(u64::MAX);
    query.limit = Some(u64::MAX);
    assert_eq!(table_scan_cap(&query), Some(usize::MAX));
    query.order_by =
        super::record_filters::table_query("SELECT * FROM orders ORDER BY rid").order_by;
    assert_eq!(table_scan_cap(&query), None);
    query.order_by.clear();
    query.limit = None;
    assert_eq!(table_scan_cap(&query), None);
}

fn depth_and_first_child() {
    let mut plan = node("table_scan", vec![]);
    for _ in 0..reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
        plan = node("entity_search", vec![plan]);
    }
    let host = Host::default();
    assert_eq!(
        execute_table_plan_with_host(&host, &plan, &query())
            .expect("depth boundary")
            .len(),
        3
    );
    let host = Host::default();
    assert_eq!(
        execute_table_plan_with_host(&host, &node("entity_search", vec![plan]), &query())
            .expect_err("depth cap"),
        "canonical table plan exceeds maximum nesting depth"
    );
    assert!(host.events.borrow().is_empty());
    let host = Host::default();
    let plan = node(
        "entity_search",
        vec![node("index_seek", vec![]), node("unsupported", vec![])],
    );
    assert_eq!(
        execute_table_plan_with_host(&host, &plan, &query())
            .expect("first child")
            .len(),
        3
    );
    assert_eq!(*host.events.borrow(), ["scan:index_seek"]);
}
