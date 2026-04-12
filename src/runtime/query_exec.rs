use super::*;

pub(super) fn execute_runtime_table_query(
    db: &RedDB,
    query: &TableQuery,
    index_store: Option<&super::index_store::IndexStore>,
) -> RedDBResult<UnifiedResult> {
    // ── AGGREGATE PATH: COUNT, AVG, SUM, MIN, MAX, GROUP BY ──
    if has_aggregate_projections(&query.columns) {
        return execute_aggregate_query(db, query);
    }

    // ── FAST ENTITY-ID PATH: O(1) lookup for WHERE _entity_id = N ──
    if query.filter.is_some()
        && query.order_by.is_empty()
        && query.group_by.is_empty()
        && query.having.is_none()
        && query.expand.is_none()
        && query.offset.is_none()
        && !is_universal_query_source(&query.table)
    {
        if let Some(entity_id) = extract_entity_id_from_filter(&query.filter) {
            let store = db.store();
            if let Some(entity) = store.get(&query.table, EntityId::new(entity_id)) {
                let json = execute_runtime_serialize_single_entity(&entity);
                return Ok(UnifiedResult {
                    columns: Vec::new(),
                    records: Vec::new(),
                    stats: crate::storage::query::unified::QueryStats {
                        rows_scanned: 1,
                        ..Default::default()
                    },
                    pre_serialized_json: Some(json),
                });
            }
            return Ok(UnifiedResult::default());
        }
    }

    let records = execute_runtime_canonical_table_query_indexed(db, query, index_store)?;
    let columns = projected_columns(&records, &query.columns);

    Ok(UnifiedResult {
        columns,
        records,
        stats: Default::default(),
        pre_serialized_json: None,
    })
}

/// Execute a filtered scan and serialize matching entities directly to JSON.
/// Returns None if the collection doesn't exist (falls through to normal path).
/// Write a u64 as decimal digits.
#[inline(always)]
fn write_u64(buf: &mut Vec<u8>, n: u64) {
    let mut b = itoa::Buffer::new();
    let s = b.format(n);
    buf.extend_from_slice(s.as_bytes());
}

/// Write an entity's fields as JSON bytes into a Vec<u8> buffer.
#[inline(always)]
fn write_entity_json_bytes(buf: &mut Vec<u8>, entity: &UnifiedEntity) {
    buf.push(b'{');
    buf.extend_from_slice(b"\"_entity_id\":");
    write_u64(buf, entity.id.raw());
    buf.extend_from_slice(b",\"_collection\":");
    write_json_bytes(buf, entity.kind.collection().as_bytes());
    buf.extend_from_slice(b",\"_kind\":");
    write_json_bytes(buf, entity.kind.storage_type().as_bytes());
    buf.extend_from_slice(b",\"_created_at\":");
    write_u64(buf, entity.created_at);
    buf.extend_from_slice(b",\"_updated_at\":");
    write_u64(buf, entity.updated_at);
    buf.extend_from_slice(b",\"_sequence_id\":");
    write_u64(buf, entity.sequence_id);
    buf.extend_from_slice(b",\"_entity_type\":\"table\",\"_capabilities\":\"structured,table\"");

    if let EntityKind::TableRow { row_id, .. } = &entity.kind {
        buf.extend_from_slice(b",\"row_id\":");
        write_u64(buf, *row_id);
    }

    // User fields (handles both named HashMap and columnar schema)
    if let EntityData::Row(ref row) = entity.data {
        for (key, value) in row.iter_fields() {
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
fn execute_indexed_scan_to_json(
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
    let entity_ids = idx_store.hash_lookup(&query.table, &reg_idx.name, &eq_val_bytes);

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
    sys_prefix.extend_from_slice(b",\"_collection\":");
    write_json_bytes(&mut sys_prefix, table_name.as_bytes());
    sys_prefix.extend_from_slice(
        b",\"_kind\":\"table\",\"_entity_type\":\"table\",\"_capabilities\":\"structured,table\"",
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
        buf.extend_from_slice(b"{\"_entity_id\":");
        write_u64(&mut buf, entity.id.raw());
        buf.extend_from_slice(&sys_prefix);
        buf.extend_from_slice(b",\"_created_at\":");
        write_u64(&mut buf, entity.created_at);
        buf.extend_from_slice(b",\"_updated_at\":");
        write_u64(&mut buf, entity.updated_at);
        buf.extend_from_slice(b",\"_sequence_id\":");
        write_u64(&mut buf, entity.sequence_id);
        if let EntityKind::TableRow { row_id, .. } = &entity.kind {
            buf.extend_from_slice(b",\"row_id\":");
            write_u64(&mut buf, *row_id);
        }
        if let EntityData::Row(ref row) = entity.data {
            {
                for (key, value) in row.iter_fields() {
                    buf.push(b',');
                    write_json_bytes(&mut buf, key.as_bytes());
                    buf.push(b':');
                    write_value_bytes(&mut buf, value);
                }
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
fn try_sorted_index_lookup(
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
            let lo = value_to_i64_for_index(low)?;
            let hi = value_to_i64_for_index(high)?;
            let ids = idx_store.sorted.range_lookup(table, col, lo, hi);
            // If too many results, full scan is faster than N individual get() calls
            if ids.len() > 2000 {
                return None;
            }
            Some(ids)
        }
        Filter::Compare { field, op, value } if *op == CompareOp::Gt || *op == CompareOp::Ge => {
            let col = match field {
                FieldRef::TableColumn { column, .. } => column.as_str(),
                _ => return None,
            };
            if !idx_store.sorted.has_index(table, col) {
                return None;
            }
            let threshold = value_to_i64_for_index(value)?;
            let adjusted = if *op == CompareOp::Gt {
                threshold
            } else {
                threshold - 1
            };
            let ids = idx_store.sorted.gt_lookup(table, col, adjusted);
            if ids.len() > 2000 {
                return None;
            }
            Some(ids)
        }
        Filter::And(_left, _right) => {
            // For AND filters, don't use sorted index — the hash index path
            // handles the equality part, and the remaining filter is evaluated
            // on the candidates. Using sorted index here returns too many results.
            None
        }
        _ => None,
    }
}

fn value_to_i64_for_index(val: &Value) -> Option<i64> {
    match val {
        Value::Integer(n) => Some(*n),
        Value::UnsignedInteger(n) => Some(*n as i64),
        Value::Float(f) => Some(*f as i64),
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
    sys_prefix.extend_from_slice(b",\"_collection\":");
    write_json_bytes(&mut sys_prefix, table_name.as_bytes());
    sys_prefix.extend_from_slice(
        b",\"_kind\":\"table\",\"_entity_type\":\"table\",\"_capabilities\":\"structured,table\"",
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
        buf.extend_from_slice(b"{\"_entity_id\":");
        write_u64(&mut buf, entity.id.raw());
        buf.extend_from_slice(&sys_prefix);
        buf.extend_from_slice(b",\"_created_at\":");
        write_u64(&mut buf, entity.created_at);
        buf.extend_from_slice(b",\"_updated_at\":");
        write_u64(&mut buf, entity.updated_at);
        buf.extend_from_slice(b",\"_sequence_id\":");
        write_u64(&mut buf, entity.sequence_id);
        if let EntityKind::TableRow { row_id, .. } = &entity.kind {
            buf.extend_from_slice(b",\"row_id\":");
            write_u64(&mut buf, *row_id);
        }
        if let EntityData::Row(ref row) = entity.data {
            {
                for (key, value) in row.iter_fields() {
                    buf.push(b',');
                    write_json_bytes(&mut buf, key.as_bytes());
                    buf.push(b':');
                    write_value_bytes(&mut buf, value);
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
    sys_prefix.extend_from_slice(b",\"_collection\":");
    write_json_bytes(&mut sys_prefix, table_name.as_bytes());
    sys_prefix.extend_from_slice(
        b",\"_kind\":\"table\",\"_entity_type\":\"table\",\"_capabilities\":\"structured,table\"",
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
        buf.extend_from_slice(b"{\"_entity_id\":");
        write_u64(&mut buf, entity.id.raw());
        buf.extend_from_slice(&sys_prefix);
        buf.extend_from_slice(b",\"_created_at\":");
        write_u64(&mut buf, entity.created_at);
        buf.extend_from_slice(b",\"_updated_at\":");
        write_u64(&mut buf, entity.updated_at);
        buf.extend_from_slice(b",\"_sequence_id\":");
        write_u64(&mut buf, entity.sequence_id);
        if let EntityKind::TableRow { row_id, .. } = &entity.kind {
            buf.extend_from_slice(b",\"row_id\":");
            write_u64(&mut buf, *row_id);
        }
        if let EntityData::Row(ref row) = entity.data {
            {
                for (key, value) in row.iter_fields() {
                    buf.push(b',');
                    write_json_bytes(&mut buf, key.as_bytes());
                    buf.push(b':');
                    write_value_bytes(&mut buf, value);
                }
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
        Filter::And(left, right) => extract_index_candidate_from_filter(left)
            .or_else(|| extract_index_candidate_from_filter(right)),
        _ => None,
    }
}

/// Turbo path for SELECT * FROM table [LIMIT N] — no WHERE clause.
fn execute_unfiltered_scan_to_json(
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
    sys_prefix.extend_from_slice(b",\"_collection\":");
    write_json_bytes(&mut sys_prefix, table_name.as_bytes());
    sys_prefix.extend_from_slice(
        b",\"_kind\":\"table\",\"_entity_type\":\"table\",\"_capabilities\":\"structured,table\"",
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
        buf.extend_from_slice(b"{\"_entity_id\":");
        write_u64(&mut buf, entity.id.raw());
        buf.extend_from_slice(&sys_prefix);
        buf.extend_from_slice(b",\"_created_at\":");
        write_u64(&mut buf, entity.created_at);
        buf.extend_from_slice(b",\"_updated_at\":");
        write_u64(&mut buf, entity.updated_at);
        buf.extend_from_slice(b",\"_sequence_id\":");
        write_u64(&mut buf, entity.sequence_id);
        if let EntityKind::TableRow { row_id, .. } = &entity.kind {
            buf.extend_from_slice(b",\"row_id\":");
            write_u64(&mut buf, *row_id);
        }
        if let EntityData::Row(ref row) = entity.data {
            if field_keys_cache.is_none() {
                let mut cache = Vec::new();
                for (key, _) in row.iter_fields() {
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

fn execute_filtered_scan_to_json(
    db: &RedDB,
    query: &TableQuery,
) -> RedDBResult<Option<UnifiedResult>> {
    let manager = match db.store().get_collection(query.table.as_str()) {
        Some(m) => m,
        None => return Ok(None),
    };

    let filter = query.filter.as_ref().unwrap();
    let table_name = query.table.as_str();
    let table_alias = query.alias.as_deref().unwrap_or(table_name);
    let limit = query.limit.unwrap_or(10000) as usize;

    // Pre-compute the collection-level JSON prefix that's the same for every entity
    // This avoids re-encoding _collection, _kind, _entity_type, _capabilities per row
    let mut sys_prefix = Vec::with_capacity(128);
    sys_prefix.extend_from_slice(b",\"_collection\":");
    write_json_bytes(&mut sys_prefix, table_name.as_bytes());
    sys_prefix.extend_from_slice(
        b",\"_kind\":\"table\",\"_entity_type\":\"table\",\"_capabilities\":\"structured,table\"",
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
        buf.extend_from_slice(b"{\"_entity_id\":");
        write_u64(&mut buf, entity.id.raw());
        buf.extend_from_slice(&sys_prefix);
        buf.extend_from_slice(b",\"_created_at\":");
        write_u64(&mut buf, entity.created_at);
        buf.extend_from_slice(b",\"_updated_at\":");
        write_u64(&mut buf, entity.updated_at);
        buf.extend_from_slice(b",\"_sequence_id\":");
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

pub(super) struct RuntimeTableExecutionContext<'a> {
    query: &'a TableQuery,
    table_name: &'a str,
    table_alias: &'a str,
}

fn execute_runtime_canonical_table_query_indexed(
    db: &RedDB,
    query: &TableQuery,
    index_store: Option<&super::index_store::IndexStore>,
) -> RedDBResult<Vec<UnifiedRecord>> {
    // ── ULTRA-FAST PATH: entity_id lookup bypasses planner entirely ──
    if let Some(entity_id) = extract_entity_id_from_filter(&query.filter) {
        let store = db.store();
        if let Some(entity) = store.get(&query.table, EntityId::new(entity_id)) {
            return Ok(runtime_table_record_from_entity(entity)
                .into_iter()
                .collect());
        }
        return Ok(Vec::new());
    }

    // ── INDEX-ASSISTED PATH: use hash index for O(1) equality lookups ──
    if let (Some(idx_store), Some(ref filter)) = (index_store, &query.filter) {
        if let Some((column, value_bytes)) = extract_index_candidate(filter) {
            if let Some(idx) = idx_store.find_index_for_column(&query.table, &column) {
                let entity_ids = idx_store.hash_lookup(&query.table, &idx.name, &value_bytes);
                if !entity_ids.is_empty() {
                    let store = db.store();
                    let mut records = Vec::new();
                    for eid in entity_ids {
                        if let Some(entity) = store.get(&query.table, eid) {
                            if let Some(record) = runtime_table_record_from_entity(entity) {
                                records.push(record);
                            }
                        }
                    }
                    return Ok(records);
                }
            }
        }
    }

    // ── FAST PATH: Simple filtered scan — bypass planner for basic WHERE queries ──
    // Evaluates the filter directly on raw entity data to avoid materializing
    // UnifiedRecord for every entity in the collection.
    // Excludes universal entity sources (e.g. "any") which span all collections.
    if query.filter.is_some()
        && query.group_by.is_empty()
        && query.having.is_none()
        && query.expand.is_none()
        && !is_universal_query_source(&query.table)
    {
        let manager = db
            .store()
            .get_collection(query.table.as_str())
            .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;

        let filter = query.filter.as_ref().unwrap();
        let table_name = query.table.as_str();
        let table_alias = query.alias.as_deref().unwrap_or(table_name);
        let limit = query.limit.unwrap_or(10000) as usize;

        // Bloom filter: extract PK key for segment pruning
        let bloom_key = extract_bloom_key_for_pk(filter);
        if let Some(ref key) = bloom_key {
            let (entities, _pruned) = manager.query_with_bloom_hint(Some(key), |e| e.data.is_row());
            if entities.is_empty() {
                return Ok(Vec::new());
            }
        }

        // Extract explicit column names for projection pushdown
        let select_cols = extract_select_column_names(&query.columns);

        // Pre-filter at entity level, only materialize records that pass
        let mut records: Vec<UnifiedRecord> = Vec::new();
        manager.for_each_entity(|entity| {
            if records.len() >= limit {
                return false; // stop iteration
            }
            if !entity.data.is_row() {
                return true; // skip non-row entities, continue
            }
            if evaluate_entity_filter(entity, filter, table_name, table_alias) {
                let record = if select_cols.is_empty() {
                    runtime_table_record_from_entity(entity.clone())
                } else {
                    runtime_table_record_from_entity_projected(entity.clone(), &select_cols)
                };
                if let Some(record) = record {
                    records.push(record);
                }
            }
            true // continue
        });

        // Apply ORDER BY if present
        if !query.order_by.is_empty() {
            let order_by = &query.order_by;
            records.sort_by(|left, right| {
                compare_runtime_order(left, right, order_by, Some(table_name), Some(table_alias))
            });
        }

        // Apply OFFSET
        if let Some(offset) = query.offset {
            let offset = offset as usize;
            if offset < records.len() {
                records = records.into_iter().skip(offset).collect();
            } else {
                records.clear();
            }
        }

        return Ok(records);
    }

    // ── FAST PATH: Unfiltered scan — bypass planner for simple SELECT * ──
    if query.filter.is_none()
        && query.group_by.is_empty()
        && query.having.is_none()
        && query.expand.is_none()
    {
        let mut records = scan_runtime_table_source_records(db, query.table.as_str())?;
        let table_name = query.table.as_str();
        let table_alias = query.alias.as_deref().unwrap_or(table_name);

        if !query.order_by.is_empty() {
            records.sort_by(|left, right| {
                compare_runtime_order(
                    left,
                    right,
                    &query.order_by,
                    Some(table_name),
                    Some(table_alias),
                )
            });
        }

        if let Some(offset) = query.offset {
            let offset = offset as usize;
            if offset < records.len() {
                records = records.into_iter().skip(offset).collect();
            } else {
                records.clear();
            }
        }

        if let Some(limit) = query.limit {
            records.truncate(limit as usize);
        }

        return Ok(records);
    }

    let plan = CanonicalPlanner::new(db).build(&QueryExpr::Table(query.clone()));
    let table_name = query.table.as_str();
    let table_alias = query.alias.as_deref().unwrap_or(table_name);
    let context = RuntimeTableExecutionContext {
        query,
        table_name,
        table_alias,
    };
    execute_runtime_canonical_table_node(db, &plan.root, &context)
}

pub(super) fn execute_runtime_canonical_table_node(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    context: &RuntimeTableExecutionContext<'_>,
) -> RedDBResult<Vec<UnifiedRecord>> {
    match node.operator.as_str() {
        "table_scan" | "index_seek" | "entity_scan" | "document_path_index_seek" => {
            // ── FAST PATH 1: Direct entity_id lookup (O(1) instead of full scan) ──
            if let Some(entity_id) = extract_entity_id_from_filter(&context.query.filter) {
                let store = db.store();
                if let Some(entity) = store.get(&context.query.table, EntityId::new(entity_id)) {
                    return Ok(runtime_table_record_from_entity(entity)
                        .into_iter()
                        .collect());
                }
                return Ok(Vec::new());
            }

            // ── FAST PATH 2: Filtered scan with entity-level pre-filter ──
            // Evaluates the WHERE clause directly on raw entity data, only
            // creating UnifiedRecord for entities that match the filter.
            // Skip for universal sources ("any") which need cross-collection scanning.
            if context.query.filter.is_some()
                && !is_universal_query_source(context.query.table.as_str())
            {
                let manager = db
                    .store()
                    .get_collection(context.query.table.as_str())
                    .ok_or_else(|| RedDBError::NotFound(context.query.table.clone()))?;

                let filter = context.query.filter.as_ref().unwrap();
                let table_name = context.table_name;
                let table_alias = context.table_alias;
                let limit = context.query.limit.unwrap_or(10000) as usize;

                let select_cols = extract_select_column_names(&context.query.columns);
                let mut records: Vec<UnifiedRecord> = Vec::new();
                manager.for_each_entity(|entity| {
                    if records.len() >= limit {
                        return false;
                    }
                    if !entity.data.is_row() {
                        return true;
                    }
                    if evaluate_entity_filter(entity, filter, table_name, table_alias) {
                        let record = if select_cols.is_empty() {
                            runtime_table_record_from_entity(entity.clone())
                        } else {
                            runtime_table_record_from_entity_projected(entity.clone(), &select_cols)
                        };
                        if let Some(record) = record {
                            records.push(record);
                        }
                    }
                    true
                });
                return Ok(records);
            }

            // ── DEFAULT: Full scan ──
            scan_runtime_table_source_records(db, context.query.table.as_str())
        }
        "filter" | "entity_filter" => {
            // ── FAST PATH: Direct entity_id lookup (O(1)) ──
            if let Some(entity_id) = extract_entity_id_from_filter(&context.query.filter) {
                let store = db.store();
                if let Some(entity) = store.get(&context.query.table, EntityId::new(entity_id)) {
                    return Ok(runtime_table_record_from_entity(entity)
                        .into_iter()
                        .collect());
                }
                return Ok(Vec::new());
            }

            let mut records = execute_runtime_canonical_table_child(db, node, context)?;
            if let Some(filter) = context.query.filter.as_ref() {
                records.retain(|record| {
                    evaluate_runtime_filter(
                        record,
                        filter,
                        Some(context.table_name),
                        Some(context.table_alias),
                    )
                });
            }
            Ok(records)
        }
        "document_path_filter" => {
            let mut records = execute_runtime_canonical_table_child(db, node, context)?;
            if let Some(filter) = context.query.filter.as_ref() {
                records.retain(|record| {
                    runtime_record_has_document_capability(record)
                        && evaluate_runtime_document_filter(
                            record,
                            filter,
                            Some(context.table_name),
                            Some(context.table_alias),
                        )
                });
            }
            Ok(records)
        }
        "sort" | "entity_sort" | "document_sort" => {
            let mut records = execute_runtime_canonical_table_child(db, node, context)?;
            if !context.query.order_by.is_empty() {
                records.sort_by(|left, right| {
                    compare_runtime_order(
                        left,
                        right,
                        &context.query.order_by,
                        Some(context.table_name),
                        Some(context.table_alias),
                    )
                });
            } else if node.operator == "entity_sort" {
                records.sort_by(compare_runtime_ranked_records);
            }
            Ok(records)
        }
        "offset" | "entity_offset" => {
            let records = execute_runtime_canonical_table_child(db, node, context)?;
            let offset = context.query.offset.unwrap_or(0) as usize;
            Ok(records.into_iter().skip(offset).collect())
        }
        "limit" | "entity_limit" => {
            let records = execute_runtime_canonical_table_child(db, node, context)?;
            let limit = context.query.limit.map(|value| value as usize);
            Ok(match limit {
                Some(limit) => records.into_iter().take(limit).collect(),
                None => records,
            })
        }
        "entity_search" => execute_runtime_canonical_table_child(db, node, context),
        "entity_topk" => {
            let mut records = execute_runtime_canonical_table_child(db, node, context)?;
            records.sort_by(compare_runtime_ranked_records);
            let limit = node
                .details
                .get("k")
                .and_then(|value| value.parse::<usize>().ok())
                .or_else(|| context.query.limit.map(|value| value as usize));
            Ok(match limit {
                Some(limit) => records.into_iter().take(limit).collect(),
                None => records,
            })
        }
        "projection" | "document_projection" | "entity_projection" => {
            let records = execute_runtime_canonical_table_child(db, node, context)?;
            let document_projection = node.operator == "document_projection";
            let entity_projection = node.operator == "entity_projection";
            Ok(records
                .iter()
                .map(|record| {
                    project_runtime_record(
                        record,
                        &context.query.columns,
                        Some(context.table_name),
                        Some(context.table_alias),
                        document_projection,
                        entity_projection,
                    )
                })
                .collect())
        }
        other => Err(RedDBError::Query(format!(
            "unsupported canonical table operator {other}"
        ))),
    }
}

pub(super) fn execute_runtime_canonical_table_child(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    context: &RuntimeTableExecutionContext<'_>,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let child = node.children.first().ok_or_else(|| {
        RedDBError::Query(format!(
            "canonical table operator {} is missing its child plan",
            node.operator
        ))
    })?;
    execute_runtime_canonical_table_node(db, child, context)
}

pub(super) fn runtime_record_has_document_capability(record: &UnifiedRecord) -> bool {
    record
        .values
        .get("_capabilities")
        .and_then(|value| match value {
            crate::storage::schema::Value::Text(value) => Some(value),
            _ => None,
        })
        .map(|capabilities| {
            capabilities
                .split(',')
                .any(|capability| capability.trim() == "document")
        })
        .unwrap_or(false)
}

pub(super) fn evaluate_runtime_document_filter(
    record: &UnifiedRecord,
    filter: &crate::storage::query::ast::Filter,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> bool {
    evaluate_runtime_filter(record, filter, table_name, table_alias)
}

pub(super) fn runtime_record_rank_score(record: &UnifiedRecord) -> f64 {
    [
        "_score",
        "hybrid_score",
        "final_score",
        "score",
        "graph_score",
        "table_score",
        "graph_match",
        "vector_score",
        "vector_similarity",
        "structured_score",
        "structured_match",
        "text_relevance",
    ]
    .into_iter()
    .find_map(|field| record.values.get(field).and_then(runtime_value_number))
    .unwrap_or(0.0)
}

pub(super) fn compare_runtime_ranked_records(
    left: &UnifiedRecord,
    right: &UnifiedRecord,
) -> Ordering {
    runtime_record_rank_score(right)
        .partial_cmp(&runtime_record_rank_score(left))
        .unwrap_or(Ordering::Equal)
        .then_with(|| runtime_record_identity_key(left).cmp(&runtime_record_identity_key(right)))
}

pub(super) fn execute_runtime_join_query(
    db: &RedDB,
    query: &JoinQuery,
) -> RedDBResult<UnifiedResult> {
    let records = execute_runtime_canonical_join_query(db, query)?;
    let columns = projected_columns(&records, &query.return_);

    Ok(UnifiedResult {
        columns,
        records,
        stats: Default::default(),
        pre_serialized_json: None,
    })
}

pub(super) fn execute_runtime_canonical_join_query(
    db: &RedDB,
    query: &JoinQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let plan = CanonicalPlanner::new(db).build(&QueryExpr::Join(query.clone()));
    execute_runtime_canonical_join_node(db, &plan.root, query)
}

pub(super) fn execute_runtime_canonical_join_node(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    query: &JoinQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let (left_table_name, left_table_alias, right_table_name, right_table_alias) =
        runtime_join_table_context(query);

    match node.operator.as_str() {
        "filter" => {
            let mut records = execute_runtime_canonical_join_child(db, node, query)?;
            if let Some(filter) = query.filter.as_ref() {
                records.retain(|record| {
                    evaluate_runtime_join_filter(
                        record,
                        filter,
                        left_table_name,
                        left_table_alias,
                        right_table_name,
                        right_table_alias,
                    )
                });
            }
            Ok(records)
        }
        "sort" | "document_sort" | "entity_sort" => {
            let mut records = execute_runtime_canonical_join_child(db, node, query)?;
            if !query.order_by.is_empty() {
                records.sort_by(|left, right| {
                    compare_runtime_join_order(
                        left,
                        right,
                        &query.order_by,
                        left_table_name,
                        left_table_alias,
                        right_table_name,
                        right_table_alias,
                    )
                });
            } else if node.operator == "entity_sort" {
                records.sort_by(compare_runtime_ranked_records);
            }
            Ok(records)
        }
        "offset" => {
            let records = execute_runtime_canonical_join_child(db, node, query)?;
            let offset = query.offset.unwrap_or(0) as usize;
            Ok(records.into_iter().skip(offset).collect())
        }
        "limit" => {
            let records = execute_runtime_canonical_join_child(db, node, query)?;
            let limit = query.limit.map(|value| value as usize);
            Ok(match limit {
                Some(limit) => records.into_iter().take(limit).collect(),
                None => records,
            })
        }
        "projection" => {
            let records = execute_runtime_canonical_join_child(db, node, query)?;
            Ok(records
                .iter()
                .map(|record| {
                    project_runtime_join_record(
                        record,
                        &query.return_,
                        left_table_name,
                        left_table_alias,
                        right_table_name,
                        right_table_alias,
                    )
                })
                .collect())
        }
        "join" => execute_runtime_canonical_join_base(
            db,
            node,
            query,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        ),
        other => Err(RedDBError::Query(format!(
            "unsupported canonical join operator {other}"
        ))),
    }
}

pub(super) fn execute_runtime_canonical_join_base(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    query: &JoinQuery,
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
) -> RedDBResult<Vec<UnifiedRecord>> {
    if node.children.len() != 2 {
        return Err(RedDBError::Query(
            "canonical join operator must contain exactly two child plans".to_string(),
        ));
    }

    let join_type = canonical_join_type(node)?;
    let left_join_field = canonical_join_field(node, "left_field")?;
    let right_join_field = canonical_join_field(node, "right_field")?;
    let join_strategy = canonical_join_strategy(node)?;

    let left_query = match query.left.as_ref() {
        QueryExpr::Table(table) => table,
        _ => {
            return Err(RedDBError::Query(
                "runtime joins currently require a table expression on the left side".to_string(),
            ))
        }
    };

    let left_records =
        execute_runtime_canonical_expr_node(db, &node.children[0], query.left.as_ref())?;

    let right_records =
        execute_runtime_canonical_expr_node(db, &node.children[1], query.right.as_ref())?;

    // Auto-upgrade to hash join for large datasets
    let join_strategy = if matches!(join_strategy, CanonicalJoinStrategy::NestedLoop)
        && left_records.len() * right_records.len() > 10_000
    {
        CanonicalJoinStrategy::HashJoin
    } else {
        join_strategy
    };

    match join_strategy {
        CanonicalJoinStrategy::IndexedNestedLoop => execute_runtime_indexed_join(
            left_query,
            &left_records,
            left_table_name,
            left_table_alias,
            &left_join_field,
            &right_records,
            right_table_name,
            right_table_alias,
            &right_join_field,
            join_type,
        ),
        CanonicalJoinStrategy::HashJoin => execute_runtime_hash_join(
            left_query,
            &left_records,
            left_table_name,
            left_table_alias,
            &left_join_field,
            &right_records,
            right_table_name,
            right_table_alias,
            &right_join_field,
            join_type,
        ),
        CanonicalJoinStrategy::NestedLoop => execute_runtime_full_scan_join(
            left_query,
            &left_records,
            left_table_name,
            left_table_alias,
            &left_join_field,
            &right_records,
            right_table_name,
            right_table_alias,
            &right_join_field,
            join_type,
        ),
        CanonicalJoinStrategy::GraphLookupJoin => execute_runtime_graph_lookup_join(
            left_query,
            &left_records,
            left_table_name,
            left_table_alias,
            &left_join_field,
            &right_records,
            right_table_name,
            right_table_alias,
            &right_join_field,
            join_type,
        ),
    }
}

pub(super) fn execute_runtime_canonical_join_child(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    query: &JoinQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let child = node.children.first().ok_or_else(|| {
        RedDBError::Query(format!(
            "canonical join operator {} is missing its child plan",
            node.operator
        ))
    })?;
    execute_runtime_canonical_join_node(db, child, query)
}

pub(super) fn runtime_join_table_context(
    query: &JoinQuery,
) -> (Option<&str>, Option<&str>, Option<&str>, Option<&str>) {
    let (left_table_name, left_table_alias) = match query.left.as_ref() {
        QueryExpr::Table(table) => (
            Some(table.table.as_str()),
            Some(table.alias.as_deref().unwrap_or(table.table.as_str())),
        ),
        _ => (None, None),
    };
    let (right_table_name, right_table_alias) = match query.right.as_ref() {
        QueryExpr::Table(table) => (
            Some(table.table.as_str()),
            Some(table.alias.as_deref().unwrap_or(table.table.as_str())),
        ),
        QueryExpr::Graph(graph) => (Some("graph"), graph.alias.as_deref().or(Some("graph"))),
        QueryExpr::Path(path) => (Some("path"), path.alias.as_deref().or(Some("path"))),
        QueryExpr::Vector(vector) => (Some("vector"), vector.alias.as_deref().or(Some("vector"))),
        QueryExpr::Hybrid(hybrid) => (Some("hybrid"), hybrid.alias.as_deref().or(Some("hybrid"))),
        QueryExpr::Join(_) => (Some("join"), Some("join")),
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
        | QueryExpr::QueueCommand(_) => (None, None),
    };

    (
        left_table_name,
        left_table_alias,
        right_table_name,
        right_table_alias,
    )
}

pub(super) fn resolve_runtime_join_field(
    record: &UnifiedRecord,
    field: &FieldRef,
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
) -> Option<Value> {
    match field {
        FieldRef::TableColumn { table, column } if !table.is_empty() => {
            if let Some(value) = record.values.get(&format!("{table}.{column}")) {
                return Some(value.clone());
            }

            let matches_left =
                runtime_table_context_matches(table.as_str(), left_table_name, left_table_alias);
            let matches_right =
                runtime_table_context_matches(table.as_str(), right_table_name, right_table_alias);
            if !(matches_left || matches_right) {
                return None;
            }

            record
                .values
                .get(column)
                .cloned()
                .or_else(|| resolve_runtime_document_path(record, column))
        }
        _ => resolve_runtime_field(record, field, None, None),
    }
}

pub(super) fn project_runtime_join_record(
    source: &UnifiedRecord,
    projections: &[Projection],
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
) -> UnifiedRecord {
    let select_all = projections.is_empty()
        || projections
            .iter()
            .any(|item| matches!(item, Projection::All));
    let mut record = UnifiedRecord::new();
    record.nodes = source.nodes.clone();
    record.edges = source.edges.clone();
    record.paths = source.paths.clone();
    record.vector_results = source.vector_results.clone();

    if select_all {
        for key in visible_value_keys(source) {
            if let Some(value) = source.values.get(&key) {
                record.values.insert(key, value.clone());
            }
        }
    }

    for projection in projections {
        if matches!(projection, Projection::All) {
            continue;
        }

        let label = projection_name(projection);
        let value = match projection {
            Projection::Column(column) | Projection::Alias(column, _) => source
                .values
                .get(column)
                .cloned()
                .or_else(|| resolve_runtime_document_path(source, column)),
            Projection::Field(field, _) => resolve_runtime_join_field(
                source,
                field,
                left_table_name,
                left_table_alias,
                right_table_name,
                right_table_alias,
            ),
            Projection::Expression(filter, _) => {
                Some(Value::Boolean(evaluate_runtime_join_filter(
                    source,
                    filter,
                    left_table_name,
                    left_table_alias,
                    right_table_name,
                    right_table_alias,
                )))
            }
            Projection::Function(_, _) => Some(Value::Null),
            Projection::All => None,
        };

        record.values.insert(label, value.unwrap_or(Value::Null));
    }

    record
}

pub(super) fn evaluate_runtime_join_filter(
    record: &UnifiedRecord,
    filter: &Filter,
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
) -> bool {
    match filter {
        Filter::Compare { field, op, value } => resolve_runtime_join_field(
            record,
            field,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        )
        .as_ref()
        .and_then(|candidate| evaluate_metadata_field_compare(field, candidate, *op, value))
        .or_else(|| {
            resolve_runtime_join_field(
                record,
                field,
                left_table_name,
                left_table_alias,
                right_table_name,
                right_table_alias,
            )
            .as_ref()
            .map(|candidate| compare_runtime_values(candidate, value, *op))
        })
        .unwrap_or(false),
        Filter::And(left, right) => {
            evaluate_runtime_join_filter(
                record,
                left,
                left_table_name,
                left_table_alias,
                right_table_name,
                right_table_alias,
            ) && evaluate_runtime_join_filter(
                record,
                right,
                left_table_name,
                left_table_alias,
                right_table_name,
                right_table_alias,
            )
        }
        Filter::Or(left, right) => {
            evaluate_runtime_join_filter(
                record,
                left,
                left_table_name,
                left_table_alias,
                right_table_name,
                right_table_alias,
            ) || evaluate_runtime_join_filter(
                record,
                right,
                left_table_name,
                left_table_alias,
                right_table_name,
                right_table_alias,
            )
        }
        Filter::Not(inner) => !evaluate_runtime_join_filter(
            record,
            inner,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        ),
        Filter::IsNull(field) => resolve_runtime_join_field(
            record,
            field,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        )
        .map(|value| value == Value::Null)
        .unwrap_or(true),
        Filter::IsNotNull(field) => resolve_runtime_join_field(
            record,
            field,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        )
        .map(|value| value != Value::Null)
        .unwrap_or(false),
        Filter::In { field, values } => resolve_runtime_join_field(
            record,
            field,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        )
        .as_ref()
        .is_some_and(|candidate| {
            evaluate_metadata_field_in(field, candidate, values).unwrap_or_else(|| {
                values
                    .iter()
                    .any(|value| compare_runtime_values(candidate, value, CompareOp::Eq))
            })
        }),
        Filter::Between { field, low, high } => resolve_runtime_join_field(
            record,
            field,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        )
        .as_ref()
        .is_some_and(|candidate| {
            compare_runtime_values(candidate, low, CompareOp::Ge)
                && compare_runtime_values(candidate, high, CompareOp::Le)
        }),
        Filter::Like { field, pattern } => resolve_runtime_join_field(
            record,
            field,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        )
        .as_ref()
        .and_then(runtime_value_text)
        .is_some_and(|value| like_matches(&value, pattern)),
        Filter::StartsWith { field, prefix } => resolve_runtime_join_field(
            record,
            field,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        )
        .as_ref()
        .and_then(runtime_value_text)
        .is_some_and(|value| value.starts_with(prefix)),
        Filter::EndsWith { field, suffix } => resolve_runtime_join_field(
            record,
            field,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        )
        .as_ref()
        .and_then(runtime_value_text)
        .is_some_and(|value| value.ends_with(suffix)),
        Filter::Contains { field, substring } => resolve_runtime_join_field(
            record,
            field,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        )
        .as_ref()
        .and_then(runtime_value_text)
        .is_some_and(|value| value.contains(substring)),
    }
}

pub(super) fn compare_runtime_join_order(
    left: &UnifiedRecord,
    right: &UnifiedRecord,
    clauses: &[OrderByClause],
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
) -> Ordering {
    for clause in clauses {
        let left_value = resolve_runtime_join_field(
            left,
            &clause.field,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        );
        let right_value = resolve_runtime_join_field(
            right,
            &clause.field,
            left_table_name,
            left_table_alias,
            right_table_name,
            right_table_alias,
        );
        let ordering = compare_runtime_optional_values(
            left_value.as_ref(),
            right_value.as_ref(),
            clause.nulls_first,
        );
        if ordering != Ordering::Equal {
            return if clause.ascending {
                ordering
            } else {
                ordering.reverse()
            };
        }
    }

    runtime_record_identity_key(left).cmp(&runtime_record_identity_key(right))
}

pub(super) fn execute_runtime_canonical_expr_node(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    expr: &QueryExpr,
) -> RedDBResult<Vec<UnifiedRecord>> {
    match expr {
        QueryExpr::Table(table) => {
            let table_name = table.table.as_str();
            let table_alias = table.alias.as_deref().unwrap_or(table_name);
            let context = RuntimeTableExecutionContext {
                query: table,
                table_name,
                table_alias,
            };
            execute_runtime_canonical_table_node(db, node, &context)
        }
        QueryExpr::Graph(_) | QueryExpr::Path(_) => {
            let graph = materialize_graph(db.store().as_ref())?;
            let node_properties = materialize_graph_node_properties(db.store().as_ref())?;
            let result =
                crate::storage::query::unified::UnifiedExecutor::execute_on_with_node_properties(
                    &graph,
                    expr,
                    node_properties,
                )
                .map_err(|err| RedDBError::Query(err.to_string()))?;
            Ok(result.records)
        }
        QueryExpr::Vector(vector) => Ok(execute_runtime_vector_query(db, vector)?.records),
        QueryExpr::Hybrid(hybrid) => Ok(execute_runtime_hybrid_query(db, hybrid)?.records),
        other => Err(RedDBError::Query(format!(
            "canonical join execution does not yet support {} child expressions",
            query_expr_name(other)
        ))),
    }
}

pub(super) fn execute_runtime_vector_query(
    db: &RedDB,
    query: &VectorQuery,
) -> RedDBResult<UnifiedResult> {
    let plan = CanonicalPlanner::new(db).build(&QueryExpr::Vector(query.clone()));
    let records = execute_runtime_canonical_vector_node(db, &plan.root, query)?;

    Ok(UnifiedResult {
        columns: collect_visible_columns(&records),
        records,
        stats: Default::default(),
        pre_serialized_json: None,
    })
}

pub(super) fn execute_runtime_canonical_vector_node(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    query: &VectorQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    match node.operator.as_str() {
        "vector_ann_hnsw" | "vector_ann_ivf" | "vector_exact_scan" => {
            let vector = resolve_runtime_vector_source(db, &query.query_vector)?;
            let matches = runtime_vector_matches(db, query, &vector)?;
            Ok(matches
                .into_iter()
                .map(runtime_vector_record_from_match)
                .collect())
        }
        "metadata_filter" => {
            let mut records = execute_runtime_canonical_vector_child(db, node, query)?;
            if let Some(filter) = query.filter.as_ref() {
                records.retain(|record| {
                    runtime_vector_record_matches_filter(db, &query.collection, record, filter)
                });
            }
            Ok(records)
        }
        "similarity_threshold" => {
            let mut records = execute_runtime_canonical_vector_child(db, node, query)?;
            if let Some(threshold) = query.threshold {
                records.retain(|record| runtime_record_rank_score(record) >= threshold as f64);
            }
            Ok(records)
        }
        "topk" => {
            let mut records = execute_runtime_canonical_vector_child(db, node, query)?;
            records.sort_by(compare_runtime_ranked_records);
            Ok(records.into_iter().take(query.k.max(1)).collect())
        }
        "projection" => execute_runtime_canonical_vector_child(db, node, query),
        other => Err(RedDBError::Query(format!(
            "unsupported canonical vector operator {other}"
        ))),
    }
}

pub(super) fn execute_runtime_canonical_vector_child(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    query: &VectorQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let child = node.children.first().ok_or_else(|| {
        RedDBError::Query(format!(
            "canonical vector operator {} is missing its child plan",
            node.operator
        ))
    })?;
    execute_runtime_canonical_vector_node(db, child, query)
}

pub(super) fn runtime_vector_matches(
    db: &RedDB,
    query: &VectorQuery,
    vector: &[f32],
) -> RedDBResult<Vec<SimilarResult>> {
    let manager = db
        .store()
        .get_collection(&query.collection)
        .ok_or_else(|| RedDBError::NotFound(query.collection.clone()))?;

    if query.filter.is_none() {
        let mut results = db.similar(&query.collection, vector, manager.count().max(1));
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.entity_id.raw().cmp(&b.entity_id.raw()))
        });
        return Ok(results);
    }

    let mut results: Vec<SimilarResult> = manager
        .query_all(|_| true)
        .into_iter()
        .filter_map(|entity| {
            let score = runtime_entity_vector_similarity(&entity, vector);
            let distance = (1.0 - score).max(0.0);
            (score > 0.0).then_some(SimilarResult {
                entity_id: entity.id,
                score,
                distance,
                entity,
            })
        })
        .collect();

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.entity_id.raw().cmp(&b.entity_id.raw()))
    });
    Ok(results)
}

pub(super) fn runtime_vector_record_matches_filter(
    db: &RedDB,
    collection: &str,
    record: &UnifiedRecord,
    filter: &VectorMetadataFilter,
) -> bool {
    let entity_id = record
        .values
        .get("entity_id")
        .or_else(|| record.values.get("_entity_id"))
        .and_then(|value| match value {
            Value::UnsignedInteger(value) => Some(EntityId::new(*value)),
            Value::Integer(value) if *value >= 0 => Some(EntityId::new(*value as u64)),
            _ => None,
        });

    let Some(entity_id) = entity_id else {
        return false;
    };

    let metadata = db
        .store()
        .get_metadata(collection, entity_id)
        .unwrap_or_default();
    let entry = runtime_metadata_entry(&metadata);
    filter.matches(&entry)
}

pub(super) fn execute_runtime_hybrid_query(
    db: &RedDB,
    query: &HybridQuery,
) -> RedDBResult<UnifiedResult> {
    let plan = CanonicalPlanner::new(db).build(&QueryExpr::Hybrid(query.clone()));
    let mut records = execute_runtime_canonical_hybrid_node(db, &plan.root, query)?;
    if let Some(limit) = query.limit {
        records.truncate(limit);
    }

    Ok(UnifiedResult {
        columns: collect_visible_columns(&records),
        records,
        stats: Default::default(),
        pre_serialized_json: None,
    })
}

pub(super) fn execute_runtime_canonical_hybrid_node(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    query: &HybridQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    match node.operator.as_str() {
        "entity_search" => execute_runtime_canonical_hybrid_child(db, node, query),
        "entity_topk" => {
            let mut records = execute_runtime_canonical_hybrid_child(db, node, query)?;
            records.sort_by(compare_runtime_ranked_records);
            let limit = node
                .details
                .get("k")
                .and_then(|value| value.parse::<usize>().ok())
                .or(query.limit);
            Ok(match limit {
                Some(limit) => records.into_iter().take(limit).collect(),
                None => records,
            })
        }
        "hybrid_fusion" => execute_runtime_canonical_hybrid_fusion(db, node, query),
        other => Err(RedDBError::Query(format!(
            "unsupported canonical hybrid operator {other}"
        ))),
    }
}

pub(super) fn execute_runtime_canonical_hybrid_child(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    query: &HybridQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let child = node.children.first().ok_or_else(|| {
        RedDBError::Query(format!(
            "canonical hybrid operator {} is missing its child plan",
            node.operator
        ))
    })?;
    execute_runtime_canonical_hybrid_node(db, child, query)
}

pub(super) fn execute_runtime_canonical_hybrid_fusion(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    query: &HybridQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    if node.children.len() != 2 {
        return Err(RedDBError::Query(
            "canonical hybrid_fusion operator must contain exactly two child plans".to_string(),
        ));
    }

    let structured =
        execute_runtime_canonical_expr_node(db, &node.children[0], query.structured.as_ref())?;
    let vector_expr = QueryExpr::Vector(query.vector.clone());
    let vector = execute_runtime_canonical_expr_node(db, &node.children[1], &vector_expr)?;

    let mut structured_map = HashMap::new();
    let mut structured_rank = HashMap::new();
    for (index, record) in structured.iter().cloned().enumerate() {
        let key = runtime_record_identity_key(&record);
        structured_rank.insert(key.clone(), index);
        structured_map.insert(key, record);
    }

    let mut vector_map = HashMap::new();
    let mut vector_rank = HashMap::new();
    for (index, record) in vector.iter().cloned().enumerate() {
        let key = runtime_record_identity_key(&record);
        vector_rank.insert(key.clone(), index);
        vector_map.insert(key, record);
    }

    let ordered_keys = hybrid_candidate_keys(&structured_map, &vector_map, &query.fusion);

    let mut scored_records = Vec::new();
    for key in ordered_keys {
        let structured_record = structured_map.get(&key);
        let vector_record = vector_map.get(&key);
        let s_rank = structured_rank.get(&key).copied();
        let v_rank = vector_rank.get(&key).copied();
        let s_score = structured_record
            .as_ref()
            .map_or(0.0, |record| runtime_structured_score(record, s_rank));
        let v_score = vector_record
            .as_ref()
            .map_or(0.0, |r| runtime_vector_score(r));

        let score = match &query.fusion {
            FusionStrategy::Rerank { weight } => {
                if structured_record.is_none() {
                    continue;
                }
                ((1.0 - *weight as f64) * s_score) + ((*weight as f64) * v_score)
            }
            FusionStrategy::FilterThenSearch | FusionStrategy::SearchThenFilter => {
                if structured_record.is_none() || vector_record.is_none() {
                    continue;
                }
                v_score
            }
            FusionStrategy::Intersection => {
                if structured_record.is_none() || vector_record.is_none() {
                    continue;
                }
                (s_score + v_score) / 2.0
            }
            FusionStrategy::Union {
                structured_weight,
                vector_weight,
            } => ((*structured_weight as f64) * s_score) + ((*vector_weight as f64) * v_score),
            FusionStrategy::RRF { k } => {
                let mut total = 0.0;
                if let Some(rank) = s_rank {
                    total += 1.0 / (*k as f64 + rank as f64 + 1.0);
                }
                if let Some(rank) = v_rank {
                    total += 1.0 / (*k as f64 + rank as f64 + 1.0);
                }
                total
            }
        };

        let mut record = merge_hybrid_records(structured_record, vector_record);
        record.set("score", Value::Float(score));
        record.set("_score", Value::Float(score));
        record.set("final_score", Value::Float(score));
        record.set("hybrid_score", Value::Float(score));
        record.set(
            "structured_score",
            if structured_record.is_some() {
                Value::Float(s_score)
            } else {
                Value::Null
            },
        );
        record.set(
            "vector_score",
            if vector_record.is_some() {
                Value::Float(v_score)
            } else {
                Value::Null
            },
        );
        record.set(
            "vector_similarity",
            if vector_record.is_some() {
                Value::Float(v_score)
            } else {
                Value::Null
            },
        );
        record.set(
            "structured_rank",
            s_rank
                .map(|value| Value::UnsignedInteger(value as u64))
                .unwrap_or(Value::Null),
        );
        record.set(
            "vector_rank",
            v_rank
                .map(|value| Value::UnsignedInteger(value as u64))
                .unwrap_or(Value::Null),
        );
        scored_records.push((score, record));
    }

    scored_records.sort_by(|left, right| compare_runtime_ranked_records(&left.1, &right.1));
    Ok(scored_records
        .into_iter()
        .map(|(_, record)| record)
        .collect())
}

/// Extract the first equality condition from an AND filter for fast pre-filtering.
/// For `WHERE city = 'NYC' AND age > 30`, returns Some(("city", Value::Text("NYC"))).
/// This lets us do a direct HashMap lookup before the full filter evaluation.
fn extract_equality_prefilter(filter: &Filter) -> Option<(String, Value)> {
    use crate::storage::query::ast::{CompareOp, FieldRef};
    match filter {
        Filter::Compare { field, op, value } if *op == CompareOp::Eq => {
            let col = match field {
                FieldRef::TableColumn { column, .. } => column.clone(),
                _ => return None,
            };
            // Skip system fields (they're not in named HashMap)
            if col.starts_with('_') {
                return None;
            }
            Some((col, value.clone()))
        }
        Filter::And(left, right) => {
            extract_equality_prefilter(left).or_else(|| extract_equality_prefilter(right))
        }
        _ => None,
    }
}

/// Extract entity_id from `WHERE _entity_id = N` for O(1) direct lookup.
pub(crate) fn extract_entity_id_from_filter(
    filter: &Option<crate::storage::query::ast::Filter>,
) -> Option<u64> {
    use crate::storage::query::ast::{CompareOp, FieldRef, Filter};
    let filter = filter.as_ref()?;
    match filter {
        Filter::Compare { field, op, value } if *op == CompareOp::Eq => {
            let field_name = match field {
                FieldRef::TableColumn { column, .. } => column.as_str(),
                _ => return None,
            };
            if field_name != "_entity_id" && field_name != "entity_id" {
                return None;
            }
            match value {
                Value::Integer(n) => Some(*n as u64),
                Value::UnsignedInteger(n) => Some(*n),
                _ => None,
            }
        }
        Filter::And(left, right) => extract_entity_id_from_filter(&Some(*left.clone()))
            .or_else(|| extract_entity_id_from_filter(&Some(*right.clone()))),
        _ => None,
    }
}

/// Extract a bloom filter key hint from a PK/ID equality filter ONLY.
///
/// Bloom filters only index entity IDs and primary keys. Using them for
/// general column values causes incorrect pruning (false negatives).
/// Restricted to: _entity_id, row_id, id, key.
fn extract_bloom_key_for_pk(filter: &crate::storage::query::ast::Filter) -> Option<Vec<u8>> {
    use crate::storage::query::ast::{CompareOp, FieldRef, Filter};
    match filter {
        Filter::Compare { field, op, value } if *op == CompareOp::Eq => {
            // Only use bloom for PK/ID fields
            let field_name = match field {
                FieldRef::TableColumn { column, .. } => column.as_str(),
                _ => return None,
            };
            if !matches!(field_name, "_entity_id" | "row_id" | "id" | "key") {
                return None;
            }
            let key = match value {
                Value::Text(s) => s.as_bytes().to_vec(),
                Value::Integer(n) => n.to_le_bytes().to_vec(),
                Value::UnsignedInteger(n) => n.to_le_bytes().to_vec(),
                _ => return None,
            };
            Some(key)
        }
        Filter::And(left, right) => {
            extract_bloom_key_for_pk(left).or_else(|| extract_bloom_key_for_pk(right))
        }
        _ => None,
    }
}

/// Extract a (column_name, value_bytes) from a simple equality filter for index lookup.
fn extract_index_candidate(
    filter: &crate::storage::query::ast::Filter,
) -> Option<(String, Vec<u8>)> {
    use crate::storage::query::ast::{CompareOp, FieldRef, Filter};
    match filter {
        Filter::Compare { field, op, value } if *op == CompareOp::Eq => {
            let column = match field {
                FieldRef::TableColumn { column, .. } => column.clone(),
                _ => return None,
            };
            let bytes = match value {
                Value::Text(s) => s.as_bytes().to_vec(),
                Value::Integer(n) => n.to_le_bytes().to_vec(),
                Value::UnsignedInteger(n) => n.to_le_bytes().to_vec(),
                _ => return None,
            };
            Some((column, bytes))
        }
        Filter::And(left, right) => {
            extract_index_candidate(left).or_else(|| extract_index_candidate(right))
        }
        _ => None,
    }
}

/// Extract simple column names from SELECT projections for projection pushdown.
/// Returns empty Vec for SELECT * or when projections contain expressions/functions.
fn extract_select_column_names(projections: &[Projection]) -> Vec<String> {
    if projections.is_empty() || projections.iter().any(|p| matches!(p, Projection::All)) {
        return Vec::new();
    }
    projections
        .iter()
        .filter_map(|p| match p {
            Projection::Column(c) | Projection::Alias(c, _) => Some(c.clone()),
            Projection::Field(FieldRef::TableColumn { column: c, .. }, _) => Some(c.clone()),
            _ => None,
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Entity-level filter evaluation
// ─────────────────────────────────────────────────────────────────────────────
// These functions evaluate SQL WHERE clauses directly against raw UnifiedEntity
// data, avoiding the expensive intermediate step of creating a UnifiedRecord
// (which allocates a HashMap and copies ~10 system fields + all user fields).
//
// For a 5000-row table with a filter matching ~100 rows, this avoids creating
// ~4900 throwaway UnifiedRecords.
// ─────────────────────────────────────────────────────────────────────────────

/// Resolve a field reference directly from an entity, without creating a UnifiedRecord.
/// Returns a borrowed Value when possible, or an owned Value for computed fields.
fn resolve_entity_field<'a>(
    entity: &'a crate::storage::unified::entity::UnifiedEntity,
    field: &FieldRef,
    table_name: &str,
    table_alias: &str,
) -> Option<std::borrow::Cow<'a, Value>> {
    use std::borrow::Cow;

    let column = match field {
        FieldRef::TableColumn { table, column } => {
            // If table qualifier is present, verify it matches
            if !table.is_empty()
                && !runtime_table_context_matches(
                    table.as_str(),
                    Some(table_name),
                    Some(table_alias),
                )
            {
                return None;
            }
            column.as_str()
        }
        _ => return None,
    };

    // System fields — accessed directly from entity struct fields
    match column {
        "_entity_id" | "entity_id" => {
            return Some(Cow::Owned(Value::UnsignedInteger(entity.id.raw())));
        }
        "_created_at" => {
            return Some(Cow::Owned(Value::UnsignedInteger(entity.created_at)));
        }
        "_updated_at" => {
            return Some(Cow::Owned(Value::UnsignedInteger(entity.updated_at)));
        }
        "_sequence_id" => {
            return Some(Cow::Owned(Value::UnsignedInteger(entity.sequence_id)));
        }
        "_collection" => {
            return Some(Cow::Owned(Value::Text(
                entity.kind.collection().to_string(),
            )));
        }
        "_kind" => {
            return Some(Cow::Owned(Value::Text(
                entity.kind.storage_type().to_string(),
            )));
        }
        "row_id" => {
            if let crate::storage::unified::entity::EntityKind::TableRow { row_id, .. } =
                &entity.kind
            {
                return Some(Cow::Owned(Value::UnsignedInteger(*row_id)));
            }
            return None;
        }
        _ => {}
    }

    // User fields — row data (named HashMap or columnar schema)
    if let Some(row) = entity.data.as_row() {
        if let Some(value) = row.get_field(column) {
            return Some(Cow::Borrowed(value));
        }
        // Positional column fallback (c0, c1, ...)
        if column.starts_with('c') {
            if let Ok(index) = column[1..].parse::<usize>() {
                if let Some(value) = row.columns.get(index) {
                    return Some(Cow::Borrowed(value));
                }
            }
        }
    }

    // Node properties
    if let EntityData::Node(ref node) = entity.data {
        if let Some(value) = node.properties.get(column) {
            return Some(Cow::Borrowed(value));
        }
    }

    // Edge properties
    if let EntityData::Edge(ref edge) = entity.data {
        if column == "weight" {
            return Some(Cow::Owned(Value::Float(edge.weight as f64)));
        }
        if let Some(value) = edge.properties.get(column) {
            return Some(Cow::Borrowed(value));
        }
    }

    // EntityKind fields (label, node_type, from_node, to_node)
    match &entity.kind {
        EntityKind::GraphNode(ref gn) => match column {
            "label" => return Some(Cow::Owned(Value::Text(gn.label.to_string()))),
            "node_type" => return Some(Cow::Owned(Value::Text(gn.node_type.to_string()))),
            _ => {}
        },
        EntityKind::GraphEdge(ref ge) => match column {
            "label" => return Some(Cow::Owned(Value::Text(ge.label.to_string()))),
            "from_node" => return Some(Cow::Owned(Value::Text(ge.from_node.to_string()))),
            "to_node" => return Some(Cow::Owned(Value::Text(ge.to_node.to_string()))),
            _ => {}
        },
        _ => {}
    }

    None
}

/// Evaluate a SQL Filter directly against a UnifiedEntity without creating a
/// UnifiedRecord. This is the main performance optimization for filtered scans.
pub(crate) fn evaluate_entity_filter(
    entity: &crate::storage::unified::entity::UnifiedEntity,
    filter: &Filter,
    table_name: &str,
    table_alias: &str,
) -> bool {
    match filter {
        Filter::Compare { field, op, value } => {
            resolve_entity_field(entity, field, table_name, table_alias)
                .as_ref()
                .map(|candidate| compare_runtime_values(candidate.as_ref(), value, *op))
                .unwrap_or(false)
        }
        Filter::And(left, right) => {
            evaluate_entity_filter(entity, left, table_name, table_alias)
                && evaluate_entity_filter(entity, right, table_name, table_alias)
        }
        Filter::Or(left, right) => {
            evaluate_entity_filter(entity, left, table_name, table_alias)
                || evaluate_entity_filter(entity, right, table_name, table_alias)
        }
        Filter::Not(inner) => !evaluate_entity_filter(entity, inner, table_name, table_alias),
        Filter::IsNull(field) => resolve_entity_field(entity, field, table_name, table_alias)
            .map(|value| value.as_ref() == &Value::Null)
            .unwrap_or(true),
        Filter::IsNotNull(field) => resolve_entity_field(entity, field, table_name, table_alias)
            .map(|value| value.as_ref() != &Value::Null)
            .unwrap_or(false),
        Filter::In { field, values } => {
            resolve_entity_field(entity, field, table_name, table_alias)
                .as_ref()
                .is_some_and(|candidate| {
                    values.iter().any(|value| {
                        compare_runtime_values(candidate.as_ref(), value, CompareOp::Eq)
                    })
                })
        }
        Filter::Between { field, low, high } => {
            resolve_entity_field(entity, field, table_name, table_alias)
                .as_ref()
                .is_some_and(|candidate| {
                    compare_runtime_values(candidate.as_ref(), low, CompareOp::Ge)
                        && compare_runtime_values(candidate.as_ref(), high, CompareOp::Le)
                })
        }
        Filter::Like { field, pattern } => {
            resolve_entity_field(entity, field, table_name, table_alias)
                .as_ref()
                .and_then(|v| runtime_value_text(v.as_ref()))
                .is_some_and(|value| like_matches(&value, pattern))
        }
        Filter::StartsWith { field, prefix } => {
            resolve_entity_field(entity, field, table_name, table_alias)
                .as_ref()
                .and_then(|v| runtime_value_text(v.as_ref()))
                .is_some_and(|value| value.starts_with(prefix))
        }
        Filter::EndsWith { field, suffix } => {
            resolve_entity_field(entity, field, table_name, table_alias)
                .as_ref()
                .and_then(|v| runtime_value_text(v.as_ref()))
                .is_some_and(|value| value.ends_with(suffix))
        }
        Filter::Contains { field, substring } => {
            resolve_entity_field(entity, field, table_name, table_alias)
                .as_ref()
                .and_then(|v| runtime_value_text(v.as_ref()))
                .is_some_and(|value| value.contains(substring))
        }
    }
}

/// Check if any projection is an aggregate function.
fn has_aggregate_projections(projections: &[Projection]) -> bool {
    projections.iter().any(|p| {
        matches!(
            p,
            Projection::Function(name, _)
                if matches!(
                    name.as_str(),
                    "COUNT" | "AVG" | "SUM" | "MIN" | "MAX"
                    | "STDDEV" | "VARIANCE" | "MEDIAN" | "PERCENTILE"
                    | "GROUP_CONCAT" | "FIRST" | "LAST" | "ARRAY_AGG"
                    | "COUNT_DISTINCT"
                )
        )
    })
}

/// Execute a query with aggregate functions (COUNT, AVG, SUM, MIN, MAX, GROUP BY).
fn execute_aggregate_query(db: &RedDB, query: &TableQuery) -> RedDBResult<UnifiedResult> {
    let manager = db
        .store()
        .get_collection(query.table.as_str())
        .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;

    let filter = query.filter.as_ref();
    let table_name = query.table.as_str();
    let table_alias = query.alias.as_deref().unwrap_or(table_name);
    let has_group_by = !query.group_by.is_empty();

    // Accumulators per group (empty string key = no grouping)
    let mut groups: std::collections::HashMap<String, AggState> = std::collections::HashMap::new();

    manager.for_each_entity(|entity| {
        if !entity.data.is_row() {
            return true;
        }
        if let Some(f) = filter {
            if !evaluate_entity_filter(entity, f, table_name, table_alias) {
                return true;
            }
        }

        let row = match entity.data.as_row() {
            Some(r) => r,
            None => return true,
        };

        // Determine group key
        let group_key = if has_group_by {
            query
                .group_by
                .iter()
                .filter_map(|col| row.get_field(col))
                .map(|v| format!("{v:?}"))
                .collect::<Vec<_>>()
                .join("|")
        } else {
            String::new()
        };

        let state = groups.entry(group_key).or_default();
        state.count += 1;

        // Accumulate values for each aggregate projection
        for proj in &query.columns {
            if let Projection::Function(func, args) = proj {
                let col_name = match args.first() {
                    Some(Projection::Column(c)) => c.as_str(),
                    Some(Projection::All) => "_count",
                    _ => continue,
                };
                if col_name == "_count" {
                    continue;
                } // COUNT(*) just needs count

                let val = match row.get_field(col_name) {
                    Some(v) => v,
                    None => continue,
                };
                let num = value_to_f64(val);

                match func.as_str() {
                    "SUM" | "AVG" => {
                        if let Some(n) = num {
                            *state.sums.entry(col_name.to_string()).or_insert(0.0) += n;
                        }
                    }
                    "MIN" => {
                        if let Some(n) = num {
                            let entry = state.mins.entry(col_name.to_string()).or_insert(f64::MAX);
                            if n < *entry {
                                *entry = n;
                            }
                        }
                    }
                    "MAX" => {
                        if let Some(n) = num {
                            let entry = state.maxs.entry(col_name.to_string()).or_insert(f64::MIN);
                            if n > *entry {
                                *entry = n;
                            }
                        }
                    }
                    "STDDEV" | "VARIANCE" => {
                        if let Some(n) = num {
                            *state.sums.entry(col_name.to_string()).or_insert(0.0) += n;
                            *state.sum_squares.entry(col_name.to_string()).or_insert(0.0) += n * n;
                            *state.agg_counts.entry(col_name.to_string()).or_insert(0) += 1;
                        }
                    }
                    "MEDIAN" | "PERCENTILE" => {
                        if let Some(n) = num {
                            state
                                .all_values
                                .entry(col_name.to_string())
                                .or_default()
                                .push(n);
                        }
                    }
                    "GROUP_CONCAT" => {
                        let text = match val {
                            Value::Text(s) => s.clone(),
                            other => format!("{:?}", other),
                        };
                        state
                            .concat_values
                            .entry(col_name.to_string())
                            .or_default()
                            .push(text);
                    }
                    "FIRST" => {
                        state
                            .first_values
                            .entry(col_name.to_string())
                            .or_insert_with(|| val.clone());
                    }
                    "LAST" => {
                        state.last_values.insert(col_name.to_string(), val.clone());
                    }
                    "ARRAY_AGG" => {
                        state
                            .array_values
                            .entry(col_name.to_string())
                            .or_default()
                            .push(val.clone());
                    }
                    "COUNT_DISTINCT" => {
                        let key = format!("{:?}", val);
                        state
                            .distinct_sets
                            .entry(col_name.to_string())
                            .or_default()
                            .insert(key);
                    }
                    _ => {}
                }
            }
        }
        true
    });

    // Build result records from accumulated groups
    let mut records = Vec::with_capacity(groups.len().max(1));
    let mut columns = Vec::new();

    for (group_key, state) in &groups {
        let mut record = UnifiedRecord::new();

        // Add GROUP BY columns
        if has_group_by {
            let parts: Vec<&str> = group_key.split('|').collect();
            for (i, col) in query.group_by.iter().enumerate() {
                if columns.is_empty() || !columns.contains(col) {
                    columns.push(col.clone());
                }
                let val_str = parts.get(i).unwrap_or(&"").trim_matches('"');
                // Try to parse back — simplified
                let val_str = val_str
                    .strip_prefix("Text(\"")
                    .and_then(|s| s.strip_suffix("\")"))
                    .unwrap_or(val_str);
                let val_str = val_str
                    .strip_prefix("Integer(")
                    .and_then(|s| s.strip_suffix(")"))
                    .unwrap_or(val_str);
                record.set(col, Value::Text(val_str.to_string()));
            }
        }

        // Add aggregate results
        for proj in &query.columns {
            if let Projection::Function(func, args) = proj {
                let col_name = match args.first() {
                    Some(Projection::Column(c)) => c.as_str(),
                    Some(Projection::All) => "*",
                    _ => continue,
                };
                let result_name = if col_name == "*" {
                    format!("{}(*)", func.to_lowercase())
                } else {
                    format!("{}({})", func.to_lowercase(), col_name)
                };

                if !columns.contains(&result_name) {
                    columns.push(result_name.clone());
                }

                let result_val = match func.as_str() {
                    "COUNT" => Value::Integer(state.count as i64),
                    "SUM" => {
                        let s = state.sums.get(col_name).copied().unwrap_or(0.0);
                        Value::Float(s)
                    }
                    "AVG" => {
                        let s = state.sums.get(col_name).copied().unwrap_or(0.0);
                        Value::Float(if state.count > 0 {
                            s / state.count as f64
                        } else {
                            0.0
                        })
                    }
                    "MIN" => {
                        let m = state.mins.get(col_name).copied().unwrap_or(0.0);
                        Value::Float(m)
                    }
                    "MAX" => {
                        let m = state.maxs.get(col_name).copied().unwrap_or(0.0);
                        Value::Float(m)
                    }
                    "VARIANCE" => {
                        let n = state.agg_counts.get(col_name).copied().unwrap_or(0) as f64;
                        if n > 0.0 {
                            let sum = state.sums.get(col_name).copied().unwrap_or(0.0);
                            let sum_sq = state.sum_squares.get(col_name).copied().unwrap_or(0.0);
                            Value::Float(sum_sq / n - (sum / n).powi(2))
                        } else {
                            Value::Float(0.0)
                        }
                    }
                    "STDDEV" => {
                        let n = state.agg_counts.get(col_name).copied().unwrap_or(0) as f64;
                        if n > 0.0 {
                            let sum = state.sums.get(col_name).copied().unwrap_or(0.0);
                            let sum_sq = state.sum_squares.get(col_name).copied().unwrap_or(0.0);
                            let variance = sum_sq / n - (sum / n).powi(2);
                            Value::Float(variance.max(0.0).sqrt())
                        } else {
                            Value::Float(0.0)
                        }
                    }
                    "MEDIAN" => {
                        let mut vals = state.all_values.get(col_name).cloned().unwrap_or_default();
                        if vals.is_empty() {
                            Value::Float(0.0)
                        } else {
                            vals.sort_by(|a, b| {
                                a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                            });
                            let mid = vals.len() / 2;
                            if vals.len() % 2 == 0 {
                                Value::Float((vals[mid - 1] + vals[mid]) / 2.0)
                            } else {
                                Value::Float(vals[mid])
                            }
                        }
                    }
                    "PERCENTILE" => {
                        let pct = args
                            .get(1)
                            .and_then(|a| match a {
                                Projection::Column(c) => {
                                    c.strip_prefix("LIT:").and_then(|s| s.parse::<f64>().ok())
                                }
                                _ => None,
                            })
                            .unwrap_or(0.5);
                        let mut vals = state.all_values.get(col_name).cloned().unwrap_or_default();
                        if vals.is_empty() {
                            Value::Float(0.0)
                        } else {
                            vals.sort_by(|a, b| {
                                a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                            });
                            let idx = ((pct * (vals.len() as f64 - 1.0)).round() as usize)
                                .min(vals.len() - 1);
                            Value::Float(vals[idx])
                        }
                    }
                    "GROUP_CONCAT" => {
                        let vals = state
                            .concat_values
                            .get(col_name)
                            .cloned()
                            .unwrap_or_default();
                        Value::Text(vals.join(", "))
                    }
                    "FIRST" => state
                        .first_values
                        .get(col_name)
                        .cloned()
                        .unwrap_or(Value::Null),
                    "LAST" => state
                        .last_values
                        .get(col_name)
                        .cloned()
                        .unwrap_or(Value::Null),
                    "ARRAY_AGG" => {
                        let vals = state
                            .array_values
                            .get(col_name)
                            .cloned()
                            .unwrap_or_default();
                        Value::Array(vals)
                    }
                    "COUNT_DISTINCT" => {
                        let set = state
                            .distinct_sets
                            .get(col_name)
                            .map(|s| s.len())
                            .unwrap_or(0);
                        Value::Integer(set as i64)
                    }
                    _ => Value::Null,
                };
                record.set(&result_name, result_val);
            }
        }

        records.push(record);
    }

    // If no groups matched, return a single row with zeros
    if records.is_empty() && !has_group_by {
        let mut record = UnifiedRecord::new();
        for proj in &query.columns {
            if let Projection::Function(func, args) = proj {
                let col_name = match args.first() {
                    Some(Projection::Column(c)) => c.as_str(),
                    Some(Projection::All) => "*",
                    _ => continue,
                };
                let name = if col_name == "*" {
                    format!("{}(*)", func.to_lowercase())
                } else {
                    format!("{}({})", func.to_lowercase(), col_name)
                };
                if !columns.contains(&name) {
                    columns.push(name.clone());
                }
                record.set(
                    &name,
                    match func.as_str() {
                        "COUNT" => Value::Integer(0),
                        _ => Value::Float(0.0),
                    },
                );
            }
        }
        records.push(record);
    }

    Ok(UnifiedResult {
        columns,
        records,
        stats: Default::default(),
        pre_serialized_json: None,
    })
}

#[derive(Default)]
struct AggState {
    count: u64,
    sums: std::collections::HashMap<String, f64>,
    mins: std::collections::HashMap<String, f64>,
    maxs: std::collections::HashMap<String, f64>,
    // For STDDEV/VARIANCE: collect sum of squares
    sum_squares: std::collections::HashMap<String, f64>,
    agg_counts: std::collections::HashMap<String, u64>,
    // For MEDIAN/PERCENTILE: collect all values
    all_values: std::collections::HashMap<String, Vec<f64>>,
    // For GROUP_CONCAT: collect strings
    concat_values: std::collections::HashMap<String, Vec<String>>,
    // For FIRST/LAST: track first and last seen values
    first_values: std::collections::HashMap<String, Value>,
    last_values: std::collections::HashMap<String, Value>,
    // For ARRAY_AGG: collect all values
    array_values: std::collections::HashMap<String, Vec<Value>>,
    // For COUNT(DISTINCT): collect unique values
    distinct_sets: std::collections::HashMap<String, std::collections::HashSet<String>>,
}

fn value_to_f64(val: &Value) -> Option<f64> {
    match val {
        Value::Integer(n) => Some(*n as f64),
        Value::UnsignedInteger(n) => Some(*n as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}
