//! Constant-time resident bounds for the optional context index.
use super::{ContextPosting, EntityKeys};
use std::collections::{btree_map::Entry, BTreeMap};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

pub(super) struct ContextMemory {
    bytes: AtomicU64,
    posting_roots: AtomicBool,
    reverse_capacity: AtomicUsize,
    indexed_capacity: AtomicUsize,
    enabled_capacity: AtomicUsize,
}

impl Default for ContextMemory {
    fn default() -> Self {
        Self {
            // Empty B-tree roots and the fixed index bookkeeping.
            bytes: AtomicU64::new(0),
            posting_roots: AtomicBool::new(false),
            reverse_capacity: AtomicUsize::new(0),
            indexed_capacity: AtomicUsize::new(0),
            enabled_capacity: AtomicUsize::new(0),
        }
    }
}

impl ContextMemory {
    fn ensure_posting_roots(&self) {
        if !self.posting_roots.swap(true, Ordering::Relaxed) {
            self.add(2048);
        }
    }

    pub(super) fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }
    pub(super) fn add(&self, bytes: usize) {
        let _ = self
            .bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                Some(used.saturating_add(bytes as u64))
            });
    }
    pub(super) fn release(&self, bytes: usize) {
        let previous = self.bytes.fetch_sub(bytes as u64, Ordering::Relaxed);
        assert!(
            previous >= bytes as u64,
            "context memory accounting underflow"
        );
    }
    fn observe_capacity(&self, observed: usize, high_water: &AtomicUsize, bucket: usize) {
        let previous = high_water.fetch_max(observed, Ordering::Relaxed);
        if observed > previous {
            self.add(
                (observed - previous)
                    .saturating_mul(bucket.saturating_add(16))
                    .saturating_mul(8)
                    .div_ceil(7),
            );
        }
    }
    pub(super) fn reverse_capacity(&self, n: usize) {
        self.observe_capacity(
            n,
            &self.reverse_capacity,
            std::mem::size_of::<(u64, EntityKeys)>(),
        );
    }
    pub(super) fn indexed_capacity(&self, n: usize) {
        self.observe_capacity(n, &self.indexed_capacity, std::mem::size_of::<u64>());
    }
    pub(super) fn enabled_capacity(&self, n: usize) {
        self.observe_capacity(n, &self.enabled_capacity, std::mem::size_of::<String>());
    }
}

pub(super) fn keys_bytes(keys: &EntityKeys) -> usize {
    keys.token_keys
        .capacity()
        .saturating_mul(std::mem::size_of::<String>())
        .saturating_add(keys.token_keys.iter().map(String::capacity).sum::<usize>())
        .saturating_add(
            keys.field_value_keys
                .capacity()
                .saturating_mul(std::mem::size_of::<(String, String)>()),
        )
        .saturating_add(
            keys.field_value_keys
                .iter()
                .map(|(a, b)| a.capacity().saturating_add(b.capacity()))
                .sum::<usize>(),
        )
}

pub(super) trait PostingKey: Ord {
    fn heap_bytes(&self) -> usize;
}
impl PostingKey for String {
    fn heap_bytes(&self) -> usize {
        self.capacity()
    }
}
impl PostingKey for (String, String) {
    fn heap_bytes(&self) -> usize {
        self.0.capacity().saturating_add(self.1.capacity())
    }
}

fn entry_bytes<K>() -> usize {
    std::mem::size_of::<(K, Vec<ContextPosting>)>() + 64
}
fn posting_heap(posting: &ContextPosting) -> usize {
    posting
        .collection
        .capacity()
        .saturating_add(posting.field.capacity())
}
fn posting_slots(postings: &Vec<ContextPosting>) -> usize {
    // Four slots per live posting cover Vec growth (including its old buffer).
    // After removals, retained capacity must remain charged even with few hits.
    postings
        .capacity()
        .max(postings.len().saturating_mul(4))
        .saturating_mul(std::mem::size_of::<ContextPosting>())
}

pub(super) fn insert_posting<K: PostingKey>(
    map: &mut BTreeMap<K, Vec<ContextPosting>>,
    key: K,
    posting: ContextPosting,
    memory: &ContextMemory,
) {
    memory.ensure_posting_roots();
    let postings = match map.entry(key) {
        Entry::Occupied(entry) => entry.into_mut(),
        Entry::Vacant(entry) => {
            memory.add(entry_bytes::<K>().saturating_add(entry.key().heap_bytes()));
            entry.insert(Vec::new())
        }
    };
    let before = posting_slots(postings);
    memory.add(posting_heap(&posting));
    postings.push(posting);
    memory.add(posting_slots(postings).saturating_sub(before));
}

pub(super) fn remove_postings<K: PostingKey>(
    map: &mut BTreeMap<K, Vec<ContextPosting>>,
    key: &K,
    entity_id: super::EntityId,
    memory: &ContextMemory,
) {
    let Some(postings) = map.get_mut(key) else {
        return;
    };
    let before = posting_slots(postings);
    let mut heap = 0usize;
    postings.retain(|p| {
        if p.entity_id == entity_id {
            heap = heap.saturating_add(posting_heap(p));
            false
        } else {
            true
        }
    });
    memory.release(heap.saturating_add(before.saturating_sub(posting_slots(postings))));
    if postings.is_empty() {
        if let Some((owned_key, empty)) = map.remove_entry(key) {
            memory.release(
                entry_bytes::<K>()
                    .saturating_add(owned_key.heap_bytes())
                    .saturating_add(posting_slots(&empty)),
            );
        }
    }
}

/// Upper bound for one fresh entry, its posting and reverse lookup copy.
/// Counting shared keys again is conservative; admission never borrows credit
/// from another writer removing or appending to the same posting list.
pub(super) fn growth_entry<K>(
    key_heap: usize,
    collection_bytes: usize,
    field_bytes: usize,
) -> usize {
    entry_bytes::<K>()
        .saturating_add(key_heap.saturating_mul(2))
        .saturating_add(4 * std::mem::size_of::<ContextPosting>())
        .saturating_add(collection_bytes)
        .saturating_add(field_bytes)
        .saturating_add(2 * std::mem::size_of::<K>())
}

pub(super) fn estimate_entity(collection: &str, entity: &super::UnifiedEntity) -> u64 {
    use super::EntityData;
    use reddb_types::Value;
    let complex = |value: &Value| matches!(value, Value::Json(_) | Value::Array(_));
    let has_complex = match &entity.data {
        EntityData::Row(row) => {
            row.columns.iter().any(complex)
                || row.named.as_ref().is_some_and(|v| v.values().any(complex))
        }
        EntityData::Node(node) => node.properties.values().any(complex),
        EntityData::Edge(edge) => edge.properties.values().any(complex),
        _ => false,
    };
    if has_complex {
        // Do not parse an unadmitted JSON document into another owned tree.
        // Tokenizers already cap tokens/pairs; keep decode scratch reserved too.
        let payload = crate::storage::memory_size::entity_bytes(entity);
        let field_bytes = match &entity.data {
            EntityData::Row(row) => row
                .named
                .as_ref()
                .map_or(64, |v| v.keys().map(String::len).max().unwrap_or(0)),
            EntityData::Node(node) => node.properties.keys().map(String::len).max().unwrap_or(0),
            EntityData::Edge(edge) => edge.properties.keys().map(String::len).max().unwrap_or(0),
            _ => 64,
        };
        return complex_bound(collection, payload, field_bytes);
    }
    let tokens = super::extract_entity_tokens(entity);
    let pairs = super::extract_field_lookup_pairs(entity);
    let mut bytes = 16usize * 1024 + collection.len().saturating_mul(64);
    for (token, field) in &tokens {
        bytes = bytes.saturating_add(growth_entry::<String>(
            token.len(),
            collection.len(),
            field.len(),
        ));
        bytes = bytes
            .saturating_add(3 * std::mem::size_of::<(String, String)>())
            .saturating_add(token.len().saturating_mul(3))
            .saturating_add(field.len().saturating_mul(3));
    }
    for (field, value) in &pairs {
        let heap = field.len().saturating_add(value.len());
        bytes = bytes.saturating_add(growth_entry::<(String, String)>(
            heap,
            collection.len(),
            field.len(),
        ));
        bytes = bytes
            .saturating_add(3 * std::mem::size_of::<(String, String)>())
            .saturating_add(heap.saturating_mul(3));
    }
    bytes as u64
}

/// Complex values reserve capped index entries and JSON-decoding scratch
/// without parsing an unadmitted payload into another owned representation.
pub(super) fn complex_bound(collection: &str, payload: usize, field_bytes: usize) -> u64 {
    let field_bytes = field_bytes.max(super::super::tokenization::MAX_TOKEN_LEN);
    let entry = 2048usize
        .saturating_add(collection.len())
        .saturating_add(field_bytes.saturating_mul(8));
    (payload as u64).saturating_mul(32).saturating_add(
        ((super::MAX_FIELD_INDEX_PAIRS + super::MAX_INDEX_TOKENS) as u64)
            .saturating_mul(entry as u64),
    )
}

#[cfg(test)]
mod tests {
    use super::super::{ContextIndex, EntityData, EntityId, EntityKind, UnifiedEntity};
    use crate::storage::entity::RowData;
    use reddb_types::Value;

    fn row(id: u64, body: &str) -> UnifiedEntity {
        UnifiedEntity::new(
            EntityId::new(id),
            EntityKind::TableRow {
                table: "docs".into(),
                row_id: id,
            },
            EntityData::Row(RowData {
                columns: Vec::new(),
                named: Some([("body".into(), Value::text(body))].into()),
                schema: None,
            }),
        )
    }

    #[test]
    fn replacement_and_removal_keep_search_and_accounting_consistent() {
        let index = ContextIndex::new();
        index.set_collection_enabled("docs", true);
        let empty = index.memory_bytes();
        let original = row(1, "original shared");
        let estimate = index.estimate_entity_growth("docs", &original);
        index.index_entity("docs", &original);
        let original_bytes = index.memory_bytes();
        assert!(original_bytes > empty);
        assert!(original_bytes - empty <= estimate);
        for _ in 0..100 {
            let replacement = row(1, "replaced shared");
            index.index_entity("docs", &replacement);
            assert!(index
                .search_field("body", "original", true, 10, None)
                .is_empty());
            assert_eq!(
                index.search_field("body", "replaced", true, 10, None).len(),
                1
            );
            assert_eq!(index.memory_bytes(), original_bytes);
            index.index_entity("docs", &original);
            assert_eq!(index.memory_bytes(), original_bytes);
        }
        index.remove_entity(EntityId::new(1));
        assert!(index.memory_bytes() < original_bytes);
        assert!(
            index.memory_bytes() >= empty,
            "retained map capacity stays charged"
        );
        let drained = index.memory_bytes();
        index.remove_entity(EntityId::new(1));
        assert_eq!(index.memory_bytes(), drained);
        assert_eq!(index.stats().indexed_entities, 0);
        assert!(index.search("shared", 10, None).is_empty());
    }

    #[test]
    fn shared_posting_capacity_remains_charged_after_deletions() {
        let index = ContextIndex::new();
        index.set_collection_enabled("docs", true);
        let entities: Vec<_> = (1..=100).map(|id| row(id, "shared")).collect();
        index.index_entities("docs", &entities);
        let full = index.memory_bytes();
        for id in 1..100 {
            let before = index.memory_bytes();
            index.remove_entity(EntityId::new(id));
            assert!(index.memory_bytes() <= before);
        }
        let partial = index.memory_bytes();
        assert!(partial < full);
        assert_eq!(
            index.search_field("body", "shared", true, 100, None).len(),
            1
        );
        index.remove_entity(EntityId::new(100));
        assert!(index.memory_bytes() < partial);
        assert!(index
            .search_field("body", "shared", true, 100, None)
            .is_empty());
    }
}
