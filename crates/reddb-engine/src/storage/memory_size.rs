//! Payload estimates shared by segment accounting and mutation admission.
//! Shared values are charged per resident owner: an Arc count changing must
//! not change the amount released when a segment is reclaimed.

use super::entity::{EntityData, UnifiedEntity};
use reddb_types::Value;

/// Hash tables reserve control bytes and empty buckets in addition to live slots.
/// The next bucket power covers the standard library's 7/8 load factor (and its
/// smaller initial tables). Capacity is observed while the owner is locked.
pub(crate) fn hash_table_bytes<K, V>(capacity: usize) -> usize {
    if capacity == 0 {
        return 0;
    }
    capacity
        .saturating_add(1)
        .checked_next_power_of_two()
        .unwrap_or(usize::MAX)
        .saturating_mul(std::mem::size_of::<(K, V)>().saturating_add(1))
        .saturating_add(32)
}

fn shared_text_bytes(length: usize) -> usize {
    let word = std::mem::size_of::<usize>();
    length.div_ceil(word).saturating_add(2).saturating_mul(word)
}

/// A root may remain allocated after the last removal. Other B-tree nodes are
/// at least half full; three slots per entry cover their unused slots, links
/// and headers. The root allowance covers the small, partly occupied root.
pub(crate) fn btree_map_bytes<K, V>(entries: usize, root_allocated: bool) -> usize {
    if !root_allocated {
        return 0;
    }
    let slot = std::mem::size_of::<(K, V)>();
    entries
        .saturating_mul(slot.saturating_mul(3).saturating_add(64))
        .saturating_add(slot.saturating_mul(12))
        .saturating_add(256)
}

/// Amortized hash bucket growth plus the initial four-ID version bucket.
/// Fingerprints retain no key payload; the complete key stays in the entity.
pub fn key_lookup_growth_bytes(value: Option<&Value>) -> usize {
    if matches!(value, Some(Value::Text(_))) {
        std::mem::size_of::<super::segment_key_lookup::SegmentKeyLookup>()
            + 4 * (std::mem::size_of::<(u64, Vec<super::entity::EntityId>)>() + 1)
            + 4 * std::mem::size_of::<super::entity::EntityId>()
    } else {
        0
    }
}

pub fn canonical_heap_bytes(key: &reddb_types::CanonicalKey) -> usize {
    use reddb_types::CanonicalKey;
    match key {
        CanonicalKey::Text(_, value) => shared_text_bytes(value.len()),
        CanonicalKey::Bytes(_, value) => value.capacity(),
        CanonicalKey::PairTextU64(_, value, _) => value.capacity(),
        CanonicalKey::PairTextText(_, first, second) => {
            first.capacity().saturating_add(second.capacity())
        }
        _ => 0,
    }
}

/// Two bounds and their canonical keys may retain the replacement payload.
pub fn zone_growth_bytes(value: &Value) -> usize {
    value_heap_bytes(value).saturating_mul(4)
}

pub fn entity_zone_growth_bytes(entity: &UnifiedEntity) -> usize {
    let EntityData::Row(row) = &entity.data else {
        return 0;
    };
    if let Some(named) = &row.named {
        named
            .values()
            .map(zone_growth_bytes)
            .fold(0, usize::saturating_add)
    } else {
        row.columns
            .iter()
            .map(zone_growth_bytes)
            .fold(0, usize::saturating_add)
    }
}

pub fn value_heap_bytes(value: &Value) -> usize {
    if !matches!(value, Value::Array(_)) {
        return scalar_heap_bytes(value);
    }
    array_heap_bytes(value)
}

#[inline(never)]
fn array_heap_bytes(value: &Value) -> usize {
    // One iterator per permitted parser level, never recursion or heap scratch.
    const LEVELS: usize = reddb_rql::limits::JSON_LITERAL_MAX_DEPTH as usize + 1;
    let mut stack: [std::slice::Iter<'_, Value>; LEVELS] = std::array::from_fn(|_| [].iter());
    let mut depth = 0;
    let mut current = Some(value);
    let mut bytes = 0usize;
    loop {
        if let Some(value) = current.take() {
            if let Value::Array(values) = value {
                if depth == LEVELS {
                    // Values supplied directly by an API can exceed parser
                    // limits. An unrepresentable estimate cannot be admitted.
                    return usize::MAX;
                }
                bytes = bytes.saturating_add(
                    values
                        .capacity()
                        .saturating_mul(std::mem::size_of::<Value>()),
                );
                stack[depth] = values.iter();
                depth += 1;
            } else {
                bytes = bytes.saturating_add(scalar_heap_bytes(value));
            }
        }
        if depth == 0 {
            return bytes;
        }
        current = stack[depth - 1].next();
        if current.is_none() {
            depth -= 1;
        }
    }
}

fn scalar_heap_bytes(value: &Value) -> usize {
    match value {
        Value::Text(value) => shared_text_bytes(value.len()),
        Value::Blob(value) | Value::Json(value) | Value::Secret(value) => value.capacity(),
        Value::Vector(value) => value.capacity().saturating_mul(std::mem::size_of::<f32>()),
        Value::NodeRef(value)
        | Value::EdgeRef(value)
        | Value::VectorRef(value, _)
        | Value::RowRef(value, _)
        | Value::DocRef(value, _)
        | Value::TableRef(value)
        | Value::Email(value)
        | Value::Url(value)
        | Value::Password(value)
        | Value::DecimalText(value)
        | Value::AssetCode(value) => value.capacity(),
        Value::KeyRef(collection, key) => collection.capacity().saturating_add(key.capacity()),
        Value::Money { asset_code, .. } => asset_code.capacity(),
        _ => 0,
    }
}

fn named_fields_bytes(fields: &std::collections::HashMap<String, Value>) -> usize {
    fields.iter().fold(
        hash_table_bytes::<String, Value>(fields.capacity()),
        |bytes, (name, value)| {
            bytes
                .saturating_add(name.capacity())
                .saturating_add(value_heap_bytes(value))
        },
    )
}

pub fn entity_bytes(entity: &UnifiedEntity) -> usize {
    let data = match &entity.data {
        EntityData::Row(row) => {
            let slots = row
                .columns
                .capacity()
                .saturating_mul(std::mem::size_of::<Value>());
            let columns = row.columns.iter().fold(slots, |bytes, value| {
                bytes.saturating_add(value_heap_bytes(value))
            });
            columns
                .saturating_add(row.named.as_ref().map_or(0, named_fields_bytes))
                .saturating_add(match &entity.kind {
                    super::entity::EntityKind::TableRow { table, .. } => {
                        shared_text_bytes(table.len())
                    }
                    _ => 0,
                })
        }
        EntityData::Node(node) => {
            named_fields_bytes(&node.properties).saturating_add(match &entity.kind {
                super::entity::EntityKind::GraphNode(kind) => kind.node_type.len(),
                _ => 0,
            })
        }
        EntityData::Edge(edge) => named_fields_bytes(&edge.properties),
        EntityData::Vector(vector) => std::mem::size_of_val(vector.dense.as_slice())
            .saturating_add(vector.sparse.as_ref().map_or(0, |sparse| {
                std::mem::size_of_val(sparse.indices.as_slice())
                    .saturating_add(std::mem::size_of_val(sparse.values.as_slice()))
            }))
            .saturating_add(vector.content.as_ref().map_or(0, String::len)),
        EntityData::TimeSeries(point) => 64usize
            .saturating_add(point.metric.len())
            .saturating_add(named_fields_bytes(&point.fields))
            .saturating_add(point.tags.iter().fold(0usize, |bytes, (name, value)| {
                bytes
                    .saturating_add(64)
                    .saturating_add(name.len())
                    .saturating_add(value.len())
            })),
        EntityData::QueueMessage(message) => {
            128usize.saturating_add(value_heap_bytes(&message.payload))
        }
    };
    entity.embeddings().iter().fold(
        std::mem::size_of::<UnifiedEntity>()
            .saturating_add(data)
            .saturating_add(std::mem::size_of_val(entity.cross_refs())),
        |bytes, embedding| {
            bytes
                .saturating_add(std::mem::size_of_val(embedding.vector.as_slice()))
                .saturating_add(embedding.name.len())
                .saturating_add(embedding.model.len())
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_arrays_charge_slots_and_variable_payloads_per_owner() {
        let shared = Value::text("shared payload");
        let value = Value::Array(vec![shared.clone(), Value::Array(vec![shared])]);
        assert_eq!(
            value_heap_bytes(&value),
            3 * std::mem::size_of::<Value>() + 2 * shared_text_bytes("shared payload".len())
        );
        let another_owner = value.clone();
        assert_eq!(value_heap_bytes(&value), value_heap_bytes(&another_owner));
    }

    #[test]
    fn excessively_nested_api_values_fail_closed() {
        let mut value = Value::Null;
        for _ in 0..reddb_rql::limits::JSON_LITERAL_MAX_DEPTH + 2 {
            value = Value::Array(vec![value]);
        }
        assert_eq!(value_heap_bytes(&value), usize::MAX);
        assert_eq!(zone_growth_bytes(&value), usize::MAX);
    }
}
