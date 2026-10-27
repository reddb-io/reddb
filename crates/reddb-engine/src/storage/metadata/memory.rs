//! Retained metadata ownership, sampled without walking all stored entities.
use super::{MetadataValue, RefTarget};

#[derive(Debug, Default)]
pub(super) struct MetadataMemory {
    pub owned_bytes: usize,
    pub key_set_bytes: usize,
    pub entity_capacity: usize,
    pub complex_capacity: usize,
    pub tree_roots: u8,
    unrepresentable: bool,
}

impl MetadataMemory {
    pub fn add_owned(&mut self, bytes: usize) {
        if let Some(total) = self.owned_bytes.checked_add(bytes) {
            self.owned_bytes = total;
        } else {
            self.unrepresentable = true;
        }
        self.unrepresentable |= bytes == usize::MAX;
    }

    pub fn release_owned(&mut self, bytes: usize) {
        // An API value beyond the bounded traversal depth keeps admission
        // conservatively closed until this owner is reclaimed.
        if !self.unrepresentable {
            self.owned_bytes = self
                .owned_bytes
                .checked_sub(bytes)
                .expect("metadata releases only its charged ownership");
        }
    }

    pub fn payload_bytes(&self) -> usize {
        if self.unrepresentable {
            usize::MAX
        } else {
            self.owned_bytes
        }
    }
}

enum Children<'a> {
    Array(std::slice::Iter<'a, MetadataValue>),
    Object(std::collections::hash_map::Values<'a, String, MetadataValue>),
}

impl<'a> Children<'a> {
    fn next(&mut self) -> Option<&'a MetadataValue> {
        match self {
            Self::Array(values) => values.next(),
            Self::Object(values) => values.next(),
        }
    }
}

pub(super) fn value_heap_bytes(value: &MetadataValue) -> usize {
    const LEVELS: usize = reddb_rql::limits::JSON_LITERAL_MAX_DEPTH as usize + 1;
    let mut stack: [Children<'_>; LEVELS] = std::array::from_fn(|_| Children::Array([].iter()));
    let mut depth = 0;
    let mut current = Some(value);
    let mut bytes = 0usize;
    loop {
        if let Some(value) = current.take() {
            let children = match value {
                MetadataValue::Array(values) => {
                    bytes = bytes.saturating_add(
                        values
                            .capacity()
                            .saturating_mul(std::mem::size_of::<MetadataValue>()),
                    );
                    Some(Children::Array(values.iter()))
                }
                MetadataValue::Object(values) => {
                    bytes = bytes.saturating_add(crate::storage::memory_size::hash_table_bytes::<
                        String,
                        MetadataValue,
                    >(values.capacity()));
                    bytes = values
                        .keys()
                        .fold(bytes, |total, key| total.saturating_add(key.capacity()));
                    Some(Children::Object(values.values()))
                }
                MetadataValue::String(value) => {
                    bytes = bytes.saturating_add(value.capacity());
                    None
                }
                MetadataValue::Bytes(value) => {
                    bytes = bytes.saturating_add(value.capacity());
                    None
                }
                MetadataValue::Reference(value) => {
                    bytes = bytes.saturating_add(reference_heap_bytes(value));
                    None
                }
                MetadataValue::References(values) => {
                    bytes = bytes.saturating_add(
                        values
                            .capacity()
                            .saturating_mul(std::mem::size_of::<RefTarget>()),
                    );
                    bytes = values.iter().fold(bytes, |total, value| {
                        total.saturating_add(reference_heap_bytes(value))
                    });
                    None
                }
                _ => None,
            };
            if let Some(children) = children {
                if depth == LEVELS {
                    return usize::MAX;
                }
                stack[depth] = children;
                depth += 1;
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

fn reference_heap_bytes(value: &RefTarget) -> usize {
    match value {
        RefTarget::TableRow { table, .. } => table.capacity(),
        RefTarget::Node { collection, .. }
        | RefTarget::Edge { collection, .. }
        | RefTarget::Vector { collection, .. }
        | RefTarget::Entity { collection, .. } => collection.capacity(),
    }
}
