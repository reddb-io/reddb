use std::collections::BTreeMap;

#[path = "logical_helpers.rs"]
mod logical_helpers;

use crate::storage::query::ast::{
    FieldRef, Filter, FusionStrategy, JoinQuery, JoinType, OrderByClause, Projection, QueryExpr,
    TableQuery, VectorSource,
};
use crate::storage::query::is_universal_entity_source as is_universal_query_source;
use crate::storage::schema::Value;
use crate::storage::RedDB;

use super::{AccessPathDecision, CanonicalLogicalNode, CardinalityEstimate, CostEstimator};
use logical_helpers::*;

pub(super) fn logical_plan_node_with_catalog(db: &RedDB, expr: &QueryExpr) -> CanonicalLogicalNode {
    match expr {
        QueryExpr::Table(query) => {
            let mut details = BTreeMap::new();
            let is_any = is_universal_entity_source(query.table.as_str());
            let access = if is_any {
                AccessPathDecision {
                    path: "entity_scan",
                    index_hint: None,
                    reason: "universal entity space requested".to_string(),
                    warning: None,
                }
            } else {
                table_access_path_hint(db, query)
            };
            let scan_estimate = if is_any {
                universal_entity_cardinality(db)
            } else if access.path == "document_path_index_seek" {
                document_index_cardinality(db, query.table.as_str())
            } else {
                base_collection_cardinality(db, query.table.as_str())
            };
            details.insert("access_path".to_string(), access.path.to_string());
            details.insert("access_path_reason".to_string(), access.reason);
            if let Some(warning) = access.warning {
                details.insert("lifecycle_warning".to_string(), warning);
            }
            if let Some(index_hint) = access.index_hint {
                details.insert("index_hint".to_string(), index_hint);
            }
            details.insert("universal".to_string(), is_any.to_string());
            details.insert("filter".to_string(), query.filter.is_some().to_string());
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
                query.columns.len().to_string(),
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
            if let Some(filter) = &query.filter {
                let document_path_filter = uses_document_path_filter(db, query);
                let filter_estimate = if document_path_filter {
                    document_filtered_cardinality(query)
                } else {
                    table_filtered_cardinality(query)
                };
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
            if has_explicit_projection(&query.columns) {
                let document_projection = is_document_projection(db, query);
                node = wrap_unary_plan(
                    if document_projection {
                        "document_projection"
                    } else if is_any {
                        "entity_projection"
                    } else {
                        "projection"
                    },
                    btree_details([("columns", projection_summary(&query.columns))]),
                    None,
                    node,
                );
            }
            node
        }
        QueryExpr::Graph(query) => {
            let mut details = BTreeMap::new();
            let access = graph_access_path_hint(db);
            let estimate = estimate_cardinality(expr);
            details.insert("access_path".to_string(), access.path.to_string());
            details.insert("access_path_reason".to_string(), access.reason);
            if let Some(warning) = access.warning {
                details.insert("lifecycle_warning".to_string(), warning);
            }
            if let Some(index_hint) = access.index_hint {
                details.insert("index_hint".to_string(), index_hint);
            }
            details.insert("nodes".to_string(), query.pattern.nodes.len().to_string());
            details.insert("edges".to_string(), query.pattern.edges.len().to_string());
            details.insert("filter".to_string(), query.filter.is_some().to_string());
            details.insert("return_count".to_string(), query.return_.len().to_string());
            let mut node = CanonicalLogicalNode {
                operator: access.path.to_string(),
                source: None,
                details,
                estimated_rows: estimate.rows,
                estimated_selectivity: estimate.selectivity,
                estimated_confidence: estimate.confidence,
                operator_cost: operator_cost_estimate(access.path, estimate.rows),
                children: Vec::new(),
            };
            if let Some(filter) = &query.filter {
                node = wrap_unary_plan(
                    "filter",
                    btree_details([("predicate", filter_summary(filter))]),
                    None,
                    node,
                );
            }
            if has_explicit_projection(&query.return_) {
                node = wrap_unary_plan(
                    "projection",
                    btree_details([("columns", projection_summary(&query.return_))]),
                    None,
                    node,
                );
            }
            node
        }
        QueryExpr::Join(query) => {
            let mut details = BTreeMap::new();
            let estimate = estimate_cardinality(expr);
            let join_strategy = join_strategy_hint(query);
            details.insert(
                "join_type".to_string(),
                match query.join_type {
                    JoinType::Inner => "inner",
                    JoinType::LeftOuter => "left_outer",
                    JoinType::RightOuter => "right_outer",
                }
                .to_string(),
            );
            details.insert(
                "left_field".to_string(),
                field_ref_canonical_string(&query.on.left_field),
            );
            details.insert(
                "right_field".to_string(),
                field_ref_canonical_string(&query.on.right_field),
            );
            details.insert(
                "left_expr_kind".to_string(),
                query_expr_kind(query.left.as_ref()).to_string(),
            );
            details.insert(
                "right_expr_kind".to_string(),
                query_expr_kind(query.right.as_ref()).to_string(),
            );
            details.insert("join_strategy".to_string(), join_strategy.to_string());
            details.insert(
                "join_strategy_reason".to_string(),
                join_strategy_reason(query).to_string(),
            );
            details.insert("filter".to_string(), query.filter.is_some().to_string());
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
            details.insert("return_count".to_string(), query.return_.len().to_string());
            let mut node = CanonicalLogicalNode {
                operator: "join".to_string(),
                source: None,
                details,
                estimated_rows: estimate.rows,
                estimated_selectivity: estimate.selectivity,
                estimated_confidence: estimate.confidence,
                operator_cost: operator_cost_estimate("join", estimate.rows),
                children: vec![
                    logical_plan_node_with_catalog(db, query.left.as_ref()),
                    logical_plan_node_with_catalog(db, query.right.as_ref()),
                ],
            };
            if let Some(filter) = &query.filter {
                node = wrap_unary_plan(
                    "filter",
                    btree_details([("predicate", filter_summary(filter))]),
                    None,
                    node,
                );
            }
            if !query.order_by.is_empty() || requires_implicit_entity_sort_join(query) {
                node = wrap_unary_plan(
                    if uses_document_path_join_sort(query) {
                        "document_sort"
                    } else if requires_implicit_entity_sort_join(query) {
                        "entity_sort"
                    } else {
                        "sort"
                    },
                    btree_details([(
                        "keys",
                        if query.order_by.is_empty() && requires_implicit_entity_sort_join(query) {
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
                    "offset",
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
                    "limit",
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
            if has_explicit_projection(&query.return_) {
                node = wrap_unary_plan(
                    "projection",
                    btree_details([("columns", projection_summary(&query.return_))]),
                    None,
                    node,
                );
            }
            node
        }
        QueryExpr::Path(query) => {
            let mut details = BTreeMap::new();
            let access = graph_access_path_hint(db);
            let estimate = estimate_cardinality(expr);
            details.insert("access_path".to_string(), access.path.to_string());
            details.insert("access_path_reason".to_string(), access.reason);
            if let Some(warning) = access.warning {
                details.insert("lifecycle_warning".to_string(), warning);
            }
            if let Some(index_hint) = access.index_hint {
                details.insert("index_hint".to_string(), index_hint);
            }
            details.insert("max_length".to_string(), query.max_length.to_string());
            details.insert("via_count".to_string(), query.via.len().to_string());
            details.insert("filter".to_string(), query.filter.is_some().to_string());
            details.insert("return_count".to_string(), query.return_.len().to_string());
            let mut node = CanonicalLogicalNode {
                operator: access.path.to_string(),
                source: None,
                details,
                estimated_rows: estimate.rows,
                estimated_selectivity: estimate.selectivity,
                estimated_confidence: estimate.confidence,
                operator_cost: operator_cost_estimate(access.path, estimate.rows),
                children: Vec::new(),
            };
            if let Some(filter) = &query.filter {
                node = wrap_unary_plan(
                    "filter",
                    btree_details([("predicate", filter_summary(filter))]),
                    None,
                    node,
                );
            }
            if has_explicit_projection(&query.return_) {
                node = wrap_unary_plan(
                    "projection",
                    btree_details([("columns", projection_summary(&query.return_))]),
                    None,
                    node,
                );
            }
            node
        }
        QueryExpr::Vector(query) => {
            let mut details = BTreeMap::new();
            let access = vector_access_path_hint(db, query.collection.as_str());
            let scan_estimate = base_collection_cardinality(db, query.collection.as_str());
            details.insert("access_path".to_string(), access.path.to_string());
            details.insert("access_path_reason".to_string(), access.reason);
            if let Some(warning) = access.warning {
                details.insert("lifecycle_warning".to_string(), warning);
            }
            if let Some(index_hint) = access.index_hint {
                details.insert("index_hint".to_string(), index_hint);
            }
            details.insert("k".to_string(), query.k.to_string());
            details.insert("filter".to_string(), query.filter.is_some().to_string());
            details.insert(
                "metric".to_string(),
                query
                    .metric
                    .map(|metric| format!("{metric:?}"))
                    .unwrap_or_else(|| "default".to_string()),
            );
            details.insert(
                "threshold".to_string(),
                query
                    .threshold
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "none".to_string()),
            );
            details.insert(
                "vector_source".to_string(),
                match &query.query_vector {
                    VectorSource::Literal(values) => format!("literal({})", values.len()),
                    VectorSource::Text(_) => "text".to_string(),
                    VectorSource::Reference {
                        collection,
                        vector_id,
                    } => {
                        format!("reference({collection}:{vector_id})")
                    }
                    VectorSource::Subquery(_) => "subquery".to_string(),
                },
            );
            let mut children = Vec::new();
            if let VectorSource::Subquery(expr) = &query.query_vector {
                children.push(logical_plan_node_with_catalog(db, expr.as_ref()));
            }
            let mut node = CanonicalLogicalNode {
                operator: access.path.to_string(),
                source: Some(query.collection.clone()),
                details,
                estimated_rows: scan_estimate.rows,
                estimated_selectivity: scan_estimate.selectivity,
                estimated_confidence: scan_estimate.confidence,
                operator_cost: operator_cost_estimate(access.path, scan_estimate.rows),
                children,
            };
            if query.filter.is_some() {
                let estimate = heuristic_selectivity(&node, 0.5, 0.75);
                node = wrap_unary_plan(
                    "metadata_filter",
                    btree_details([("predicate", "present".to_string())]),
                    Some(estimate),
                    node,
                );
            }
            if query.threshold.is_some() {
                let estimate = heuristic_selectivity(&node, 0.5, 0.8);
                node = wrap_unary_plan(
                    "similarity_threshold",
                    btree_details([(
                        "threshold",
                        query
                            .threshold
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "none".to_string()),
                    )]),
                    Some(estimate),
                    node,
                );
            }
            let topk_estimate = limit_cardinality(
                node.estimated_rows,
                node.estimated_selectivity,
                node.estimated_confidence,
                Some(query.k as u64),
            );
            node = wrap_unary_plan(
                "topk",
                btree_details([("k", query.k.to_string())]),
                Some(topk_estimate),
                node,
            );
            if query.include_vectors || query.include_metadata {
                node = wrap_unary_plan(
                    "projection",
                    btree_details([
                        ("include_vectors", query.include_vectors.to_string()),
                        ("include_metadata", query.include_metadata.to_string()),
                    ]),
                    None,
                    node,
                );
            }
            node
        }
        QueryExpr::Hybrid(query) => {
            let mut details = BTreeMap::new();
            let estimate = estimate_cardinality(expr);
            details.insert(
                "fusion".to_string(),
                match &query.fusion {
                    FusionStrategy::Rerank { weight } => format!("rerank({weight})"),
                    FusionStrategy::FilterThenSearch => "filter_then_search".to_string(),
                    FusionStrategy::SearchThenFilter => "search_then_filter".to_string(),
                    FusionStrategy::RRF { k } => format!("rrf({k})"),
                    FusionStrategy::Intersection => "intersection".to_string(),
                    FusionStrategy::Union {
                        structured_weight,
                        vector_weight,
                    } => format!("union({structured_weight},{vector_weight})"),
                },
            );
            details.insert(
                "limit".to_string(),
                query
                    .limit
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "none".to_string()),
            );
            let node = CanonicalLogicalNode {
                operator: "hybrid_fusion".to_string(),
                source: None,
                details,
                estimated_rows: estimate.rows,
                estimated_selectivity: estimate.selectivity,
                estimated_confidence: estimate.confidence,
                operator_cost: operator_cost_estimate("hybrid_fusion", estimate.rows),
                children: vec![
                    logical_plan_node_with_catalog(db, query.structured.as_ref()),
                    logical_plan_node_with_catalog(db, &QueryExpr::Vector(query.vector.clone())),
                ],
            };
            if is_universal_query_expr(query.structured.as_ref()) {
                let mut node = wrap_unary_plan(
                    "entity_search",
                    btree_details([
                        ("search_mode", "hybrid".to_string()),
                        ("universal", "true".to_string()),
                        ("ranking_mode", hybrid_ranking_mode(&query.fusion)),
                    ]),
                    Some(estimate),
                    node,
                );
                if let Some(limit) = query.limit {
                    let topk_estimate = limit_cardinality(
                        node.estimated_rows,
                        node.estimated_selectivity,
                        node.estimated_confidence,
                        Some(limit as u64),
                    );
                    node = wrap_unary_plan(
                        "entity_topk",
                        btree_details([
                            ("k", limit.to_string()),
                            ("ranking_mode", hybrid_ranking_mode(&query.fusion)),
                        ]),
                        Some(topk_estimate),
                        node,
                    );
                }
                node
            } else {
                node
            }
        }
        // DML/DDL statements produce a simple passthrough plan node
        QueryExpr::Insert(_)
        | QueryExpr::Update(_)
        | QueryExpr::Delete(_)
        | QueryExpr::CreateTable(_)
        | QueryExpr::DropTable(_)
        | QueryExpr::AlterTable(_)
        | QueryExpr::GraphCommand(_)
        | QueryExpr::SearchCommand(_)
        | QueryExpr::Ask(_) => {
            let mut details = BTreeMap::new();
            details.insert("type".to_string(), "dml_ddl".to_string());
            CanonicalLogicalNode {
                operator: "dml_ddl".to_string(),
                source: None,
                details,
                estimated_rows: 0.0,
                estimated_selectivity: 1.0,
                estimated_confidence: 1.0,
                operator_cost: 1.0,
                children: Vec::new(),
            }
        }
    }
}
