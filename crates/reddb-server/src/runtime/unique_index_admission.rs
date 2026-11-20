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
        if self.vcs_is_versioned(&index.collection)? {
            let (snapshot, _retention) = self.native_read_snapshot()?;
            let mut conflict = None;
            if let Some(manager) = store.get_collection(&index.collection) {
                manager.try_for_each_entity(usize::MAX, |entity| {
                    if exclude != Some(entity.id)
                        && reddb_engine::transaction::read::entity_reserves_unique_key(
                            &snapshot, entity,
                        )
                    {
                        if let Some(row) = entity.data.as_row() {
                            let fields: Vec<_> = row
                                .iter_fields()
                                .map(|(name, value)| (name.to_string(), value.clone()))
                                .collect();
                            if index_key(index, &fields).as_deref() == Some(key) {
                                conflict = Some(entity.id);
                                return false;
                            }
                        }
                    }
                    true
                })?;
            }
            return Ok(conflict);
        }
        let pocket = index.hash_lookup_name();
        let candidates = self
            .index_store_ref()
            .hash_lookup(&index.collection, &pocket, key)
            .map_err(RedDBError::Internal)?;
        let read_workspace = if !candidates.is_empty() {
            match store.get_collection(&index.collection) {
                Some(manager) if manager.has_backing() => Some(self.reserve_index_scan(&manager)?),
                _ => None,
            }
        } else {
            None
        };
        let read_bytes_max = read_workspace
            .as_ref()
            .map_or(usize::MAX, |(_, limit)| *limit);
        for id in candidates {
            if exclude == Some(id) {
                continue;
            }
            if let Some(entity) = store.try_get(&index.collection, id, read_bytes_max)? {
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

    /// A row in the disk base that reserves a declared `PRIMARY KEY`/`UNIQUE`
    /// key. The contract's resident probe only sees rows still in memory, so
    /// a constraint without a unique index over the same columns must look at
    /// the base as well, or a checkpoint silently drops the constraint.
    ///
    /// A HASH index over exactly these columns narrows the search to its
    /// candidates; the contract's own rule still decides whether a candidate
    /// reserves the key. Without one this costs a bounded pass over the
    /// collection.
    pub(crate) fn cold_constraint_conflict(
        &self,
        collection: &str,
        columns: &[String],
        fields: &[(String, Value)],
        signatures: &[String],
        exclude: Option<EntityId>,
    ) -> RedDBResult<Option<EntityId>> {
        let store = self.db().store();
        let Some(manager) = store.get_collection(collection) else {
            return Ok(None);
        };
        if !manager.has_backing() {
            return Ok(None);
        }
        let (_workspace, read_bytes_max) = self.reserve_index_scan(&manager)?;
        let snapshots = self.snapshot_manager();
        let own_xids = self.own_transaction_xids();
        let reserves = |entity: &crate::storage::UnifiedEntity| {
            if exclude == Some(entity.id)
                || !snapshots.row_reserves_unique_key(entity.xmin, entity.xmax, &own_xids)
            {
                return false;
            }
            // Same key representation as the resident probe.
            entity.data.as_row().is_some_and(|row| {
                columns.iter().zip(signatures).all(|(column, signature)| {
                    row.get_field(column)
                        .is_some_and(|value| format!("{value:?}") == *signature)
                })
            })
        };
        let candidates = self
            .index_store_ref()
            .list_indices(collection)
            .into_iter()
            .find(|index| {
                matches!(index.method, super::index_store::IndexMethodKind::Hash)
                    && matches_target(index, Some(columns))
            })
            .and_then(|index| Some((index_key(&index, fields)?, index)));
        if let Some((key, index)) = candidates {
            let ids = self
                .index_store_ref()
                .hash_lookup(collection, &index.hash_lookup_name(), &key)
                .map_err(RedDBError::Internal)?;
            for id in ids {
                if let Some(entity) = store.try_get(collection, id, read_bytes_max)? {
                    if reserves(&entity) {
                        return Ok(Some(id));
                    }
                }
            }
            return Ok(None);
        }
        let mut conflict = None;
        manager.try_for_each_entity(read_bytes_max, |entity| {
            if reserves(entity) {
                conflict = Some(entity.id);
            }
            conflict.is_none()
        })?;
        Ok(conflict)
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

#[cfg(test)]
mod tests {
    use crate::{RedDBOptions, RedDBRuntime};
    use reddb_types::Value;

    /// A table whose rows all live in the disk base, like after a restart.
    fn cold_table(ddl: &[&str]) -> (tempfile::TempDir, RedDBRuntime) {
        let directory = tempfile::tempdir().expect("constraint fixture");
        let options = RedDBOptions::persistent(directory.path().join("constraints.rdb"))
            .with_memory_budget(64 * 1024 * 1024)
            .with_storage_profile(
                crate::storage::profile::StorageDeployPreset::PrimaryReplicaDev.selection(),
            )
            .expect("profile");
        let seed = RedDBRuntime::with_options(options.clone()).expect("seed");
        for sql in ddl {
            seed.execute_query(sql).expect(sql);
        }
        seed.flush().expect("seed checkpoint");
        drop(seed);
        let runtime = crate::storage::RedDB::with_disk_residency_for_qualification(|| {
            RedDBRuntime::with_options(options)
        })
        .expect("cold runtime");
        (directory, runtime)
    }

    fn values(runtime: &RedDBRuntime, sql: &str, column: &str) -> Vec<Value> {
        runtime
            .execute_query(sql)
            .expect(sql)
            .result
            .records
            .iter()
            .filter_map(|record| record.get(column).cloned())
            .collect()
    }

    #[test]
    fn primary_key_still_rejects_duplicates_of_rows_on_disk() {
        let (_directory, runtime) = cold_table(&[
            "CREATE TABLE accounts (id TEXT PRIMARY KEY, v TEXT)",
            "INSERT INTO accounts (id, v) VALUES ('a', '1'), ('b', '2')",
        ]);
        let error = runtime
            .execute_query("INSERT INTO accounts (id, v) VALUES ('a', 'duplicate')")
            .expect_err("a primary key on disk must still reject its duplicate");
        assert!(error.to_string().contains("primary key"), "{error}");
        // An upsert must find the row on disk and update it, not append.
        runtime
            .execute_query(
                "INSERT INTO accounts (id, v) VALUES ('b', 'upserted') \
                 ON CONFLICT (id) DO UPDATE SET v = 'upserted'",
            )
            .expect("upsert over a row on disk");
        assert_eq!(
            values(&runtime, "SELECT v FROM accounts WHERE id = 'b'", "v"),
            vec![Value::text("upserted")]
        );
        // An UPDATE may not move a row onto a key held on disk either, but
        // rewriting a row under its own key stays legal.
        assert!(runtime
            .execute_query("UPDATE accounts SET id = 'a' WHERE id = 'b'")
            .is_err());
        runtime
            .execute_query("UPDATE accounts SET v = 'again' WHERE id = 'a'")
            .expect("update that keeps its key");
        // A key freed by a delete can be taken again.
        runtime
            .execute_query("DELETE FROM accounts WHERE id = 'a'")
            .expect("delete");
        runtime
            .execute_query("INSERT INTO accounts (id, v) VALUES ('a', 'reborn')")
            .expect("key freed by delete");
        assert_eq!(
            values(&runtime, "SELECT id FROM accounts ORDER BY id", "id"),
            vec![Value::text("a"), Value::text("b")]
        );
    }

    #[test]
    fn a_hash_index_on_the_key_serves_the_constraint_check_on_disk() {
        // The index is not unique: it only narrows the rows the constraint
        // has to look at. The constraint's own rule still decides.
        let (_directory, runtime) = cold_table(&[
            "CREATE TABLE accounts (id TEXT PRIMARY KEY, v TEXT)",
            "CREATE INDEX accounts_id ON accounts (id) USING HASH",
            "INSERT INTO accounts (id, v) VALUES ('a', '1'), ('b', '2')",
        ]);
        let error = runtime
            .execute_query("INSERT INTO accounts (id, v) VALUES ('a', 'duplicate')")
            .expect_err("duplicate primary key on disk");
        assert!(error.to_string().contains("primary key"), "{error}");
        runtime
            .execute_query(
                "INSERT INTO accounts (id, v) VALUES ('b', 'upserted') \
                 ON CONFLICT (id) DO UPDATE SET v = 'upserted'",
            )
            .expect("upsert over a row on disk");
        assert_eq!(
            values(&runtime, "SELECT v FROM accounts WHERE id = 'b'", "v"),
            vec![Value::text("upserted")]
        );
        assert!(runtime
            .execute_query("UPDATE accounts SET id = 'a' WHERE id = 'b'")
            .is_err());
        // Deleting and re-inserting a key inside one transaction, then
        // rolling back, leaves the original row holding it.
        for sql in [
            "BEGIN",
            "DELETE FROM accounts WHERE id = 'a'",
            "INSERT INTO accounts (id, v) VALUES ('a', 'replacement')",
            "ROLLBACK",
        ] {
            runtime.execute_query(sql).expect(sql);
        }
        assert_eq!(
            values(&runtime, "SELECT v FROM accounts WHERE id = 'a'", "v"),
            vec![Value::text("1")]
        );
        runtime
            .execute_query("INSERT INTO accounts (id, v) VALUES ('c', '3')")
            .expect("new key");
        assert_eq!(
            values(&runtime, "SELECT id FROM accounts ORDER BY id", "id").len(),
            3
        );
    }

    #[test]
    fn unique_column_still_rejects_duplicates_of_rows_on_disk() {
        let (_directory, runtime) = cold_table(&[
            "CREATE TABLE members (id INTEGER PRIMARY KEY, email TEXT UNIQUE, org TEXT)",
            "INSERT INTO members (id, email, org) VALUES (1, 'a@x', 'o'), (2, 'b@x', 'o')",
        ]);
        assert!(runtime
            .execute_query("INSERT INTO members (id, email, org) VALUES (3, 'a@x', 'o')")
            .is_err());
        runtime
            .execute_query("INSERT INTO members (id, email, org) VALUES (3, 'c@x', 'o')")
            .expect("a new key is admitted");
        assert_eq!(
            values(&runtime, "SELECT id FROM members ORDER BY id", "id").len(),
            3
        );
    }
}
