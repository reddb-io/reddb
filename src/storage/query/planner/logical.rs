use std::collections::BTreeMap;

use crate::storage::query::ast::{
    FieldRef, Filter, FusionStrategy, JoinQuery, JoinType, OrderByClause, Projection, QueryExpr,
    TableQuery, VectorSource,
};
use crate::storage::query::is_universal_entity_source as is_universal_query_source;
use crate::storage::schema::Value;
use crate::storage::RedDB;

use super::{CanonicalLogicalNode, CardinalityEstimate};

#[derive(Debug, Clone)]
struct AccessPathDecision {
    path: &'static str,
    index_hint: Option<String>,
    reason: String,
    warning: Option<String>,
}

pub(super) fn logical_plan_node_with_catalog(
    db: &RedDB,
    expr: &QueryExpr,
) -> CanonicalLogicalNode {
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
                query.limit
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "none".to_string()),
            );
            details.insert(
                "offset".to_string(),
                query.offset
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "none".to_string()),
            );
            details.insert("projection_count".to_string(), query.columns.len().to_string());
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
                let filter_estimate = if uses_document_path_filter(db, query) {
                    document_filtered_cardinality(query)
                } else {
                    table_filtered_cardinality(query)
                };
                node = wrap_unary_plan(
                    if is_any {
                        "entity_filter"
                    } else if uses_document_path_filter(db, query) {
                        "document_path_filter"
                    } else {
                        "filter"
                    },
                    btree_details([("predicate", filter_summary(filter))]),
                    Some(filter_estimate),
                    node,
                );
            }
            if !query.order_by.is_empty() {
                node = wrap_unary_plan(
                    if is_any { "entity_sort" } else { "sort" },
                    btree_details([("keys", order_by_summary(&query.order_by))]),
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
                        query.offset
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
                    btree_details([
                        (
                            "limit",
                            query.limit
                                .map(|value| value.to_string())
                                .unwrap_or_else(|| "none".to_string()),
                        ),
                    ]),
                    Some(limit_estimate),
                    node,
                );
            }
            if has_explicit_projection(&query.columns) {
                node = wrap_unary_plan(
                    if is_any {
                        "entity_projection"
                    } else if is_document_projection(db, query) {
                        "document_projection"
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
            details.insert(
                "join_strategy".to_string(),
                join_strategy.to_string(),
            );
            details.insert(
                "join_strategy_reason".to_string(),
                join_strategy_reason(query).to_string(),
            );
            CanonicalLogicalNode {
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
            }
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
                query.metric
                    .map(|metric| format!("{metric:?}"))
                    .unwrap_or_else(|| "default".to_string()),
            );
            details.insert(
                "threshold".to_string(),
                query.threshold
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "none".to_string()),
            );
            details.insert(
                "vector_source".to_string(),
                match &query.query_vector {
                    VectorSource::Literal(values) => format!("literal({})", values.len()),
                    VectorSource::Text(_) => "text".to_string(),
                    VectorSource::Reference { collection, vector_id } => {
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
                        query.threshold
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
                query.limit
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
                        Some(limit),
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
    }
}

fn wrap_unary_plan(
    operator: &str,
    details: BTreeMap<String, String>,
    estimate: Option<CardinalityEstimate>,
    child: CanonicalLogicalNode,
) -> CanonicalLogicalNode {
    let estimate = estimate.unwrap_or_else(|| CardinalityEstimate {
        rows: child.estimated_rows,
        selectivity: child.estimated_selectivity,
        confidence: child.estimated_confidence,
    });
    CanonicalLogicalNode {
        operator: operator.to_string(),
        source: None,
        details,
        estimated_rows: estimate.rows,
        estimated_selectivity: estimate.selectivity,
        estimated_confidence: estimate.confidence,
        operator_cost: operator_cost_estimate(operator, estimate.rows),
        children: vec![child],
    }
}

fn btree_details<const N: usize>(pairs: [(&str, String); N]) -> BTreeMap<String, String> {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

fn estimate_cardinality(expr: &QueryExpr) -> CardinalityEstimate {
    CostEstimator::new().estimate_cardinality(expr)
}

fn base_collection_cardinality(db: &RedDB, collection: &str) -> CardinalityEstimate {
    let rows = db
        .catalog_snapshot()
        .stats_by_collection
        .get(collection)
        .map(|stats| stats.entities as f64)
        .unwrap_or(1000.0);
    CardinalityEstimate {
        rows,
        selectivity: 1.0,
        confidence: 0.85,
    }
}

fn universal_entity_cardinality(db: &RedDB) -> CardinalityEstimate {
    CardinalityEstimate {
        rows: db.catalog_snapshot().total_entities as f64,
        selectivity: 1.0,
        confidence: 0.7,
    }
}

fn table_filtered_cardinality(query: &TableQuery) -> CardinalityEstimate {
    let mut filtered = query.clone();
    filtered.limit = None;
    filtered.offset = None;
    estimate_cardinality(&QueryExpr::Table(filtered))
}

fn document_index_cardinality(db: &RedDB, collection: &str) -> CardinalityEstimate {
    let base = base_collection_cardinality(db, collection);
    CardinalityEstimate {
        rows: (base.rows * 0.35).max(1.0),
        selectivity: (base.selectivity * 0.35).min(1.0),
        confidence: (base.confidence * 0.95).min(0.98),
    }
}

fn document_filtered_cardinality(query: &TableQuery) -> CardinalityEstimate {
    let mut filtered = query.clone();
    filtered.limit = None;
    filtered.offset = None;
    let estimate = estimate_cardinality(&QueryExpr::Table(filtered));
    CardinalityEstimate {
        rows: (estimate.rows * 0.5).max(1.0),
        selectivity: (estimate.selectivity * 0.5).min(1.0),
        confidence: (estimate.confidence * 0.95).min(0.98),
    }
}

fn limit_cardinality(
    rows: f64,
    selectivity: f64,
    confidence: f64,
    limit: Option<u64>,
) -> CardinalityEstimate {
    let limited_rows = limit.map(|value| rows.min(value as f64)).unwrap_or(rows);
    CardinalityEstimate {
        rows: limited_rows,
        selectivity,
        confidence,
    }
}

fn offset_cardinality(
    rows: f64,
    selectivity: f64,
    confidence: f64,
    offset: Option<u64>,
) -> CardinalityEstimate {
    let remaining_rows = offset
        .map(|value| (rows - value as f64).max(0.0))
        .unwrap_or(rows);
    CardinalityEstimate {
        rows: remaining_rows,
        selectivity,
        confidence,
    }
}

fn heuristic_selectivity(
    node: &CanonicalLogicalNode,
    factor: f64,
    confidence_factor: f64,
) -> CardinalityEstimate {
    CardinalityEstimate {
        rows: (node.estimated_rows * factor).max(1.0),
        selectivity: node.estimated_selectivity * factor,
        confidence: node.estimated_confidence * confidence_factor,
    }
}

fn operator_cost_estimate(operator: &str, rows: f64) -> f64 {
    let base = rows.max(1.0);
    let multiplier = match operator {
        "entity_scan" => 1.4,
        "table_scan" => 1.0,
        "index_seek" => 0.35,
        "document_path_index_seek" => 0.42,
        "graph_scan" => 1.25,
        "graph_adjacency_expand" => 0.55,
        "vector_exact_scan" => 1.6,
        "vector_ann_hnsw" => 0.38,
        "vector_ann_ivf" => 0.46,
        "entity_search" => 0.72,
        "entity_topk" => 0.16,
        "filter" => 0.25,
        "entity_filter" => 0.28,
        "document_path_filter" => 0.33,
        "sort" => 0.8,
        "entity_sort" => 0.82,
        "limit" => 0.08,
        "entity_limit" => 0.1,
        "offset" => 0.05,
        "entity_offset" => 0.06,
        "projection" => 0.12,
        "entity_projection" => 0.15,
        "document_projection" => 0.18,
        "metadata_filter" => 0.3,
        "similarity_threshold" => 0.2,
        "topk" => 0.14,
        "join" => 1.1,
        "hybrid_fusion" => 0.9,
        _ => 0.5,
    };
    base * multiplier
}

fn table_access_path_hint(db: &RedDB, query: &TableQuery) -> AccessPathDecision {
    let collection = query.table.as_str();
    let all_indexes = db.index_statuses();
    let mut indexes = all_indexes
        .clone()
        .into_iter()
        .filter(|status| {
            status.collection.as_deref() == Some(collection)
                && status.declared
                && status.operational
                && status.enabled
        })
        .collect::<Vec<_>>();
    indexes.sort_by(|left, right| left.kind.cmp(&right.kind).then(left.name.cmp(&right.name)));

    if query.filter.is_some()
        && is_document_like_collection(db, collection)
        && let Some(index) = indexes
            .iter()
            .find(|status| status.kind == "document.pathvalue")
    {
        return AccessPathDecision {
            path: "document_path_index_seek",
            index_hint: Some(index.name.clone()),
            reason: "document path/value index is declared, operational, and enabled".to_string(),
            warning: None,
        };
    }

    if let Some(index) = indexes
        .iter()
        .find(|status| status.kind == "btree" || status.kind == "text.fulltext")
    {
        return AccessPathDecision {
            path: "index_seek",
            index_hint: Some(index.name.clone()),
            reason: "structured or lexical index is declared, operational, and enabled".to_string(),
            warning: None,
        };
    }

    let warning = all_indexes
        .into_iter()
        .find(|status| status.collection.as_deref() == Some(collection))
        .and_then(index_lifecycle_warning);

    AccessPathDecision {
        path: "table_scan",
        index_hint: None,
        reason: "no usable declared operational index was available".to_string(),
        warning,
    }
}

fn uses_document_path_filter(db: &RedDB, query: &TableQuery) -> bool {
    query.filter.is_some()
        && !is_universal_entity_source(query.table.as_str())
        && is_document_like_collection(db, query.table.as_str())
        && db.index_statuses().into_iter().any(|status| {
            status.collection.as_deref() == Some(query.table.as_str())
                && status.kind == "document.pathvalue"
                && status.declared
                && status.operational
                && status.enabled
        })
}

fn is_document_projection(db: &RedDB, query: &TableQuery) -> bool {
    has_explicit_projection(&query.columns)
        && !is_universal_entity_source(query.table.as_str())
        && is_document_like_collection(db, query.table.as_str())
}

fn is_document_like_collection(db: &RedDB, collection: &str) -> bool {
    db.catalog_model_snapshot()
        .collections
        .into_iter()
        .find(|descriptor| descriptor.name == collection)
        .map(|descriptor| matches!(descriptor.model, crate::catalog::CollectionModel::Document | crate::catalog::CollectionModel::Mixed))
        .unwrap_or(false)
}

fn is_universal_entity_source(table: &str) -> bool {
    is_universal_query_source(table)
}

fn is_universal_query_expr(expr: &QueryExpr) -> bool {
    match expr {
        QueryExpr::Table(query) => is_universal_entity_source(query.table.as_str()),
        QueryExpr::Join(query) => is_universal_query_expr(query.left.as_ref()),
        QueryExpr::Hybrid(query) => is_universal_query_expr(query.structured.as_ref()),
        _ => false,
    }
}

fn hybrid_ranking_mode(fusion: &FusionStrategy) -> String {
    match fusion {
        FusionStrategy::Rerank { .. } => "rerank".to_string(),
        FusionStrategy::FilterThenSearch => "filter_then_search".to_string(),
        FusionStrategy::SearchThenFilter => "search_then_filter".to_string(),
        FusionStrategy::RRF { .. } => "rrf".to_string(),
        FusionStrategy::Intersection => "intersection".to_string(),
        FusionStrategy::Union { .. } => "union".to_string(),
    }
}

fn vector_access_path_hint(db: &RedDB, collection: &str) -> AccessPathDecision {
    let all_indexes = db.index_statuses();
    let indexes = all_indexes
        .clone()
        .into_iter()
        .filter(|status| {
            status.collection.as_deref() == Some(collection)
                && status.declared
                && status.operational
                && status.enabled
        })
        .collect::<Vec<_>>();

    if let Some(index) = indexes.iter().find(|status| status.kind == "vector.hnsw") {
        return AccessPathDecision {
            path: "vector_ann_hnsw",
            index_hint: Some(index.name.clone()),
            reason: "HNSW ANN index is declared, operational, and enabled".to_string(),
            warning: None,
        };
    }
    if let Some(index) = indexes
        .iter()
        .find(|status| status.kind == "vector.inverted")
    {
        return AccessPathDecision {
            path: "vector_ann_ivf",
            index_hint: Some(index.name.clone()),
            reason: "IVF ANN index is declared, operational, and enabled".to_string(),
            warning: None,
        };
    }

    AccessPathDecision {
        path: "vector_exact_scan",
        index_hint: None,
        reason: "no usable ANN index was available; planner fell back to exact scan".to_string(),
        warning: all_indexes
            .into_iter()
            .find(|status| status.collection.as_deref() == Some(collection))
            .and_then(index_lifecycle_warning),
    }
}

fn graph_access_path_hint(db: &RedDB) -> AccessPathDecision {
    if let Some(index) = db
        .index_statuses()
        .into_iter()
        .find(|status| {
            status.kind == "graph.adjacency"
                && status.declared
                && status.operational
                && status.enabled
        })
    {
        return AccessPathDecision {
            path: "graph_adjacency_expand",
            index_hint: Some(index.name),
            reason: "graph adjacency index is declared, operational, and enabled".to_string(),
            warning: None,
        };
    }

    AccessPathDecision {
        path: "graph_scan",
        index_hint: None,
        reason: "no usable graph adjacency index was available".to_string(),
        warning: db
            .index_statuses()
            .into_iter()
            .find(|status| status.kind == "graph.adjacency")
            .and_then(index_lifecycle_warning),
    }
}

fn index_lifecycle_warning(
    status: crate::catalog::CatalogIndexStatus,
) -> Option<String> {
    match status.lifecycle_state.as_str() {
        "disabled" => Some(format!("index {} is disabled", status.name)),
        "stale" => Some(format!("index {} is stale", status.name)),
        "failed" => Some(format!("index {} is failed", status.name)),
        "building" => Some(format!("index {} is still building", status.name)),
        "declared" => Some(format!("index {} is declared but not operational", status.name)),
        "orphaned-operational" => Some(format!(
            "index {} is operational without matching declaration",
            status.name
        )),
        _ => None,
    }
}

fn join_strategy_hint(query: &JoinQuery) -> &'static str {
    match (&*query.left, &*query.right) {
        (QueryExpr::Table(_), QueryExpr::Table(_)) => "indexed_nested_loop",
        (QueryExpr::Table(_), QueryExpr::Graph(_) | QueryExpr::Path(_)) => "graph_lookup_join",
        _ => "nested_loop",
    }
}

fn join_strategy_reason(query: &JoinQuery) -> &'static str {
    match (&*query.left, &*query.right) {
        (QueryExpr::Table(_), QueryExpr::Table(_)) => {
            "both sides are structured sources, so the planner prefers the indexed nested-loop family"
        }
        (QueryExpr::Table(_), QueryExpr::Graph(_) | QueryExpr::Path(_)) => {
            "the right side expands graph/path state, so the planner keeps a graph lookup join shape"
        }
        _ => {
            "the current canonical runtime only supports the nested-loop family for this join expression pair"
        }
    }
}

fn has_explicit_projection(projections: &[Projection]) -> bool {
    !projections.is_empty() && !projections.iter().all(|projection| matches!(projection, Projection::All))
}

fn projection_summary(projections: &[Projection]) -> String {
    projections
        .iter()
        .map(|projection| match projection {
            Projection::All => "*".to_string(),
            Projection::Column(name) => name.clone(),
            Projection::Alias(name, alias) => format!("{name} AS {alias}"),
            Projection::Function(name, args) => format!("{name}({})", args.len()),
            Projection::Expression(_, alias) => alias.clone().unwrap_or_else(|| "expr".to_string()),
            Projection::Field(field, alias) => alias
                .clone()
                .unwrap_or_else(|| field_ref_summary(field)),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn order_by_summary(order_by: &[OrderByClause]) -> String {
    order_by
        .iter()
        .map(|clause| {
            format!(
                "{} {} nulls_{}",
                field_ref_summary(&clause.field),
                if clause.ascending { "asc" } else { "desc" },
                if clause.nulls_first { "first" } else { "last" }
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn filter_summary(filter: &Filter) -> String {
    match filter {
        Filter::Compare { field, op, value } => {
            format!("{} {} {}", field_ref_summary(field), op, summarize_value(value))
        }
        Filter::And(left, right) => format!("({}) AND ({})", filter_summary(left), filter_summary(right)),
        Filter::Or(left, right) => format!("({}) OR ({})", filter_summary(left), filter_summary(right)),
        Filter::Not(inner) => format!("NOT ({})", filter_summary(inner)),
        Filter::IsNull(field) => format!("{} IS NULL", field_ref_summary(field)),
        Filter::IsNotNull(field) => format!("{} IS NOT NULL", field_ref_summary(field)),
        Filter::In { field, values } => format!("{} IN [{}]", field_ref_summary(field), values.len()),
        Filter::Between { field, low, high } => format!(
            "{} BETWEEN {} AND {}",
            field_ref_summary(field),
            summarize_value(low),
            summarize_value(high)
        ),
        Filter::Like { field, pattern } => format!("{} LIKE {:?}", field_ref_summary(field), pattern),
        Filter::StartsWith { field, prefix } => {
            format!("{} STARTS WITH {:?}", field_ref_summary(field), prefix)
        }
        Filter::EndsWith { field, suffix } => {
            format!("{} ENDS WITH {:?}", field_ref_summary(field), suffix)
        }
        Filter::Contains { field, substring } => {
            format!("{} CONTAINS {:?}", field_ref_summary(field), substring)
        }
    }
}

fn field_ref_summary(field: &FieldRef) -> String {
    match field {
        FieldRef::TableColumn { table, column } => format!("{table}.{column}"),
        FieldRef::NodeProperty { alias, property } => format!("{alias}.{property}"),
        FieldRef::EdgeProperty { alias, property } => format!("{alias}.{property}"),
        FieldRef::NodeId { alias } => format!("{alias}.id"),
    }
}

fn field_ref_canonical_string(field: &FieldRef) -> String {
    match field {
        FieldRef::TableColumn { table, column } => format!("table:{table}.{column}"),
        FieldRef::NodeProperty { alias, property } => format!("node:{alias}.{property}"),
        FieldRef::EdgeProperty { alias, property } => format!("edge:{alias}.{property}"),
        FieldRef::NodeId { alias } => format!("node_id:{alias}"),
    }
}

fn query_expr_kind(expr: &QueryExpr) -> &'static str {
    match expr {
        QueryExpr::Table(_) => "table",
        QueryExpr::Graph(_) => "graph",
        QueryExpr::Join(_) => "join",
        QueryExpr::Path(_) => "path",
        QueryExpr::Vector(_) => "vector",
        QueryExpr::Hybrid(_) => "hybrid",
    }
}

fn summarize_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Boolean(value) => value.to_string(),
        Value::Integer(value) => value.to_string(),
        Value::UnsignedInteger(value) => value.to_string(),
        Value::Float(value) => value.to_string(),
        Value::Text(value) => format!("{value:?}"),
        Value::Blob(value) => format!("blob({})", value.len()),
        Value::Vector(value) => format!("vector({})", value.len()),
        Value::Json(_) => "json".to_string(),
        Value::Timestamp(value) => value.to_string(),
        Value::Duration(value) => value.to_string(),
        Value::Uuid(_) => "uuid".to_string(),
        Value::IpAddr(value) => value.to_string(),
        Value::MacAddr(_) => "mac".to_string(),
        Value::RowRef(table, row_id) => format!("row_ref({table}:{row_id})"),
        Value::VectorRef(collection, vector_id) => format!("vector_ref({collection}:{vector_id})"),
        Value::NodeRef(value) => value.clone(),
        Value::EdgeRef(value) => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::query::ast::{Projection, QueryExpr, TableQuery};

    fn make_simple_query() -> QueryExpr {
        QueryExpr::Table(TableQuery {
            table: "hosts".to_string(),
            alias: None,
            columns: vec![Projection::All],
            filter: None,
            order_by: vec![],
            limit: None,
            offset: None,
        })
    }

    #[test]
    fn test_planner_creates_plan() {
        let mut planner = QueryPlanner::new();
        let query = make_simple_query();
        let plan = planner.plan(query);
        assert!(plan.cost.total > 0.0);
    }

    #[test]
    fn test_planner_caches_plans() {
        let mut planner = QueryPlanner::new();
        let query = make_simple_query();

        // First call - cache miss
        let _ = planner.plan(query.clone());
        assert_eq!(planner.cache_stats().misses, 1);
        assert_eq!(planner.cache_stats().hits, 0);

        // Second call - cache hit
        let _ = planner.plan(query);
        assert_eq!(planner.cache_stats().hits, 1);
    }

    #[test]
    fn test_cache_invalidation() {
        let mut planner = QueryPlanner::new();
        let query = make_simple_query();

        let _ = planner.plan(query.clone());
        assert_eq!(planner.cache_stats().size, 1);

        planner.clear_cache();
        assert_eq!(planner.cache_stats().size, 0);
    }
}
