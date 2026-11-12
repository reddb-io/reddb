//! Native canonical table-plan traversal. Hosts own read admission and effects.
use crate::query::{logical_plan::CanonicalLogicalNode, unified::UnifiedRecord};
use reddb_rql::ast::TableQuery;

/// A scan returns already-admitted records. Native MVCC, tenancy/RLS, resource
/// guards, index/replica paths, computed sort keys, windows and output policy
/// stay in the host. Callback errors propagate without conversion.
pub trait TablePlanHost {
    type Error;
    fn scan(&self, node: &CanonicalLogicalNode) -> Result<Vec<UnifiedRecord>, Self::Error>;
    fn visit_scan(
        &self,
        node: &CanonicalLogicalNode,
        visit: &mut dyn FnMut(UnifiedRecord) -> Result<bool, Self::Error>,
    ) -> Result<(), Self::Error> {
        for record in self.scan(node)? {
            if !visit(record)? {
                break;
            }
        }
        Ok(())
    }
    fn row_operators(&self) -> bool {
        false
    }
    fn admit_materialization(&self) -> Result<(), Self::Error> {
        Ok(())
    }
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
    let mut records = Vec::new();
    visit_table_plan_with_host(host, node, query, &mut |record| {
        records.push(record);
        Ok(true)
    })?;
    Ok(records)
}

pub fn visit_table_plan_with_host<H: TablePlanHost>(
    host: &H,
    node: &CanonicalLogicalNode,
    query: &TableQuery,
    visit: &mut dyn FnMut(UnifiedRecord) -> Result<bool, H::Error>,
) -> Result<(), H::Error> {
    visit_table_plan_at_depth(host, node, query, 0, visit)
}

fn visit_table_plan_at_depth<H: TablePlanHost>(
    host: &H,
    node: &CanonicalLogicalNode,
    query: &TableQuery,
    depth: u32,
    visit: &mut dyn FnMut(UnifiedRecord) -> Result<bool, H::Error>,
) -> Result<(), H::Error> {
    if depth > reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
        return Err(host.query_error("canonical table plan exceeds maximum nesting depth".into()));
    }
    let mut stopped = false;
    let mut emit = |record| {
        if stopped {
            return Ok(false);
        }
        stopped = !visit(record)?;
        Ok(!stopped)
    };
    let child_node = || {
        node.children.first().ok_or_else(|| {
            host.query_error(format!(
                "canonical table operator {} is missing its child plan",
                node.operator
            ))
        })
    };
    let scan_child = &mut |sink: &mut dyn FnMut(UnifiedRecord) -> Result<bool, H::Error>| {
        visit_table_plan_at_depth(host, child_node()?, query, depth + 1, sink)
    };
    let collect_child = &mut || {
        host.admit_materialization()?;
        let mut rows = Vec::new();
        scan_child(&mut |row| {
            rows.push(row);
            Ok(true)
        })?;
        Ok::<_, H::Error>(rows)
    };
    match node.operator.as_str() {
        "table_scan" | "index_seek" | "entity_scan" | "document_path_index_seek" => {
            host.visit_scan(node, &mut emit)
        }
        "filter" | "entity_filter" | "document_path_filter" => {
            let document = node.operator == "document_path_filter";
            if !document {
                if let Some(rows) = host.try_filter_lookup()? {
                    for row in rows {
                        if !emit(row)? {
                            break;
                        }
                    }
                    return Ok(());
                }
            }
            if host.row_operators() {
                scan_child(&mut |row| {
                    for row in host.filter_records(vec![row], document)? {
                        if !emit(row)? {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                })
            } else {
                for row in host.filter_records(collect_child()?, document)? {
                    if !emit(row)? {
                        break;
                    }
                }
                Ok(())
            }
        }
        "sort" | "entity_sort" | "document_sort" => {
            let mut rows = collect_child()?;
            host.sort_records(node, &mut rows)?;
            for row in rows {
                if !emit(row)? {
                    break;
                }
            }
            Ok(())
        }
        "offset" | "entity_offset" => {
            let mut skipped = 0u64;
            let offset = query.offset.unwrap_or(0);
            scan_child(&mut |row| {
                if skipped < offset {
                    skipped += 1;
                    Ok(true)
                } else {
                    emit(row)
                }
            })
        }
        "limit" | "entity_limit" => {
            let limit = query.limit.unwrap_or(u64::MAX);
            if limit == 0 {
                if !host.row_operators() {
                    let _ = collect_child()?;
                }
                return Ok(());
            }
            let mut count = 0u64;
            scan_child(&mut |row| {
                count += 1;
                Ok(emit(row)? && count < limit)
            })
        }
        "entity_search" => scan_child(&mut emit),
        "entity_topk" => {
            let mut rows = collect_child()?;
            host.rank_records(&mut rows);
            let limit = node
                .details
                .get("k")
                .and_then(|value| value.parse::<u64>().ok())
                .or(query.limit)
                .unwrap_or(u64::MAX);
            for row in rows
                .into_iter()
                .take(usize::try_from(limit).unwrap_or(usize::MAX))
            {
                if !emit(row)? {
                    break;
                }
            }
            Ok(())
        }
        "projection" | "document_projection" | "entity_projection" => {
            if host.row_operators() {
                scan_child(&mut |row| {
                    for row in host.project_records(node, vec![row])? {
                        if !emit(row)? {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                })
            } else {
                for row in host.project_records(node, collect_child()?)? {
                    if !emit(row)? {
                        break;
                    }
                }
                Ok(())
            }
        }
        other => Err(host.query_error(format!("unsupported canonical table operator {other}"))),
    }
}
