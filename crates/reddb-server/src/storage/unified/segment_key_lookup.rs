//! Disposable physical candidates for KV keys. Visibility belongs to the caller.
use std::collections::{hash_map::RandomState, HashMap, HashSet};
use std::hash::BuildHasher;

use super::entity::{EntityData, EntityId, UnifiedEntity};
use reddb_types::Value;

#[derive(Default)]
pub(super) struct SegmentKeyLookup {
    fingerprints: RandomState,
    entries: HashMap<u64, Vec<EntityId>>,
    ids_bytes: usize,
    mutated: HashSet<EntityId>,
}

impl SegmentKeyLookup {
    pub(super) fn key(entity: &UnifiedEntity) -> Option<&str> {
        let EntityData::Row(row) = &entity.data else {
            return None;
        };
        match row.get_field("key")? {
            Value::Text(key) => Some(key),
            _ => None,
        }
    }

    pub(super) fn insert(&mut self, entity: &UnifiedEntity) {
        self.mutated.remove(&entity.id);
        let Some(key) = Self::key(entity) else {
            return;
        };
        let fingerprint = self.fingerprints.hash_one(key);
        let ids = self.entries.entry(fingerprint).or_default();
        let capacity = ids.capacity();
        ids.push(entity.id);
        self.ids_bytes += (ids.capacity() - capacity) * std::mem::size_of::<EntityId>();
    }

    pub(super) fn remove(&mut self, entity: &UnifiedEntity) {
        self.mutated.remove(&entity.id);
        let Some(key) = Self::key(entity) else {
            return;
        };
        let fingerprint = self.fingerprints.hash_one(key);
        let Some(ids) = self.entries.get_mut(&fingerprint) else {
            return;
        };
        ids.retain(|id| *id != entity.id);
        if ids.is_empty() {
            self.ids_bytes -= ids.capacity() * std::mem::size_of::<EntityId>();
            self.entries.remove(&fingerprint);
        }
    }

    pub(super) fn candidates(&self, key: &str) -> &[EntityId] {
        self.entries
            .get(&self.fingerprints.hash_one(key))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub(super) fn mark_mutated(&mut self, entity: &UnifiedEntity) {
        self.remove(entity);
        self.mutated.insert(entity.id);
    }

    pub(super) fn mutated_candidates(&self) -> impl Iterator<Item = &EntityId> {
        self.mutated.iter()
    }

    pub(super) fn memory_bytes(&self) -> u64 {
        if self.entries.capacity() == 0 && self.mutated.capacity() == 0 {
            return 0;
        }
        (std::mem::size_of::<Self>()
            + self.entries.capacity() * (std::mem::size_of::<(u64, Vec<EntityId>)>() + 1)
            + self.ids_bytes
            + self.mutated.capacity() * (std::mem::size_of::<EntityId>() + 1)) as u64
    }
}
