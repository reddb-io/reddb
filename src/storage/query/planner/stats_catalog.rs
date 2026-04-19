use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use super::cost::{ColumnStats, TableStats};
use super::histogram::{Bucket, ColumnValue, Histogram, MostCommonValues};
use crate::api::CatalogSnapshot;
use crate::storage::schema::{value_to_canonical_key, CanonicalKey, Value};
use crate::storage::unified::entity::{EntityId, RowData};
use crate::storage::UnifiedStore;
use crate::storage::{EntityData, EntityKind, UnifiedEntity};

pub(crate) const STATS_COLLECTION: &str = "red_stats";
const DEFAULT_ROWS_PER_PAGE: u64 = 100;
const DEFAULT_SAMPLE_TARGET: usize = 30_000;
const DEFAULT_BUCKETS: usize = 100;
const DEFAULT_MCV_SIZE: usize = 100;

#[derive(Debug, Clone)]
pub(crate) struct AnalyzedColumnStats {
    pub name: String,
    pub distinct_count: u64,
    pub null_count: u64,
    pub min_value: Option<Value>,
    pub max_value: Option<Value>,
    pub histogram: Option<Histogram>,
    pub mcv: Option<MostCommonValues>,
}

#[derive(Debug, Clone)]
pub(crate) struct AnalyzedTableStats {
    pub table: String,
    pub row_count: u64,
    pub avg_row_size: u64,
    pub page_count: u64,
    pub columns: Vec<AnalyzedColumnStats>,
}

#[derive(Default)]
pub(crate) struct PersistedPlannerStats {
    pub tables: HashMap<String, TableStats>,
    pub histograms: HashMap<(String, String), Histogram>,
    pub mcvs: HashMap<(String, String), MostCommonValues>,
}

pub(crate) fn analyze_collection(store: &UnifiedStore, table: &str) -> Option<AnalyzedTableStats> {
    let manager = store.get_collection(table)?;
    let entities = manager
        .query_all(|_| true)
        .into_iter()
        .map(|entity| (entity.id, fields_from_entity(&entity)))
        .collect::<Vec<_>>();
    Some(analyze_entity_fields(table, &entities))
}

pub(crate) fn analyze_entity_fields(
    table: &str,
    entities: &[(EntityId, Vec<(String, Value)>)],
) -> AnalyzedTableStats {
    let row_count = entities.len() as u64;
    let page_count = (row_count / DEFAULT_ROWS_PER_PAGE).max(1);
    let avg_row_size = average_row_size(entities);
    let sample_indices = sample_indices(entities.len(), DEFAULT_SAMPLE_TARGET);

    let mut column_names = BTreeSet::new();
    for (_, fields) in entities {
        for (name, _) in fields {
            column_names.insert(name.clone());
        }
    }

    let columns = column_names
        .into_iter()
        .map(|column| analyze_column(&column, entities, &sample_indices))
        .collect();

    AnalyzedTableStats {
        table: table.to_string(),
        row_count,
        avg_row_size,
        page_count,
        columns,
    }
}

pub(crate) fn persist_table_stats(store: &UnifiedStore, stats: &AnalyzedTableStats) {
    let _ = store.get_or_create_collection(STATS_COLLECTION);
    clear_existing_table_stats(store, &stats.table);

    insert_stats_row(
        store,
        vec![
            ("kind".to_string(), Value::Text("table".to_string())),
            ("table".to_string(), Value::Text(stats.table.clone())),
            (
                "row_count".to_string(),
                Value::UnsignedInteger(stats.row_count),
            ),
            (
                "avg_row_size".to_string(),
                Value::UnsignedInteger(stats.avg_row_size),
            ),
            (
                "page_count".to_string(),
                Value::UnsignedInteger(stats.page_count),
            ),
        ],
    );

    for column in &stats.columns {
        let hist_bounds = column
            .histogram
            .as_ref()
            .map(histogram_bounds)
            .unwrap_or_default();
        let hist_total = column
            .histogram
            .as_ref()
            .map(|hist| hist.total_count)
            .unwrap_or(0);
        let (mcv_values, mcv_freqs) = column
            .mcv
            .as_ref()
            .map(mcv_arrays)
            .unwrap_or_else(|| (Vec::new(), Vec::new()));

        // Cap each array to a per-array byte budget so the serialised
        // stats row never exceeds the B-tree's `MAX_VALUE_SIZE`. Wide
        // columns (long TEXT MCVs, JSON histogram bounds) used to
        // overflow on bulk rebuild — see the storage engine's
        // `B-tree bulk rebuild error: Value too large`. Truncating
        // is loss-tolerant: planner accuracy degrades gracefully but
        // the engine stays sound.
        const PER_ARRAY_BYTE_BUDGET: usize = 256;
        let hist_bounds = truncate_array_values_to_fit(hist_bounds, PER_ARRAY_BYTE_BUDGET);
        let mcv_values = truncate_array_values_to_fit(mcv_values, PER_ARRAY_BYTE_BUDGET);
        // mcv_freqs stays in lock-step with mcv_values — drop the same tail.
        let mcv_freqs = if mcv_freqs.len() > mcv_values.len() {
            mcv_freqs.into_iter().take(mcv_values.len()).collect()
        } else {
            mcv_freqs
        };

        insert_stats_row(
            store,
            vec![
                ("kind".to_string(), Value::Text("column".to_string())),
                ("table".to_string(), Value::Text(stats.table.clone())),
                ("column".to_string(), Value::Text(column.name.clone())),
                (
                    "distinct_count".to_string(),
                    Value::UnsignedInteger(column.distinct_count),
                ),
                (
                    "null_count".to_string(),
                    Value::UnsignedInteger(column.null_count),
                ),
                (
                    "min_value".to_string(),
                    column.min_value.clone().unwrap_or(Value::Null),
                ),
                (
                    "max_value".to_string(),
                    column.max_value.clone().unwrap_or(Value::Null),
                ),
                (
                    "hist_total_count".to_string(),
                    Value::UnsignedInteger(hist_total),
                ),
                ("hist_bounds".to_string(), Value::Array(hist_bounds)),
                ("mcv_values".to_string(), Value::Array(mcv_values)),
                ("mcv_freqs".to_string(), Value::Array(mcv_freqs)),
            ],
        );
    }
}

pub(crate) fn clear_table_stats(store: &UnifiedStore, table: &str) {
    clear_existing_table_stats(store, table);
}

pub(crate) fn load_persisted_stats(
    store: &UnifiedStore,
    snapshot: &CatalogSnapshot,
) -> PersistedPlannerStats {
    let mut loaded = PersistedPlannerStats::default();
    for (name, cstats) in &snapshot.stats_by_collection {
        let row_count = cstats.entities as u64;
        loaded.tables.insert(
            name.clone(),
            TableStats {
                row_count,
                avg_row_size: 128,
                page_count: (row_count / DEFAULT_ROWS_PER_PAGE).max(1),
                columns: Vec::new(),
            },
        );
    }

    let Some(manager) = store.get_collection(STATS_COLLECTION) else {
        return loaded;
    };

    for entity in manager.query_all(|_| true) {
        let Some(row) = entity.data.as_row() else {
            continue;
        };
        let Some(kind) = text_field(row, "kind") else {
            continue;
        };
        let Some(table_name) = text_field(row, "table") else {
            continue;
        };
        match kind {
            "table" => {
                let entry = loaded.tables.entry(table_name.to_string()).or_default();
                entry.row_count = u64_field(row, "row_count").unwrap_or(entry.row_count);
                entry.avg_row_size =
                    u64_field(row, "avg_row_size").unwrap_or(u64::from(entry.avg_row_size)) as u32;
                entry.page_count = u64_field(row, "page_count").unwrap_or(entry.page_count);
            }
            "column" => {
                let Some(column_name) = text_field(row, "column") else {
                    continue;
                };
                let min_value = row.get_field("min_value").and_then(non_null_value);
                let max_value = row.get_field("max_value").and_then(non_null_value);

                let entry = loaded.tables.entry(table_name.to_string()).or_default();
                entry.columns.push(ColumnStats {
                    name: column_name.to_string(),
                    distinct_count: u64_field(row, "distinct_count").unwrap_or(0),
                    null_count: u64_field(row, "null_count").unwrap_or(0),
                    min_value: min_value.as_ref().map(value_debug_string),
                    max_value: max_value.as_ref().map(value_debug_string),
                    has_index: false,
                });

                if let Some(bounds) = array_field(row, "hist_bounds") {
                    let total = u64_field(row, "hist_total_count").unwrap_or(0);
                    if let Some(histogram) = histogram_from_bounds(&bounds, total) {
                        loaded
                            .histograms
                            .insert((table_name.to_string(), column_name.to_string()), histogram);
                    }
                }

                let values = array_field(row, "mcv_values").unwrap_or_default();
                let freqs = array_field(row, "mcv_freqs").unwrap_or_default();
                if let Some(mcv) = mcv_from_arrays(&values, &freqs) {
                    loaded
                        .mcvs
                        .insert((table_name.to_string(), column_name.to_string()), mcv);
                }
            }
            _ => {}
        }
    }

    loaded
}

fn analyze_column(
    column: &str,
    entities: &[(EntityId, Vec<(String, Value)>)],
    sample_indices: &[usize],
) -> AnalyzedColumnStats {
    let mut null_count = 0u64;
    let mut distinct = HashSet::new();
    let mut min_key: Option<CanonicalKey> = None;
    let mut min_value: Option<Value> = None;
    let mut max_key: Option<CanonicalKey> = None;
    let mut max_value: Option<Value> = None;

    for (_, fields) in entities {
        match field_value(fields, column) {
            Some(Value::Null) | None => null_count += 1,
            Some(value) => {
                distinct.insert(value.clone());
                if let Some(key) = value_to_canonical_key(value) {
                    if min_key.as_ref().is_none_or(|current| key < *current) {
                        min_key = Some(key.clone());
                        min_value = Some(value.clone());
                    }
                    if max_key.as_ref().is_none_or(|current| key > *current) {
                        max_key = Some(key);
                        max_value = Some(value.clone());
                    }
                }
            }
        }
    }

    let mut sample_for_hist = Vec::new();
    let mut sample_counts = HashMap::<ColumnValue, u64>::new();
    for idx in sample_indices {
        if let Some(value) = entities
            .get(*idx)
            .and_then(|(_, fields)| field_value(fields, column))
            .filter(|value| !matches!(value, Value::Null))
        {
            if let Some(hist_value) = value_to_histogram_value(value) {
                sample_for_hist.push(hist_value.clone());
                *sample_counts.entry(hist_value).or_insert(0) += 1;
            }
        }
    }

    let histogram = if sample_for_hist.is_empty() {
        None
    } else {
        Some(Histogram::equi_depth_from_sample(
            sample_for_hist.clone(),
            DEFAULT_BUCKETS.min(sample_for_hist.len()),
        ))
    };

    let mcv = if sample_counts.is_empty() {
        None
    } else {
        let mut items = sample_counts.into_iter().collect::<Vec<_>>();
        items.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        items.truncate(DEFAULT_MCV_SIZE);
        let total = sample_for_hist.len() as f64;
        Some(MostCommonValues::new(
            items
                .into_iter()
                .map(|(value, count)| (value, count as f64 / total))
                .collect(),
        ))
    };

    AnalyzedColumnStats {
        name: column.to_string(),
        distinct_count: distinct.len() as u64,
        null_count,
        min_value,
        max_value,
        histogram,
        mcv,
    }
}

fn average_row_size(entities: &[(EntityId, Vec<(String, Value)>)]) -> u64 {
    if entities.is_empty() {
        return 0;
    }
    let total = entities
        .iter()
        .map(|(_, fields)| {
            fields
                .iter()
                .map(|(name, value)| name.len() + approximate_value_size(value))
                .sum::<usize>()
        })
        .sum::<usize>();
    (total / entities.len()) as u64
}

fn sample_indices(total_rows: usize, sample_target: usize) -> Vec<usize> {
    if total_rows <= sample_target {
        return (0..total_rows).collect();
    }
    let mut reservoir = crate::storage::query::analyze_cmd::Reservoir::new(sample_target, 1);
    for idx in 0..total_rows {
        reservoir.observe(idx);
    }
    reservoir.into_sorted_indices()
}

fn fields_from_entity(entity: &UnifiedEntity) -> Vec<(String, Value)> {
    match &entity.data {
        EntityData::Row(row) => named_fields_from_row(row),
        EntityData::Node(node) => node
            .properties
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        _ => Vec::new(),
    }
}

fn named_fields_from_row(row: &RowData) -> Vec<(String, Value)> {
    if let Some(named) = &row.named {
        return named
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
    }
    if let Some(schema) = &row.schema {
        return schema
            .iter()
            .zip(row.columns.iter())
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
    }
    Vec::new()
}

fn field_value<'a>(fields: &'a [(String, Value)], column: &str) -> Option<&'a Value> {
    fields
        .iter()
        .find(|(name, _)| name == column)
        .map(|(_, value)| value)
}

fn value_to_histogram_value(value: &Value) -> Option<ColumnValue> {
    match value {
        Value::Integer(v)
        | Value::BigInt(v)
        | Value::Timestamp(v)
        | Value::Duration(v)
        | Value::Decimal(v)
        | Value::TimestampMs(v) => Some(ColumnValue::Int(*v)),
        Value::UnsignedInteger(v) => Some(ColumnValue::Int(*v as i64)),
        Value::Phone(v) => Some(ColumnValue::Int(*v as i64)),
        Value::Semver(v) => Some(ColumnValue::Int(i64::from(*v))),
        Value::Port(v) => Some(ColumnValue::Int(i64::from(*v))),
        Value::PageRef(v) => Some(ColumnValue::Int(i64::from(*v))),
        Value::EnumValue(v) => Some(ColumnValue::Int(i64::from(*v))),
        Value::Date(v) => Some(ColumnValue::Int(i64::from(*v))),
        Value::Time(v) => Some(ColumnValue::Int(i64::from(*v))),
        Value::Latitude(v) => Some(ColumnValue::Int(i64::from(*v))),
        Value::Longitude(v) => Some(ColumnValue::Int(i64::from(*v))),
        Value::Float(v) if v.is_finite() => Some(ColumnValue::Float(*v)),
        Value::Text(v)
        | Value::Email(v)
        | Value::Url(v)
        | Value::NodeRef(v)
        | Value::EdgeRef(v)
        | Value::TableRef(v)
        | Value::Password(v) => Some(ColumnValue::Text(v.clone())),
        _ => None,
    }
}

fn histogram_bounds(histogram: &Histogram) -> Vec<Value> {
    if histogram.buckets.is_empty() {
        return Vec::new();
    }
    let mut bounds = Vec::with_capacity(histogram.buckets.len() + 1);
    bounds.push(column_value_to_value(&histogram.buckets[0].min));
    for bucket in &histogram.buckets {
        bounds.push(column_value_to_value(&bucket.max));
    }
    bounds
}

fn histogram_from_bounds(bounds: &[Value], total_count: u64) -> Option<Histogram> {
    if bounds.len() < 2 {
        return None;
    }
    let bucket_count = bounds.len() - 1;
    let per_bucket = total_count / bucket_count as u64;
    let remainder = total_count % bucket_count as u64;
    let mut buckets = Vec::with_capacity(bucket_count);
    for idx in 0..bucket_count {
        let min = value_to_histogram_value(&bounds[idx])?;
        let max = value_to_histogram_value(&bounds[idx + 1])?;
        let count = per_bucket + u64::from((idx as u64) < remainder);
        buckets.push(Bucket { min, max, count });
    }
    Some(Histogram {
        buckets,
        total_count,
    })
}

fn mcv_arrays(mcv: &MostCommonValues) -> (Vec<Value>, Vec<Value>) {
    let values = mcv
        .values
        .iter()
        .map(|(value, _)| column_value_to_value(value))
        .collect();
    let freqs = mcv
        .values
        .iter()
        .map(|(_, freq)| Value::Float(*freq))
        .collect();
    (values, freqs)
}

fn mcv_from_arrays(values: &[Value], freqs: &[Value]) -> Option<MostCommonValues> {
    if values.is_empty() || values.len() != freqs.len() {
        return None;
    }
    let mut entries = Vec::with_capacity(values.len());
    for (value, freq) in values.iter().zip(freqs.iter()) {
        let column_value = value_to_histogram_value(value)?;
        let frequency = match freq {
            Value::Float(v) => *v,
            Value::Integer(v) => *v as f64,
            Value::UnsignedInteger(v) => *v as f64,
            _ => return None,
        };
        entries.push((column_value, frequency));
    }
    Some(MostCommonValues::new(entries))
}

fn column_value_to_value(value: &ColumnValue) -> Value {
    match value {
        ColumnValue::Int(v) => Value::Integer(*v),
        ColumnValue::Float(v) => Value::Float(*v),
        ColumnValue::Text(v) => Value::Text(v.clone()),
    }
}

fn clear_existing_table_stats(store: &UnifiedStore, table: &str) {
    let Some(manager) = store.get_collection(STATS_COLLECTION) else {
        return;
    };
    let existing = manager.query_all(|entity| {
        entity.data.as_row().is_some_and(|row| {
            text_field(row, "table") == Some(table) && text_field(row, "kind").is_some()
        })
    });
    for entity in existing {
        let _ = store.delete(STATS_COLLECTION, entity.id);
    }
}

/// Conservative byte-size estimator for a `Value` — overcounts is
/// fine, undercount would defeat the purpose. Used by
/// `truncate_array_values_to_fit` to keep stats arrays within the
/// engine's per-value budget.
fn estimate_value_bytes(value: &Value) -> usize {
    match value {
        Value::Null | Value::Boolean(_) => 1,
        Value::Integer(_)
        | Value::UnsignedInteger(_)
        | Value::TimestampMs(_)
        | Value::Timestamp(_)
        | Value::Duration(_)
        | Value::Decimal(_) => 8,
        Value::Float(_) => 8,
        Value::Date(_) | Value::Time(_) => 4,
        Value::Text(s)
        | Value::Email(s)
        | Value::Url(s)
        | Value::AssetCode(s)
        | Value::NodeRef(s)
        | Value::EdgeRef(s) => s.len() + 4,
        Value::Blob(b) | Value::Json(b) => b.len() + 4,
        Value::Vector(v) => v.len() * 4 + 4,
        Value::Array(items) => 4 + items.iter().map(estimate_value_bytes).sum::<usize>(),
        // Pessimistic upper bound for the long tail of compact
        // variants (Uuid, Color, Money, Cidr, GeoPoint, …). Cheap
        // insurance — over-counting just truncates a touch sooner.
        _ => 32,
    }
}

/// Drop trailing entries until the array's estimated serialised size
/// fits inside `byte_budget`. Lossy by design — callers (planner stats
/// MCVs / histograms) prefer reduced fidelity over engine errors.
fn truncate_array_values_to_fit(mut values: Vec<Value>, byte_budget: usize) -> Vec<Value> {
    let mut total = 4usize; // outer Array overhead
    for (idx, v) in values.iter().enumerate() {
        total += estimate_value_bytes(v);
        if total > byte_budget {
            values.truncate(idx);
            return values;
        }
    }
    values
}

fn insert_stats_row(store: &UnifiedStore, fields: Vec<(String, Value)>) {
    let entity = UnifiedEntity::new(
        EntityId::new(0),
        EntityKind::TableRow {
            table: Arc::from(STATS_COLLECTION),
            row_id: 0,
        },
        EntityData::Row(RowData {
            columns: Vec::new(),
            named: Some(fields.into_iter().collect()),
            schema: None,
        }),
    );
    let _ = store.insert_auto(STATS_COLLECTION, entity);
}

fn text_field<'a>(row: &'a RowData, field: &str) -> Option<&'a str> {
    row.get_field(field).and_then(|value| match value {
        Value::Text(v) => Some(v.as_str()),
        _ => None,
    })
}

fn u64_field(row: &RowData, field: &str) -> Option<u64> {
    row.get_field(field).and_then(|value| match value {
        Value::UnsignedInteger(v) => Some(*v),
        Value::Integer(v) if *v >= 0 => Some(*v as u64),
        _ => None,
    })
}

fn array_field(row: &RowData, field: &str) -> Option<Vec<Value>> {
    row.get_field(field).and_then(|value| match value {
        Value::Array(values) => Some(values.clone()),
        _ => None,
    })
}

fn non_null_value(value: &Value) -> Option<Value> {
    if matches!(value, Value::Null) {
        None
    } else {
        Some(value.clone())
    }
}

fn value_debug_string(value: &Value) -> String {
    match value {
        Value::Text(v)
        | Value::Email(v)
        | Value::Url(v)
        | Value::NodeRef(v)
        | Value::EdgeRef(v)
        | Value::TableRef(v)
        | Value::Password(v) => v.clone(),
        _ => format!("{value:?}"),
    }
}

fn approximate_value_size(value: &Value) -> usize {
    match value {
        Value::Null => 0,
        Value::Integer(_)
        | Value::UnsignedInteger(_)
        | Value::Float(_)
        | Value::Timestamp(_)
        | Value::Duration(_)
        | Value::Phone(_)
        | Value::Semver(_)
        | Value::Date(_)
        | Value::Time(_)
        | Value::Decimal(_)
        | Value::TimestampMs(_)
        | Value::Ipv4(_)
        | Value::Port(_)
        | Value::Latitude(_)
        | Value::Longitude(_)
        | Value::PageRef(_) => 8,
        Value::Boolean(_) | Value::EnumValue(_) => 1,
        Value::Text(v)
        | Value::Email(v)
        | Value::Url(v)
        | Value::NodeRef(v)
        | Value::EdgeRef(v)
        | Value::TableRef(v)
        | Value::Password(v) => v.len(),
        Value::Blob(v) | Value::Json(v) | Value::Secret(v) => v.len(),
        Value::MacAddr(_) => 6,
        Value::Uuid(_) | Value::Ipv6(_) => 16,
        Value::Vector(v) => v.len() * std::mem::size_of::<f32>(),
        Value::VectorRef(collection, _)
        | Value::RowRef(collection, _)
        | Value::DocRef(collection, _) => collection.len() + 8,
        Value::Color(_) => 3,
        Value::Cidr(_, _) => 5,
        Value::Array(values) => values.iter().map(approximate_value_size).sum(),
        Value::Subnet(_, _) => 8,
        Value::GeoPoint(_, _) => 8,
        Value::Country2(_) | Value::Lang2(_) => 2,
        Value::Country3(_) | Value::Currency(_) => 3,
        Value::Lang5(_) | Value::ColorAlpha(_) => 5,
        Value::AssetCode(code) => code.len(),
        Value::Money { asset_code, .. } => asset_code.len() + 9,
        Value::BigInt(_) => 8,
        Value::KeyRef(collection, key) => collection.len() + key.len(),
        Value::IpAddr(_) => 16,
    }
}
