//! Shared helpers for the `red_schema` virtual-table family.
//!
//! Extracted from the `red_schema` dispatcher (issue #1640). Holds the
//! value-coercion helpers (`json_value`, `timestamp_ms_value`,
//! `value_as_ms`), the catalog name mappers (`ref_kind_name`,
//! `collection_model_name`, `schema_mode_name`, `distance_metric_name`),
//! the collection visibility/tenant/internal helpers shared by every
//! family submodule, and the case-insensitive SQL string utilities used
//! by the dispatcher's rewrite/detect logic.

use super::*;

pub(super) fn timestamp_ms_value(value: u128) -> Value {
    i64::try_from(value)
        .map(Value::TimestampMs)
        .unwrap_or(Value::Null)
}

pub(super) fn value_as_ms(value: &crate::storage::schema::Value) -> Option<i64> {
    use crate::storage::schema::Value;
    match value {
        Value::TimestampMs(v) => Some(*v),
        Value::Timestamp(v) => Some(v.saturating_mul(1_000)),
        Value::BigInt(v) => Some(*v),
        Value::UnsignedInteger(v) => i64::try_from(*v).ok(),
        Value::Integer(v) => Some(*v),
        _ => None,
    }
}

pub(super) fn json_value(value: crate::json::Value) -> Value {
    crate::json::to_vec(&value)
        .map(Value::Json)
        .unwrap_or(Value::Null)
}

pub(super) fn ref_kind_name(kind: crate::application::vcs::RefKind) -> &'static str {
    match kind {
        crate::application::vcs::RefKind::Branch => "branch",
        crate::application::vcs::RefKind::Tag => "tag",
        crate::application::vcs::RefKind::Head => "head",
    }
}

pub(super) fn collection_model_name(model: CollectionModel) -> &'static str {
    match model {
        CollectionModel::Table => "table",
        CollectionModel::Document => "document",
        CollectionModel::Graph => "graph",
        CollectionModel::Vector => "vector",
        CollectionModel::Hll => "hll",
        CollectionModel::Sketch => "sketch",
        CollectionModel::Filter => "filter",
        CollectionModel::Kv => "kv",
        CollectionModel::Config => "config",
        CollectionModel::Vault => "vault",
        CollectionModel::Mixed => "mixed",
        CollectionModel::TimeSeries => "time_series",
        CollectionModel::Queue => "queue",
        CollectionModel::Metrics => "metrics",
    }
}

pub(super) fn schema_mode_name(mode: SchemaMode) -> &'static str {
    match mode {
        SchemaMode::Strict => "strict",
        SchemaMode::SemiStructured => "semi_structured",
        SchemaMode::Dynamic => "dynamic",
    }
}

pub(super) fn distance_metric_name(
    metric: crate::storage::engine::distance::DistanceMetric,
) -> &'static str {
    match metric {
        crate::storage::engine::distance::DistanceMetric::L2 => "l2",
        crate::storage::engine::distance::DistanceMetric::Cosine => "cosine",
        crate::storage::engine::distance::DistanceMetric::InnerProduct => "inner_product",
    }
}

pub(super) fn collection_is_visible(
    collection: &str,
    visible_collections: Option<&HashSet<String>>,
) -> bool {
    visible_collections.is_none_or(|visible| visible.contains(collection))
}

pub(super) fn on_disk_bytes_value(
    store: &crate::storage::unified::UnifiedStore,
    collection: &str,
) -> Value {
    crate::storage::disk_accountant::bytes_on_disk_for(store, collection)
        .map(Value::UnsignedInteger)
        .unwrap_or(Value::Null)
}

pub(super) fn collection_tenant(
    store: &crate::storage::unified::UnifiedStore,
    collection: &str,
) -> Option<String> {
    match store.get_config(&format!("red.collection_tenants.{collection}")) {
        Some(Value::Text(value)) => Some(value.to_string()),
        _ => None,
    }
}

pub(super) fn row_text(row: &crate::storage::unified::entity::RowData, field: &str) -> Option<String> {
    match row.get_field(field)?.clone() {
        Value::Text(value) => Some(value.to_string()),
        Value::NodeRef(value) => Some(value),
        Value::EdgeRef(value) => Some(value),
        Value::TableRef(value) => Some(value),
        _ => None,
    }
}

fn discover_queue_dlqs(store: &UnifiedStore) -> HashSet<String> {
    const QUEUE_META_COLLECTION: &str = "red_queue_meta";

    let Some(manager) = store.get_collection(QUEUE_META_COLLECTION) else {
        return HashSet::new();
    };

    manager
        .query_all(|entity| {
            entity
                .data
                .as_row()
                .is_some_and(|row| row_text(row, "kind").as_deref() == Some("queue_config"))
        })
        .into_iter()
        .filter_map(|entity| {
            let row = entity.data.as_row()?;
            row_text(row, "dlq")
        })
        .collect()
}

pub(super) struct InternalCollectionRegistry {
    dlqs: HashSet<String>,
}

impl InternalCollectionRegistry {
    pub(super) fn from_store(store: &UnifiedStore) -> Self {
        Self {
            dlqs: discover_queue_dlqs(store),
        }
    }

    pub(super) fn is_internal(&self, collection: &str) -> bool {
        collection.starts_with("red_")
            || collection.starts_with("red.")
            || collection == "audit_log"
            || collection == "__tenant_iso"
            || collection.starts_with("__tenant_")
            || collection.starts_with("__policy_")
            || self.dlqs.contains(collection)
    }
}

pub(super) fn contains_case_insensitive_outside_quotes(haystack: &str, needle: &str) -> bool {
    find_case_insensitive_outside_quotes(haystack, needle).is_some()
}

pub(super) fn matches_ignore_ascii_case(value: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|candidate| value.eq_ignore_ascii_case(candidate))
}

pub(super) fn replace_case_insensitive_outside_quotes(
    haystack: &str,
    needle: &str,
    replacement: &str,
) -> Option<String> {
    let mut out = String::new();
    let mut rest = haystack;
    let mut changed = false;

    while let Some(idx) = find_case_insensitive_outside_quotes(rest, needle) {
        out.push_str(&rest[..idx]);
        out.push_str(replacement);
        rest = &rest[idx + needle.len()..];
        changed = true;
    }

    if changed {
        out.push_str(rest);
        Some(out)
    } else {
        None
    }
}

pub(super) fn find_case_insensitive_outside_quotes(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    let bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;

    while i < bytes.len() {
        match bytes[i] {
            b'\'' if !in_double => {
                if in_single && bytes.get(i + 1) == Some(&b'\'') {
                    i += 2;
                    continue;
                }
                in_single = !in_single;
                i += 1;
                continue;
            }
            b'"' if !in_single => {
                in_double = !in_double;
                i += 1;
                continue;
            }
            _ => {}
        }

        if !in_single
            && !in_double
            && i + needle_bytes.len() <= bytes.len()
            && bytes[i..i + needle_bytes.len()].eq_ignore_ascii_case(needle_bytes)
        {
            return Some(i);
        }
        i += 1;
    }
    None
}
