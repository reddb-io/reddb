//! Payload estimates shared by segment accounting and mutation admission.
//! Shared values are charged per resident owner: an Arc count changing must
//! not change the amount released when a segment is reclaimed.

use super::entity::{EntityData, UnifiedEntity};
use reddb_types::Value;

pub(crate) fn canonical_heap_bytes(key: &reddb_types::CanonicalKey) -> usize {
    use reddb_types::CanonicalKey;
    match key {
        CanonicalKey::Text(_, value) => value.len(),
        CanonicalKey::Bytes(_, value) => value.len(),
        CanonicalKey::PairTextU64(_, value, _) => value.len(),
        CanonicalKey::PairTextText(_, first, second) => first.len().saturating_add(second.len()),
        _ => 0,
    }
}

/// Two bounds and their canonical keys may retain the replacement payload.
pub(crate) fn zone_growth_bytes(value: &Value) -> usize {
    value_heap_bytes(value).saturating_mul(4)
}

pub(crate) fn entity_zone_growth_bytes(entity: &UnifiedEntity) -> usize {
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

pub(crate) fn value_heap_bytes(value: &Value) -> usize {
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
                bytes = bytes.saturating_add(std::mem::size_of_val(values.as_slice()));
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
        Value::Text(value) => value.len(),
        Value::Blob(value) | Value::Json(value) | Value::Secret(value) => value.len(),
        Value::Vector(value) => std::mem::size_of_val(value.as_slice()),
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
        | Value::AssetCode(value) => value.len(),
        Value::KeyRef(collection, key) => collection.len().saturating_add(key.len()),
        Value::Money { asset_code, .. } => asset_code.len(),
        _ => 0,
    }
}

fn named_fields_bytes(fields: &std::collections::HashMap<String, Value>) -> usize {
    fields.iter().fold(0usize, |bytes, (name, value)| {
        bytes
            .saturating_add(64)
            .saturating_add(name.len())
            .saturating_add(value_heap_bytes(value))
    })
}

pub(crate) fn entity_bytes(entity: &UnifiedEntity) -> usize {
    let data = match &entity.data {
        EntityData::Row(row) => {
            let columns = row.columns.iter().fold(0usize, |bytes, value| {
                bytes
                    .saturating_add(64)
                    .saturating_add(value_heap_bytes(value))
            });
            columns.saturating_add(row.named.as_ref().map_or(0, named_fields_bytes))
        }
        EntityData::Node(node) => named_fields_bytes(&node.properties),
        EntityData::Edge(edge) => named_fields_bytes(&edge.properties),
        EntityData::Vector(vector) => std::mem::size_of_val(vector.dense.as_slice())
            .saturating_add(vector.sparse.as_ref().map_or(0, |sparse| {
                std::mem::size_of_val(sparse.indices.as_slice())
                    .saturating_add(std::mem::size_of_val(sparse.values.as_slice()))
            })),
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
        let value = Value::Array(vec![shared.clone(), Value::Array(vec![shared].into())].into());
        assert_eq!(
            value_heap_bytes(&value),
            3 * std::mem::size_of::<Value>() + 2 * "shared payload".len()
        );
        let another_owner = value.clone();
        assert_eq!(value_heap_bytes(&value), value_heap_bytes(&another_owner));
    }

    #[test]
    fn excessively_nested_api_values_fail_closed() {
        let mut value = Value::Null;
        for _ in 0..reddb_rql::limits::JSON_LITERAL_MAX_DEPTH + 2 {
            value = Value::Array(vec![value].into());
        }
        assert_eq!(value_heap_bytes(&value), usize::MAX);
        assert_eq!(zone_growth_bytes(&value), usize::MAX);
    }
}
