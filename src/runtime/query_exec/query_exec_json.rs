use super::*;

/// Execute a filtered scan and serialize matching entities directly to JSON.
/// Returns None if the collection doesn't exist (falls through to normal path).
/// Write a u64 as decimal digits.
#[inline(always)]
fn write_u64(buf: &mut Vec<u8>, n: u64) {
    let mut b = itoa::Buffer::new();
    let s = b.format(n);
    buf.extend_from_slice(s.as_bytes());
}

/// Emit `"created_at":X,"updated_at":Y` for an entity, preferring the
/// declared row columns (e.g. from `CREATE TABLE ... WITH timestamps =`
/// true) over the entity's internal `created_at`/`updated_at` fields.
///
/// The caller is responsible for the leading `,` before the block.
#[inline(always)]
fn write_timestamp_fields_json(buf: &mut Vec<u8>, entity: &UnifiedEntity) {
    let (row_created_at, row_updated_at) = match &entity.data {
        EntityData::Row(row) => {
            let mut created = None;
            let mut updated = None;
            for (key, value) in row.iter_fields() {
                match key {
                    "created_at" => created = Some(value.clone()),
                    "updated_at" => updated = Some(value.clone()),
                    _ => {}
                }
                if created.is_some() && updated.is_some() {
                    break;
                }
            }
            (created, updated)
        }
        _ => (None, None),
    };
    buf.extend_from_slice(b"\"created_at\":");
    if let Some(v) = &row_created_at {
        write_value_bytes(buf, v);
    } else {
        write_u64(buf, entity.created_at);
    }
    buf.extend_from_slice(b",\"updated_at\":");
    if let Some(v) = &row_updated_at {
        write_value_bytes(buf, v);
    } else {
        write_u64(buf, entity.updated_at);
    }
}

/// Write an entity's fields as JSON bytes into a Vec<u8> buffer.
///
/// `created_at` and `updated_at` are de-duplicated: if the row carries
/// them as declared user columns (e.g. from `CREATE TABLE ... WITH
/// timestamps = true`), we emit the row's value and skip the iter
/// pass for those keys. Otherwise we fall back to the entity's
/// internal `created_at`/`updated_at` stamps.
#[inline(always)]
fn write_entity_json_bytes(buf: &mut Vec<u8>, entity: &UnifiedEntity) {
    buf.push(b'{');
    buf.extend_from_slice(b"\"red_entity_id\":");
    write_u64(buf, entity.id.raw());
    buf.extend_from_slice(b",\"red_collection\":");
    write_json_bytes(buf, entity.kind.collection().as_bytes());
    buf.extend_from_slice(b",\"red_kind\":");
    write_json_bytes(buf, entity.kind.storage_type().as_bytes());
    buf.push(b',');
    write_timestamp_fields_json(buf, entity);
    buf.extend_from_slice(b",\"red_sequence_id\":");
    write_u64(buf, entity.sequence_id);
    let (entity_type, capabilities) = match &entity.data {
        EntityData::Row(_) => ("table", "structured,table"),
        EntityData::TimeSeries(_) => ("timeseries", "timeseries,metric,temporal"),
        _ => ("unknown", "unknown"),
    };
    buf.extend_from_slice(b",\"red_entity_type\":");
    write_json_bytes(buf, entity_type.as_bytes());
    buf.extend_from_slice(b",\"red_capabilities\":");
    write_json_bytes(buf, capabilities.as_bytes());

    if let EntityKind::TableRow { row_id, .. } = &entity.kind {
        buf.extend_from_slice(b",\"row_id\":");
        write_u64(buf, *row_id);
    }

    match &entity.data {
        EntityData::Row(row) => {
            for (key, value) in row.iter_fields() {
                // Skip the two auto-timestamp columns — already emitted
                // once at the top of the JSON from either the row value
                // itself or `entity.created_at/updated_at`.
                if key == "created_at" || key == "updated_at" {
                    continue;
                }
                buf.push(b',');
                write_json_bytes(buf, key.as_bytes());
                buf.push(b':');
                write_value_bytes(buf, value);
            }
            if false {
                // Dead code kept for compiler — positional fallback no longer needed
                for (i, value) in row.columns.iter().enumerate() {
                    buf.push(b',');
                    buf.push(b'"');
                    buf.push(b'c');
                    itoa::Buffer::new()
                        .format(i)
                        .as_bytes()
                        .iter()
                        .for_each(|b| buf.push(*b));
                    buf.extend_from_slice(b"\":");
                    write_value_bytes(buf, value);
                }
            }
        }
        EntityData::TimeSeries(ts) => {
            buf.extend_from_slice(b",\"metric\":");
            write_json_bytes(buf, ts.metric.as_bytes());
            buf.extend_from_slice(b",\"timestamp_ns\":");
            write_u64(buf, ts.timestamp_ns);
            buf.extend_from_slice(b",\"timestamp\":");
            write_u64(buf, ts.timestamp_ns);
            buf.extend_from_slice(b",\"time\":");
            write_u64(buf, ts.timestamp_ns);
            buf.extend_from_slice(b",\"value\":");
            write_value_bytes(buf, &Value::Float(ts.value));
            if !ts.tags.is_empty() {
                buf.extend_from_slice(b",\"tags\":");
                write_value_bytes(buf, &timeseries_tags_json_value(&ts.tags));
            }
        }
        _ => {}
    }
    buf.push(b'}');
}

/// Write a JSON-quoted string to bytes. Assumes most strings are ASCII-safe.
#[inline(always)]
fn write_json_bytes(buf: &mut Vec<u8>, s: &[u8]) {
    buf.push(b'"');
    for &b in s {
        match b {
            b'"' => buf.extend_from_slice(b"\\\""),
            b'\\' => buf.extend_from_slice(b"\\\\"),
            b'\n' => buf.extend_from_slice(b"\\n"),
            b'\r' => buf.extend_from_slice(b"\\r"),
            b'\t' => buf.extend_from_slice(b"\\t"),
            b if b < 0x20 => {
                // \u00XX escape for control characters
                buf.extend_from_slice(b"\\u00");
                let hi = b >> 4;
                let lo = b & 0xf;
                buf.push(if hi < 10 { b'0' + hi } else { b'a' + hi - 10 });
                buf.push(if lo < 10 { b'0' + lo } else { b'a' + lo - 10 });
            }
            _ => buf.push(b),
        }
    }
    buf.push(b'"');
}

/// Write a Value as JSON bytes.
#[inline(always)]
fn write_value_bytes(buf: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Null => buf.extend_from_slice(b"null"),
        Value::Boolean(true) => buf.extend_from_slice(b"true"),
        Value::Boolean(false) => buf.extend_from_slice(b"false"),
        Value::Integer(n) => {
            let mut b = itoa::Buffer::new();
            let s = b.format(*n);
            buf.extend_from_slice(s.as_bytes());
        }
        Value::UnsignedInteger(n) => {
            let mut b = itoa::Buffer::new();
            let s = b.format(*n);
            buf.extend_from_slice(s.as_bytes());
        }
        Value::Float(f) => {
            if f.is_finite() {
                let mut b = ryu::Buffer::new();
                let s = b.format(*f);
                buf.extend_from_slice(s.as_bytes());
            } else {
                buf.extend_from_slice(b"null");
            }
        }
        Value::Text(s) => write_json_bytes(buf, s.as_bytes()),
        Value::Array(values) => {
            buf.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    buf.push(b',');
                }
                write_value_bytes(buf, value);
            }
            buf.push(b']');
        }
        Value::Json(value) => {
            if std::str::from_utf8(value).is_ok() {
                buf.extend_from_slice(value);
            } else {
                buf.extend_from_slice(b"null");
            }
        }
        Value::Timestamp(t) => {
            let mut b = itoa::Buffer::new();
            let s = b.format(*t);
            buf.extend_from_slice(s.as_bytes());
        }
        _ => buf.extend_from_slice(b"null"),
    }
}

/// Serialize a single entity to the full result JSON wrapper.
pub(super) fn execute_runtime_serialize_single_entity(entity: &UnifiedEntity) -> String {
    let mut buf = Vec::with_capacity(512);
    buf.extend_from_slice(
        b"{\"columns\":[],\"record_count\":1,\"selection\":{\"scope\":\"any\"},\"records\":[",
    );
    write_entity_json_bytes(&mut buf, entity);
    buf.extend_from_slice(b"]}");
    // SAFETY: we only wrote valid UTF-8 bytes
    unsafe { String::from_utf8_unchecked(buf) }
}

/// Index-assisted filtered scan: use hash index for equality column, then evaluate
/// remaining predicates only on matching entities. Turns O(N) scan into O(K) lookup.
pub(super) fn execute_indexed_scan_to_json(
    db: &RedDB,
    query: &TableQuery,
    idx_store: &super::index_store::IndexStore,
) -> RedDBResult<Option<UnifiedResult>> {
    let filter = match &query.filter {
        Some(f) => f,
        None => return Ok(None),
    };

    // Try sorted index for range/between queries first
    if let Some(entity_ids) = try_sorted_index_lookup(filter, &query.table, idx_store) {
        return build_indexed_result_json(db, query, filter, entity_ids);
    }

    // Try hash index for equality queries — scan with candidate set
    let (eq_col, eq_val_bytes) = match extract_index_candidate_from_filter(filter) {
        Some(pair) => pair,
        None => return Ok(None),
    };
    let reg_idx = match idx_store.find_index_for_column(&query.table, &eq_col) {
        Some(idx) => idx,
        None => return Ok(None),
    };
    let entity_ids = idx_store
        .hash_lookup(&query.table, &reg_idx.name, &eq_val_bytes)
        .map_err(|err| RedDBError::Internal(format!("hash index lookup failed: {err}")))?;

    // For AND queries with hash index: scan with candidate set (avoids individual store.get())
    // This is faster than N individual get() calls because it iterates the HashMap sequentially
    if !entity_ids.is_empty() && entity_ids.len() < 5000 {
        let candidate_set: std::collections::HashSet<EntityId> =
            entity_ids.iter().copied().collect();
        return execute_scan_with_candidates_to_json(db, query, filter, &candidate_set);
    }
    if entity_ids.is_empty() {
        let json = r#"{"columns":[],"record_count":0,"selection":{"scope":"any"},"records":[]}"#
            .to_string();
        return Ok(Some(UnifiedResult {
            columns: Vec::new(),
            records: Vec::new(),
            stats: crate::storage::query::unified::QueryStats::default(),
            pre_serialized_json: Some(json),
        }));
    }

    let store = db.store();
    let table_name = query.table.as_str();
    let table_alias = query.alias.as_deref().unwrap_or(table_name);
    let limit = query.limit.unwrap_or(10000) as usize;

    // Pre-compute JSON prefix
    let mut sys_prefix = Vec::with_capacity(128);
    sys_prefix.extend_from_slice(b",\"red_collection\":");
    write_json_bytes(&mut sys_prefix, table_name.as_bytes());
    sys_prefix.extend_from_slice(
        b",\"red_kind\":\"table\",\"red_entity_type\":\"table\",\"red_capabilities\":\"structured,table\"",
    );

    let mut buf = Vec::with_capacity(256 + entity_ids.len().min(limit) * 200);
    buf.extend_from_slice(b"{\"columns\":[],\"record_count\":");
    let count_pos = buf.len();
    buf.extend_from_slice(b"0,\"selection\":{\"scope\":\"any\"},\"records\":[");

    let mut count: u64 = 0;

    // Only fetch & check entities returned by the index
    for eid in &entity_ids {
        if count as usize >= limit {
            break;
        }

        let entity = match store.get(&query.table, *eid) {
            Some(e) => e,
            None => continue,
        };

        if !entity.data.is_row() {
            continue;
        }

        // Evaluate the FULL filter (the index only handled the equality part)
        if !evaluate_entity_filter(&entity, filter, table_name, table_alias) {
            continue;
        }

        if count > 0 {
            buf.push(b',');
        }
        buf.extend_from_slice(b"{\"red_entity_id\":");
        write_u64(&mut buf, entity.id.raw());
        buf.extend_from_slice(&sys_prefix);
        buf.push(b',');
        write_timestamp_fields_json(&mut buf, &entity);
        buf.extend_from_slice(b",\"red_sequence_id\":");
        write_u64(&mut buf, entity.sequence_id);
        if let EntityKind::TableRow { row_id, .. } = &entity.kind {
            buf.extend_from_slice(b",\"row_id\":");
            write_u64(&mut buf, *row_id);
        }
        if let EntityData::Row(ref row) = entity.data {
            for (key, value) in row.iter_fields() {
                if key == "created_at" || key == "updated_at" {
                    continue;
                }
                buf.push(b',');
                write_json_bytes(&mut buf, key.as_bytes());
                buf.push(b':');
                write_value_bytes(&mut buf, value);
            }
        }
        buf.push(b'}');
        count += 1;
    }

    buf.extend_from_slice(b"]}");

    let mut count_buf = itoa::Buffer::new();
    let count_str = count_buf.format(count);
    buf.splice(
        count_pos..count_pos + 1,
        count_str.as_bytes().iter().copied(),
    );

    let json_string = unsafe { String::from_utf8_unchecked(buf) };
    Ok(Some(UnifiedResult {
        columns: Vec::new(),
        records: Vec::new(),
        stats: crate::storage::query::unified::QueryStats {
            rows_scanned: count,
            ..Default::default()
        },
        pre_serialized_json: Some(json_string),
    }))
}

/// Extract a (column_name, value_bytes) from the first equality condition in a filter.
/// Used to find hash index candidates.
/// Try to use a sorted (BTree) index for BETWEEN / > / < conditions.
pub(super) fn try_sorted_index_lookup(
    filter: &Filter,
    table: &str,
    idx_store: &super::index_store::IndexStore,
) -> Option<Vec<EntityId>> {
    match filter {
        Filter::Between { field, low, high } => {
            let col = match field {
                FieldRef::TableColumn { column, .. } => column.as_str(),
                _ => return None,
            };
            if !idx_store.sorted.has_index(table, col) {
                return None;
            }
            let lo = super::index_store::value_to_sorted_numeric_key(low)?;
            let hi = super::index_store::value_to_sorted_numeric_key(high)?;
            let ids = idx_store.sorted.range_lookup(table, col, lo, hi)?;
            // If too many results, full scan is faster than N individual get() calls
            if ids.len() > 5000 {
                return None;
            }
            Some(ids)
        }
        Filter::Compare { field, op, value }
            if matches!(
                *op,
                CompareOp::Lt | CompareOp::Le | CompareOp::Gt | CompareOp::Ge
            ) =>
        {
            let col = match field {
                FieldRef::TableColumn { column, .. } => column.as_str(),
                _ => return None,
            };
            if !idx_store.sorted.has_index(table, col) {
                return None;
            }
            let threshold = super::index_store::value_to_sorted_numeric_key(value)?;
            let ids = match *op {
                CompareOp::Lt => idx_store.sorted.lt_lookup(table, col, threshold)?,
                CompareOp::Le => idx_store.sorted.le_lookup(table, col, threshold)?,
                CompareOp::Gt => idx_store.sorted.gt_lookup(table, col, threshold)?,
                CompareOp::Ge => idx_store.sorted.ge_lookup(table, col, threshold)?,
                _ => unreachable!("non-range compare op guarded above"),
            };
            if ids.len() > 5000 {
                return None;
            }
            Some(ids)
        }
        Filter::And(_, _) => {
            // For AND filters, don't use sorted index — the hash index path
            // handles the equality part, and the remaining filter is evaluated
            // on the candidates. Using sorted index here returns too many results.
            None
        }
        _ => None,
    }
}

/// Build the JSON result from a set of entity IDs (from index lookup).
/// Scan entities sequentially but only process those in the candidate set (from hash index).
/// Faster than individual store.get() because HashMap iteration is sequential/cache-friendly.
fn execute_scan_with_candidates_to_json(
    db: &RedDB,
    query: &TableQuery,
    filter: &Filter,
    candidates: &std::collections::HashSet<EntityId>,
) -> RedDBResult<Option<UnifiedResult>> {
    let manager = match db.store().get_collection(query.table.as_str()) {
        Some(m) => m,
        None => return Ok(None),
    };

    let table_name = query.table.as_str();
    let table_alias = query.alias.as_deref().unwrap_or(table_name);
    let limit = query.limit.unwrap_or(10000) as usize;

    let mut sys_prefix = Vec::with_capacity(128);
    sys_prefix.extend_from_slice(b",\"red_collection\":");
    write_json_bytes(&mut sys_prefix, table_name.as_bytes());
    sys_prefix.extend_from_slice(
        b",\"red_kind\":\"table\",\"red_entity_type\":\"table\",\"red_capabilities\":\"structured,table\"",
    );

    let mut buf = Vec::with_capacity(256 + candidates.len().min(limit) * 200);
    buf.extend_from_slice(b"{\"columns\":[],\"record_count\":");
    let count_pos = buf.len();
    buf.extend_from_slice(b"0,\"selection\":{\"scope\":\"any\"},\"records\":[");

    let mut count: u64 = 0;
    let mut hit_limit = false;

    manager.for_each_entity(|entity| {
        if hit_limit {
            return false;
        }
        // Skip entities not in the candidate set (from hash index)
        if !candidates.contains(&entity.id) {
            return true;
        }
        if !entity.data.is_row() {
            return true;
        }
        // Evaluate the FULL filter (hash index only handled equality part)
        if !evaluate_entity_filter(entity, filter, table_name, table_alias) {
            return true;
        }

        if count > 0 {
            buf.push(b',');
        }
        buf.extend_from_slice(b"{\"red_entity_id\":");
        write_u64(&mut buf, entity.id.raw());
        buf.extend_from_slice(&sys_prefix);
        buf.push(b',');
        write_timestamp_fields_json(&mut buf, entity);
        buf.extend_from_slice(b",\"red_sequence_id\":");
        write_u64(&mut buf, entity.sequence_id);
        if let EntityKind::TableRow { row_id, .. } = &entity.kind {
            buf.extend_from_slice(b",\"row_id\":");
            write_u64(&mut buf, *row_id);
        }
        if let EntityData::Row(ref row) = entity.data {
            for (key, value) in row.iter_fields() {
                if key == "created_at" || key == "updated_at" {
                    continue;
                }
                buf.push(b',');
                write_json_bytes(&mut buf, key.as_bytes());
                buf.push(b':');
                write_value_bytes(&mut buf, value);
            }
        }
        buf.push(b'}');
        count += 1;
        if count as usize >= limit {
            hit_limit = true;
            return false;
        }
        true
    });

    buf.extend_from_slice(b"]}");
    let mut count_buf = itoa::Buffer::new();
    let count_str = count_buf.format(count);
    buf.splice(
        count_pos..count_pos + 1,
        count_str.as_bytes().iter().copied(),
    );

    let json_string = unsafe { String::from_utf8_unchecked(buf) };
    Ok(Some(UnifiedResult {
        columns: Vec::new(),
        records: Vec::new(),
        stats: crate::storage::query::unified::QueryStats {
            rows_scanned: count,
            ..Default::default()
        },
        pre_serialized_json: Some(json_string),
    }))
}

fn build_indexed_result_json(
    db: &RedDB,
    query: &TableQuery,
    filter: &Filter,
    entity_ids: Vec<EntityId>,
) -> RedDBResult<Option<UnifiedResult>> {
    if entity_ids.is_empty() {
        let json = r#"{"columns":[],"record_count":0,"selection":{"scope":"any"},"records":[]}"#
            .to_string();
        return Ok(Some(UnifiedResult {
            columns: Vec::new(),
            records: Vec::new(),
            stats: crate::storage::query::unified::QueryStats::default(),
            pre_serialized_json: Some(json),
        }));
    }

    let store = db.store();
    let table_name = query.table.as_str();
    let table_alias = query.alias.as_deref().unwrap_or(table_name);
    let limit = query.limit.unwrap_or(10000) as usize;

    let mut sys_prefix = Vec::with_capacity(128);
    sys_prefix.extend_from_slice(b",\"red_collection\":");
    write_json_bytes(&mut sys_prefix, table_name.as_bytes());
    sys_prefix.extend_from_slice(
        b",\"red_kind\":\"table\",\"red_entity_type\":\"table\",\"red_capabilities\":\"structured,table\"",
    );

    let mut buf = Vec::with_capacity(256 + entity_ids.len().min(limit) * 200);
    buf.extend_from_slice(b"{\"columns\":[],\"record_count\":");
    let count_pos = buf.len();
    buf.extend_from_slice(b"0,\"selection\":{\"scope\":\"any\"},\"records\":[");

    let mut count: u64 = 0;

    for eid in &entity_ids {
        if count as usize >= limit {
            break;
        }
        let entity = match store.get(&query.table, *eid) {
            Some(e) => e,
            None => continue,
        };
        if !entity.data.is_row() {
            continue;
        }
        // Evaluate the FULL filter (index only handled one predicate)
        if !evaluate_entity_filter(&entity, filter, table_name, table_alias) {
            continue;
        }

        if count > 0 {
            buf.push(b',');
        }
        buf.extend_from_slice(b"{\"red_entity_id\":");
        write_u64(&mut buf, entity.id.raw());
        buf.extend_from_slice(&sys_prefix);
        buf.push(b',');
        write_timestamp_fields_json(&mut buf, &entity);
        buf.extend_from_slice(b",\"red_sequence_id\":");
        write_u64(&mut buf, entity.sequence_id);
        if let EntityKind::TableRow { row_id, .. } = &entity.kind {
            buf.extend_from_slice(b",\"row_id\":");
            write_u64(&mut buf, *row_id);
        }
        if let EntityData::Row(ref row) = entity.data {
            for (key, value) in row.iter_fields() {
                if key == "created_at" || key == "updated_at" {
                    continue;
                }
                buf.push(b',');
                write_json_bytes(&mut buf, key.as_bytes());
                buf.push(b':');
                write_value_bytes(&mut buf, value);
            }
        }
        buf.push(b'}');
        count += 1;
    }

    buf.extend_from_slice(b"]}");
    let mut count_buf = itoa::Buffer::new();
    let count_str = count_buf.format(count);
    buf.splice(
        count_pos..count_pos + 1,
        count_str.as_bytes().iter().copied(),
    );

    let json_string = unsafe { String::from_utf8_unchecked(buf) };
    Ok(Some(UnifiedResult {
        columns: Vec::new(),
        records: Vec::new(),
        stats: crate::storage::query::unified::QueryStats {
            rows_scanned: count,
            ..Default::default()
        },
        pre_serialized_json: Some(json_string),
    }))
}

fn extract_index_candidate_from_filter(filter: &Filter) -> Option<(String, Vec<u8>)> {
    use crate::storage::query::ast::{CompareOp, FieldRef};

    match filter {
        Filter::Compare { field, op, value } if *op == CompareOp::Eq => {
            let col = match field {
                FieldRef::TableColumn { column, .. } => column.clone(),
                _ => return None,
            };
            if col.starts_with('_') {
                return None;
            }
            let bytes = match value {
                Value::Text(s) => s.as_bytes().to_vec(),
                Value::Integer(n) => n.to_le_bytes().to_vec(),
                Value::UnsignedInteger(n) => n.to_le_bytes().to_vec(),
                _ => return None,
            };
            Some((col, bytes))
        }
        Filter::And(left, right) => {
            extract_index_candidate_from_filter(left)
                .or_else(|| extract_index_candidate_from_filter(right))
        }
        _ => None,
    }
}

/// Turbo path for SELECT * FROM table [LIMIT N] — no WHERE clause.
pub(super) fn execute_unfiltered_scan_to_json(
    db: &RedDB,
    query: &TableQuery,
) -> RedDBResult<Option<UnifiedResult>> {
    let manager = match db.store().get_collection(query.table.as_str()) {
        Some(m) => m,
        None => return Ok(None),
    };

    let table_name = query.table.as_str();
    let limit = query.limit.unwrap_or(10000) as usize;
    let offset = query.offset.unwrap_or(0) as usize;

    let mut sys_prefix = Vec::with_capacity(128);
    sys_prefix.extend_from_slice(b",\"red_collection\":");
    write_json_bytes(&mut sys_prefix, table_name.as_bytes());
    sys_prefix.extend_from_slice(
        b",\"red_kind\":\"table\",\"red_entity_type\":\"table\",\"red_capabilities\":\"structured,table\"",
    );

    let mut buf = Vec::with_capacity(256 + limit.min(1000) * 200);
    buf.extend_from_slice(b"{\"columns\":[],\"record_count\":");
    let count_pos = buf.len();
    buf.extend_from_slice(b"0,\"selection\":{\"scope\":\"any\"},\"records\":[");

    let mut count: u64 = 0;
    let mut skipped: u64 = 0;
    let mut field_keys_cache: Option<Vec<(String, Vec<u8>)>> = None;

    manager.for_each_entity(|entity| {
        if count as usize >= limit {
            return false;
        }
        if !entity.data.is_row() {
            return true;
        }
        if (skipped as usize) < offset {
            skipped += 1;
            return true;
        }

        if count > 0 {
            buf.push(b',');
        }
        buf.extend_from_slice(b"{\"red_entity_id\":");
        write_u64(&mut buf, entity.id.raw());
        buf.extend_from_slice(&sys_prefix);
        buf.push(b',');
        write_timestamp_fields_json(&mut buf, entity);
        buf.extend_from_slice(b",\"red_sequence_id\":");
        write_u64(&mut buf, entity.sequence_id);
        if let EntityKind::TableRow { row_id, .. } = &entity.kind {
            buf.extend_from_slice(b",\"row_id\":");
            write_u64(&mut buf, *row_id);
        }
        if let EntityData::Row(ref row) = entity.data {
            if field_keys_cache.is_none() {
                let mut cache = Vec::new();
                for (key, _) in row.iter_fields() {
                    if key == "created_at" || key == "updated_at" {
                        continue;
                    }
                    let mut encoded = Vec::with_capacity(key.len() + 4);
                    encoded.push(b',');
                    write_json_bytes(&mut encoded, key.as_bytes());
                    encoded.push(b':');
                    cache.push((key.to_string(), encoded));
                }
                field_keys_cache = Some(cache);
            }
            if let Some(ref cache) = field_keys_cache {
                for (key, encoded_prefix) in cache {
                    buf.extend_from_slice(encoded_prefix);
                    if let Some(value) = row.get_field(key) {
                        write_value_bytes(&mut buf, value);
                    } else {
                        buf.extend_from_slice(b"null");
                    }
                }
            }
        }
        buf.push(b'}');
        count += 1;
        true
    });

    buf.extend_from_slice(b"]}");
    let mut count_buf = itoa::Buffer::new();
    let count_str = count_buf.format(count);
    buf.splice(
        count_pos..count_pos + 1,
        count_str.as_bytes().iter().copied(),
    );

    let json_string = unsafe { String::from_utf8_unchecked(buf) };
    Ok(Some(UnifiedResult {
        columns: Vec::new(),
        records: Vec::new(),
        stats: crate::storage::query::unified::QueryStats {
            rows_scanned: count,
            ..Default::default()
        },
        pre_serialized_json: Some(json_string),
    }))
}

pub(super) fn execute_filtered_scan_to_json(
    db: &RedDB,
    query: &TableQuery,
) -> RedDBResult<Option<UnifiedResult>> {
    let manager = match db.store().get_collection(query.table.as_str()) {
        Some(m) => m,
        None => return Ok(None),
    };

    let filter = query.filter.as_ref().ok_or_else(|| {
        RedDBError::Internal("filtered JSON scan called without a WHERE clause".into())
    })?;
    let table_name = query.table.as_str();
    let table_alias = query.alias.as_deref().unwrap_or(table_name);
    let limit = query.limit.unwrap_or(10000) as usize;

    // Pre-compute the collection-level JSON prefix that's the same for every entity
    // This avoids re-encoding _collection, _kind, _entity_type, _capabilities per row
    let mut sys_prefix = Vec::with_capacity(128);
    sys_prefix.extend_from_slice(b",\"red_collection\":");
    write_json_bytes(&mut sys_prefix, table_name.as_bytes());
    sys_prefix.extend_from_slice(
        b",\"red_kind\":\"table\",\"red_entity_type\":\"table\",\"red_capabilities\":\"structured,table\"",
    );
    let sys_prefix = sys_prefix; // freeze

    // Build JSON as raw bytes
    let mut buf = Vec::with_capacity(256 + limit * 200);
    buf.extend_from_slice(b"{\"columns\":[],\"record_count\":");
    let count_pos = buf.len();
    buf.extend_from_slice(b"0,\"selection\":{\"scope\":\"any\"},\"records\":[");

    let mut count: u64 = 0;
    let mut hit_limit = false;

    let eq_prefilter = extract_equality_prefilter(filter);

    // Pre-encoded field name cache: populated on first matching entity.
    // Each entry is the JSON-encoded key prefix: ,\"name\":
    let mut field_keys_cache: Option<Vec<(String, Vec<u8>)>> = None;

    manager.for_each_entity(|entity| {
        if hit_limit {
            return false;
        }
        if !entity.data.is_row() {
            return true;
        }

        // Fast pre-filter: direct HashMap lookup for equality condition
        if let Some((ref col, ref val)) = eq_prefilter {
            if let EntityData::Row(ref row) = entity.data {
                match row.get_field(col.as_str()) {
                    Some(v) if v == val => {}
                    _ => return true,
                }
            }
        }

        if !evaluate_entity_filter(entity, filter, table_name, table_alias) {
            return true;
        }

        if count > 0 {
            buf.push(b',');
        }
        buf.extend_from_slice(b"{\"red_entity_id\":");
        write_u64(&mut buf, entity.id.raw());
        buf.extend_from_slice(&sys_prefix);
        buf.push(b',');
        write_timestamp_fields_json(&mut buf, entity);
        buf.extend_from_slice(b",\"red_sequence_id\":");
        write_u64(&mut buf, entity.sequence_id);
        if let EntityKind::TableRow { row_id, .. } = &entity.kind {
            buf.extend_from_slice(b",\"row_id\":");
            write_u64(&mut buf, *row_id);
        }
        if let EntityData::Row(ref row) = entity.data {
            // Build field key cache on first entity (pre-encode JSON key prefixes)
            if field_keys_cache.is_none() {
                let mut cache = Vec::new();
                for (key, _) in row.iter_fields() {
                    if key == "created_at" || key == "updated_at" {
                        continue;
                    }
                    let mut encoded = Vec::with_capacity(key.len() + 4);
                    encoded.push(b',');
                    write_json_bytes(&mut encoded, key.as_bytes());
                    encoded.push(b':');
                    cache.push((key.to_string(), encoded));
                }
                field_keys_cache = Some(cache);
            }

            if let Some(ref cache) = field_keys_cache {
                for (key, encoded_prefix) in cache {
                    buf.extend_from_slice(encoded_prefix);
                    if let Some(value) = row.get_field(key) {
                        write_value_bytes(&mut buf, value);
                    } else {
                        buf.extend_from_slice(b"null");
                    }
                }
            }
        }
        buf.push(b'}');
        count += 1;

        if count as usize >= limit {
            hit_limit = true;
            return false;
        }
        true
    });

    buf.extend_from_slice(b"]}");

    // Patch record_count
    let mut count_buf = itoa::Buffer::new();
    let count_str = count_buf.format(count);
    buf.splice(
        count_pos..count_pos + 1,
        count_str.as_bytes().iter().copied(),
    );

    // SAFETY: we only wrote valid UTF-8 bytes (ASCII JSON)
    let json_string = unsafe { String::from_utf8_unchecked(buf) };

    Ok(Some(UnifiedResult {
        columns: Vec::new(),
        records: Vec::new(),
        stats: crate::storage::query::unified::QueryStats {
            rows_scanned: count,
            ..Default::default()
        },
        pre_serialized_json: Some(json_string),
    }))
}
