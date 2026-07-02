//! Runtime join strategies (cross/nested-loop/hash/graph/indexed/full-scan).
//!
//! Extracted from `join_filter.rs` as part of the join_filter
//! directory refactor (parent re-exports the whole module).
use super::*;

/// Emit the Cartesian product of two record sets as a flat Vec of
/// merged records. Shared by every runtime join loop — CROSS JOIN
/// has no predicate, so the loop contents are identical regardless
/// of which dispatcher was chosen (nested / hash / graph / indexed).
fn cross_join_records(
    left_records: &[UnifiedRecord],
    right_records: &[UnifiedRecord],
    left_query: &TableQuery,
    right_alias_or_name: Option<&str>,
) -> Vec<UnifiedRecord> {
    let mut records = Vec::with_capacity(left_records.len() * right_records.len());
    for left_record in left_records {
        for right_record in right_records {
            records.push(merge_join_records(
                Some(left_record),
                Some(right_record),
                left_query,
                right_alias_or_name,
            ));
        }
    }
    records
}

pub(crate) fn execute_runtime_nested_loop_join(
    left_query: &TableQuery,
    left_records: &[UnifiedRecord],
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    left_join_field: &FieldRef,
    right_records: &[UnifiedRecord],
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
    right_join_field: &FieldRef,
    join_type: JoinType,
) -> RedDBResult<Vec<UnifiedRecord>> {
    if matches!(join_type, JoinType::Cross) {
        return Ok(cross_join_records(
            left_records,
            right_records,
            left_query,
            right_table_alias.or(right_table_name),
        ));
    }

    let mut matched_right = vec![false; right_records.len()];
    let mut records = Vec::new();

    for left_record in left_records {
        let mut matched = false;
        for (index, right_record) in right_records.iter().enumerate() {
            if join_condition_matches(
                left_record,
                left_table_name,
                left_table_alias,
                left_join_field,
                right_record,
                right_table_name,
                right_table_alias,
                right_join_field,
            ) {
                matched = true;
                matched_right[index] = true;
                records.push(merge_join_records(
                    Some(left_record),
                    Some(right_record),
                    left_query,
                    right_table_alias.or(right_table_name),
                ));
            }
        }

        if !matched && matches!(join_type, JoinType::LeftOuter | JoinType::FullOuter) {
            records.push(merge_join_records(
                Some(left_record),
                None,
                left_query,
                None,
            ));
        }
    }

    if matches!(join_type, JoinType::RightOuter | JoinType::FullOuter) {
        for (matched, right_record) in matched_right.into_iter().zip(right_records.iter()) {
            if !matched {
                records.push(merge_join_records(
                    None,
                    Some(right_record),
                    left_query,
                    right_table_alias.or(right_table_name),
                ));
            }
        }
    }

    Ok(records)
}

/// Hash join — O(n+m) instead of O(n*m) for large record sets.
/// Builds a hash table on the right side, probes with the left side.
pub(crate) fn execute_runtime_hash_join(
    left_query: &TableQuery,
    left_records: &[UnifiedRecord],
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    left_join_field: &FieldRef,
    right_records: &[UnifiedRecord],
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
    right_join_field: &FieldRef,
    join_type: JoinType,
) -> RedDBResult<Vec<UnifiedRecord>> {
    if matches!(join_type, JoinType::Cross) {
        return Ok(cross_join_records(
            left_records,
            right_records,
            left_query,
            right_table_alias.or(right_table_name),
        ));
    }
    // Build hash table on right side. The build-side cardinality is the
    // right record count, so pre-size to avoid incremental rehashing.
    // Keys stay `String` (raw `Value::to_string()`) here to preserve this
    // path's distinct null/empty-bucket semantics asserted by the #1339
    // baseline tests; the prefix-namespaced typed keys live on the indexed
    // and graph-lookup paths.
    let mut hash_table: HashMap<String, Vec<usize>> = HashMap::with_capacity(right_records.len());
    for (idx, right_record) in right_records.iter().enumerate() {
        let key = resolve_runtime_field(
            right_record,
            right_join_field,
            right_table_name,
            right_table_alias,
        )
        .map(|v| v.to_string())
        .unwrap_or_default();
        hash_table.entry(key).or_default().push(idx);
    }

    let mut matched_right = vec![false; right_records.len()];
    let mut records = Vec::new();

    // Probe with left side — O(1) lookup per left record
    for left_record in left_records {
        let key = resolve_runtime_field(
            left_record,
            left_join_field,
            left_table_name,
            left_table_alias,
        )
        .map(|v| v.to_string())
        .unwrap_or_default();

        let mut matched = false;
        if let Some(indices) = hash_table.get(&key) {
            for &idx in indices {
                matched = true;
                matched_right[idx] = true;
                records.push(merge_join_records(
                    Some(left_record),
                    Some(&right_records[idx]),
                    left_query,
                    right_table_alias.or(right_table_name),
                ));
            }
        }

        if !matched && matches!(join_type, JoinType::LeftOuter | JoinType::FullOuter) {
            records.push(merge_join_records(
                Some(left_record),
                None,
                left_query,
                None,
            ));
        }
    }

    if matches!(join_type, JoinType::RightOuter | JoinType::FullOuter) {
        for (matched, right_record) in matched_right.into_iter().zip(right_records.iter()) {
            if !matched {
                records.push(merge_join_records(
                    None,
                    Some(right_record),
                    left_query,
                    right_table_alias.or(right_table_name),
                ));
            }
        }
    }

    Ok(records)
}

pub(crate) fn execute_runtime_graph_lookup_join(
    left_query: &TableQuery,
    left_records: &[UnifiedRecord],
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    left_join_field: &FieldRef,
    right_records: &[UnifiedRecord],
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
    right_join_field: &FieldRef,
    join_type: JoinType,
) -> RedDBResult<Vec<UnifiedRecord>> {
    if matches!(join_type, JoinType::Cross) {
        return Ok(cross_join_records(
            left_records,
            right_records,
            left_query,
            right_table_alias.or(right_table_name),
        ));
    }
    let mut right_index: HashMap<RuntimeJoinKey, Vec<usize>> =
        HashMap::with_capacity(right_records.len());
    for (index, right_record) in right_records.iter().enumerate() {
        let keys = runtime_graph_join_record_keys(
            right_record,
            right_join_field,
            right_table_name,
            right_table_alias,
        );
        for key in keys {
            right_index.entry(key).or_default().push(index);
        }
    }

    let mut matched_right = vec![false; right_records.len()];
    let mut records = Vec::new();

    for left_record in left_records {
        let candidate_indexes = runtime_graph_join_probe_indexes(
            left_record,
            left_join_field,
            left_table_name,
            left_table_alias,
            &right_index,
        );
        let mut matched = false;

        for index in candidate_indexes {
            let right_record = &right_records[index];
            if join_condition_matches(
                left_record,
                left_table_name,
                left_table_alias,
                left_join_field,
                right_record,
                right_table_name,
                right_table_alias,
                right_join_field,
            ) {
                matched = true;
                matched_right[index] = true;
                records.push(merge_join_records(
                    Some(left_record),
                    Some(right_record),
                    left_query,
                    right_table_alias.or(right_table_name),
                ));
            }
        }

        if !matched && matches!(join_type, JoinType::LeftOuter | JoinType::FullOuter) {
            records.push(merge_join_records(
                Some(left_record),
                None,
                left_query,
                None,
            ));
        }
    }

    if matches!(join_type, JoinType::RightOuter | JoinType::FullOuter) {
        for (matched, right_record) in matched_right.into_iter().zip(right_records.iter()) {
            if !matched {
                records.push(merge_join_records(
                    None,
                    Some(right_record),
                    left_query,
                    right_table_alias.or(right_table_name),
                ));
            }
        }
    }

    Ok(records)
}

pub(crate) fn execute_runtime_indexed_join(
    left_query: &TableQuery,
    left_records: &[UnifiedRecord],
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    left_join_field: &FieldRef,
    right_records: &[UnifiedRecord],
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
    right_join_field: &FieldRef,
    join_type: JoinType,
) -> RedDBResult<Vec<UnifiedRecord>> {
    if matches!(join_type, JoinType::Cross) {
        return Ok(cross_join_records(
            left_records,
            right_records,
            left_query,
            right_table_alias.or(right_table_name),
        ));
    }
    let mut right_index: HashMap<RuntimeJoinKey, Vec<usize>> =
        HashMap::with_capacity(right_records.len());
    for (index, right_record) in right_records.iter().enumerate() {
        let Some(value) = resolve_runtime_field(
            right_record,
            right_join_field,
            right_table_name,
            right_table_alias,
        ) else {
            continue;
        };
        let Some(key) = runtime_join_lookup_key(&value) else {
            continue;
        };
        right_index.entry(key).or_default().push(index);
    }

    let mut matched_right = vec![false; right_records.len()];
    let mut records = Vec::new();

    for left_record in left_records {
        let left_value = resolve_runtime_field(
            left_record,
            left_join_field,
            left_table_name,
            left_table_alias,
        );
        // Borrow the candidate list out of `right_index` instead of cloning
        // it: the probe loop never mutates `right_index`, so the indexed
        // hash bucket can be iterated in place with zero temporary allocation.
        let lookup_key = left_value.as_ref().and_then(runtime_join_lookup_key);
        let candidate_indexes: &[usize] = lookup_key
            .as_ref()
            .and_then(|key| right_index.get(key))
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut matched = false;

        for &index in candidate_indexes {
            let right_record = &right_records[index];
            if join_condition_matches(
                left_record,
                left_table_name,
                left_table_alias,
                left_join_field,
                right_record,
                right_table_name,
                right_table_alias,
                right_join_field,
            ) {
                matched = true;
                matched_right[index] = true;
                records.push(merge_join_records(
                    Some(left_record),
                    Some(right_record),
                    left_query,
                    right_table_alias.or(right_table_name),
                ));
            }
        }

        if !matched && matches!(join_type, JoinType::LeftOuter | JoinType::FullOuter) {
            records.push(merge_join_records(
                Some(left_record),
                None,
                left_query,
                None,
            ));
        }
    }

    if matches!(join_type, JoinType::RightOuter | JoinType::FullOuter) {
        for (matched, right_record) in matched_right.into_iter().zip(right_records.iter()) {
            if !matched {
                records.push(merge_join_records(
                    None,
                    Some(right_record),
                    left_query,
                    right_table_alias.or(right_table_name),
                ));
            }
        }
    }

    Ok(records)
}

/// Typed internal join key. Replaces the formatted, prefix-namespaced
/// string keys (`"n:…"` / `"b:…"` / `"t:…"` / `"id:…"`) that the indexed
/// and graph-lookup join paths used to build and probe their hash indexes.
///
/// Each variant is its own namespace, so the old prefix-collision behaviour
/// is preserved without per-row string formatting: a numeric key never
/// collides with a textual one, and a value key never collides with an
/// identity key. Numeric keys hash on the `f64` bit pattern — distinct
/// finite/infinite floats have distinct bit patterns, so this reproduces
/// the old `format!("n:{number}")` equality for every value that can
/// actually appear as a join key.
///
/// `Text` and `Identity` carry user-controlled strings, so this type is
/// only ever indexed with the default `std` `HashMap` hasher (SipHash);
/// no weak hasher is applied to user-controlled keys.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub(crate) enum RuntimeJoinKey {
    /// Numeric key class — was `"n:{number}"`.
    Number(u64),
    /// Boolean key class — was `"b:{boolean}"`.
    Boolean(bool),
    /// Textual key class — was `"t:{text}"`.
    Text(String),
    /// Identity / reference key class — was `"id:{identity}"`.
    Identity(String),
}

pub(crate) fn runtime_join_lookup_key(value: &Value) -> Option<RuntimeJoinKey> {
    if let Some(number) = runtime_value_number(value) {
        return Some(RuntimeJoinKey::Number(number.to_bits()));
    }
    if let Value::Boolean(boolean) = value {
        return Some(RuntimeJoinKey::Boolean(*boolean));
    }
    runtime_value_text(value).map(RuntimeJoinKey::Text)
}

pub(crate) fn runtime_join_lookup_keys(value: &Value) -> Vec<RuntimeJoinKey> {
    let mut keys = Vec::new();
    if let Some(key) = runtime_join_lookup_key(value) {
        keys.push(key);
    }
    if let Some(identity) = runtime_join_identity_key(value) {
        keys.push(RuntimeJoinKey::Identity(identity));
    }
    keys.sort();
    keys.dedup();
    keys
}

pub(crate) fn runtime_join_identity_key(value: &Value) -> Option<String> {
    if let Some(number) = runtime_value_number(value) {
        return Some(number.to_string());
    }
    let text = runtime_value_text(value)?;
    if let Some((_, suffix)) = text.rsplit_once(':') {
        return Some(suffix.to_string());
    }
    Some(text)
}

pub(crate) fn runtime_graph_join_record_keys(
    record: &UnifiedRecord,
    right_join_field: &FieldRef,
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
) -> Vec<RuntimeJoinKey> {
    let mut keys = Vec::new();

    if let Some(value) = resolve_runtime_field(
        record,
        right_join_field,
        right_table_name,
        right_table_alias,
    ) {
        keys.extend(runtime_join_lookup_keys(&value));
    }

    for hint in ["_source_node", "_source_edge", "_linked_identity"] {
        if let Some(value) = record.get(hint) {
            keys.extend(runtime_join_lookup_keys(value));
        }
    }

    for node in record.nodes.values() {
        keys.extend(runtime_join_lookup_keys(&Value::NodeRef(node.id.clone())));
    }

    for edge in record.edges.values() {
        keys.extend(runtime_join_lookup_keys(&Value::NodeRef(edge.from.clone())));
        keys.extend(runtime_join_lookup_keys(&Value::NodeRef(edge.to.clone())));
    }

    keys.sort();
    keys.dedup();
    keys
}

pub(crate) fn runtime_graph_join_probe_indexes(
    left_record: &UnifiedRecord,
    left_join_field: &FieldRef,
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    right_index: &HashMap<RuntimeJoinKey, Vec<usize>>,
) -> Vec<usize> {
    let mut candidates = BTreeSet::new();
    if let Some(value) = resolve_runtime_field(
        left_record,
        left_join_field,
        left_table_name,
        left_table_alias,
    ) {
        for key in runtime_join_lookup_keys(&value) {
            if let Some(indexes) = right_index.get(&key) {
                candidates.extend(indexes.iter().copied());
            }
        }
    }
    for hint in ["_source_node", "_source_edge", "_linked_identity"] {
        if let Some(value) = left_record.get(hint) {
            for key in runtime_join_lookup_keys(value) {
                if let Some(indexes) = right_index.get(&key) {
                    candidates.extend(indexes.iter().copied());
                }
            }
        }
    }
    candidates.into_iter().collect()
}

pub(crate) fn execute_runtime_full_scan_join(
    left_query: &TableQuery,
    left_records: &[UnifiedRecord],
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    left_join_field: &FieldRef,
    right_records: &[UnifiedRecord],
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
    right_join_field: &FieldRef,
    join_type: JoinType,
) -> RedDBResult<Vec<UnifiedRecord>> {
    execute_runtime_nested_loop_join(
        left_query,
        left_records,
        left_table_name,
        left_table_alias,
        left_join_field,
        right_records,
        right_table_name,
        right_table_alias,
        right_join_field,
        join_type,
    )
}
