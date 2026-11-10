//! Disposable physical candidates for keyed rows. Visibility belongs to the caller.
use std::collections::{hash_map::RandomState, BTreeSet, HashMap, HashSet};
use std::hash::BuildHasher;

use super::entity::{EntityData, EntityId, UnifiedEntity};
use reddb_types::Value;

#[derive(Default)]
pub struct SegmentKeyLookup {
    fingerprints: RandomState,
    entries: HashMap<u64, Vec<EntityId>>,
    ids_bytes: usize,
    mutated: HashSet<EntityId>,
    queue_expirations: BTreeSet<(u64, u64, EntityId)>,
}

impl SegmentKeyLookup {
    pub fn key(entity: &UnifiedEntity) -> Option<&str> {
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
        if let Some((queue, key, expires)) = Self::queue_dedup(entity) {
            let queue_hash = self.fingerprints.hash_one(queue);
            self.queue_expirations
                .insert((queue_hash, expires, entity.id));
            if let Some(key) = key {
                let fingerprint = self.fingerprints.hash_one(("queue-dedup", queue, key));
                self.insert_fingerprint(fingerprint, entity.id);
            }
        }
        let Some(key) = Self::key(entity) else { return };
        let fingerprint = self.fingerprints.hash_one(key);
        self.insert_fingerprint(fingerprint, entity.id);
    }

    fn insert_fingerprint(&mut self, fingerprint: u64, id: EntityId) {
        let ids = self.entries.entry(fingerprint).or_default();
        if ids.last() == Some(&id) {
            return;
        }
        let capacity = ids.capacity();
        ids.push(id);
        self.ids_bytes += (ids.capacity() - capacity) * std::mem::size_of::<EntityId>();
    }

    pub(super) fn remove(&mut self, entity: &UnifiedEntity) {
        self.mutated.remove(&entity.id);
        if let Some((queue, key, expires)) = Self::queue_dedup(entity) {
            let queue_hash = self.fingerprints.hash_one(queue);
            self.queue_expirations
                .remove(&(queue_hash, expires, entity.id));
            if let Some(key) = key {
                self.remove_fingerprint(
                    self.fingerprints.hash_one(("queue-dedup", queue, key)),
                    entity.id,
                );
            }
        }
        if let Some(key) = Self::key(entity) {
            self.remove_fingerprint(self.fingerprints.hash_one(key), entity.id);
        }
    }

    fn remove_fingerprint(&mut self, fingerprint: u64, id: EntityId) {
        let Some(ids) = self.entries.get_mut(&fingerprint) else {
            return;
        };
        ids.retain(|candidate| *candidate != id);
        if ids.is_empty() {
            self.ids_bytes -= ids.capacity() * std::mem::size_of::<EntityId>();
            self.entries.remove(&fingerprint);
        }
    }

    /// Internal producer-dedup rows remain ordinary source rows. Index all
    /// physical versions; visibility and full-string checks belong to readers.
    pub fn queue_dedup(entity: &UnifiedEntity) -> Option<(&str, Option<&str>, u64)> {
        let row = entity.data.as_row()?;
        if text(row.get_field("kind")?)? != "queue_push_dedup" {
            return None;
        }
        let queue = text(row.get_field("queue")?)?;
        let key = row.get_field("dedup_key").and_then(text);
        let expires = match row.get_field("expires_at_ns") {
            Some(Value::UnsignedInteger(value)) => *value,
            Some(Value::Integer(value)) => u64::try_from(*value).unwrap_or(0),
            _ => 0,
        };
        Some((queue, key, expires))
    }

    pub(super) fn queue_candidates(&self, queue: &str, key: &str) -> &[EntityId] {
        self.entries
            .get(&self.fingerprints.hash_one(("queue-dedup", queue, key)))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub(super) fn queue_expired(
        &self,
        queue: &str,
        now: u64,
    ) -> impl Iterator<Item = EntityId> + '_ {
        let fingerprint = self.fingerprints.hash_one(queue);
        self.queue_expirations
            .range((fingerprint, 0, EntityId::new(0))..=(fingerprint, now, EntityId::new(u64::MAX)))
            .map(|(_, _, id)| *id)
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
        if self.entries.capacity() == 0
            && self.mutated.capacity() == 0
            && self.queue_expirations.is_empty()
        {
            return 0;
        }
        (std::mem::size_of::<Self>()
            + self.entries.capacity() * (std::mem::size_of::<(u64, Vec<EntityId>)>() + 1)
            + self.ids_bytes
            + self.queue_expirations.len() * (2 * std::mem::size_of::<(u64, u64, EntityId)>())
            + self.mutated.capacity() * (std::mem::size_of::<EntityId>() + 1)) as u64
    }
}

fn text(value: &Value) -> Option<&str> {
    match value {
        Value::Text(value) => Some(value),
        _ => None,
    }
}
