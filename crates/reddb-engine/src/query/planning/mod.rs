//! Canonical query-plan construction. Hosts retain catalog/index/statistics decisions.
pub mod aggregation;
pub mod helpers;
use crate::query::logical_plan::{AccessPathDecision, CanonicalLogicalNode, CardinalityEstimate};
use crate::query::runtime::is_universal_entity_source;
use helpers::*;
use reddb_rql::ast::TableQuery;
use reddb_rql::sql_lowering::{effective_table_filter, effective_table_projections};
use std::collections::BTreeMap;

/// Read-only catalog decisions. Universal access selection and operator order
/// stay canonical; physical availability and cardinality stay in the host.
pub trait TablePlanningHost {
    fn access_path(&self, query: &TableQuery) -> AccessPathDecision;
    fn scan_cardinality(
        &self,
        query: &TableQuery,
        access: &AccessPathDecision,
    ) -> CardinalityEstimate;
    fn uses_document_path_filter(&self, query: &TableQuery) -> bool;
    fn filter_cardinality(&self, query: &TableQuery, document_path: bool) -> CardinalityEstimate;
    fn is_document_projection(&self, query: &TableQuery) -> bool;
}

/// Build the native canonical table operator chain from the canonical AST.
/// The chain has at most six nodes; predicate metadata has a separate depth cap.
pub fn build_table_plan_with_host<H: TablePlanningHost>(
    host: &H,
    query: &TableQuery,
) -> CanonicalLogicalNode {
    let mut details = BTreeMap::new();
    let effective_projection_count = effective_table_projections(query).len();
    let effective_filter = effective_table_filter(query);
    let is_any = is_universal_entity_source(query.table.as_str());
    let access = if is_any {
        AccessPathDecision {
            path: "entity_scan",
            index_hint: None,
            reason: "universal entity space requested".to_string(),
            warning: None,
        }
    } else {
        host.access_path(query)
    };
    let scan_estimate = host.scan_cardinality(query, &access);
    details.insert("access_path".to_string(), access.path.to_string());
    details.insert("access_path_reason".to_string(), access.reason);
    if let Some(warning) = access.warning {
        details.insert("lifecycle_warning".to_string(), warning);
    }
    if let Some(index_hint) = access.index_hint {
        details.insert("index_hint".to_string(), index_hint);
    }
    details.insert("universal".to_string(), is_any.to_string());
    details.insert("filter".to_string(), effective_filter.is_some().to_string());
    details.insert("order_by".to_string(), query.order_by.len().to_string());
    details.insert(
        "limit".to_string(),
        query
            .limit
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string()),
    );
    details.insert(
        "offset".to_string(),
        query
            .offset
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string()),
    );
    details.insert(
        "projection_count".to_string(),
        effective_projection_count.to_string(),
    );
    let mut node = CanonicalLogicalNode {
        operator: access.path.to_string(),
        source: Some(query.table.clone()),
        details,
        estimated_rows: scan_estimate.rows,
        estimated_selectivity: scan_estimate.selectivity,
        estimated_confidence: scan_estimate.confidence,
        operator_cost: operator_cost_estimate(access.path, scan_estimate.rows),
        children: Vec::new(),
    };
    if let Some(filter) = &effective_filter {
        let document_path_filter = host.uses_document_path_filter(query);
        let filter_estimate = host.filter_cardinality(query, document_path_filter);
        node = wrap_unary_plan(
            if document_path_filter {
                "document_path_filter"
            } else if is_any {
                "entity_filter"
            } else {
                "filter"
            },
            btree_details([("predicate", filter_summary(filter))]),
            Some(filter_estimate),
            node,
        );
    }
    if !query.order_by.is_empty() || requires_implicit_entity_sort(query) {
        node = wrap_unary_plan(
            if uses_document_path_sort(query) {
                "document_sort"
            } else if is_any {
                "entity_sort"
            } else {
                "sort"
            },
            btree_details([(
                "keys",
                if query.order_by.is_empty() && is_any {
                    "_score desc, _entity_id asc".to_string()
                } else {
                    order_by_summary(&query.order_by)
                },
            )]),
            None,
            node,
        );
    }
    if query.offset.is_some() {
        let offset_estimate = offset_cardinality(
            node.estimated_rows,
            node.estimated_selectivity,
            node.estimated_confidence,
            query.offset,
        );
        node = wrap_unary_plan(
            if is_any { "entity_offset" } else { "offset" },
            btree_details([(
                "offset",
                query
                    .offset
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "none".to_string()),
            )]),
            Some(offset_estimate),
            node,
        );
    }
    if query.limit.is_some() {
        let limit_estimate = limit_cardinality(
            node.estimated_rows,
            node.estimated_selectivity,
            node.estimated_confidence,
            query.limit,
        );
        node = wrap_unary_plan(
            if is_any { "entity_limit" } else { "limit" },
            btree_details([(
                "limit",
                query
                    .limit
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "none".to_string()),
            )]),
            Some(limit_estimate),
            node,
        );
    }
    if table_has_explicit_projection(query) {
        let document_projection = host.is_document_projection(query);
        node = wrap_unary_plan(
            if document_projection {
                "document_projection"
            } else if is_any {
                "entity_projection"
            } else {
                "projection"
            },
            btree_details([("columns", table_projection_summary(query))]),
            None,
            node,
        );
    }
    node
}
