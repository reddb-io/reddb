//! Disposable primary-key lookup for a segment. Fingerprints only narrow the
//! candidate set; the caller checks the complete key before accepting a match.
use std::collections::{hash_map::DefaultHasher, HashMap};
use std::hash::{Hash, Hasher};

use super::entity::{EntityData, EntityId, UnifiedEntity};

pub(super) struct SegmentPrimaryKey {
    pub columns: Vec<String>,
    entries: HashMap<u64, Vec<EntityId>>,
    ids_bytes: usize,
}

impl SegmentPrimaryKey {
    pub fn new(columns: &[String]) -> Self {
        Self {
            columns: columns.to_vec(),
            entries: HashMap::new(),
            ids_bytes: 0,
        }
    }

    pub fn insert(&mut self, entity: &UnifiedEntity) {
        let EntityData::Row(row) = &entity.data else {
            return;
        };
        let values: Option<Vec<String>> = self
            .columns
            .iter()
            .map(|column| row.get_field(column).map(|value| format!("{value:?}")))
            .collect();
        let Some(values) = values else {
            return;
        };
        let ids = self.entries.entry(fingerprint(&values)).or_default();
        let capacity = ids.capacity();
        ids.push(entity.id);
        self.ids_bytes += (ids.capacity() - capacity) * std::mem::size_of::<EntityId>();
    }

    pub fn candidates(&self, signatures: &[String]) -> &[EntityId] {
        self.entries
            .get(&fingerprint(signatures))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.columns.capacity() * std::mem::size_of::<String>()
            + self.columns.iter().map(String::capacity).sum::<usize>()
            + self.entries.capacity() * (std::mem::size_of::<(u64, Vec<EntityId>)>() + 1)
            + self.ids_bytes
    }
}

fn fingerprint(signatures: &[String]) -> u64 {
    let mut hasher = DefaultHasher::new();
    signatures.hash(&mut hasher);
    hasher.finish()
}
