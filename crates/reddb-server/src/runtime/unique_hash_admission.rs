//! Admission for standalone UNIQUE HASH indexes, before row/WAL installation.

use super::index_store::{index_field_value, value_to_bytes, RegisteredIndex};
use crate::{RedDBError, RedDBResult, RedDBRuntime};
use reddb_types::Value;
use std::collections::HashSet;

fn matches_target(index: &RegisteredIndex, target: Option<&[String]>) -> bool {
    target.is_none_or(|target| {
        target.len() == index.columns.len()
            && target.iter().all(|column| {
                index
                    .columns
                    .iter()
                    .any(|indexed| indexed.eq_ignore_ascii_case(column))
            })
    })
}

fn index_key(index: &RegisteredIndex, fields: &[(String, Value)]) -> Option<Vec<u8>> {
    // Match the physical HASH writer exactly, including NULL and legacy
    // numeric encodings. Logical SQL equality is not its key representation.
    index
        .columns
        .first()
        .and_then(|column| index_field_value(fields, column))
        .map(|value| value_to_bytes(value.as_ref()))
}

impl RedDBRuntime {
    pub(crate) fn has_unique_hash_target(&self, collection: &str, target: &[String]) -> bool {
        self.index_store_ref()
            .unique_hash_indexes(collection)
            .iter()
            .any(|index| matches_target(index, Some(target)))
    }

    pub(crate) fn unique_hash_conflict_id(
        &self,
        collection: &str,
        fields: &[(String, Value)],
        target: Option<&[String]>,
    ) -> RedDBResult<Option<crate::storage::EntityId>> {
        for index in self.index_store_ref().unique_hash_indexes(collection) {
            if !matches_target(&index, target) {
                continue;
            }
            if let Some(key) = index_key(&index, fields) {
                if let Some(id) = self.unique_hash_key_conflict(&index, &key)? {
                    return Ok(Some(id));
                }
            }
        }
        Ok(None)
    }

    fn unique_hash_key_conflict(
        &self,
        index: &RegisteredIndex,
        key: &[u8],
    ) -> RedDBResult<Option<crate::storage::EntityId>> {
        let store = self.db().store();
        let snapshots = self.snapshot_manager();
        let own_xids = self.own_transaction_xids();
        for id in self
            .index_store_ref()
            .hash_lookup(&index.collection, &index.name, key)
            .map_err(RedDBError::Internal)?
        {
            if let Some(entity) = store.get(&index.collection, id) {
                if snapshots.row_reserves_unique_key(entity.xmin, entity.xmax, &own_xids) {
                    if snapshots.is_active(entity.xmin) && !own_xids.contains(&entity.xmin) {
                        return Err(RedDBError::Query(format!(
                            "serialization conflict: unique index '{}' is owned by active transaction {}; retry after it resolves",
                            index.name, entity.xmin
                        )));
                    }
                    return Ok(Some(id));
                }
            }
            // Aborted/obsolete physical entries must not reject a replacement.
            // The collection constraint gate is held through this cleanup and
            // the eventual insertion by all row mutation callers.
            self.index_store_ref()
                .hash
                .remove(&index.collection, &index.name, key, id)
                .map_err(|error| RedDBError::Internal(error.to_string()))?;
        }
        Ok(None)
    }

    pub(crate) fn has_unique_hash_batch_conflict(
        &self,
        collection: &str,
        fields: &[(String, Value)],
        accepted: &[Vec<(String, Value)>],
        target: Option<&[String]>,
    ) -> bool {
        self.index_store_ref()
            .unique_hash_indexes(collection)
            .iter()
            .any(|index| {
                matches_target(index, target)
                    && index_key(index, fields).is_some_and(|key| {
                        accepted
                            .iter()
                            .any(|row| index_key(index, row).as_ref() == Some(&key))
                    })
            })
    }

    pub(crate) fn enforce_unique_hash_rows(
        &self,
        collection: &str,
        rows: &[super::mutation::MutationRow],
    ) -> RedDBResult<()> {
        for index in self.index_store_ref().unique_hash_indexes(collection) {
            let mut proposed = HashSet::with_capacity(rows.len());
            for row in rows {
                let Some(key) = index_key(&index, &row.fields) else {
                    continue;
                };
                if self.unique_hash_key_conflict(&index, &key)?.is_some() || !proposed.insert(key) {
                    return Err(RedDBError::Query(format!(
                        "duplicate key violates unique index '{}' on collection '{}'",
                        index.name, collection
                    )));
                }
            }
        }
        Ok(())
    }
}
