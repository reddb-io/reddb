use std::collections::BTreeMap;

use crate::storage::query::ast::{
    FieldRef, Filter, FusionStrategy, JoinQuery, OrderByClause, Projection, QueryExpr, TableQuery,
};
use crate::storage::query::is_universal_entity_source as is_universal_query_source;
use crate::storage::schema::Value;
use crate::storage::RedDB;

use super::{AccessPathDecision, CanonicalLogicalNode, CardinalityEstimate, CostEstimator};

pub(crate) fn wrap_unary_plan(
    operator: &str,
    details: BTreeMap<String, String>,
    estimate: Option<CardinalityEstimate>,
    child: CanonicalLogicalNode,
) -> CanonicalLogicalNode {
    let estimate = estimate.unwrap_or(CardinalityEstimate {
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

pub(crate) fn btree_details<const N: usize>(
    pairs: [(&str, String); N],
) -> BTreeMap<String, String> {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

pub(crate) fn estimate_cardinality(expr: &QueryExpr) -> CardinalityEstimate {
    CostEstimator::new().estimate_cardinality(expr)
}

pub(crate) fn base_collection_cardinality(db: &RedDB, collection: &str) -> CardinalityEstimate {
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

pub(crate) fn universal_entity_cardinality(db: &RedDB) -> CardinalityEstimate {
    CardinalityEstimate {
        rows: db.catalog_snapshot().total_entities as f64,
        selectivity: 1.0,
        confidence: 0.7,
    }
}

pub(crate) fn table_filtered_cardinality(query: &TableQuery) -> CardinalityEstimate {
    let mut filtered = query.clone();
    filtered.limit = None;
    filtered.offset = None;
    estimate_cardinality(&QueryExpr::Table(filtered))
}

pub(crate) fn document_index_cardinality(db: &RedDB, collection: &str) -> CardinalityEstimate {
    let base = base_collection_cardinality(db, collection);
    CardinalityEstimate {
        rows: (base.rows * 0.35).max(1.0),
        selectivity: (base.selectivity * 0.35).min(1.0),
        confidence: (base.confidence * 0.95).min(0.98),
    }
}

pub(crate) fn document_filtered_cardinality(query: &TableQuery) -> CardinalityEstimate {
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

pub(crate) fn limit_cardinality(
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

pub(crate) fn offset_cardinality(
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

pub(crate) fn heuristic_selectivity(
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

pub(crate) fn operator_cost_estimate(operator: &str, rows: f64) -> f64 {
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
        "document_sort" => 0.86,
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

pub(crate) fn table_access_path_hint(db: &RedDB, query: &TableQuery) -> AccessPathDecision {
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

    if query.filter.is_some() && is_document_like_collection(db, collection) {
        if let Some(index) = indexes
            .iter()
            .find(|status| status.kind == "document.pathvalue")
        {
            return AccessPathDecision {
                path: "document_path_index_seek",
                index_hint: Some(index.name.clone()),
                reason: "document path/value index is declared, operational, and enabled"
                    .to_string(),
                warning: None,
            };
        }
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

pub(crate) fn uses_document_path_filter(db: &RedDB, query: &TableQuery) -> bool {
    query
        .filter
        .as_ref()
        .is_some_and(|filter| filter_uses_document_path(filter, query))
        || (query.filter.is_some()
            && !is_universal_entity_source(query.table.as_str())
            && is_document_like_collection(db, query.table.as_str())
            && db.index_statuses().into_iter().any(|status| {
                status.collection.as_deref() == Some(query.table.as_str())
                    && status.kind == "document.pathvalue"
                    && status.declared
                    && status.operational
                    && status.enabled
            }))
}

pub(crate) fn is_document_projection(db: &RedDB, query: &TableQuery) -> bool {
    has_explicit_projection(&query.columns)
        && (query
            .columns
            .iter()
            .any(|projection| projection_uses_document_path(projection, query))
            || (!is_universal_entity_source(query.table.as_str())
                && is_document_like_collection(db, query.table.as_str())))
}

pub(crate) fn uses_document_path_sort(query: &TableQuery) -> bool {
    query
        .order_by
        .iter()
        .any(|clause| field_ref_uses_document_path(&clause.field, query))
}

pub(crate) fn requires_implicit_entity_sort(query: &TableQuery) -> bool {
    is_universal_entity_source(query.table.as_str())
        && query.order_by.is_empty()
        && (query.offset.is_some() || query.limit.is_some())
}

pub(crate) fn uses_document_path_join_sort(query: &JoinQuery) -> bool {
    query
        .order_by
        .iter()
        .any(|clause| join_field_ref_uses_document_path(&clause.field, query))
}

pub(crate) fn requires_implicit_entity_sort_join(query: &JoinQuery) -> bool {
    let left_universal = match query.left.as_ref() {
        QueryExpr::Table(left) => is_universal_query_source(left.table.as_str()),
        _ => false,
    };
    let right_universal = match query.right.as_ref() {
        QueryExpr::Table(right) => is_universal_query_source(right.table.as_str()),
        _ => false,
    };
    (left_universal || right_universal)
        && query.order_by.is_empty()
        && (query.limit.is_some() || query.offset.is_some())
}

pub(crate) fn filter_uses_document_path(filter: &Filter, query: &TableQuery) -> bool {
    match filter {
        Filter::Compare { field, .. }
        | Filter::IsNull(field)
        | Filter::IsNotNull(field)
        | Filter::In { field, .. }
        | Filter::Between { field, .. }
        | Filter::Like { field, .. }
        | Filter::StartsWith { field, .. }
        | Filter::EndsWith { field, .. }
        | Filter::Contains { field, .. } => field_ref_uses_document_path(field, query),
        Filter::CompareFields { left, right, .. } => {
            field_ref_uses_document_path(left, query) || field_ref_uses_document_path(right, query)
        }
        Filter::And(left, right) | Filter::Or(left, right) => {
            filter_uses_document_path(left, query) || filter_uses_document_path(right, query)
        }
        Filter::Not(inner) => filter_uses_document_path(inner, query),
    }
}

pub(crate) fn projection_uses_document_path(projection: &Projection, query: &TableQuery) -> bool {
    match projection {
        Projection::Column(name) | Projection::Alias(name, _) => {
            name.split_once('.').is_some_and(|(head, tail)| {
                tail.contains('.') || (head != query.table && query.alias.as_deref() != Some(head))
            })
        }
        Projection::Field(field, _) => field_ref_uses_document_path(field, query),
        Projection::Expression(filter, _) => filter_uses_document_path(filter, query),
        Projection::Function(_, _) | Projection::All => false,
    }
}

pub(crate) fn field_ref_uses_document_path(field: &FieldRef, query: &TableQuery) -> bool {
    match field {
        FieldRef::TableColumn { table, column } => {
            column.contains('.')
                || (!table.is_empty()
                    && table != &query.table
                    && query.alias.as_deref() != Some(table.as_str()))
        }
        _ => false,
    }
}

pub(crate) fn join_field_ref_uses_document_path(field: &FieldRef, query: &JoinQuery) -> bool {
    match field {
        FieldRef::TableColumn { table, column } => {
            column.contains('.')
                || (!table.is_empty() && !join_query_exposes_field_table(query, table))
        }
        _ => false,
    }
}

pub(crate) fn join_query_exposes_field_table(query: &JoinQuery, table: &str) -> bool {
    join_expr_exposes_field_table(query.left.as_ref(), table)
        || join_expr_exposes_field_table(query.right.as_ref(), table)
}

pub(crate) fn join_expr_exposes_field_table(expr: &QueryExpr, table: &str) -> bool {
    match expr {
        QueryExpr::Table(query) => {
            query.table == table
                || query.alias.as_deref() == Some(table)
                || (is_universal_entity_source(query.table.as_str())
                    && is_universal_entity_source(table))
        }
        QueryExpr::Graph(query) => query.alias.as_deref() == Some(table) || table == "graph",
        QueryExpr::Path(query) => query.alias.as_deref() == Some(table) || table == "path",
        QueryExpr::Vector(query) => query.alias.as_deref() == Some(table) || table == "vector",
        QueryExpr::Hybrid(query) => query.alias.as_deref() == Some(table) || table == "hybrid",
        QueryExpr::Join(query) => join_query_exposes_field_table(query, table),
        QueryExpr::Insert(_)
        | QueryExpr::Update(_)
        | QueryExpr::Delete(_)
        | QueryExpr::CreateTable(_)
        | QueryExpr::DropTable(_)
        | QueryExpr::AlterTable(_)
        | QueryExpr::GraphCommand(_)
        | QueryExpr::SearchCommand(_)
        | QueryExpr::CreateIndex(_)
        | QueryExpr::DropIndex(_)
        | QueryExpr::ProbabilisticCommand(_)
        | QueryExpr::Ask(_)
        | QueryExpr::SetConfig { .. }
        | QueryExpr::ShowConfig { .. }
        | QueryExpr::CreateTimeSeries(_)
        | QueryExpr::DropTimeSeries(_)
        | QueryExpr::CreateQueue(_)
        | QueryExpr::DropQueue(_)
        | QueryExpr::QueueCommand(_) => false,
    }
}

pub(crate) fn is_document_like_collection(db: &RedDB, collection: &str) -> bool {
    db.catalog_model_snapshot()
        .collections
        .into_iter()
        .find(|descriptor| descriptor.name == collection)
        .map(|descriptor| {
            matches!(
                descriptor.model,
                crate::catalog::CollectionModel::Document | crate::catalog::CollectionModel::Mixed
            )
        })
        .unwrap_or(false)
}

pub(crate) fn is_universal_entity_source(table: &str) -> bool {
    is_universal_query_source(table)
}

pub(crate) fn is_universal_query_expr(expr: &QueryExpr) -> bool {
    match expr {
        QueryExpr::Table(query) => is_universal_entity_source(query.table.as_str()),
        QueryExpr::Join(query) => is_universal_query_expr(query.left.as_ref()),
        QueryExpr::Hybrid(query) => is_universal_query_expr(query.structured.as_ref()),
        _ => false,
    }
}

pub(crate) fn hybrid_ranking_mode(fusion: &FusionStrategy) -> String {
    match fusion {
        FusionStrategy::Rerank { .. } => "rerank".to_string(),
        FusionStrategy::FilterThenSearch => "filter_then_search".to_string(),
        FusionStrategy::SearchThenFilter => "search_then_filter".to_string(),
        FusionStrategy::RRF { .. } => "rrf".to_string(),
        FusionStrategy::Intersection => "intersection".to_string(),
        FusionStrategy::Union { .. } => "union".to_string(),
    }
}

pub(crate) fn vector_access_path_hint(db: &RedDB, collection: &str) -> AccessPathDecision {
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

pub(crate) fn graph_access_path_hint(db: &RedDB) -> AccessPathDecision {
    if let Some(index) = db.index_statuses().into_iter().find(|status| {
        status.kind == "graph.adjacency" && status.declared && status.operational && status.enabled
    }) {
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

pub(crate) fn index_lifecycle_warning(
    status: crate::catalog::CatalogIndexStatus,
) -> Option<String> {
    match status.lifecycle_state.as_str() {
        "disabled" => Some(format!("index {} is disabled", status.name)),
        "stale" => Some(format!("index {} is stale", status.name)),
        "failed" => Some(format!("index {} is failed", status.name)),
        "building" => Some(format!("index {} is still building", status.name)),
        "declared" => Some(format!(
            "index {} is declared but not operational",
            status.name
        )),
        "orphaned-operational" => Some(format!(
            "index {} is operational without matching declaration",
            status.name
        )),
        _ => None,
    }
}

pub(crate) fn join_strategy_hint(query: &JoinQuery) -> &'static str {
    match (&*query.left, &*query.right) {
        (QueryExpr::Table(_), QueryExpr::Table(_)) => "indexed_nested_loop",
        (QueryExpr::Table(_), QueryExpr::Graph(_) | QueryExpr::Path(_)) => "graph_lookup_join",
        _ => "nested_loop",
    }
}

pub(crate) fn join_strategy_reason(query: &JoinQuery) -> &'static str {
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

pub(crate) fn has_explicit_projection(projections: &[Projection]) -> bool {
    !projections.is_empty()
        && !projections
            .iter()
            .all(|projection| matches!(projection, Projection::All))
}

pub(crate) fn projection_summary(projections: &[Projection]) -> String {
    projections
        .iter()
        .map(|projection| match projection {
            Projection::All => "*".to_string(),
            Projection::Column(name) => name.clone(),
            Projection::Alias(name, alias) => format!("{name} AS {alias}"),
            Projection::Function(name, args) => format!("{name}({})", args.len()),
            Projection::Expression(_, alias) => alias.clone().unwrap_or_else(|| "expr".to_string()),
            Projection::Field(field, alias) => {
                alias.clone().unwrap_or_else(|| field_ref_summary(field))
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn order_by_summary(order_by: &[OrderByClause]) -> String {
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

pub(crate) fn filter_summary(filter: &Filter) -> String {
    match filter {
        Filter::Compare { field, op, value } => {
            format!(
                "{} {} {}",
                field_ref_summary(field),
                op,
                summarize_value(value)
            )
        }
        Filter::CompareFields { left, op, right } => {
            format!(
                "{} {} {}",
                field_ref_summary(left),
                op,
                field_ref_summary(right)
            )
        }
        Filter::And(left, right) => {
            format!("({}) AND ({})", filter_summary(left), filter_summary(right))
        }
        Filter::Or(left, right) => {
            format!("({}) OR ({})", filter_summary(left), filter_summary(right))
        }
        Filter::Not(inner) => format!("NOT ({})", filter_summary(inner)),
        Filter::IsNull(field) => format!("{} IS NULL", field_ref_summary(field)),
        Filter::IsNotNull(field) => format!("{} IS NOT NULL", field_ref_summary(field)),
        Filter::In { field, values } => {
            format!("{} IN [{}]", field_ref_summary(field), values.len())
        }
        Filter::Between { field, low, high } => format!(
            "{} BETWEEN {} AND {}",
            field_ref_summary(field),
            summarize_value(low),
            summarize_value(high)
        ),
        Filter::Like { field, pattern } => {
            format!("{} LIKE {:?}", field_ref_summary(field), pattern)
        }
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

pub(crate) fn field_ref_summary(field: &FieldRef) -> String {
    match field {
        FieldRef::TableColumn { table, column } => {
            if table.is_empty() {
                column.clone()
            } else {
                format!("{table}.{column}")
            }
        }
        FieldRef::NodeProperty { alias, property } => format!("{alias}.{property}"),
        FieldRef::EdgeProperty { alias, property } => format!("{alias}.{property}"),
        FieldRef::NodeId { alias } => format!("{alias}.id"),
    }
}

pub(crate) fn field_ref_canonical_string(field: &FieldRef) -> String {
    match field {
        FieldRef::TableColumn { table, column } => format!("table:{table}.{column}"),
        FieldRef::NodeProperty { alias, property } => format!("node:{alias}.{property}"),
        FieldRef::EdgeProperty { alias, property } => format!("edge:{alias}.{property}"),
        FieldRef::NodeId { alias } => format!("node_id:{alias}"),
    }
}

pub(crate) fn query_expr_kind(expr: &QueryExpr) -> &'static str {
    match expr {
        QueryExpr::Table(_) => "table",
        QueryExpr::Graph(_) => "graph",
        QueryExpr::Join(_) => "join",
        QueryExpr::Path(_) => "path",
        QueryExpr::Vector(_) => "vector",
        QueryExpr::Hybrid(_) => "hybrid",
        QueryExpr::Insert(_) => "insert",
        QueryExpr::Update(_) => "update",
        QueryExpr::Delete(_) => "delete",
        QueryExpr::CreateTable(_) => "create_table",
        QueryExpr::DropTable(_) => "drop_table",
        QueryExpr::AlterTable(_) => "alter_table",
        QueryExpr::GraphCommand(_) => "graph_command",
        QueryExpr::SearchCommand(_) => "search_command",
        QueryExpr::CreateIndex(_) => "create_index",
        QueryExpr::DropIndex(_) => "drop_index",
        QueryExpr::ProbabilisticCommand(_) => "probabilistic_command",
        QueryExpr::Ask(_) => "ask",
        QueryExpr::SetConfig { .. } => "set_config",
        QueryExpr::ShowConfig { .. } => "show_config",
        QueryExpr::CreateTimeSeries(_) => "create_timeseries",
        QueryExpr::DropTimeSeries(_) => "drop_timeseries",
        QueryExpr::CreateQueue(_) => "create_queue",
        QueryExpr::DropQueue(_) => "drop_queue",
        QueryExpr::QueueCommand(_) => "queue_command",
    }
}

pub(crate) fn summarize_value(value: &Value) -> String {
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
        Value::Color([r, g, b]) => format!("#{:02X}{:02X}{:02X}", r, g, b),
        Value::Email(value) => value.clone(),
        Value::Url(value) => value.clone(),
        Value::Phone(n) => format!("+{}", n),
        Value::Semver(packed) => format!(
            "{}.{}.{}",
            packed / 1_000_000,
            (packed / 1_000) % 1_000,
            packed % 1_000
        ),
        Value::Cidr(ip, prefix) => format!(
            "{}.{}.{}.{}/{}",
            (ip >> 24) & 0xFF,
            (ip >> 16) & 0xFF,
            (ip >> 8) & 0xFF,
            ip & 0xFF,
            prefix
        ),
        Value::Date(days) => format!("date({})", days),
        Value::Time(ms) => format!("time({})", ms),
        Value::Decimal(v) => format!("{:.4}", *v as f64 / 10_000.0),
        Value::EnumValue(i) => format!("enum({})", i),
        Value::Array(elems) => format!("array({})", elems.len()),
        Value::TimestampMs(ms) => format!("timestamp_ms({})", ms),
        Value::Ipv4(ip) => format!(
            "{}.{}.{}.{}",
            (ip >> 24) & 0xFF,
            (ip >> 16) & 0xFF,
            (ip >> 8) & 0xFF,
            ip & 0xFF
        ),
        Value::Ipv6(bytes) => format!("{}", std::net::Ipv6Addr::from(*bytes)),
        Value::Subnet(ip, mask) => {
            let prefix = mask.leading_ones();
            format!(
                "{}.{}.{}.{}/{}",
                (ip >> 24) & 0xFF,
                (ip >> 16) & 0xFF,
                (ip >> 8) & 0xFF,
                ip & 0xFF,
                prefix
            )
        }
        Value::Port(p) => format!("port({})", p),
        Value::Latitude(micro) => format!("lat({:.6})", *micro as f64 / 1_000_000.0),
        Value::Longitude(micro) => format!("lon({:.6})", *micro as f64 / 1_000_000.0),
        Value::GeoPoint(lat, lon) => format!(
            "geo({:.6},{:.6})",
            *lat as f64 / 1_000_000.0,
            *lon as f64 / 1_000_000.0
        ),
        Value::Country2(c) => format!("country({})", String::from_utf8_lossy(c)),
        Value::Country3(c) => format!("country({})", String::from_utf8_lossy(c)),
        Value::Lang2(c) => format!("lang({})", String::from_utf8_lossy(c)),
        Value::Lang5(c) => format!("lang({})", String::from_utf8_lossy(c)),
        Value::Currency(c) => format!("currency({})", String::from_utf8_lossy(c)),
        Value::ColorAlpha([r, g, b, a]) => format!("#{:02X}{:02X}{:02X}{:02X}", r, g, b, a),
        Value::BigInt(v) => format!("bigint({})", v),
        Value::KeyRef(col, key) => format!("key_ref({}:{})", col, key),
        Value::DocRef(col, id) => format!("doc_ref({}#{})", col, id),
        Value::TableRef(name) => format!("table_ref({})", name),
        Value::PageRef(page_id) => format!("page_ref({})", page_id),
        Value::Secret(bytes) => format!("secret({} bytes)", bytes.len()),
        Value::Password(_) => "password(***)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use crate::storage::query::ast::{Projection, QueryExpr, TableQuery};
    use crate::storage::query::planner::QueryPlanner;

    fn make_simple_query() -> QueryExpr {
        QueryExpr::Table(TableQuery {
            table: "hosts".to_string(),
            source: None,
            alias: None,
            columns: vec![Projection::All],
            filter: None,
            group_by: Vec::new(),
            having: None,
            order_by: vec![],
            limit: None,
            offset: None,
            expand: None,
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
