//! Native canonical table-plan traversal. Hosts own read admission and effects.
use crate::query::{logical_plan::CanonicalLogicalNode, unified::UnifiedRecord};
use reddb_rql::ast::TableQuery;

/// A scan returns already-admitted records. Native MVCC, tenancy/RLS, resource
/// guards, index/replica paths, computed sort keys, windows and output policy
/// stay in the host. Callback errors propagate without conversion.
pub trait TablePlanHost {
    type Error;
    fn scan(&self, node: &CanonicalLogicalNode) -> Result<Vec<UnifiedRecord>, Self::Error>;
    fn try_filter_lookup(&self) -> Result<Option<Vec<UnifiedRecord>>, Self::Error>;
    fn filter_records(
        &self,
        records: Vec<UnifiedRecord>,
        document_gate: bool,
    ) -> Result<Vec<UnifiedRecord>, Self::Error>;
    fn sort_records(
        &self,
        node: &CanonicalLogicalNode,
        records: &mut Vec<UnifiedRecord>,
    ) -> Result<(), Self::Error>;
    fn rank_records(&self, records: &mut Vec<UnifiedRecord>);
    fn project_records(
        &self,
        node: &CanonicalLogicalNode,
        records: Vec<UnifiedRecord>,
    ) -> Result<Vec<UnifiedRecord>, Self::Error>;
    fn query_error(&self, message: String) -> Self::Error;
}

/// Existing scan pushdown bound, saturated for wide SQL values and sums.
/// An ordered scan must retain all candidates for sorting.
pub fn table_scan_cap(query: &TableQuery) -> Option<usize> {
    if !query.order_by.is_empty() {
        return None;
    }
    let limit = usize::try_from(query.limit?).unwrap_or(usize::MAX);
    let offset = usize::try_from(query.offset.unwrap_or(0)).unwrap_or(usize::MAX);
    Some(offset.saturating_add(limit))
}

pub fn execute_table_plan_with_host<H: TablePlanHost>(
    host: &H,
    node: &CanonicalLogicalNode,
    query: &TableQuery,
) -> Result<Vec<UnifiedRecord>, H::Error> {
    execute_table_plan_at_depth(host, node, query, 0)
}

fn execute_table_plan_at_depth<H: TablePlanHost>(
    host: &H,
    node: &CanonicalLogicalNode,
    query: &TableQuery,
    depth: u32,
) -> Result<Vec<UnifiedRecord>, H::Error> {
    if depth > reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
        return Err(host.query_error("canonical table plan exceeds maximum nesting depth".into()));
    }
    let child = || {
        let child = node.children.first().ok_or_else(|| {
            host.query_error(format!(
                "canonical table operator {} is missing its child plan",
                node.operator
            ))
        })?;
        execute_table_plan_at_depth(host, child, query, depth + 1)
    };
    match node.operator.as_str() {
        "table_scan" | "index_seek" | "entity_scan" | "document_path_index_seek" => host.scan(node),
        "filter" | "entity_filter" => {
            if let Some(records) = host.try_filter_lookup()? {
                return Ok(records);
            }
            host.filter_records(child()?, false)
        }
        "document_path_filter" => host.filter_records(child()?, true),
        "sort" | "entity_sort" | "document_sort" => {
            let mut records = child()?;
            host.sort_records(node, &mut records)?;
            Ok(records)
        }
        "offset" | "entity_offset" => {
            let records = child()?;
            let offset = usize::try_from(query.offset.unwrap_or(0)).unwrap_or(usize::MAX);
            Ok(records.into_iter().skip(offset).collect())
        }
        "limit" | "entity_limit" => {
            let records = child()?;
            Ok(match query.limit {
                Some(limit) => records
                    .into_iter()
                    .take(usize::try_from(limit).unwrap_or(usize::MAX))
                    .collect(),
                None => records,
            })
        }
        "entity_search" => child(),
        "entity_topk" => {
            let mut records = child()?;
            host.rank_records(&mut records);
            let limit = node
                .details
                .get("k")
                .and_then(|value| value.parse::<u64>().ok())
                .or(query.limit)
                .map(|value| usize::try_from(value).unwrap_or(usize::MAX));
            Ok(match limit {
                Some(limit) => records.into_iter().take(limit).collect(),
                None => records,
            })
        }
        "projection" | "document_projection" | "entity_projection" => {
            host.project_records(node, child()?)
        }
        other => Err(host.query_error(format!("unsupported canonical table operator {other}"))),
    }
}
