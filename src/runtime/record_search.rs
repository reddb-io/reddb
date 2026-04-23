use super::*;
use crate::storage::query::sql_lowering::effective_table_projections;
use crate::storage::query::unified::{
    sys_key_created_at, sys_key_red_capabilities, sys_key_red_collection, sys_key_red_entity_id,
    sys_key_red_entity_type, sys_key_red_kind, sys_key_red_sequence_id, sys_key_row_id,
    sys_key_updated_at,
};

/// Per-thread cache of composite schemas `[red_entity_id, created_at,
/// updated_at, <user columns…>]`, keyed on the `Arc<Vec<String>>`
/// identity of the underlying row schema. A 4.5k-row scan hits this
/// cache on every row after the first, so the Arc<Vec<Arc<str>>>
/// result is refcount-cloned instead of rebuilt.
fn sys_schema_with_row_columns(
    schema: &std::sync::Arc<Vec<String>>,
) -> std::sync::Arc<Vec<std::sync::Arc<str>>> {
    use std::cell::RefCell;
    thread_local! {
        static CACHE: RefCell<Option<(usize, std::sync::Arc<Vec<std::sync::Arc<str>>>)>> =
            const { RefCell::new(None) };
    }
    let key = std::sync::Arc::as_ptr(schema) as usize;
    CACHE.with(|c| {
        let mut slot = c.borrow_mut();
        if let Some((k, v)) = slot.as_ref() {
            if *k == key {
                return std::sync::Arc::clone(v);
            }
        }
        let mut out: Vec<std::sync::Arc<str>> = Vec::with_capacity(3 + schema.len());
        out.push(sys_key_red_entity_id());
        out.push(sys_key_created_at());
        out.push(sys_key_updated_at());
        for name in schema.iter() {
            out.push(std::sync::Arc::from(name.as_str()));
        }
        let arc = std::sync::Arc::new(out);
        *slot = Some((key, std::sync::Arc::clone(&arc)));
        arc
    })
}

#[inline(never)]
pub(super) fn scan_runtime_table_source_records(
    db: &RedDB,
    table: &str,
) -> RedDBResult<Vec<UnifiedRecord>> {
    scan_runtime_table_source_records_limited(db, table, None)
}

/// Like `scan_runtime_table_source_records` but stops after `limit`
/// visible rows when one is supplied. Pushes LIMIT down so that
/// `SELECT * FROM t LIMIT N` does not materialise the full table
/// before truncating — the hot case for dashboards and `\d`-style
/// exploration queries.
pub(super) fn scan_runtime_table_source_records_limited(
    db: &RedDB,
    table: &str,
    limit: Option<usize>,
) -> RedDBResult<Vec<UnifiedRecord>> {
    use crate::runtime::impl_core::{
        capture_current_snapshot, entity_visible_under_current_snapshot,
        entity_visible_with_context,
    };

    if is_universal_entity_source(table) {
        // Cross-collection scan runs inside std::thread::scope — capture
        // the snapshot so worker threads see the same MVCC view instead
        // of defaulting to "no snapshot" (every row visible).
        let snap_ctx = capture_current_snapshot();
        let records: Vec<UnifiedRecord> = db
            .store()
            .query_all(move |e| entity_visible_with_context(snap_ctx.as_ref(), e))
            .into_iter()
            .filter_map(|(_, entity)| runtime_any_record_from_entity(entity))
            .collect();
        let records = match limit {
            Some(n) if records.len() > n => records.into_iter().take(n).collect(),
            _ => records,
        };
        return Ok(records);
    }

    let manager = db
        .store()
        .get_collection(table)
        .ok_or_else(|| RedDBError::NotFound(table.to_string()))?;

    // A5 — parallel scan: for large unfiltered tables, collect entities once
    // then convert to records in parallel using the thread-pool coordinator.
    // The threshold guards against spawn overhead dominating on small tables.
    // When a LIMIT is supplied and it's below the parallel threshold, prefer
    // the sequential path so we can stop scanning as soon as we have enough.
    use crate::storage::query::executors::parallel_scan::MIN_PARALLEL_ROWS;
    let entity_count = manager.count();
    let sequential_cap = limit.unwrap_or(usize::MAX);
    let go_parallel = entity_count >= MIN_PARALLEL_ROWS && sequential_cap >= MIN_PARALLEL_ROWS;
    if go_parallel {
        let mut entities: Vec<crate::storage::unified::entity::UnifiedEntity> =
            Vec::with_capacity(entity_count);
        manager.for_each_entity(|e| {
            if entity_visible_under_current_snapshot(e) {
                entities.push(e.clone());
            }
            true
        });
        let mut records = crate::storage::query::executors::parallel_scan::parallel_scan_default(
            &entities,
            |chunk| {
                chunk
                    .iter()
                    .filter_map(|e| runtime_table_record_from_entity(e.clone()))
                    .collect()
            },
        );
        if let Some(n) = limit {
            records.truncate(n);
        }
        return Ok(records);
    }

    // Sequential path — short-circuits at `limit` rows so an unfiltered
    // SELECT * LIMIT 100 on a 1M-row table doesn't build the whole set
    // before truncating.
    let mut records: Vec<UnifiedRecord> = match limit {
        Some(n) => Vec::with_capacity(n),
        None => Vec::new(),
    };
    manager.for_each_entity(|entity| {
        if !entity_visible_under_current_snapshot(entity) {
            return true;
        }
        if let Some(record) = runtime_table_record_from_entity(entity.clone()) {
            records.push(record);
            if let Some(n) = limit {
                if records.len() >= n {
                    return false; // stop scan early
                }
            }
        }
        true
    });
    Ok(records)
}

/// Scan with bloom filter optimization: when we know the exact key we're looking for,
/// use the bloom filter to skip segments that definitely don't contain it.
pub(super) fn scan_runtime_table_with_bloom_hint(
    db: &RedDB,
    table: &str,
    key_hint: Option<&[u8]>,
) -> RedDBResult<(Vec<UnifiedRecord>, bool)> {
    use crate::runtime::impl_core::entity_visible_under_current_snapshot;

    let manager = db
        .store()
        .get_collection(table)
        .ok_or_else(|| RedDBError::NotFound(table.to_string()))?;

    let (entities, pruned) = manager.query_with_bloom_hint(key_hint, |_| true);
    let records = entities
        .into_iter()
        .filter(entity_visible_under_current_snapshot)
        .filter_map(runtime_table_record_from_entity)
        .collect();
    Ok((records, pruned))
}

pub(super) fn is_universal_entity_source(table: &str) -> bool {
    is_universal_query_source(table)
}

/// Lean materialization for the index scan hot path.
///
/// Emits `red_entity_id`, `created_at`, `updated_at`, plus the raw user data
/// columns. Skips the heavier red_* metadata fields (collection, kind, type,
/// capabilities, sequence_id, row_id). Each skipped field is one fewer string
/// clone and one fewer HashMap insert per entity.
///
/// Used when the caller already knows the collection name and doesn't need
/// the full metadata in the result (e.g. SELECT * range/filtered scans).
/// Borrow-based variant of `runtime_table_record_lean` — used by scan
/// hot paths (via `SegmentManager::for_each_id`) that can't afford the
/// full `UnifiedEntity::clone`. Copies only the field values that land
/// in the output record.
#[inline]
pub(super) fn runtime_table_record_lean_ref(entity: &UnifiedEntity) -> Option<UnifiedRecord> {
    let created_at = entity.created_at;
    let updated_at = entity.updated_at;
    let row = match &entity.data {
        EntityData::Row(row) => row,
        _ => return None,
    };
    if let Some(named) = &row.named {
        let mut record = UnifiedRecord::with_capacity(3 + named.len());
        record.set_arc(
            sys_key_red_entity_id(),
            Value::UnsignedInteger(entity.id.raw()),
        );
        record.set_arc(sys_key_created_at(), Value::UnsignedInteger(created_at));
        record.set_arc(sys_key_updated_at(), Value::UnsignedInteger(updated_at));
        for (key, value) in named.iter() {
            record.set(key, value.clone());
        }
        Some(record)
    } else if let Some(schema) = &row.schema {
        // Columnar fast-path: build the record with the schema-shared
        // Vec<Arc<str>> side-channel so 4.5k rows × N fields deallocate
        // as one contiguous Vec each instead of a HashMap per record.
        let sys_schema = sys_schema_with_row_columns(schema);
        let mut values: Vec<Value> = Vec::with_capacity(sys_schema.len());
        values.push(Value::UnsignedInteger(entity.id.raw()));
        values.push(Value::UnsignedInteger(created_at));
        values.push(Value::UnsignedInteger(updated_at));
        values.extend(row.columns.iter().cloned());
        Some(UnifiedRecord::from_columnar(sys_schema, values))
    } else {
        let mut record = UnifiedRecord::with_capacity(3 + row.columns.len());
        record.set_arc(
            sys_key_red_entity_id(),
            Value::UnsignedInteger(entity.id.raw()),
        );
        record.set_arc(sys_key_created_at(), Value::UnsignedInteger(created_at));
        record.set_arc(sys_key_updated_at(), Value::UnsignedInteger(updated_at));
        for (i, value) in row.columns.iter().enumerate() {
            record.set(&format!("c{i}"), value.clone());
        }
        Some(record)
    }
}

#[inline]
pub(super) fn runtime_table_record_lean(entity: UnifiedEntity) -> Option<UnifiedRecord> {
    let created_at = entity.created_at;
    let updated_at = entity.updated_at;
    let row = match entity.data {
        EntityData::Row(row) => row,
        _ => return None,
    };
    if let Some(named) = row.named {
        let mut record = UnifiedRecord::with_capacity(3 + named.len());
        record.set_arc(
            sys_key_red_entity_id(),
            Value::UnsignedInteger(entity.id.raw()),
        );
        record.set_arc(sys_key_created_at(), Value::UnsignedInteger(created_at));
        record.set_arc(sys_key_updated_at(), Value::UnsignedInteger(updated_at));
        // `set_owned` consumes the already-heap-allocated String key
        // instead of `&str → String` clone. For SELECT * on a wide
        // result set this saves ~15k allocations per query.
        for (key, value) in named {
            record.set_owned(key, value);
        }
        Some(record)
    } else if let Some(ref schema) = row.schema {
        let mut record = UnifiedRecord::with_capacity(3 + schema.len());
        record.set_arc(
            sys_key_red_entity_id(),
            Value::UnsignedInteger(entity.id.raw()),
        );
        record.set_arc(sys_key_created_at(), Value::UnsignedInteger(created_at));
        record.set_arc(sys_key_updated_at(), Value::UnsignedInteger(updated_at));
        for (name, value) in schema.iter().zip(row.columns.into_iter()) {
            record.set(name, value);
        }
        Some(record)
    } else {
        let mut record = UnifiedRecord::with_capacity(3 + row.columns.len());
        record.set_arc(
            sys_key_red_entity_id(),
            Value::UnsignedInteger(entity.id.raw()),
        );
        record.set_arc(sys_key_created_at(), Value::UnsignedInteger(created_at));
        record.set_arc(sys_key_updated_at(), Value::UnsignedInteger(updated_at));
        for (i, value) in row.columns.into_iter().enumerate() {
            record.set(&format!("c{i}"), value);
        }
        Some(record)
    }
}

#[inline(never)]
pub(super) fn runtime_table_record_from_entity(entity: UnifiedEntity) -> Option<UnifiedRecord> {
    match entity.data {
        EntityData::Row(row) => {
            // Pre-allocate: ~9 system fields + user fields
            let user_field_count = row
                .named
                .as_ref()
                .map(|n| n.len())
                .unwrap_or(row.columns.len());
            let mut record = UnifiedRecord::with_capacity(9 + user_field_count);

            if let EntityKind::TableRow { row_id, .. } = &entity.kind {
                record.set_arc(sys_key_row_id(), Value::UnsignedInteger(*row_id));
            }

            record.set_arc(
                sys_key_red_entity_id(),
                Value::UnsignedInteger(entity.id.raw()),
            );
            record.set(
                "red_collection",
                Value::text(entity.kind.collection().to_string()),
            );
            record.set(
                "red_kind",
                Value::text(entity.kind.storage_type().to_string()),
            );
            record.set_arc(
                sys_key_created_at(),
                Value::UnsignedInteger(entity.created_at),
            );
            record.set_arc(
                sys_key_updated_at(),
                Value::UnsignedInteger(entity.updated_at),
            );
            record.set(
                "red_sequence_id",
                Value::UnsignedInteger(entity.sequence_id),
            );

            // Use fast capability string to avoid BTreeSet allocation
            let entity_type = runtime_row_entity_type(&row);
            let capabilities_str = runtime_row_capabilities_str(&row);
            record.set_arc(
                sys_key_red_entity_type(),
                Value::text(entity_type.to_string()),
            );
            record.set(
                "red_capabilities",
                Value::text(capabilities_str.to_string()),
            );

            if let Some(named) = row.named {
                for (key, value) in named {
                    record.set(&key, value);
                }
            } else if let Some(ref schema) = row.schema {
                // Columnar storage: use shared schema for field names
                for (name, value) in schema.iter().zip(row.columns.into_iter()) {
                    record.set(name, value);
                }
            } else {
                for (index, value) in row.columns.into_iter().enumerate() {
                    record.set(&format!("c{index}"), value);
                }
            }

            Some(record)
        }
        EntityData::TimeSeries(ts) => {
            let mut record = UnifiedRecord::with_capacity(12 + ts.tags.len());
            record.set_arc(
                sys_key_red_entity_id(),
                Value::UnsignedInteger(entity.id.raw()),
            );
            record.set(
                "red_collection",
                Value::text(entity.kind.collection().to_string()),
            );
            record.set(
                "red_kind",
                Value::text(entity.kind.storage_type().to_string()),
            );
            record.set_arc(
                sys_key_created_at(),
                Value::UnsignedInteger(entity.created_at),
            );
            record.set_arc(
                sys_key_updated_at(),
                Value::UnsignedInteger(entity.updated_at),
            );
            record.set(
                "red_sequence_id",
                Value::UnsignedInteger(entity.sequence_id),
            );
            record.set_arc(
                sys_key_red_entity_type(),
                Value::text("timeseries".to_string()),
            );
            record.set(
                "red_capabilities",
                Value::text("document,timeseries,metric,temporal".to_string()),
            );
            append_timeseries_record_fields(&mut record, &ts);
            Some(record)
        }
        _ => None,
    }
}

/// Borrowed version of `runtime_table_record_from_entity` — avoids cloning the full entity.
/// Only the field values inserted into the record are cloned, not the entity struct itself.
#[inline(never)]
pub(super) fn runtime_table_record_from_entity_ref(
    entity: &UnifiedEntity,
) -> Option<UnifiedRecord> {
    match &entity.data {
        EntityData::Row(row) => {
            let user_field_count = row
                .named
                .as_ref()
                .map(|n| n.len())
                .unwrap_or(row.columns.len());
            let mut record = UnifiedRecord::with_capacity(9 + user_field_count);

            if let EntityKind::TableRow { row_id, .. } = &entity.kind {
                record.set_arc(sys_key_row_id(), Value::UnsignedInteger(*row_id));
            }

            record.set_arc(
                sys_key_red_entity_id(),
                Value::UnsignedInteger(entity.id.raw()),
            );
            record.set(
                "red_collection",
                Value::text(entity.kind.collection().to_string()),
            );
            record.set(
                "red_kind",
                Value::text(entity.kind.storage_type().to_string()),
            );
            record.set_arc(
                sys_key_created_at(),
                Value::UnsignedInteger(entity.created_at),
            );
            record.set_arc(
                sys_key_updated_at(),
                Value::UnsignedInteger(entity.updated_at),
            );
            record.set(
                "red_sequence_id",
                Value::UnsignedInteger(entity.sequence_id),
            );

            let entity_type = runtime_row_entity_type(row);
            let capabilities_str = runtime_row_capabilities_str(row);
            record.set_arc(
                sys_key_red_entity_type(),
                Value::text(entity_type.to_string()),
            );
            record.set(
                "red_capabilities",
                Value::text(capabilities_str.to_string()),
            );

            if let Some(named) = &row.named {
                for (key, value) in named {
                    record.set(key, value.clone());
                }
            } else if let Some(schema) = &row.schema {
                for (name, value) in schema.iter().zip(row.columns.iter()) {
                    record.set(name, value.clone());
                }
            } else {
                for (index, value) in row.columns.iter().enumerate() {
                    record.set(&format!("c{index}"), value.clone());
                }
            }

            Some(record)
        }
        EntityData::TimeSeries(ts) => {
            let mut record = UnifiedRecord::with_capacity(12 + ts.tags.len());
            record.set_arc(
                sys_key_red_entity_id(),
                Value::UnsignedInteger(entity.id.raw()),
            );
            record.set(
                "red_collection",
                Value::text(entity.kind.collection().to_string()),
            );
            record.set(
                "red_kind",
                Value::text(entity.kind.storage_type().to_string()),
            );
            record.set_arc(
                sys_key_created_at(),
                Value::UnsignedInteger(entity.created_at),
            );
            record.set_arc(
                sys_key_updated_at(),
                Value::UnsignedInteger(entity.updated_at),
            );
            record.set(
                "red_sequence_id",
                Value::UnsignedInteger(entity.sequence_id),
            );
            record.set_arc(
                sys_key_red_entity_type(),
                Value::text("timeseries".to_string()),
            );
            record.set(
                "red_capabilities",
                Value::text("document,timeseries,metric,temporal".to_string()),
            );
            append_timeseries_record_fields(&mut record, ts);
            Some(record)
        }
        _ => None,
    }
}

/// Projected version — only materializes requested columns for better performance.
/// Falls back to full materialization if columns is empty (SELECT *).
#[inline(never)]
pub(super) fn runtime_table_record_from_entity_projected(
    entity: UnifiedEntity,
    columns: &[String],
) -> Option<UnifiedRecord> {
    if columns.is_empty() {
        return runtime_table_record_from_entity(entity);
    }

    match entity.data {
        EntityData::Row(row) => {
            let mut record = UnifiedRecord::with_capacity(1 + columns.len());

            // Always include system fields needed for filtering
            record.set_arc(
                sys_key_red_entity_id(),
                Value::UnsignedInteger(entity.id.raw()),
            );

            if let Some(named) = row.named {
                // Named path (single-insert entities): O(1) HashMap lookup per column.
                for col in columns {
                    if let Some(value) = named.get(col) {
                        record.set(col, value.clone());
                    }
                }
            } else if let Some(ref schema) = row.schema {
                // Columnar path (bulk-insert entities): resolve column name → index
                // in schema, then access row.columns[idx]. O(n_schema * n_projected)
                // but n_projected is small (explicit SELECT list).
                for col in columns {
                    if let Some(idx) = schema.iter().position(|s| s == col) {
                        if let Some(value) = row.columns.get(idx) {
                            record.set(col, value.clone());
                        }
                    }
                }
            } else {
                // Positional-only (no schema, no names): map c0/c1/... or fallback
                for col in columns {
                    if let Some(idx) = col.strip_prefix('c').and_then(|s| s.parse::<usize>().ok()) {
                        if let Some(value) = row.columns.get(idx) {
                            record.set(col, value.clone());
                        }
                    }
                }
            }

            Some(record)
        }
        EntityData::TimeSeries(ts) => {
            let mut record = UnifiedRecord::new();
            record.set_arc(
                sys_key_red_entity_id(),
                Value::UnsignedInteger(entity.id.raw()),
            );

            for col in columns {
                match col.as_str() {
                    "metric" => record.set("metric", Value::text(ts.metric.clone())),
                    "timestamp_ns" => {
                        record.set("timestamp_ns", Value::UnsignedInteger(ts.timestamp_ns))
                    }
                    "timestamp" => record.set("timestamp", Value::UnsignedInteger(ts.timestamp_ns)),
                    "time" => record.set("time", Value::UnsignedInteger(ts.timestamp_ns)),
                    "value" => record.set("value", Value::Float(ts.value)),
                    "tags" if !ts.tags.is_empty() => {
                        record.set("tags", timeseries_tags_value(&ts.tags));
                    }
                    _ => {}
                }
            }

            Some(record)
        }
        _ => None,
    }
}

/// Ref-based projected materialization — avoids cloning the whole entity.
/// Only clones the K projected field values, not the N-K ignored ones.
/// Pre-computed index path for columnar (bulk-inserted) entities.
///
/// `idx_map` is `&[(col_pos_in_columns, schema_idx)]` — precomputed once
/// before the scan loop so every row does O(k) direct indexed access instead
/// of O(schema_len × k) linear searches.
///
/// Returns `None` when the entity is not a columnar Row (schema = None).
pub(super) fn runtime_table_record_with_col_indices(
    entity: &UnifiedEntity,
    columns: &[String],
    idx_map: &[(usize, usize)],
) -> Option<UnifiedRecord> {
    if idx_map.is_empty() {
        return None;
    }
    let row = entity.data.as_row()?;
    // Only applies to columnar entities (row.schema is Some, row.named is None)
    if row.named.is_some() || row.schema.is_none() {
        return None;
    }
    let mut record = UnifiedRecord::with_capacity(1 + idx_map.len());
    record.set_arc(
        sys_key_red_entity_id(),
        Value::UnsignedInteger(entity.id.raw()),
    );
    for &(ci, si) in idx_map {
        if let Some(value) = row.columns.get(si) {
            // Use set_owned to skip the hidden to_string() clone.
            record.set_owned(columns[ci].clone(), value.clone());
        }
    }
    Some(record)
}

/// Used by the fast-path scan when `select_cols` is non-empty.
///
/// Returns `None` for non-Row entities (caller falls back to owned path).
pub(super) fn runtime_table_record_from_entity_ref_projected(
    entity: &UnifiedEntity,
    columns: &[String],
) -> Option<UnifiedRecord> {
    if columns.is_empty() {
        return None; // caller should use full materialization for SELECT *
    }
    let row = entity.data.as_row()?;

    let mut record = UnifiedRecord::with_capacity(1 + columns.len());
    record.set_arc(
        sys_key_red_entity_id(),
        Value::UnsignedInteger(entity.id.raw()),
    );

    if let Some(ref named) = row.named {
        for col in columns {
            if let Some(value) = named.get(col.as_str()) {
                record.set(col, value.clone());
            }
        }
    } else if let Some(ref schema) = row.schema {
        for col in columns {
            if let Some(idx) = schema.iter().position(|s| s == col) {
                if let Some(value) = row.columns.get(idx) {
                    record.set(col, value.clone());
                }
            }
        }
    } else {
        for col in columns {
            if let Some(idx) = col.strip_prefix('c').and_then(|s| s.parse::<usize>().ok()) {
                if let Some(value) = row.columns.get(idx) {
                    record.set(col, value.clone());
                }
            }
        }
    }
    Some(record)
}

#[inline(never)]
pub(super) fn runtime_any_record_from_entity(entity: UnifiedEntity) -> Option<UnifiedRecord> {
    let identity_entity = entity.clone();
    let kind = entity.kind.clone();
    let collection = kind.collection().to_string();
    let storage_type = kind.storage_type().to_string();
    let entity_id = entity.id.raw();
    let created_at = entity.created_at;
    let updated_at = entity.updated_at;
    let sequence_id = entity.sequence_id;

    let (entity_type, capabilities, mut record) = match (kind, entity.data) {
        (EntityKind::TableRow { row_id, .. }, EntityData::Row(row)) => {
            let capabilities = runtime_row_capabilities(&row);
            let entity_type = runtime_row_entity_type(&row);
            let mut record = UnifiedRecord::new();
            record.set_arc(sys_key_row_id(), Value::UnsignedInteger(row_id));
            if let Some(named) = row.named {
                for (key, value) in named {
                    record.set(&key, value);
                }
            } else {
                for (index, value) in row.columns.into_iter().enumerate() {
                    record.set(&format!("c{index}"), value);
                }
            }
            (entity_type, capabilities, record)
        }
        (EntityKind::GraphNode(node), EntityData::Node(node_data)) => {
            let mut record = UnifiedRecord::new();
            record.set("id", Value::UnsignedInteger(entity_id));
            record.set("label", Value::text(node.label));
            record.set("node_type", Value::text(node.node_type));
            for (key, value) in node_data.properties {
                record.set(&key, value);
            }
            (
                "graph_node",
                runtime_record_capability_list(["graph", "graph_node"]),
                record,
            )
        }
        (EntityKind::GraphEdge(edge_kind), EntityData::Edge(edge)) => {
            let mut record = UnifiedRecord::new();
            record.set("label", Value::text(edge_kind.label));
            record.set("from", Value::NodeRef(edge_kind.from_node.clone()));
            record.set("to", Value::NodeRef(edge_kind.to_node.clone()));
            record.set("weight", Value::Float(edge.weight as f64));
            for (key, value) in edge.properties {
                record.set(&key, value);
            }
            (
                "graph_edge",
                runtime_record_capability_list(["graph", "graph_edge"]),
                record,
            )
        }
        (EntityKind::Vector { .. }, EntityData::Vector(vector)) => {
            let mut record = UnifiedRecord::new();
            record.set(
                "dimension",
                Value::UnsignedInteger(vector.dense.len() as u64),
            );
            if let Some(content) = vector.content {
                record.set("content", Value::text(content));
            }
            (
                "vector",
                runtime_record_capability_list(["vector", "similarity", "embedding"]),
                record,
            )
        }
        (EntityKind::TimeSeriesPoint(_), EntityData::TimeSeries(ts)) => {
            let mut record = UnifiedRecord::new();
            append_timeseries_record_fields(&mut record, &ts);
            (
                "timeseries",
                runtime_record_capability_list(["document", "timeseries", "metric", "temporal"]),
                record,
            )
        }
        (EntityKind::QueueMessage { position, .. }, EntityData::QueueMessage(msg)) => {
            // Phase 2.5.5 RLS universal: queue messages surface their
            // `payload` Value, priority, attempts, ack state, and
            // position so policies like
            // `USING (payload.tenant = CURRENT_TENANT())` can reach
            // the JSON payload via the dotted-path resolver.
            let mut record = UnifiedRecord::new();
            record.set("position", Value::UnsignedInteger(position));
            record.set("payload", msg.payload);
            record.set("attempts", Value::UnsignedInteger(msg.attempts as u64));
            record.set("acked", Value::Boolean(msg.acked));
            if let Some(priority) = msg.priority {
                record.set("priority", Value::Integer(priority as i64));
            }
            (
                "queue_message",
                runtime_record_capability_list(["document", "queue", "message"]),
                record,
            )
        }
        _ => return None,
    };

    record.set_arc(sys_key_red_entity_id(), Value::UnsignedInteger(entity_id));
    record.set_arc(sys_key_red_collection(), Value::text(collection));
    record.set_arc(sys_key_red_kind(), Value::text(storage_type));
    record.set_arc(sys_key_created_at(), Value::UnsignedInteger(created_at));
    record.set_arc(sys_key_updated_at(), Value::UnsignedInteger(updated_at));
    record.set_arc(
        sys_key_red_sequence_id(),
        Value::UnsignedInteger(sequence_id),
    );
    set_runtime_entity_metadata(&mut record, entity_type, capabilities);
    apply_runtime_identity_hints(&mut record, &identity_entity);

    Some(record)
}

pub(super) fn runtime_any_record_from_entity_ref(entity: &UnifiedEntity) -> Option<UnifiedRecord> {
    let kind = &entity.kind;
    let collection = kind.collection().to_string();
    let storage_type = kind.storage_type().to_string();
    let entity_id = entity.id.raw();
    let created_at = entity.created_at;
    let updated_at = entity.updated_at;
    let sequence_id = entity.sequence_id;

    let (entity_type, capabilities, mut record) = match (kind, &entity.data) {
        (EntityKind::TableRow { row_id, .. }, EntityData::Row(row)) => {
            let capabilities = runtime_row_capabilities(row);
            let entity_type = runtime_row_entity_type(row);
            let mut record = UnifiedRecord::new();
            record.set_arc(sys_key_row_id(), Value::UnsignedInteger(*row_id));
            if let Some(named) = row.named.as_ref() {
                for (key, value) in named {
                    record.set(key, value.clone());
                }
            } else if let Some(schema) = row.schema.as_ref() {
                for (name, value) in schema.iter().zip(row.columns.iter()) {
                    record.set(name, value.clone());
                }
            } else {
                for (index, value) in row.columns.iter().enumerate() {
                    record.set(&format!("c{index}"), value.clone());
                }
            }
            (entity_type, capabilities, record)
        }
        (EntityKind::GraphNode(node), EntityData::Node(node_data)) => {
            let mut record = UnifiedRecord::new();
            record.set("id", Value::UnsignedInteger(entity_id));
            record.set("label", Value::text(node.label.clone()));
            record.set("node_type", Value::text(node.node_type.clone()));
            for (key, value) in &node_data.properties {
                record.set(key, value.clone());
            }
            (
                "graph_node",
                runtime_record_capability_list(["graph", "graph_node"]),
                record,
            )
        }
        (EntityKind::GraphEdge(edge_kind), EntityData::Edge(edge)) => {
            let mut record = UnifiedRecord::new();
            record.set("label", Value::text(edge_kind.label.clone()));
            record.set("from", Value::NodeRef(edge_kind.from_node.clone()));
            record.set("to", Value::NodeRef(edge_kind.to_node.clone()));
            record.set("weight", Value::Float(edge.weight as f64));
            for (key, value) in &edge.properties {
                record.set(key, value.clone());
            }
            (
                "graph_edge",
                runtime_record_capability_list(["graph", "graph_edge"]),
                record,
            )
        }
        (EntityKind::Vector { .. }, EntityData::Vector(vector)) => {
            let mut record = UnifiedRecord::new();
            record.set(
                "dimension",
                Value::UnsignedInteger(vector.dense.len() as u64),
            );
            if let Some(content) = vector.content.as_ref() {
                record.set("content", Value::text(content.clone()));
            }
            (
                "vector",
                runtime_record_capability_list(["vector", "similarity", "embedding"]),
                record,
            )
        }
        (EntityKind::TimeSeriesPoint(_), EntityData::TimeSeries(ts)) => {
            let mut record = UnifiedRecord::new();
            append_timeseries_record_fields(&mut record, ts);
            (
                "timeseries",
                runtime_record_capability_list(["document", "timeseries", "metric", "temporal"]),
                record,
            )
        }
        _ => return None,
    };

    record.set_arc(sys_key_red_entity_id(), Value::UnsignedInteger(entity_id));
    record.set_arc(sys_key_red_collection(), Value::text(collection));
    record.set_arc(sys_key_red_kind(), Value::text(storage_type));
    record.set_arc(sys_key_created_at(), Value::UnsignedInteger(created_at));
    record.set_arc(sys_key_updated_at(), Value::UnsignedInteger(updated_at));
    record.set_arc(
        sys_key_red_sequence_id(),
        Value::UnsignedInteger(sequence_id),
    );
    set_runtime_entity_metadata(&mut record, entity_type, capabilities);
    apply_runtime_identity_hints(&mut record, entity);

    Some(record)
}

fn append_timeseries_record_fields(
    record: &mut UnifiedRecord,
    ts: &crate::storage::TimeSeriesData,
) {
    record.set("metric", Value::text(ts.metric.clone()));
    record.set("timestamp_ns", Value::UnsignedInteger(ts.timestamp_ns));
    record.set("timestamp", Value::UnsignedInteger(ts.timestamp_ns));
    record.set("time", Value::UnsignedInteger(ts.timestamp_ns));
    record.set("value", Value::Float(ts.value));
    if !ts.tags.is_empty() {
        record.set("tags", timeseries_tags_value(&ts.tags));
    }
}

fn timeseries_tags_value(tags: &std::collections::HashMap<String, String>) -> Value {
    let object = tags
        .iter()
        .map(|(key, value)| (key.clone(), crate::json::Value::String(value.clone())))
        .collect();
    let json = crate::json::Value::Object(object);
    Value::Json(crate::json::to_vec(&json).unwrap_or_default())
}

#[inline(never)]
pub(super) fn set_runtime_entity_metadata(
    record: &mut UnifiedRecord,
    entity_type: &str,
    capabilities: BTreeSet<String>,
) {
    let capabilities_text = capabilities.into_iter().collect::<Vec<_>>().join(",");
    record.set_arc(
        sys_key_red_entity_type(),
        Value::text(entity_type.to_string()),
    );
    record.set_arc(sys_key_red_capabilities(), Value::text(capabilities_text));
}

pub(super) fn runtime_record_capability_list<const N: usize>(
    values: [&str; N],
) -> BTreeSet<String> {
    values.into_iter().map(|value| value.to_string()).collect()
}

pub(super) fn runtime_row_capabilities(row: &crate::storage::RowData) -> BTreeSet<String> {
    let mut capabilities = runtime_record_capability_list(["table", "structured"]);
    if runtime_row_is_kv(row) {
        capabilities.insert("kv".to_string());
    }
    if runtime_row_has_document_capability(row) {
        capabilities.insert("document".to_string());
    }
    capabilities
}

/// Fast capability string for table rows — avoids BTreeSet allocation.
/// Returns a pre-computed comma-separated capabilities string.
pub(super) fn runtime_row_capabilities_str(row: &crate::storage::RowData) -> &'static str {
    let is_kv = runtime_row_is_kv(row);
    let is_doc = runtime_row_has_document_capability(row);
    match (is_kv, is_doc) {
        (false, false) => "structured,table",
        (true, false) => "kv,structured,table",
        (false, true) => "document,structured,table",
        (true, true) => "document,kv,structured,table",
    }
}

pub(super) fn runtime_row_entity_type(row: &crate::storage::RowData) -> &'static str {
    if runtime_row_is_kv(row) {
        return "kv";
    }

    if runtime_row_has_document_capability(row) {
        "document"
    } else {
        "table"
    }
}

fn runtime_row_is_kv(row: &crate::storage::RowData) -> bool {
    let Some(named) = row.named.as_ref() else {
        return false;
    };

    if named.len() == 2 {
        named.contains_key("key") && named.contains_key("value")
    } else if named.len() == 1 {
        named.contains_key("key") || named.contains_key("value")
    } else {
        false
    }
}

pub(super) fn runtime_row_has_document_capability(row: &crate::storage::RowData) -> bool {
    row.named
        .as_ref()
        .map(|named| named.values().any(runtime_documentish_value))
        .unwrap_or(false)
        || row.columns.iter().any(runtime_documentish_value)
}

pub(super) fn runtime_documentish_value(value: &Value) -> bool {
    matches!(value, Value::Json(_) | Value::Blob(_))
}

pub(super) fn runtime_search_collections(
    db: &RedDB,
    collections: Option<Vec<String>>,
) -> Option<Vec<String>> {
    match collections {
        Some(collections) if !collections.is_empty() => Some(collections),
        _ => Some(db.store().list_collections()),
    }
}

pub(super) fn runtime_filter_dsl_result(
    result: &mut DslQueryResult,
    entity_types: Option<Vec<String>>,
    capabilities: Option<Vec<String>>,
) {
    let entity_types = entity_types
        .map(|items| {
            items
                .into_iter()
                .map(|item| item.trim().to_ascii_lowercase())
                .filter(|item| !item.is_empty())
                .collect::<BTreeSet<_>>()
        })
        .filter(|items| !items.is_empty());
    let capabilities = capabilities
        .map(|items| {
            items
                .into_iter()
                .map(|item| item.trim().to_ascii_lowercase())
                .filter(|item| !item.is_empty())
                .collect::<BTreeSet<_>>()
        })
        .filter(|items| !items.is_empty());

    if entity_types.is_none() && capabilities.is_none() {
        return;
    }

    result.matches.retain(|item| {
        let (entity_type, item_capabilities) = runtime_entity_type_and_capabilities(&item.entity);
        let type_ok = entity_types
            .as_ref()
            .is_none_or(|accepted| accepted.contains(entity_type));
        let capability_ok = capabilities.as_ref().is_none_or(|accepted| {
            item_capabilities
                .iter()
                .any(|capability| accepted.contains(capability))
        });
        type_ok && capability_ok
    });

    normalize_runtime_dsl_result_scores(result);
}

pub(super) fn normalize_runtime_dsl_result_scores(result: &mut DslQueryResult) {
    for item in &mut result.matches {
        if let Some(final_score) = item
            .components
            .final_score
            .filter(|score| score.is_finite())
        {
            item.score = final_score;
        } else {
            item.components.final_score = Some(item.score);
        }
    }

    result.matches.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.entity.id.raw().cmp(&right.entity.id.raw()))
    });
}

pub(super) fn runtime_entity_type_and_capabilities(
    entity: &UnifiedEntity,
) -> (&'static str, BTreeSet<String>) {
    match (&entity.kind, &entity.data) {
        (EntityKind::TableRow { .. }, EntityData::Row(row)) => {
            (runtime_row_entity_type(row), runtime_row_capabilities(row))
        }
        (EntityKind::GraphNode(_), EntityData::Node(_)) => (
            "graph_node",
            runtime_record_capability_list(["graph", "graph_node"]),
        ),
        (EntityKind::GraphEdge(_), EntityData::Edge(_)) => (
            "graph_edge",
            runtime_record_capability_list(["graph", "graph_edge"]),
        ),
        (EntityKind::Vector { .. }, EntityData::Vector(_)) => (
            "vector",
            runtime_record_capability_list(["vector", "similarity", "embedding"]),
        ),
        (EntityKind::TimeSeriesPoint(_), EntityData::TimeSeries(_)) => (
            "timeseries",
            runtime_record_capability_list(["document", "timeseries", "metric", "temporal"]),
        ),
        _ => ("unknown", BTreeSet::new()),
    }
}

pub(super) fn resolve_runtime_vector_source(
    db: &RedDB,
    source: &VectorSource,
) -> RedDBResult<Vec<f32>> {
    match source {
        VectorSource::Literal(vector) => Ok(vector.clone()),
        VectorSource::Reference {
            collection: _,
            vector_id,
        } => {
            let entity = db
                .get(EntityId::new(*vector_id))
                .ok_or_else(|| RedDBError::NotFound(format!("vector:{vector_id}")))?;
            match entity.data {
                EntityData::Vector(data) => Ok(data.dense),
                _ => Err(RedDBError::Query(format!(
                    "entity {vector_id} is not a vector source"
                ))),
            }
        }
        VectorSource::Text(text) => embed_runtime_vector_text(db, text),
        VectorSource::Subquery(expr) => resolve_runtime_vector_subquery(db, expr.as_ref()),
    }
}

fn embed_runtime_vector_text(db: &RedDB, text: &str) -> RedDBResult<Vec<f32>> {
    let kv_getter = |key: &str| -> RedDBResult<Option<String>> {
        match db.get_kv("red_config", key) {
            Some((Value::Text(value), _)) => Ok(Some(value.to_string())),
            Some(_) => Ok(None),
            None => Ok(None),
        }
    };

    let provider = crate::ai::resolve_default_provider(&kv_getter);
    let model = crate::ai::resolve_default_model(&provider, &kv_getter);
    let api_key = crate::ai::resolve_api_key(&provider, None, kv_getter)?;
    let response = crate::ai::openai_embeddings(crate::ai::OpenAiEmbeddingRequest {
        api_key,
        model,
        inputs: vec![text.to_string()],
        dimensions: None,
        api_base: provider.resolve_api_base(),
    })?;

    response
        .embeddings
        .into_iter()
        .next()
        .ok_or_else(|| RedDBError::Query("embedding API returned no vectors".to_string()))
}

fn resolve_runtime_vector_subquery(db: &RedDB, expr: &QueryExpr) -> RedDBResult<Vec<f32>> {
    let records = execute_runtime_vector_subquery_records(db, expr)?;
    let record = records
        .first()
        .ok_or_else(|| RedDBError::Query("vector source subquery returned no rows".to_string()))?;

    extract_runtime_vector_from_record(db, record)?.ok_or_else(|| {
        RedDBError::Query(
            "vector source subquery must return a vector value, vector reference, or vector entity id"
                .to_string(),
        )
    })
}

fn execute_runtime_vector_subquery_records(
    db: &RedDB,
    expr: &QueryExpr,
) -> RedDBResult<Vec<UnifiedRecord>> {
    match expr {
        QueryExpr::Table(query) => Ok(execute_runtime_table_query(db, query, None)?.records),
        QueryExpr::Graph(_) | QueryExpr::Path(_) => {
            let plan = CanonicalPlanner::new(db).build(expr);
            execute_runtime_canonical_expr_node(db, &plan.root, expr)
        }
        QueryExpr::Join(query) => Ok(execute_runtime_join_query(db, query)?.records),
        QueryExpr::Vector(query) => Ok(execute_runtime_vector_query(db, query)?.records),
        QueryExpr::Hybrid(query) => Ok(execute_runtime_hybrid_query(db, query)?.records),
        other => Err(RedDBError::Query(format!(
            "vector source subqueries do not support {} statements",
            query_expr_name(other)
        ))),
    }
}

fn extract_runtime_vector_from_record(
    db: &RedDB,
    record: &UnifiedRecord,
) -> RedDBResult<Option<Vec<f32>>> {
    for key in ["dense", "vector", "embedding", "query_vector"] {
        if let Some(value) = record.get(key) {
            if let Some(vector) = resolve_runtime_vector_value(db, value)? {
                return Ok(Some(vector));
            }
        }
    }

    for key in ["red_entity_id", "entity_id", "vector_id", "id"] {
        if let Some(value) = record.get(key) {
            if let Some(vector) = resolve_runtime_vector_entity_value(db, value)? {
                return Ok(Some(vector));
            }
        }
    }

    if record.values.len() == 1 {
        if let Some(value) = record.values.values().next() {
            if let Some(vector) = resolve_runtime_vector_value(db, value)? {
                return Ok(Some(vector));
            }
        }
    }

    for value in record.values.values() {
        match value {
            Value::Vector(vector) => return Ok(Some(vector.clone())),
            Value::VectorRef(_, vector_id) => {
                if let Some(vector) = runtime_vector_entity_by_id(db, *vector_id)? {
                    return Ok(Some(vector));
                }
            }
            _ => {}
        }
    }

    Ok(None)
}

fn resolve_runtime_vector_value(db: &RedDB, value: &Value) -> RedDBResult<Option<Vec<f32>>> {
    match value {
        Value::Vector(vector) => Ok(Some(vector.clone())),
        Value::Array(values) => Ok(Some(runtime_value_array_to_vector(values)?)),
        Value::Json(bytes) => Ok(Some(runtime_json_bytes_to_vector(bytes)?)),
        Value::VectorRef(_, vector_id) => runtime_vector_entity_by_id(db, *vector_id),
        Value::UnsignedInteger(vector_id) => runtime_vector_entity_by_id(db, *vector_id),
        Value::Integer(vector_id) if *vector_id >= 0 => {
            runtime_vector_entity_by_id(db, *vector_id as u64)
        }
        _ => Ok(None),
    }
}

fn resolve_runtime_vector_entity_value(db: &RedDB, value: &Value) -> RedDBResult<Option<Vec<f32>>> {
    match value {
        Value::UnsignedInteger(vector_id) => runtime_vector_entity_by_id(db, *vector_id),
        Value::Integer(vector_id) if *vector_id >= 0 => {
            runtime_vector_entity_by_id(db, *vector_id as u64)
        }
        Value::VectorRef(_, vector_id) => runtime_vector_entity_by_id(db, *vector_id),
        _ => Ok(None),
    }
}

fn runtime_vector_entity_by_id(db: &RedDB, vector_id: u64) -> RedDBResult<Option<Vec<f32>>> {
    let Some(entity) = db.get(EntityId::new(vector_id)) else {
        return Ok(None);
    };

    match entity.data {
        EntityData::Vector(vector) => Ok(Some(vector.dense)),
        _ => Ok(None),
    }
}

fn runtime_value_array_to_vector(values: &[Value]) -> RedDBResult<Vec<f32>> {
    values
        .iter()
        .map(|value| match value {
            Value::Float(number) => Ok(*number as f32),
            Value::Integer(number) => Ok(*number as f32),
            Value::UnsignedInteger(number) => Ok(*number as f32),
            other => Err(RedDBError::Query(format!(
                "vector arrays accept only numeric values, got {other:?}"
            ))),
        })
        .collect()
}

fn runtime_json_bytes_to_vector(bytes: &[u8]) -> RedDBResult<Vec<f32>> {
    crate::json::from_slice(bytes).map_err(|err| {
        RedDBError::Query(format!("vector JSON source must be a numeric array: {err}"))
    })
}

pub(super) fn runtime_vector_record_from_match(item: SimilarResult) -> UnifiedRecord {
    let mut record = UnifiedRecord::new();
    let (entity_type, capabilities) = runtime_entity_type_and_capabilities(&item.entity);
    record.set("entity_id", Value::UnsignedInteger(item.entity_id.raw()));
    record.set(
        "red_entity_id",
        Value::UnsignedInteger(item.entity_id.raw()),
    );
    record.set("score", Value::Float(item.score as f64));
    record.set("_score", Value::Float(item.score as f64));
    record.set("final_score", Value::Float(item.score as f64));
    record.set("distance", Value::Float(item.distance as f64));
    record.set("_distance", Value::Float(item.distance as f64));
    record.set("vector_distance", Value::Float(item.distance as f64));
    record.set("vector_score", Value::Float(item.score as f64));
    record.set("vector_similarity", Value::Float(item.score as f64));
    record.set(
        "collection",
        Value::text(item.entity.kind.collection().to_string()),
    );
    record.set(
        "red_collection",
        Value::text(item.entity.kind.collection().to_string()),
    );
    record.set(
        "red_kind",
        Value::text(item.entity.kind.storage_type().to_string()),
    );
    record.set_arc(
        sys_key_created_at(),
        Value::UnsignedInteger(item.entity.created_at),
    );
    record.set_arc(
        sys_key_updated_at(),
        Value::UnsignedInteger(item.entity.updated_at),
    );
    record.set(
        "red_sequence_id",
        Value::UnsignedInteger(item.entity.sequence_id),
    );
    set_runtime_entity_metadata(&mut record, entity_type, capabilities);
    apply_runtime_identity_hints(&mut record, &item.entity);

    match item.entity.data {
        EntityData::Vector(data) => {
            record.set("dimension", Value::UnsignedInteger(data.dense.len() as u64));
            if let Some(content) = data.content {
                record.set("content", Value::text(content));
            } else {
                record.set("content", Value::Null);
            }
        }
        EntityData::Row(row) => {
            record.set("dimension", Value::Null);
            if let Some(named) = row.named {
                for (key, value) in named {
                    record.set(&key, value);
                }
            }
        }
        EntityData::Node(node) => {
            record.set("dimension", Value::Null);
            for (key, value) in node.properties {
                record.set(&key, value);
            }
        }
        EntityData::Edge(edge) => {
            record.set("dimension", Value::Null);
            record.set("weight", Value::Float(edge.weight as f64));
            for (key, value) in edge.properties {
                record.set(&key, value);
            }
        }
        EntityData::TimeSeries(ts) => {
            record.set("dimension", Value::Null);
            record.set("metric", Value::text(ts.metric));
            record.set("timestamp_ns", Value::UnsignedInteger(ts.timestamp_ns));
            record.set("value", Value::Float(ts.value));
        }
        EntityData::QueueMessage(msg) => {
            record.set("dimension", Value::Null);
            record.set("payload", msg.payload);
            record.set("attempts", Value::UnsignedInteger(msg.attempts as u64));
            record.set("acked", Value::Boolean(msg.acked));
        }
    }

    record
}

pub(super) fn hybrid_candidate_keys(
    structured: &HashMap<String, UnifiedRecord>,
    vector: &HashMap<String, UnifiedRecord>,
    fusion: &FusionStrategy,
) -> Vec<String> {
    let structured_keys: BTreeSet<String> = structured.keys().cloned().collect();
    let vector_keys: BTreeSet<String> = vector.keys().cloned().collect();

    match fusion {
        FusionStrategy::Rerank { .. } => structured_keys.into_iter().collect(),
        FusionStrategy::FilterThenSearch
        | FusionStrategy::SearchThenFilter
        | FusionStrategy::Intersection => structured_keys
            .intersection(&vector_keys)
            .cloned()
            .collect(),
        FusionStrategy::Union { .. } | FusionStrategy::RRF { .. } => {
            structured_keys.union(&vector_keys).cloned().collect()
        }
    }
}

pub(super) fn runtime_record_identity_key(record: &UnifiedRecord) -> String {
    for key in [
        "_source_row",
        "_source_node",
        "_source_edge",
        "_source_entity",
        "_linked_identity",
    ] {
        if let Some(value) = record.values.get(key) {
            if let Some(fragment) = runtime_identity_fragment(value) {
                return format!("link:{fragment}");
            }
        }
    }

    if let Some(value) = record
        .values
        .get("entity_id")
        .or_else(|| record.values.get("red_entity_id"))
    {
        if let Some(fragment) = runtime_identity_fragment(value) {
            return format!("entity:{fragment}");
        }
    }

    if let (Some(collection), Some(row_id)) = (
        record
            .values
            .get("red_collection")
            .and_then(runtime_value_text),
        record
            .values
            .get("row_id")
            .or_else(|| record.values.get("id")),
    ) {
        if let Some(fragment) = runtime_identity_fragment(row_id) {
            return format!("row:{collection}:{fragment}");
        }
    }

    if let Some((alias, node)) = record.nodes.iter().next() {
        return format!("node:{alias}:{}", node.id);
    }

    if let Some(value) = record
        .values
        .iter()
        .find_map(|(key, value)| key.ends_with(".id").then_some(value))
    {
        if let Some(fragment) = runtime_identity_fragment(value) {
            return format!("ref:{fragment}");
        }
    }

    if let Some(value) = record.values.get("id") {
        if let Some(fragment) = runtime_identity_fragment(value) {
            return format!("id:{fragment}");
        }
    }

    if let Some(node) = record.paths.first().and_then(|path| path.nodes.first()) {
        return format!("path:{node}");
    }

    format!(
        "fingerprint:{:016x}",
        runtime_record_identity_fingerprint(record)
    )
}

fn runtime_record_identity_fingerprint(record: &UnifiedRecord) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    let mix = |hash: &mut u64, bytes: &[u8]| {
        for &byte in bytes {
            *hash ^= u64::from(byte);
            *hash = hash.wrapping_mul(0x100000001b3);
        }
    };

    let mut value_keys: Vec<_> = record.values.iter().collect();
    value_keys.sort_by(|left, right| left.0.cmp(right.0));
    for (key, value) in value_keys {
        mix(&mut hash, key.as_bytes());
        mix(&mut hash, b"\x00");
        let bytes = value.to_bytes();
        mix(&mut hash, &bytes);
        mix(&mut hash, b"|");
    }

    let mut nodes: Vec<_> = record.nodes.iter().collect();
    nodes.sort_by(|left, right| left.0.cmp(right.0));
    for (alias, node) in nodes {
        mix(&mut hash, alias.as_bytes());
        mix(&mut hash, b"\x1f");
        mix(&mut hash, node.id.as_bytes());
        mix(&mut hash, node.label.as_bytes());
        mix(&mut hash, format!("{:?}", node.node_type).as_bytes());
        mix(&mut hash, b"|");
    }

    let mut edges: Vec<_> = record.edges.iter().collect();
    edges.sort_by(|left, right| left.0.cmp(right.0));
    for (alias, edge) in edges {
        mix(&mut hash, alias.as_bytes());
        mix(&mut hash, b"\x1f");
        mix(&mut hash, edge.from.as_bytes());
        mix(&mut hash, b"->");
        mix(&mut hash, edge.to.as_bytes());
        mix(&mut hash, format!("{:?}", edge.edge_type).as_bytes());
        mix(&mut hash, b"::");
        mix(&mut hash, format!("{:.8}", edge.weight).as_bytes());
        mix(&mut hash, b"|");
    }

    let mut paths: Vec<_> = record.paths.iter().collect();
    paths.sort_by(|left, right| {
        let left_node = left.nodes.first().map(|node| node.as_str()).unwrap_or("");
        let right_node = right.nodes.first().map(|node| node.as_str()).unwrap_or("");
        left_node.cmp(right_node)
    });
    for path in paths {
        for node in &path.nodes {
            mix(&mut hash, node.as_bytes());
            mix(&mut hash, b",");
        }
        mix(&mut hash, b"|");
        for edge in &path.edges {
            mix(&mut hash, edge.from.as_bytes());
            mix(&mut hash, b"->");
            mix(&mut hash, edge.to.as_bytes());
            mix(&mut hash, b"::");
            mix(&mut hash, format!("{:?}", edge.edge_type).as_bytes());
            mix(&mut hash, b":");
            mix(&mut hash, format!("{:.8}", edge.weight).as_bytes());
            mix(&mut hash, b",");
        }
        mix(&mut hash, b"|");
    }

    let mut vector_results: Vec<_> = record.vector_results.iter().collect();
    vector_results.sort_by(|left, right| {
        (left.collection.as_str(), left.id).cmp(&(right.collection.as_str(), right.id))
    });
    for result in vector_results {
        mix(&mut hash, result.collection.as_bytes());
        mix(&mut hash, b"#");
        mix(&mut hash, result.id.to_string().as_bytes());
        mix(&mut hash, b"::");
        mix(&mut hash, format!("{:.8}", result.distance).as_bytes());
    }

    hash
}

pub(super) fn runtime_identity_fragment(value: &Value) -> Option<String> {
    match value {
        Value::Integer(value) => Some(value.to_string()),
        Value::UnsignedInteger(value) => Some(value.to_string()),
        Value::Float(value) => Some(value.to_string()),
        Value::Text(value) => Some(value.to_string()),
        Value::NodeRef(value) => Some(value.clone()),
        Value::EdgeRef(value) => Some(value.clone()),
        Value::RowRef(table, row_id) => Some(format!("{table}:{row_id}")),
        Value::VectorRef(collection, vector_id) => Some(format!("{collection}:{vector_id}")),
        _ => runtime_value_text(value),
    }
}

pub(super) fn apply_runtime_identity_hints(record: &mut UnifiedRecord, entity: &UnifiedEntity) {
    for cross_ref in entity.cross_refs() {
        let value = match cross_ref.ref_type {
            RefType::VectorToRow | RefType::NodeToRow => Some(Value::RowRef(
                cross_ref.target_collection.clone(),
                cross_ref.target.raw(),
            )),
            RefType::VectorToNode | RefType::RowToNode => Some(Value::NodeRef(format!(
                "{}:{}",
                cross_ref.target_collection, cross_ref.target
            ))),
            RefType::RowToEdge | RefType::EdgeToVector => Some(Value::EdgeRef(format!(
                "{}:{}",
                cross_ref.target_collection, cross_ref.target
            ))),
            _ => Some(Value::text(format!(
                "{}:{}",
                cross_ref.target_collection, cross_ref.target
            ))),
        };

        if let Some(value) = value {
            let link_key: std::sync::Arc<str> = std::sync::Arc::from("_linked_identity");
            match cross_ref.ref_type {
                RefType::VectorToRow | RefType::NodeToRow => {
                    record
                        .values
                        .insert(std::sync::Arc::from("_source_row"), value.clone());
                    record.values.entry(link_key).or_insert(value);
                }
                RefType::VectorToNode | RefType::RowToNode => {
                    record
                        .values
                        .insert(std::sync::Arc::from("_source_node"), value.clone());
                    record.values.entry(link_key).or_insert(value);
                }
                RefType::RowToEdge | RefType::EdgeToVector => {
                    record
                        .values
                        .insert(std::sync::Arc::from("_source_edge"), value.clone());
                    record.values.entry(link_key).or_insert(value);
                }
                _ => {
                    record
                        .values
                        .entry(std::sync::Arc::from("_source_entity"))
                        .or_insert(value.clone());
                    record.values.entry(link_key).or_insert(value);
                }
            }
        }
    }
}

pub(super) fn runtime_metadata_entry(metadata: &Metadata) -> MetadataEntry {
    let mut entry = MetadataEntry::new();
    for (key, value) in metadata.iter() {
        if let Some(converted) = runtime_vector_metadata_value(value) {
            entry.insert(key.clone(), converted);
        }
    }
    entry
}

pub(super) fn runtime_vector_metadata_value(
    value: &UnifiedMetadataValue,
) -> Option<VectorMetadataValue> {
    match value {
        UnifiedMetadataValue::Null => Some(VectorMetadataValue::Null),
        UnifiedMetadataValue::Bool(value) => Some(VectorMetadataValue::Bool(*value)),
        UnifiedMetadataValue::Int(value) => Some(VectorMetadataValue::Integer(*value)),
        UnifiedMetadataValue::Float(value) => Some(VectorMetadataValue::Float(*value)),
        UnifiedMetadataValue::String(value) => Some(VectorMetadataValue::String(value.clone())),
        UnifiedMetadataValue::Timestamp(value) => Some(VectorMetadataValue::Integer(*value as i64)),
        UnifiedMetadataValue::Reference(target) => Some(VectorMetadataValue::String(
            runtime_ref_target_string(target),
        )),
        UnifiedMetadataValue::References(targets) => Some(VectorMetadataValue::String(
            targets
                .iter()
                .map(runtime_ref_target_string)
                .collect::<Vec<_>>()
                .join(","),
        )),
        UnifiedMetadataValue::Array(values) => Some(VectorMetadataValue::String(
            values
                .iter()
                .filter_map(runtime_vector_metadata_value)
                .map(|value| match value {
                    VectorMetadataValue::String(value) => value,
                    VectorMetadataValue::Integer(value) => value.to_string(),
                    VectorMetadataValue::Float(value) => value.to_string(),
                    VectorMetadataValue::Bool(value) => value.to_string(),
                    VectorMetadataValue::Null => "null".to_string(),
                })
                .collect::<Vec<_>>()
                .join(","),
        )),
        UnifiedMetadataValue::Object(_)
        | UnifiedMetadataValue::Bytes(_)
        | UnifiedMetadataValue::Geo { .. } => None,
    }
}

pub(super) fn runtime_ref_target_string(target: &RefTarget) -> String {
    match target {
        RefTarget::TableRow { table, row_id } => format!("{table}:{row_id}"),
        RefTarget::Node {
            collection,
            node_id,
        } => format!("{collection}:{node_id}"),
        RefTarget::Edge {
            collection,
            edge_id,
        } => format!("{collection}:{edge_id}"),
        RefTarget::Vector {
            collection,
            vector_id,
        } => format!("{collection}:{vector_id}"),
        RefTarget::Entity {
            collection,
            entity_id,
        } => format!("{collection}:{entity_id}"),
    }
}

pub(super) fn runtime_entity_vector_similarity(entity: &UnifiedEntity, query: &[f32]) -> f32 {
    let mut best_similarity = 0.0f32;

    for emb in entity.embeddings() {
        best_similarity = best_similarity.max(cosine_similarity(query, &emb.vector));
    }

    if let EntityData::Vector(vec_data) = &entity.data {
        best_similarity = best_similarity.max(cosine_similarity(query, &vec_data.dense));
    }

    best_similarity
}

pub(super) fn runtime_structured_score(record: &UnifiedRecord, rank: Option<usize>) -> f64 {
    if let Some(value) = record
        .values
        .get("_score")
        .or_else(|| record.values.get("final_score"))
        .or_else(|| record.values.get("score"))
        .or_else(|| record.values.get("hybrid_score"))
        .or_else(|| record.values.get("graph_score"))
        .or_else(|| record.values.get("table_score"))
        .or_else(|| record.values.get("graph_match"))
        .or_else(|| record.values.get("vector_similarity"))
        .or_else(|| record.values.get("structured_match"))
        .or_else(|| record.values.get("text_relevance"))
    {
        if let Some(number) = runtime_value_number(value) {
            return number;
        }
    }

    rank.map(|value| 1.0 / (value as f64 + 1.0)).unwrap_or(0.0)
}

pub(super) fn runtime_vector_score(record: &UnifiedRecord) -> f64 {
    record
        .values
        .get("_score")
        .or_else(|| record.values.get("final_score"))
        .or_else(|| record.values.get("score"))
        .or_else(|| record.values.get("vector_similarity"))
        .or_else(|| record.values.get("graph_score"))
        .or_else(|| record.values.get("table_score"))
        .and_then(runtime_value_number)
        .unwrap_or(0.0)
}

pub(super) fn merge_hybrid_records(
    structured: Option<&UnifiedRecord>,
    vector: Option<&UnifiedRecord>,
) -> UnifiedRecord {
    let mut merged = structured.cloned().unwrap_or_default();

    if let Some(vector_record) = vector {
        for (key, value) in &vector_record.values {
            let key_str: &str = key;
            if let Some(existing) = merged.values.get(key_str) {
                if existing != value {
                    merged.values.insert(
                        std::sync::Arc::from(format!("vector.{key_str}")),
                        value.clone(),
                    );
                }
            } else {
                merged.values.insert(key.clone(), value.clone());
            }
        }

        for (alias, node) in &vector_record.nodes {
            merged
                .nodes
                .entry(alias.clone())
                .or_insert_with(|| node.clone());
        }
        for (alias, edge) in &vector_record.edges {
            merged
                .edges
                .entry(alias.clone())
                .or_insert_with(|| edge.clone());
        }
        merged.paths.extend(vector_record.paths.clone());
        merged
            .vector_results
            .extend(vector_record.vector_results.clone());
    }

    merged
}

pub(super) fn merge_join_records(
    left: Option<&UnifiedRecord>,
    right: Option<&UnifiedRecord>,
    left_query: &TableQuery,
    right_prefix: Option<&str>,
) -> UnifiedRecord {
    let left_table_name = left_query.table.as_str();
    let left_table_alias = left_query.alias.as_deref().unwrap_or(left_table_name);
    let mut merged = UnifiedRecord::new();

    if let Some(left_record) = left {
        merged = project_runtime_record(
            left_record,
            &effective_table_projections(left_query),
            Some(left_table_name),
            Some(left_table_alias),
            false,
            false,
        );
    }

    if let Some(right_record) = right {
        for (key, value) in &right_record.values {
            let key_str: &str = key;
            if merged.values.contains_key(key_str) {
                if let Some(prefix) = right_prefix {
                    merged.values.insert(
                        std::sync::Arc::from(format!("{prefix}.{key_str}")),
                        value.clone(),
                    );
                }
            } else {
                merged.values.insert(key.clone(), value.clone());
            }
        }

        for (alias, node) in &right_record.nodes {
            merged.nodes.insert(alias.clone(), node.clone());
        }
        for (alias, edge) in &right_record.edges {
            merged.edges.insert(alias.clone(), edge.clone());
        }
        merged.paths.extend(right_record.paths.clone());
        merged
            .vector_results
            .extend(right_record.vector_results.clone());
    }

    merged
}

pub(super) fn join_condition_matches(
    left_record: &UnifiedRecord,
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    left_field: &FieldRef,
    right_record: &UnifiedRecord,
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
    right_field: &FieldRef,
) -> bool {
    let left_value =
        resolve_runtime_field(left_record, left_field, left_table_name, left_table_alias);
    let right_value = resolve_runtime_field(
        right_record,
        right_field,
        right_table_name,
        right_table_alias,
    );

    match (left_value.as_ref(), right_value.as_ref()) {
        (Some(left), Some(right)) => compare_runtime_values(left, right, CompareOp::Eq),
        _ => false,
    }
}

pub(super) fn canonical_join_type(
    node: &crate::storage::query::planner::CanonicalLogicalNode,
) -> RedDBResult<JoinType> {
    match node.details.get("join_type").map(String::as_str) {
        Some("inner") => Ok(JoinType::Inner),
        Some("left_outer") => Ok(JoinType::LeftOuter),
        Some("right_outer") => Ok(JoinType::RightOuter),
        Some("full_outer") => Ok(JoinType::FullOuter),
        Some("cross") => Ok(JoinType::Cross),
        Some(other) => Err(RedDBError::Query(format!(
            "unsupported canonical join type {other}"
        ))),
        None => Err(RedDBError::Query(
            "canonical join operator is missing join_type".to_string(),
        )),
    }
}

pub(super) fn canonical_join_field(
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    key: &str,
) -> RedDBResult<FieldRef> {
    let value = node
        .details
        .get(key)
        .ok_or_else(|| RedDBError::Query(format!("canonical join operator is missing {key}")))?;
    parse_canonical_field_ref(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CanonicalJoinStrategy {
    IndexedNestedLoop,
    GraphLookupJoin,
    HashJoin,
    NestedLoop,
}

pub(super) fn canonical_join_strategy(
    node: &crate::storage::query::planner::CanonicalLogicalNode,
) -> RedDBResult<CanonicalJoinStrategy> {
    match node.details.get("join_strategy").map(String::as_str) {
        Some("indexed_nested_loop") => Ok(CanonicalJoinStrategy::IndexedNestedLoop),
        Some("graph_lookup_join") => Ok(CanonicalJoinStrategy::GraphLookupJoin),
        Some("hash_join") => Ok(CanonicalJoinStrategy::HashJoin),
        Some("nested_loop") => Ok(CanonicalJoinStrategy::NestedLoop),
        Some(other) => Err(RedDBError::Query(format!(
            "unsupported canonical join strategy {other}"
        ))),
        None => Err(RedDBError::Query(
            "canonical join operator is missing join_strategy".to_string(),
        )),
    }
}
