//! Canonical logical-plan metadata and operator estimates.
use crate::query::logical_plan::{CanonicalLogicalNode, CardinalityEstimate};
use crate::query::runtime::is_universal_entity_source;
use reddb_rql::ast::{FieldRef, Filter, OrderByClause, Projection, TableQuery};
use reddb_rql::sql_lowering::effective_table_projections;
use reddb_types::Value;
use std::collections::BTreeMap;

pub fn wrap_unary_plan(
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

pub fn btree_details<const N: usize>(pairs: [(&str, String); N]) -> BTreeMap<String, String> {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

pub fn limit_cardinality(
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

pub fn offset_cardinality(
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

pub fn operator_cost_estimate(operator: &str, rows: f64) -> f64 {
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

pub fn uses_document_path_sort(query: &TableQuery) -> bool {
    query
        .order_by
        .iter()
        .any(|clause| field_ref_uses_document_path(&clause.field, query))
}

pub fn requires_implicit_entity_sort(query: &TableQuery) -> bool {
    is_universal_entity_source(query.table.as_str())
        && query.order_by.is_empty()
        && (query.offset.is_some() || query.limit.is_some())
}

pub fn field_ref_uses_document_path(field: &FieldRef, query: &TableQuery) -> bool {
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

pub fn has_explicit_projection(projections: &[Projection]) -> bool {
    !projections.is_empty()
        && !projections
            .iter()
            .all(|projection| matches!(projection, Projection::All))
}

pub fn table_has_explicit_projection(query: &TableQuery) -> bool {
    let projections = effective_table_projections(query);
    has_explicit_projection(&projections)
}

pub fn table_projection_summary(query: &TableQuery) -> String {
    let projections = effective_table_projections(query);
    projection_summary(&projections)
}

pub fn projection_summary(projections: &[Projection]) -> String {
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
            Projection::Window {
                name, args, alias, ..
            } => {
                let label = alias
                    .clone()
                    .unwrap_or_else(|| format!("{name}({}) OVER (...)", args.len()));
                label
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn order_by_summary(order_by: &[OrderByClause]) -> String {
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

pub fn filter_summary(filter: &Filter) -> String {
    filter_summary_at_depth(filter, 0)
}

fn filter_summary_at_depth(filter: &Filter, depth: u32) -> String {
    if depth > reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
        return "<maximum filter depth>".to_string();
    }
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
        Filter::CompareExpr { op, .. } => {
            // Stringifying a full Expr tree is a Week 5+ job —
            // for now emit an opaque placeholder so the plan
            // printer keeps working.
            format!("<expr> {} <expr>", op)
        }
        Filter::And(left, right) => {
            format!(
                "({}) AND ({})",
                filter_summary_at_depth(left, depth + 1),
                filter_summary_at_depth(right, depth + 1)
            )
        }
        Filter::Or(left, right) => {
            format!(
                "({}) OR ({})",
                filter_summary_at_depth(left, depth + 1),
                filter_summary_at_depth(right, depth + 1)
            )
        }
        Filter::Not(inner) => format!("NOT ({})", filter_summary_at_depth(inner, depth + 1)),
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

pub fn field_ref_summary(field: &FieldRef) -> String {
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

pub fn summarize_value(value: &Value) -> String {
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
        Value::Decimal(v) => Value::Decimal(*v).display_string(),
        Value::DecimalText(v) => v.clone(),
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
        Value::AssetCode(code) => format!("asset_code({})", code),
        Value::Money { .. } => format!("money({})", value.display_string()),
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
