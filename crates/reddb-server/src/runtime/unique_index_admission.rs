//! Admission for standalone UNIQUE indexes (HASH and BTREE), before row/WAL
//! installation.

use super::index_store::{unique_index_key, RegisteredIndex};
use crate::storage::EntityId;
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
    // Match the physical writer exactly, including legacy numeric encodings.
    // Logical SQL equality is not its key representation. A composite index
    // keys on every column; a NULL column reserves no key (`unique_index_key`).
    unique_index_key(&index.columns, fields)
}

fn duplicate_key_error(index: &RegisteredIndex, collection: &str) -> RedDBError {
    RedDBError::Query(format!(
        "duplicate key violates unique index '{}' on collection '{}'",
        index.name, collection
    ))
}

impl RedDBRuntime {
    pub(crate) fn has_unique_target(&self, collection: &str, target: &[String]) -> bool {
        self.index_store_ref()
            .unique_indexes(collection)
            .iter()
            .any(|index| matches_target(index, Some(target)))
    }

    pub(crate) fn unique_conflict_id(
        &self,
        collection: &str,
        fields: &[(String, Value)],
        target: Option<&[String]>,
    ) -> RedDBResult<Option<EntityId>> {
        for index in self.index_store_ref().unique_indexes(collection) {
            if !matches_target(&index, target) {
                continue;
            }
            if let Some(key) = index_key(&index, fields) {
                if let Some(id) = self.unique_key_conflict(&index, &key, None)? {
                    return Ok(Some(id));
                }
            }
        }
        Ok(None)
    }

    /// A row other than `exclude` that reserves `key` in `index`. `exclude` is
    /// the version an UPDATE replaces: it never conflicts with its successor.
    fn unique_key_conflict(
        &self,
        index: &RegisteredIndex,
        key: &[u8],
        exclude: Option<EntityId>,
    ) -> RedDBResult<Option<EntityId>> {
        let store = self.db().store();
        let snapshots = self.snapshot_manager();
        let own_xids = self.own_transaction_xids();
        let pocket = index.hash_lookup_name();
        for id in self
            .index_store_ref()
            .hash_lookup(&index.collection, &pocket, key)
            .map_err(RedDBError::Internal)?
        {
            if exclude == Some(id) {
                continue;
            }
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
                .remove(&index.collection, &pocket, key, id)
                .map_err(|error| RedDBError::Internal(error.to_string()))?;
        }
        Ok(None)
    }

    pub(crate) fn has_unique_batch_conflict(
        &self,
        collection: &str,
        fields: &[(String, Value)],
        accepted: &[Vec<(String, Value)>],
        target: Option<&[String]>,
    ) -> bool {
        self.index_store_ref()
            .unique_indexes(collection)
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

    pub(crate) fn enforce_unique_rows(
        &self,
        collection: &str,
        rows: &[super::mutation::MutationRow],
    ) -> RedDBResult<()> {
        for index in self.index_store_ref().unique_indexes(collection) {
            let mut proposed = HashSet::with_capacity(rows.len());
            for row in rows {
                let Some(key) = index_key(&index, &row.fields) else {
                    continue;
                };
                if self.unique_key_conflict(&index, &key, None)?.is_some() || !proposed.insert(key)
                {
                    return Err(duplicate_key_error(&index, collection));
                }
            }
        }
        Ok(())
    }

    /// Admit the post-images of one UPDATE: `(replaced version, new fields)`.
    /// Each must stay clear of every other live row, and of each other.
    pub(crate) fn enforce_unique_updates(
        &self,
        collection: &str,
        rows: &[(EntityId, Vec<(String, Value)>)],
    ) -> RedDBResult<()> {
        for index in self.index_store_ref().unique_indexes(collection) {
            let mut proposed = HashSet::with_capacity(rows.len());
            for (previous, fields) in rows {
                let Some(key) = index_key(&index, fields) else {
                    continue;
                };
                if self
                    .unique_key_conflict(&index, &key, Some(*previous))?
                    .is_some()
                    || !proposed.insert(key)
                {
                    return Err(duplicate_key_error(&index, collection));
                }
            }
        }
        Ok(())
    }
}
