//! Canonical entity-to-query-record materialization. Callers apply visibility,
//! authorization, tenant and secret policy before/after these representation helpers.
use super::unified::*;
use crate::storage::entity::{EntityData, EntityKind, RefType, UnifiedEntity};
use reddb_types::Value;
use std::collections::BTreeSet;

mod document_body {
    pub use reddb_types::document_body_codec::decode_container_to_json;
    pub fn is_binary_container(bytes: &[u8]) -> bool {
        bytes.starts_with(reddb_types::document_body_codec::MAGIC)
    }
    pub fn read_body_field(bytes: &[u8], name: &str) -> Option<reddb_types::Value> {
        reddb_types::document_body_codec::read_field_by_name(bytes, name)
            .ok()
            .flatten()
    }
    pub fn body_fields(bytes: &[u8]) -> Option<Vec<(String, reddb_types::Value)>> {
        reddb_types::document_body_codec::decode(bytes).ok()
    }
}

/// Marker prepended to JSON-encoded tag values written through the
/// SQL `INSERT INTO <ts> (tags, ...)` path. The character is `\x01`
/// (Start-of-Heading) — invalid as a leading byte in well-formed
/// JSON, never used by Prometheus label values, and small enough to
/// keep storage overhead at one byte per tag. See
/// `impl_dml::json_tag_value_to_string` and `decode_stored_tag_value`
/// below for the read/write pair.
pub const TIMESERIES_TAG_JSON_PREFIX: char = '\u{1}';

/// Decode a stored tag value back into a JSON value (issue #543).
///
/// Values written through the SQL INSERT path carry a
/// [`TIMESERIES_TAG_JSON_PREFIX`] marker and a compact JSON suffix;
/// we strip the marker and parse the suffix so numbers, booleans,
/// nulls, arrays, and nested objects come back as their native JSON
/// kinds rather than stringified placeholders. Values without the
/// marker — Prometheus remote-write labels, internal `__tenant_id` /
/// `__namespace` / `__reddb_kind` tags, and any pre-#543 on-disk
/// rows — are passed through as `JsonValue::String(raw)` so the
/// Prometheus and metrics surfaces keep their plain-string contract.
pub fn decode_stored_tag_value(stored: &str) -> crate::json::Value {
    if let Some(suffix) = stored.strip_prefix(TIMESERIES_TAG_JSON_PREFIX) {
        crate::json::from_str(suffix)
            .unwrap_or_else(|_| crate::json::Value::String(suffix.to_string()))
    } else {
        crate::json::Value::String(stored.to_string())
    }
}

/// Per-thread cache of composite schemas `[<user columns…>, rid, collection,
/// kind, tenant, created_at, updated_at]`, keyed on the `Arc<Vec<String>>`
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
        let mut out: Vec<std::sync::Arc<str>> = Vec::with_capacity(6 + schema.len());
        for name in schema.iter() {
            out.push(std::sync::Arc::from(name.as_str()));
        }
        out.push(sys_key_rid());
        out.push(sys_key_collection());
        out.push(sys_key_kind());
        out.push(sys_key_tenant());
        out.push(sys_key_created_at());
        out.push(sys_key_updated_at());
        let arc = std::sync::Arc::new(out);
        *slot = Some((key, std::sync::Arc::clone(&arc)));
        arc
    })
}

fn runtime_row_tenant_value(row: &crate::storage::entity::RowData) -> Value {
    for key in ["tenant_id", "tenant"] {
        if let Some(value) = row.get_field(key) {
            return value.clone();
        }
    }
    Value::Null
}

fn set_public_row_envelope(
    record: &mut UnifiedRecord,
    entity: &UnifiedEntity,
    row: &crate::storage::entity::RowData,
) {
    record.set_arc(
        sys_key_rid(),
        Value::UnsignedInteger(entity.logical_id().raw()),
    );
    record.set_arc(
        sys_key_collection(),
        Value::text(entity.kind.collection().to_string()),
    );
    record.set_arc(
        sys_key_kind(),
        Value::text(public_row_kind(row).to_string()),
    );
    record.set_arc(sys_key_tenant(), runtime_row_tenant_value(row));
    // Managed `created_at` / `updated_at` row columns (WITH timestamps =
    // true) take precedence over the second-precision envelope values.
    record.set_arc_if_absent(
        sys_key_created_at(),
        Value::UnsignedInteger(entity.created_at),
    );
    record.set_arc_if_absent(
        sys_key_updated_at(),
        Value::UnsignedInteger(entity.updated_at),
    );
}

fn set_public_graph_envelope(record: &mut UnifiedRecord, entity: &UnifiedEntity, kind: &str) {
    record.set_arc(
        sys_key_rid(),
        Value::UnsignedInteger(entity.logical_id().raw()),
    );
    record.set_arc(
        sys_key_collection(),
        Value::text(entity.kind.collection().to_string()),
    );
    record.set_arc(sys_key_kind(), Value::text(kind.to_string()));
    record.set_arc(sys_key_tenant(), Value::Null);
    record.set_arc(
        sys_key_created_at(),
        Value::UnsignedInteger(entity.created_at),
    );
    record.set_arc(
        sys_key_updated_at(),
        Value::UnsignedInteger(entity.updated_at),
    );
}

fn graph_endpoint_rid_value(endpoint: &str) -> Value {
    endpoint
        .parse::<u64>()
        .map(Value::UnsignedInteger)
        .unwrap_or_else(|_| Value::text(endpoint.to_string()))
}

fn public_row_kind(row: &crate::storage::entity::RowData) -> &'static str {
    if runtime_row_is_kv(row) {
        "kv"
    } else if runtime_row_has_document_capability(row) {
        "document"
    } else {
        "row"
    }
}

/// Lean materialization for the index scan hot path.
///
/// Emits `rid`, `created_at`, `updated_at`, plus the raw user data
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
pub fn runtime_table_record_lean_ref(entity: &UnifiedEntity) -> Option<UnifiedRecord> {
    let created_at = entity.created_at;
    let updated_at = entity.updated_at;
    let row = match &entity.data {
        EntityData::Row(row) => row,
        // Issue #414: graph/vector/queue/etc. entities must surface in
        // SELECT scans. Lean path can't construct a "pure row" for them,
        // so delegate to the universal materializer.
        _ => return runtime_any_record_from_entity_ref(entity),
    };
    if let Some(named) = &row.named {
        let mut record = UnifiedRecord::with_capacity(6 + named.len());
        for (key, value) in named.iter() {
            record.set(key, value.clone());
        }
        set_public_row_envelope(&mut record, entity, row);
        fill_document_body_fields(&mut record, None, document_row_body_bytes(row).as_deref());
        Some(record)
    } else if let Some(schema) = &row.schema {
        // Columnar fast-path: build the record with the schema-shared
        // Vec<Arc<str>> side-channel so 4.5k rows × N fields deallocate
        // as one contiguous Vec each instead of a HashMap per record.
        let sys_schema = sys_schema_with_row_columns(schema);
        let mut values: Vec<Value> = Vec::with_capacity(sys_schema.len());
        values.extend(row.columns.iter().cloned());
        values.push(Value::UnsignedInteger(entity.logical_id().raw()));
        values.push(Value::text(entity.kind.collection().to_string()));
        values.push(Value::text(public_row_kind(row).to_string()));
        values.push(runtime_row_tenant_value(row));
        values.push(Value::UnsignedInteger(created_at));
        values.push(Value::UnsignedInteger(updated_at));
        let mut record = UnifiedRecord::from_columnar(sys_schema, values);
        fill_document_body_fields(&mut record, None, document_row_body_bytes(row).as_deref());
        Some(record)
    } else {
        let mut record = UnifiedRecord::with_capacity(6 + row.columns.len());
        for (i, value) in row.columns.iter().enumerate() {
            record.set(&format!("c{i}"), value.clone());
        }
        set_public_row_envelope(&mut record, entity, row);
        Some(record)
    }
}

#[inline]
pub fn runtime_table_record_lean(entity: UnifiedEntity) -> Option<UnifiedRecord> {
    // Issue #414: surface graph/vector/queue/etc. entities in SELECT scans.
    if !matches!(entity.data, EntityData::Row(_)) {
        return runtime_any_record_from_entity(entity);
    }
    let created_at = entity.created_at;
    let updated_at = entity.updated_at;
    let logical_id = entity.logical_id().raw();
    let row = match entity.data {
        EntityData::Row(row) => row,
        _ => unreachable!(),
    };
    let collection = entity.kind.collection().to_string();
    let tenant = runtime_row_tenant_value(&row);
    let kind = public_row_kind(&row).to_string();
    // Capture the document body before `named` is consumed below.
    let doc_body = document_row_body_bytes(&row);
    if let Some(named) = row.named {
        let mut record = UnifiedRecord::with_capacity(6 + named.len());
        // `set_owned` consumes the already-heap-allocated String key
        // instead of `&str → String` clone. For SELECT * on a wide
        // result set this saves ~15k allocations per query.
        for (key, value) in named {
            record.set_owned(key, value);
        }
        record.set_arc(sys_key_rid(), Value::UnsignedInteger(logical_id));
        record.set_arc(sys_key_collection(), Value::text(collection));
        record.set_arc(sys_key_kind(), Value::text(kind));
        record.set_arc(sys_key_tenant(), tenant);
        record.set_arc_if_absent(sys_key_created_at(), Value::UnsignedInteger(created_at));
        record.set_arc_if_absent(sys_key_updated_at(), Value::UnsignedInteger(updated_at));
        // Lean SELECT * over a single-source document — expand body fields.
        fill_document_body_fields(&mut record, None, doc_body.as_deref());
        Some(record)
    } else if let Some(ref schema) = row.schema {
        let mut record = UnifiedRecord::with_capacity(6 + schema.len());
        for (name, value) in schema.iter().zip(row.columns) {
            record.set(name, value);
        }
        record.set_arc(sys_key_rid(), Value::UnsignedInteger(logical_id));
        record.set_arc(sys_key_collection(), Value::text(collection));
        record.set_arc(sys_key_kind(), Value::text(kind));
        record.set_arc(sys_key_tenant(), tenant);
        record.set_arc_if_absent(sys_key_created_at(), Value::UnsignedInteger(created_at));
        record.set_arc_if_absent(sys_key_updated_at(), Value::UnsignedInteger(updated_at));
        fill_document_body_fields(&mut record, None, doc_body.as_deref());
        Some(record)
    } else {
        let mut record = UnifiedRecord::with_capacity(6 + row.columns.len());
        for (i, value) in row.columns.into_iter().enumerate() {
            record.set(&format!("c{i}"), value);
        }
        record.set_arc(sys_key_rid(), Value::UnsignedInteger(logical_id));
        record.set_arc(sys_key_collection(), Value::text(collection));
        record.set_arc(sys_key_kind(), Value::text(kind));
        record.set_arc(sys_key_tenant(), tenant);
        record.set_arc_if_absent(sys_key_created_at(), Value::UnsignedInteger(created_at));
        record.set_arc_if_absent(sys_key_updated_at(), Value::UnsignedInteger(updated_at));
        Some(record)
    }
}

pub fn runtime_table_record_lean_in_collection(
    entity: UnifiedEntity,
    collection: &str,
) -> Option<UnifiedRecord> {
    let is_graph = matches!(entity.data, EntityData::Node(_) | EntityData::Edge(_));
    let mut record = runtime_table_record_lean(entity)?;
    if is_graph {
        record.set_arc(sys_key_collection(), Value::text(collection.to_string()));
    }
    Some(record)
}

#[inline(never)]
/// Clone a row's stored binary document `body` bytes, if any.
///
/// Sourced from the row rather than the record because a projection that does
/// not SELECT `body` would never place it on the record — yet the promoted
/// fields still have to be read out of it.
fn document_row_body_bytes(row: &crate::storage::entity::RowData) -> Option<Vec<u8>> {
    match row.get_field("body") {
        Some(Value::Json(bytes)) if document_body::is_binary_container(bytes) => {
            Some(bytes.clone())
        }
        _ => None,
    }
}

/// Fill a single-source document's top-level fields into `record` by reading
/// them from the binary `body` (`body` = the row's container bytes).
///
/// `columns = None` expands every top-level body field (SELECT *); `Some(cols)`
/// offset-reads only the requested fields. A no-op when `body` is `None`.
pub fn fill_document_body_fields(
    record: &mut UnifiedRecord,
    columns: Option<&[String]>,
    body: Option<&[u8]>,
) {
    let Some(bytes) = body else {
        return;
    };
    if let Some(body_json) = document_body::decode_container_to_json(bytes) {
        if let Ok(json_bytes) = crate::json::to_vec(&body_json) {
            record.set("body", Value::Json(json_bytes));
        }
    }
    let fields: Vec<(String, Value)> = match columns {
        None => match document_body::body_fields(bytes) {
            Some(fields) => fields,
            None => return,
        },
        Some(cols) => cols
            .iter()
            .filter_map(|col| {
                document_body::read_body_field(bytes, col).map(|value| (col.clone(), value))
            })
            .collect(),
    };
    for (name, value) in fields {
        record.set(&name, value);
    }
}

pub fn runtime_table_record_from_entity(entity: UnifiedEntity) -> Option<UnifiedRecord> {
    // Issue #414: surface graph/vector/queue/etc. entities in SELECT scans.
    if !matches!(entity.data, EntityData::Row(_) | EntityData::TimeSeries(_)) {
        return runtime_any_record_from_entity(entity);
    }
    let logical_id = entity.logical_id().raw();
    match entity.data {
        EntityData::Row(row) => {
            // Capture the document body before `named` is consumed below.
            let doc_body = document_row_body_bytes(&row);
            // Pre-allocate: ~9 system fields + user fields
            let user_field_count = row
                .named
                .as_ref()
                .map(|n| n.len())
                .unwrap_or(row.columns.len());
            let mut record = UnifiedRecord::with_capacity(6 + user_field_count);
            let collection = entity.kind.collection().to_string();
            let tenant = runtime_row_tenant_value(&row);
            let kind = public_row_kind(&row).to_string();

            if let Some(named) = row.named {
                for (key, value) in named {
                    record.set(&key, value);
                }
            } else if let Some(ref schema) = row.schema {
                // Columnar storage: use shared schema for field names
                for (name, value) in schema.iter().zip(row.columns) {
                    record.set(name, value);
                }
            } else {
                for (index, value) in row.columns.into_iter().enumerate() {
                    record.set(&format!("c{index}"), value);
                }
            }
            record.set_arc(sys_key_rid(), Value::UnsignedInteger(logical_id));
            record.set_arc(sys_key_collection(), Value::text(collection));
            record.set_arc(sys_key_kind(), Value::text(kind));
            record.set_arc(sys_key_tenant(), tenant);
            record.set_arc_if_absent(
                sys_key_created_at(),
                Value::UnsignedInteger(entity.created_at),
            );
            record.set_arc_if_absent(
                sys_key_updated_at(),
                Value::UnsignedInteger(entity.updated_at),
            );

            // SELECT * over a single-source document: expand fields from the body.
            fill_document_body_fields(&mut record, None, doc_body.as_deref());

            Some(record)
        }
        EntityData::TimeSeries(ts) => {
            let mut record = UnifiedRecord::with_capacity(12 + ts.tags.len());
            record.set_arc(sys_key_rid(), Value::UnsignedInteger(logical_id));
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
pub fn runtime_table_record_from_entity_ref(entity: &UnifiedEntity) -> Option<UnifiedRecord> {
    runtime_table_record_from_entity_ref_with_schema(entity, None)
}

pub fn runtime_table_record_from_entity_ref_with_schema(
    entity: &UnifiedEntity,
    fallback_schema: Option<&std::sync::Arc<Vec<String>>>,
) -> Option<UnifiedRecord> {
    // Issue #414: surface graph/vector/queue/etc. entities in SELECT scans.
    if !matches!(&entity.data, EntityData::Row(_) | EntityData::TimeSeries(_)) {
        return runtime_any_record_from_entity_ref(entity);
    }
    match &entity.data {
        EntityData::Row(row) => {
            let user_field_count = row
                .named
                .as_ref()
                .map(|n| n.len())
                .unwrap_or(row.columns.len());
            let mut record = UnifiedRecord::with_capacity(6 + user_field_count);

            if let Some(named) = &row.named {
                for (key, value) in named {
                    record.set(key, value.clone());
                }
            } else if let Some(schema) = row.schema.as_ref().or(fallback_schema) {
                for (name, value) in schema.iter().zip(row.columns.iter()) {
                    record.set(name, value.clone());
                }
            } else {
                for (index, value) in row.columns.iter().enumerate() {
                    record.set(&format!("c{index}"), value.clone());
                }
            }
            set_public_row_envelope(&mut record, entity, row);

            // SELECT * over a single-source document — expand body fields.
            fill_document_body_fields(&mut record, None, document_row_body_bytes(row).as_deref());

            Some(record)
        }
        EntityData::TimeSeries(ts) => {
            let mut record = UnifiedRecord::with_capacity(12 + ts.tags.len());
            record.set_arc(
                sys_key_rid(),
                Value::UnsignedInteger(entity.logical_id().raw()),
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
pub fn runtime_table_record_from_entity_projected(
    entity: UnifiedEntity,
    columns: &[String],
) -> Option<UnifiedRecord> {
    if columns.is_empty() {
        return runtime_table_record_from_entity(entity);
    }
    // Issue #414: graph/vector/queue/etc. — produce a full record so SELECT
    // can project named graph properties. Extra fields are harmless; the
    // outer projection layer keeps only the requested ones.
    if !matches!(entity.data, EntityData::Row(_) | EntityData::TimeSeries(_)) {
        return runtime_any_record_from_entity(entity);
    }
    let logical_id = entity.logical_id().raw();

    match entity.data {
        EntityData::Row(row) => {
            // Capture the document body before `named` is consumed below.
            let doc_body = document_row_body_bytes(&row);
            let mut record = UnifiedRecord::with_capacity(6 + columns.len());
            let collection = entity.kind.collection().to_string();
            let tenant = runtime_row_tenant_value(&row);
            let kind = public_row_kind(&row).to_string();

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
            record.set_arc(sys_key_rid(), Value::UnsignedInteger(logical_id));
            record.set_arc(sys_key_collection(), Value::text(collection));
            record.set_arc(sys_key_kind(), Value::text(kind));
            record.set_arc(sys_key_tenant(), tenant);
            record.set_arc_if_absent(
                sys_key_created_at(),
                Value::UnsignedInteger(entity.created_at),
            );
            record.set_arc_if_absent(
                sys_key_updated_at(),
                Value::UnsignedInteger(entity.updated_at),
            );

            // Single-source document: offset-read just the projected fields
            // from the body.
            fill_document_body_fields(&mut record, Some(columns), doc_body.as_deref());

            Some(record)
        }
        EntityData::TimeSeries(ts) => {
            let mut record = UnifiedRecord::new();
            record.set_arc(sys_key_rid(), Value::UnsignedInteger(logical_id));

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
                    _ => {
                        if let Some(value) = ts.fields.get(col) {
                            record.set(col, value.clone());
                        }
                    }
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
pub fn runtime_table_record_with_col_indices(
    entity: &UnifiedEntity,
    columns: &[String],
    idx_map: &[(usize, usize)],
) -> Option<UnifiedRecord> {
    if idx_map.is_empty() {
        return None;
    }
    let row = entity.data.as_row()?;
    // Applies to columnar entities. After a persistent reopen, rows written in
    // compact positional form may not carry row.schema; callers can still pass
    // an idx_map derived from the persisted collection contract.
    if row.named.is_some() || row.columns.is_empty() {
        return None;
    }
    if row.schema.as_ref().is_some_and(|schema| {
        idx_map
            .iter()
            .any(|&(ci, si)| schema.get(si) != columns.get(ci))
    }) {
        return None;
    }
    let mut record = UnifiedRecord::with_capacity(6 + idx_map.len());
    for &(ci, si) in idx_map {
        if let Some(value) = row.columns.get(si) {
            // Use set_owned to skip the hidden to_string() clone.
            record.set_owned(columns[ci].clone(), value.clone());
        }
    }
    set_public_row_envelope(&mut record, entity, row);
    Some(record)
}

/// Used by the fast-path scan when `select_cols` is non-empty.
///
/// Returns `None` for non-Row entities (caller falls back to owned path).
pub fn runtime_table_record_from_entity_ref_projected(
    entity: &UnifiedEntity,
    columns: &[String],
) -> Option<UnifiedRecord> {
    if columns.is_empty() {
        return None; // caller should use full materialization for SELECT *
    }
    // Issue #414: non-Row entities (graph/vector/queue) must be surfaced in
    // SELECT scans. Fall back to the universal materializer; the outer
    // projection layer keeps only the requested columns.
    if !matches!(&entity.data, EntityData::Row(_)) {
        return runtime_any_record_from_entity_ref(entity);
    }
    let row = entity.data.as_row()?;

    let mut record = UnifiedRecord::with_capacity(6 + columns.len());

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
    set_public_row_envelope(&mut record, entity, row);
    // Single-source document: offset-read the projected body fields.
    fill_document_body_fields(
        &mut record,
        Some(columns),
        document_row_body_bytes(row).as_deref(),
    );
    Some(record)
}

#[inline(never)]
pub fn runtime_any_record_from_entity(entity: UnifiedEntity) -> Option<UnifiedRecord> {
    let identity_entity = entity.clone();
    let kind = entity.kind.clone();
    let collection = kind.collection().to_string();
    let storage_type = kind.storage_type().to_string();
    let entity_id = entity.logical_id().raw();
    let created_at = entity.created_at;
    let updated_at = entity.updated_at;
    let sequence_id = entity.sequence_id;

    let (entity_type, capabilities, mut record) = match (kind, entity.data) {
        (EntityKind::TableRow { row_id, .. }, EntityData::Row(row)) => {
            let capabilities = runtime_row_capabilities(&row);
            let entity_type = runtime_row_entity_type(&row);
            let doc_body = document_row_body_bytes(&row);
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
            fill_document_body_fields(&mut record, None, doc_body.as_deref());
            (entity_type, capabilities, record)
        }
        (EntityKind::GraphNode(node), EntityData::Node(node_data)) => {
            let mut record = UnifiedRecord::new();
            record.set("label", Value::text(node.label));
            record.set("node_type", Value::text(node.node_type));
            for (key, value) in node_data.properties {
                record.set(&key, value);
            }
            set_public_graph_envelope(&mut record, &identity_entity, "node");
            (
                "graph_node",
                runtime_record_capability_list(["graph", "graph_node"]),
                record,
            )
        }
        (EntityKind::GraphEdge(edge_kind), EntityData::Edge(edge)) => {
            let mut record = UnifiedRecord::new();
            record.set("label", Value::text(edge_kind.label));
            record.set("from_rid", graph_endpoint_rid_value(&edge_kind.from_node));
            record.set("to_rid", graph_endpoint_rid_value(&edge_kind.to_node));
            record.set("weight", Value::Float(edge.weight as f64));
            for (key, value) in edge.properties {
                record.set(&key, value);
            }
            set_public_graph_envelope(&mut record, &identity_entity, "edge");
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

    if let EntityData::Row(row) = &identity_entity.data {
        set_public_row_envelope(&mut record, &identity_entity, row);
    } else if matches!(
        identity_entity.data,
        EntityData::Node(_) | EntityData::Edge(_)
    ) {
        set_runtime_entity_metadata(&mut record, entity_type, capabilities);
        apply_runtime_identity_hints(&mut record, &identity_entity);
    } else {
        // #1369 — every entity model exposes its entity-id as `rid`
        // (vector / time-series / queue rows). `rid` is mandatory for
        // every model, never optional.
        record.set_arc(sys_key_rid(), Value::UnsignedInteger(entity_id));
        record.set_arc(sys_key_red_collection(), Value::text(collection));
        record.set_arc(sys_key_red_kind(), Value::text(storage_type));
        record.set_arc_if_absent(sys_key_created_at(), Value::UnsignedInteger(created_at));
        record.set_arc_if_absent(sys_key_updated_at(), Value::UnsignedInteger(updated_at));
        record.set_arc(
            sys_key_red_sequence_id(),
            Value::UnsignedInteger(sequence_id),
        );
        set_runtime_entity_metadata(&mut record, entity_type, capabilities);
        apply_runtime_identity_hints(&mut record, &identity_entity);
    }

    Some(record)
}

pub fn runtime_any_record_from_entity_ref(entity: &UnifiedEntity) -> Option<UnifiedRecord> {
    let kind = &entity.kind;
    let collection = kind.collection().to_string();
    let storage_type = kind.storage_type().to_string();
    let entity_id = entity.logical_id().raw();
    let created_at = entity.created_at;
    let updated_at = entity.updated_at;
    let sequence_id = entity.sequence_id;

    let (entity_type, capabilities, mut record) = match (kind, &entity.data) {
        (EntityKind::TableRow { row_id, .. }, EntityData::Row(row)) => {
            let capabilities = runtime_row_capabilities(row);
            let entity_type = runtime_row_entity_type(row);
            let doc_body = document_row_body_bytes(row);
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
            fill_document_body_fields(&mut record, None, doc_body.as_deref());
            (entity_type, capabilities, record)
        }
        (EntityKind::GraphNode(node), EntityData::Node(node_data)) => {
            let mut record = UnifiedRecord::new();
            record.set("label", Value::text(node.label.clone()));
            record.set("node_type", Value::text(node.node_type.clone()));
            for (key, value) in &node_data.properties {
                record.set(key, value.clone());
            }
            set_public_graph_envelope(&mut record, entity, "node");
            (
                "graph_node",
                runtime_record_capability_list(["graph", "graph_node"]),
                record,
            )
        }
        (EntityKind::GraphEdge(edge_kind), EntityData::Edge(edge)) => {
            let mut record = UnifiedRecord::new();
            record.set("label", Value::text(edge_kind.label.clone()));
            record.set("from_rid", graph_endpoint_rid_value(&edge_kind.from_node));
            record.set("to_rid", graph_endpoint_rid_value(&edge_kind.to_node));
            record.set("weight", Value::Float(edge.weight as f64));
            for (key, value) in &edge.properties {
                record.set(key, value.clone());
            }
            // #1369 — graph edges surface their endpoints as `from_rid`/`to_rid`
            // (the canonical rid references), NOT raw `from`/`to`, so the public
            // envelope stays rid-based and never leaks the internal endpoint text
            // the INSERT stored in properties.
            set_public_graph_envelope(&mut record, entity, "edge");
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
        (EntityKind::QueueMessage { position, .. }, EntityData::QueueMessage(msg)) => {
            let mut record = UnifiedRecord::new();
            record.set("position", Value::UnsignedInteger(*position));
            record.set("payload", msg.payload.clone());
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

    if let EntityData::Row(row) = &entity.data {
        set_public_row_envelope(&mut record, entity, row);
    } else if matches!(entity.data, EntityData::Node(_) | EntityData::Edge(_)) {
        set_runtime_entity_metadata(&mut record, entity_type, capabilities);
        apply_runtime_identity_hints(&mut record, entity);
    } else {
        // #1369 — every entity model exposes its entity-id as `rid`
        // (vector / time-series / queue rows). `rid` is mandatory for
        // every model, never optional.
        record.set_arc(sys_key_rid(), Value::UnsignedInteger(entity_id));
        record.set_arc(sys_key_red_collection(), Value::text(collection));
        record.set_arc(sys_key_red_kind(), Value::text(storage_type));
        record.set_arc_if_absent(sys_key_created_at(), Value::UnsignedInteger(created_at));
        record.set_arc_if_absent(sys_key_updated_at(), Value::UnsignedInteger(updated_at));
        record.set_arc(
            sys_key_red_sequence_id(),
            Value::UnsignedInteger(sequence_id),
        );
        set_runtime_entity_metadata(&mut record, entity_type, capabilities);
        apply_runtime_identity_hints(&mut record, entity);
    }

    Some(record)
}

fn append_timeseries_record_fields(
    record: &mut UnifiedRecord,
    ts: &crate::storage::entity::TimeSeriesData,
) {
    record.set("metric", Value::text(ts.metric.clone()));
    record.set("timestamp_ns", Value::UnsignedInteger(ts.timestamp_ns));
    record.set("timestamp", Value::UnsignedInteger(ts.timestamp_ns));
    record.set("time", Value::UnsignedInteger(ts.timestamp_ns));
    record.set("value", Value::Float(ts.value));
    if !ts.tags.is_empty() {
        record.set("tags", timeseries_tags_value(&ts.tags));
    }
    for (key, value) in &ts.fields {
        record.set(key, value.clone());
    }
}

fn timeseries_tags_value(tags: &std::collections::HashMap<String, String>) -> Value {
    let object = tags
        .iter()
        .map(|(key, value)| (key.clone(), decode_stored_tag_value(value)))
        .collect();
    let json = crate::json::Value::Object(object);
    Value::Json(crate::json::to_vec(&json).unwrap_or_default())
}

#[inline(never)]
pub fn set_runtime_entity_metadata(
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

pub fn runtime_record_capability_list<const N: usize>(values: [&str; N]) -> BTreeSet<String> {
    values.into_iter().map(|value| value.to_string()).collect()
}

pub fn runtime_row_capabilities(row: &crate::storage::entity::RowData) -> BTreeSet<String> {
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
pub fn runtime_row_capabilities_str(row: &crate::storage::entity::RowData) -> &'static str {
    let is_kv = runtime_row_is_kv(row);
    let is_doc = runtime_row_has_document_capability(row);
    match (is_kv, is_doc) {
        (false, false) => "structured,table",
        (true, false) => "kv,structured,table",
        (false, true) => "document,structured,table",
        (true, true) => "document,kv,structured,table",
    }
}

pub fn runtime_row_entity_type(row: &crate::storage::entity::RowData) -> &'static str {
    if runtime_row_is_kv(row) {
        return "kv";
    }

    if runtime_row_has_document_capability(row) {
        "document"
    } else {
        "table"
    }
}

fn runtime_row_is_kv(row: &crate::storage::entity::RowData) -> bool {
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

pub fn runtime_row_has_document_capability(row: &crate::storage::entity::RowData) -> bool {
    row.named
        .as_ref()
        .map(|named| named.values().any(runtime_documentish_value))
        .unwrap_or(false)
        || row.columns.iter().any(runtime_documentish_value)
}

pub fn runtime_documentish_value(value: &Value) -> bool {
    matches!(value, Value::Json(_) | Value::Blob(_))
}

pub fn runtime_entity_type_and_capabilities(
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
        (EntityKind::QueueMessage { .. }, EntityData::QueueMessage(_)) => (
            "queue_message",
            runtime_record_capability_list(["document", "queue", "message"]),
        ),
        _ => ("unknown", BTreeSet::new()),
    }
}

pub fn apply_runtime_identity_hints(record: &mut UnifiedRecord, entity: &UnifiedEntity) {
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
                    record.set_arc(std::sync::Arc::from("_source_row"), value.clone());
                    record.overflow_entry_or_insert(link_key, value);
                }
                RefType::VectorToNode | RefType::RowToNode => {
                    record.set_arc(std::sync::Arc::from("_source_node"), value.clone());
                    record.overflow_entry_or_insert(link_key, value);
                }
                RefType::RowToEdge | RefType::EdgeToVector => {
                    record.set_arc(std::sync::Arc::from("_source_edge"), value.clone());
                    record.overflow_entry_or_insert(link_key, value);
                }
                _ => {
                    record.overflow_entry_or_insert(
                        std::sync::Arc::from("_source_entity"),
                        value.clone(),
                    );
                    record.overflow_entry_or_insert(link_key, value);
                }
            }
        }
    }
}
