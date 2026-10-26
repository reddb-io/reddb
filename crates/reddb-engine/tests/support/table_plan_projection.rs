//! Test-only host over supplied recovered records; not a browser planner/dispatcher.
use super::*;
use reddb_engine::query::{
    logical_plan::CanonicalLogicalNode,
    runtime::table_plan::{execute_table_plan_with_host, TablePlanHost},
};
use reddb_rql::ast::TableQuery;

struct TableHost<'a> {
    records: &'a [UnifiedRecord],
    query: &'a TableQuery,
    contract: &'a CollectionContract,
    projections: Host<'a>,
}
impl TablePlanHost for TableHost<'_> {
    type Error = String;
    fn scan(&self, _: &CanonicalLogicalNode) -> Result<Vec<UnifiedRecord>, String> {
        Ok(self.records.to_vec())
    }
    fn try_filter_lookup(&self) -> Result<Option<Vec<UnifiedRecord>>, String> {
        Ok(None)
    }
    fn filter_records(
        &self,
        records: Vec<UnifiedRecord>,
        document_gate: bool,
    ) -> Result<Vec<UnifiedRecord>, String> {
        assert!(!document_gate, "fixture admits plain table plans only");
        let mut output = Vec::with_capacity(records.len());
        for record in records {
            if super::super::record_filters::matches_where(&record, self.query, Some(self.contract))
                .map_err(|error| error.to_string())?
            {
                output.push(record);
            }
        }
        Ok(output)
    }
    fn sort_records(
        &self,
        node: &CanonicalLogicalNode,
        records: &mut Vec<UnifiedRecord>,
    ) -> Result<(), String> {
        assert_eq!(
            node.operator, "sort",
            "fixture admits plain table plans only"
        );
        super::super::record_ordering::sort_records(records, self.query, Some(self.contract));
        Ok(())
    }
    fn rank_records(&self, _: &mut Vec<UnifiedRecord>) {
        panic!("fixture does not admit ranked plans")
    }
    fn project_records(
        &self,
        node: &CanonicalLogicalNode,
        records: Vec<UnifiedRecord>,
    ) -> Result<Vec<UnifiedRecord>, String> {
        assert_eq!(
            node.operator, "projection",
            "fixture admits plain table plans only"
        );
        let columns = reddb_rql::sql_lowering::effective_table_projections(self.query);
        assert!(
            !columns
                .iter()
                .any(|column| matches!(column, Projection::Window { .. })),
            "fixture does not admit window effects"
        );
        records
            .iter()
            .map(|record| project(&self.projections, record, &columns))
            .collect()
    }
    fn query_error(&self, message: String) -> String {
        message
    }
}

pub(super) fn verify_recovered(
    records: &[UnifiedRecord],
    contract: &CollectionContract,
    updated: bool,
) {
    let query = super::super::record_filters::table_query("SELECT o.rid AS identity, total AS amount, quantity * 2 AS doubled FROM orders AS o WHERE total >= 10 ORDER BY total DESC LIMIT 2 OFFSET 1");
    // This fixed canonical fixture uses the same native operator names and node
    // authority. It is not a second production logical-plan builder.
    let mut plan = CanonicalLogicalNode {
        operator: "table_scan".into(),
        source: Some("orders".into()),
        ..Default::default()
    };
    for operator in ["filter", "sort", "offset", "limit", "projection"] {
        plan = CanonicalLogicalNode {
            operator: operator.into(),
            children: vec![plan],
            ..Default::default()
        };
    }
    let host = TableHost {
        records,
        query: &query,
        contract,
        projections: Host::new(Some(contract)),
    };
    let projected =
        execute_table_plan_with_host(&host, &plan, &query).expect("recovered canonical table plan");
    let expected = if updated {
        [(3, 20, 8), (2, 15, 6)]
    } else {
        [(2, 15, 6), (9_007_199_254_740_992, 10, 4)]
    };
    assert_eq!(projected.len(), expected.len());
    for (record, (id, amount, doubled)) in projected.iter().zip(expected) {
        assert_eq!(record.get("identity"), Some(&Value::UnsignedInteger(id)));
        assert_eq!(record.get("amount"), Some(&Value::Integer(amount)));
        assert_eq!(record.get("doubled"), Some(&Value::Integer(doubled)));
    }
}
