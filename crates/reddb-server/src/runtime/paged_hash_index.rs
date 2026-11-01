//! HASH equality over generation-bound secondary pages and the live WAL overlay.
//!
//! One lookup holds bounded decode scratch and an admitted candidate allocation.
//! It reads only matching digest postings plus resident changes; full raw keys
//! are rechecked, so a digest collision cannot create a match. No posting map is
//! hydrated on reopen. Missing generations use a fallible bounded physical scan.
use super::index_store::{bounded_hash_index_key, RegisteredIndex};
use super::{RedDBRuntime, RuntimeInner};
use crate::storage::unified::hash_index::{HashCandidates, HashIndexError, PagedHashIndex};
use crate::storage::unified::store::secondary_index::{
    SecondaryIndexIdentity, SecondaryIndexProjector, SECONDARY_KEY_BYTES_MAX,
};
use crate::storage::{EntityData, EntityId, EntityKind, StoreError, UnifiedEntity};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Weak};

pub(super) struct HashProjection {
    columns: Vec<String>,
    key_bytes_max: usize,
    schema: Option<Arc<Vec<String>>>,
    #[cfg(test)]
    forced_digest: Option<[u8; 32]>,
}

impl HashProjection {
    fn digest(&self, key: &[u8]) -> [u8; 32] {
        #[cfg(test)]
        if let Some(digest) = self.forced_digest {
            return digest;
        }
        Sha256::digest(key).into()
    }
    fn raw_key(
        &self,
        entity: &UnifiedEntity,
        bytes_max: usize,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        if !matches!(entity.kind, EntityKind::TableRow { .. }) {
            return Ok(None);
        }
        let EntityData::Row(row) = &entity.data else {
            return Ok(None);
        };
        let selected = |name: &str| {
            name == "body"
                || self
                    .columns
                    .iter()
                    .any(|column| column == name || column.split('.').next() == Some(name))
        };
        let fields: Vec<_> = if let Some(named) = &row.named {
            named
                .iter()
                .filter(|(name, _)| selected(name))
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect()
        } else if let Some(schema) = row.schema.as_ref().or(self.schema.as_ref()) {
            schema
                .iter()
                .zip(&row.columns)
                .filter(|(name, _)| selected(name))
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect()
        } else {
            Vec::new()
        };
        bounded_hash_index_key(&self.columns, &fields, bytes_max.min(self.key_bytes_max))
    }
}

impl SecondaryIndexProjector for HashProjection {
    fn project(
        &self,
        entity: &UnifiedEntity,
        key: &mut [u8; SECONDARY_KEY_BYTES_MAX],
    ) -> Result<Option<usize>, StoreError> {
        let Some(raw) = self.raw_key(entity, self.key_bytes_max)? else {
            return Ok(None);
        };
        key[..32].copy_from_slice(&self.digest(&raw));
        Ok(Some(32))
    }
}

fn projection_identity(columns: &[String], schema: Option<&[String]>) -> SecondaryIndexIdentity {
    let mut digest = Sha256::new();
    digest.update(b"runtime-hash-sha256-v1");
    digest.update([u8::from(schema.is_some())]);
    for names in [columns, schema.unwrap_or(&[])] {
        digest.update((names.len() as u64).to_le_bytes());
        for name in names {
            digest.update((name.len() as u64).to_le_bytes());
            digest.update(name.as_bytes());
        }
    }
    SecondaryIndexIdentity {
        codec_version: 1,
        definition: digest.finalize().into(),
    }
}

struct RuntimeHashIndex {
    runtime: Weak<RuntimeInner>,
    collection: String,
    name: String,
    identity: SecondaryIndexIdentity,
    projection: Arc<HashProjection>,
}

fn read_error(error: impl std::fmt::Display) -> HashIndexError {
    HashIndexError::Read(error.to_string())
}

fn storage_error(error: StoreError) -> HashIndexError {
    match error {
        StoreError::ReadLimitExceeded {
            required_bytes,
            limit_bytes,
        } => HashIndexError::ReadLimit {
            required_bytes,
            limit_bytes,
        },
        other => read_error(other),
    }
}

impl PagedHashIndex for RuntimeHashIndex {
    fn memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + std::mem::size_of::<HashProjection>()
            + 32 // Arc control blocks for the provider and projection.
            + self.collection.len()
            + self.name.len()
            + self
                .projection
                .columns
                .iter()
                .map(|s| s.len() + 24)
                .sum::<usize>()
            + self.projection.schema.as_ref().map_or(0, |schema| {
                schema.iter().map(|s| s.len() + 24).sum::<usize>()
            })
    }

    fn lookup(&self, key: &[u8]) -> Result<HashCandidates, HashIndexError> {
        let inner = self
            .runtime
            .upgrade()
            .ok_or_else(|| read_error("runtime closed"))?;
        let runtime = RedDBRuntime {
            inner,
            background_tasks: None,
        };
        // Small lookups must not reserve a quarter of the entire engine budget
        // per client. Retry larger rows/posting lists with admitted geometric
        // growth, releasing the previous attempt before reserving its successor.
        let ceiling = self.projection.key_bytes_max;
        let mut limit = ceiling.min(64 * 1024);
        loop {
            match self.lookup_with_limit(&runtime, key, limit) {
                Err(HashIndexError::ReadLimit { required_bytes, .. })
                    if limit < ceiling && required_bytes <= ceiling =>
                {
                    limit = limit.saturating_mul(2).max(required_bytes).min(ceiling);
                }
                result => return result,
            }
        }
    }
}

impl RuntimeHashIndex {
    fn lookup_with_limit(
        &self,
        runtime: &RedDBRuntime,
        key: &[u8],
        limit: usize,
    ) -> Result<HashCandidates, HashIndexError> {
        let store = runtime.db().store();
        let manager = store
            .get_collection(&self.collection)
            .ok_or_else(|| read_error("indexed collection missing"))?;
        // Admission samples managers; finish it before taking publication or
        // source locks. The output owner outlives both scanning and this runtime.
        let (_scratch, read_bytes_max) = runtime
            .reserve_index_scan_with_limit(&manager, limit)
            .map_err(read_error)?;
        let output = runtime
            .try_reserve_query_buffer(read_bytes_max)
            .ok_or_else(|| read_error("HASH candidates exceed shared memory budget"))?;
        let ids_max = read_bytes_max / std::mem::size_of::<EntityId>();
        let mut ids = Vec::new();
        ids.try_reserve_exact(ids_max).map_err(read_error)?;
        let mut accept = |entity: &UnifiedEntity| -> Result<(), HashIndexError> {
            let required_bytes = crate::storage::unified::memory_size::entity_bytes(entity);
            if required_bytes > read_bytes_max {
                return Err(HashIndexError::ReadLimit {
                    required_bytes,
                    limit_bytes: read_bytes_max,
                });
            }
            if self
                .projection
                .raw_key(entity, read_bytes_max)
                .map_err(storage_error)?
                .as_deref()
                == Some(key)
            {
                if ids.len() == ids_max {
                    return Err(HashIndexError::ReadLimit {
                        required_bytes: read_bytes_max
                            .saturating_add(std::mem::size_of::<EntityId>()),
                        limit_bytes: read_bytes_max,
                    });
                }
                ids.push(entity.id);
            }
            Ok(())
        };
        // Freeze publication across base capture, source recheck and overlay.
        // Native handles additionally pin both trees against page reclamation.
        let _publication = store.vault_publication_lock.lock();
        if let Some(index) = store.secondary_index(&self.collection, &self.name, &self.identity) {
            let mut cursor = index
                .candidates(&self.projection.digest(key))
                .map_err(storage_error)?;
            while let Some(id) = cursor.next_id().map_err(storage_error)? {
                if let Some(entity) = manager.try_get(id, read_bytes_max).map_err(storage_error)? {
                    accept(&entity)?;
                }
            }
            let mut failure = None;
            manager.for_each_entity(|entity| {
                if let Err(error) = accept(entity) {
                    failure = Some(error);
                    return false;
                }
                true
            });
            if let Some(error) = failure {
                return Err(error);
            }
        } else {
            let mut failure = None;
            manager
                .try_for_each_entity(read_bytes_max, |entity| {
                    if let Err(error) = accept(entity) {
                        failure = Some(error);
                        return false;
                    }
                    true
                })
                .map_err(storage_error)?;
            if let Some(error) = failure {
                return Err(error);
            }
        }
        // A replacement may be reached through both base and overlay. Sorting
        // in place avoids another set allocation and retains all physical MVCC IDs.
        ids.sort_unstable();
        ids.dedup();
        Ok(HashCandidates::owned(ids, Box::new(output)))
    }
}

impl RedDBRuntime {
    pub(super) fn prepare_paged_hash(
        &self,
        index: &RegisteredIndex,
        prepared: &super::index_store::IndexStore,
    ) -> crate::RedDBResult<()> {
        let store = self.db().store();
        let schema = store
            .get_collection(&index.collection)
            .and_then(|m| m.column_schema());
        // Encoding and schema bind the root to the complete immutable projection.
        let identity = projection_identity(&index.columns, schema.as_deref().map(Vec::as_slice));
        let projection = Arc::new(HashProjection {
            columns: index.columns.clone(),
            key_bytes_max: usize::try_from(self.memory_accounting().budget().resolved_bytes / 16)
                .unwrap_or(usize::MAX)
                .max(1),
            schema,
            #[cfg(test)]
            forced_digest: None,
        });
        store.register_secondary_index(
            &index.collection,
            &index.name,
            identity.clone(),
            projection.clone(),
        )?;
        prepared.hash.install_paged(
            &index.collection,
            &index.name,
            Arc::new(RuntimeHashIndex {
                runtime: Arc::downgrade(&self.inner),
                collection: index.collection.clone(),
                name: index.name.clone(),
                identity,
                projection,
            }),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::RedDBOptions;
    use crate::storage::memory_pools::MemoryPool;
    use reddb_types::Value;

    fn fixture() -> (tempfile::TempDir, RedDBRuntime) {
        let directory = tempfile::tempdir().expect("paged HASH fixture");
        let options = RedDBOptions::persistent(directory.path().join("hash.rdb"))
            .with_memory_budget(64 * 1024 * 1024)
            .with_storage_profile(
                crate::storage::profile::StorageDeployPreset::PrimaryReplicaDev.selection(),
            )
            .expect("profile");
        let seed = RedDBRuntime::with_options(options.clone()).expect("seed");
        for sql in [
            "CREATE TABLE hash_rows (id INTEGER, email TEXT)",
            "CREATE UNIQUE INDEX by_email ON hash_rows (email) USING HASH",
            "INSERT INTO hash_rows (id, email) VALUES (1, 'alpha')",
            "INSERT INTO hash_rows (id, email) VALUES (2, 'beta')",
        ] {
            seed.execute_query(sql).expect(sql);
        }
        seed.flush().expect("seed checkpoint");
        drop(seed);
        let runtime = crate::storage::RedDB::with_disk_residency_for_qualification(|| {
            RedDBRuntime::with_options(options)
        })
        .expect("cold runtime");
        assert!(runtime
            .index_store_ref()
            .hash
            .is_paged("hash_rows", "by_email"));
        (directory, runtime)
    }

    fn lookup(runtime: &RedDBRuntime, key: &str) -> HashCandidates {
        runtime
            .index_store_ref()
            .hash
            .lookup_owned("hash_rows", "by_email", key.as_bytes())
            .expect("lookup")
    }

    #[test]
    fn paged_hash_restore_and_lookup_do_not_scan_prepared_source() {
        use crate::storage::engine::BTree;
        use reddb_engine::storage::collection_read::{
            CollectionCursor, CollectionRead, EncodedRecord, PagedCollection,
        };
        struct NoScan(Arc<dyn CollectionRead + Send + Sync>);
        impl CollectionRead for NoScan {
            fn get_record(
                &self,
                id: EntityId,
                limit: usize,
            ) -> Result<Option<EncodedRecord>, StoreError> {
                self.0.get_record(id, limit)
            }
            fn cursor(&self) -> Result<Box<dyn CollectionCursor + '_>, StoreError> {
                Err(StoreError::Serialization(
                    "unexpected source-wide scan".into(),
                ))
            }
            fn owned_cursor(&self) -> Result<Box<dyn CollectionCursor>, StoreError> {
                Err(StoreError::Serialization(
                    "unexpected source-wide scan".into(),
                ))
            }
        }
        let (_directory, runtime) = fixture();
        let store = runtime.db().store();
        let root = store.collection_root_page("hash_rows").expect("root");
        let manager = store.get_collection("hash_rows").expect("hash_rows");
        manager.install_backing(
            Arc::new(NoScan(Arc::new(PagedCollection::new(Arc::new(
                BTree::with_root(Arc::clone(store.pager().expect("pager")), root),
            ))))),
            root,
            store.format_version(),
            1024 * 1024,
            2,
        );
        runtime
            .rebuild_runtime_indexes_for_table("hash_rows")
            .expect("metadata-only restore");
        assert_eq!(lookup(&runtime, "alpha").len(), 1);
        assert!(lookup(&runtime, "missing").is_empty());
    }

    #[test]
    fn paged_hash_key_expansion_is_bounded_before_allocation() {
        let fields = vec![("payload".into(), Value::text("x".repeat(1024)))];
        let columns = vec!["payload".into(); 128];
        let error = bounded_hash_index_key(&columns, &fields, 4096)
            .expect_err("bound repeated composite fields");
        assert!(matches!(error, StoreError::ReadLimitExceeded { .. }));
        let single = bounded_hash_index_key(&columns[..1], &fields, 1024)
            .expect("exact bound")
            .expect("key");
        assert_eq!(single.len(), 1024);
    }

    #[test]
    fn paged_hash_concurrent_lookup_mutation_and_checkpoint_finish() {
        let (_directory, runtime) = fixture();
        let (send, receive) = std::sync::mpsc::channel();
        let mut workers = Vec::new();
        for writer in [false, true] {
            let runtime = runtime.clone();
            let send = send.clone();
            workers.push(std::thread::spawn(move || {
                for round in 0..24 {
                    if writer {
                        let sql = format!(
                            "INSERT INTO hash_rows (id, email) VALUES ({}, 'concurrent-{round}')",
                            round + 10
                        );
                        runtime.execute_query(&sql).expect("concurrent insert");
                        runtime.flush().expect("concurrent checkpoint");
                    } else {
                        assert_eq!(lookup(&runtime, "alpha").len(), 1);
                        assert!(runtime
                            .index_store_ref()
                            .hash
                            .lookup_resident("hash_rows", "by_email", b"alpha")
                            .expect("resident probe")
                            .is_none());
                    }
                }
                send.send(()).expect("completion");
            }));
        }
        for _ in 0..2 {
            receive
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("no publication/admission/index lock inversion");
        }
        for worker in workers {
            worker.join().expect("worker");
        }
    }

    #[test]
    fn paged_hash_sixteen_clients_retain_small_results_within_shared_budget() {
        let (_directory, runtime) = fixture();
        let accounting = runtime.memory_accounting();
        let before = accounting.used_bytes(MemoryPool::QueryBuffers);
        let release = Arc::new(std::sync::Barrier::new(17));
        let (send, receive) = std::sync::mpsc::channel();
        let mut workers = Vec::new();
        for _ in 0..16 {
            let runtime = runtime.clone();
            let release = Arc::clone(&release);
            let send = send.clone();
            workers.push(std::thread::spawn(move || {
                let candidates =
                    runtime
                        .index_store_ref()
                        .hash
                        .lookup_owned("hash_rows", "by_email", b"alpha");
                send.send(
                    candidates
                        .as_ref()
                        .map(|ids| ids.len())
                        .map_err(ToString::to_string),
                )
                .expect("result");
                release.wait();
                drop(candidates);
            }));
        }
        let results: Vec<_> = (0..16)
            .map(|_| {
                receive
                    .recv_timeout(std::time::Duration::from_secs(30))
                    .expect("all clients finish admission")
            })
            .collect();
        let retained = accounting.used_bytes(MemoryPool::QueryBuffers) - before;
        release.wait();
        for worker in workers {
            worker.join().expect("client");
        }
        for result in results {
            assert_eq!(result.expect("small concurrent lookup admitted"), 1);
        }
        assert!(retained <= 16 * 64 * 1024, "{retained}");
        assert_eq!(accounting.used_bytes(MemoryPool::QueryBuffers), before);
    }

    #[test]
    fn paged_hash_large_key_retries_with_admitted_scratch() {
        let (_directory, runtime) = fixture();
        let key = "x".repeat(256 * 1024);
        runtime
            .execute_query_with_params(
                "INSERT INTO hash_rows (id, email) VALUES (3, $1)",
                &[Value::text(key.clone())],
            )
            .expect("large key");
        for checkpoint in [false, true] {
            if checkpoint {
                runtime.flush().expect("publish large key");
            }
            assert_eq!(lookup(&runtime, &key).len(), 1);
        }
    }

    #[test]
    fn paged_hash_overlay_mutations_and_checkpoint_rebase() {
        let (_directory, runtime) = fixture();
        assert_eq!(lookup(&runtime, "alpha").len(), 1);
        for sql in [
            "INSERT INTO hash_rows (id, email) VALUES (3, 'gamma')",
            "UPDATE hash_rows SET email = 'renamed' WHERE email = 'alpha'",
            "DELETE FROM hash_rows WHERE email = 'beta'",
        ] {
            assert_eq!(runtime.execute_query(sql).expect(sql).affected_rows, 1);
        }
        for checkpoint in [false, true] {
            if checkpoint {
                runtime.flush().expect("rebase");
            }
            // Physical candidates may include obsolete MVCC versions. SQL must
            // recheck visibility; direct lookup must find the new overlay keys.
            for key in ["gamma", "renamed"] {
                assert!(!lookup(&runtime, key).is_empty(), "{key}");
                let result = runtime
                    .execute_query_with_params(
                        "SELECT id FROM hash_rows WHERE email = $1",
                        &[Value::text(key)],
                    )
                    .expect("visible new key");
                assert_eq!(result.result.records.len(), 1, "{key}");
            }
            for key in ["alpha", "beta"] {
                let result = runtime
                    .execute_query_with_params(
                        "SELECT id FROM hash_rows WHERE email = $1",
                        &[Value::text(key)],
                    )
                    .expect("obsolete key");
                assert!(result.result.records.is_empty(), "{key}");
            }
            assert!(runtime
                .execute_query("INSERT INTO hash_rows (id, email) VALUES (4, 'renamed')")
                .is_err());
        }
        assert!(runtime.index_store_ref().hash.memory_bytes() < 8192);
    }

    #[test]
    fn paged_hash_candidates_retain_admission_until_iterator_is_dropped() {
        let (_directory, runtime) = fixture();
        let accounting = runtime.memory_accounting();
        let before = accounting.used_bytes(MemoryPool::QueryBuffers);
        let ids = lookup(&runtime, "alpha");
        let retained = accounting.used_bytes(MemoryPool::QueryBuffers);
        assert!(retained > before);
        let mut iterator = ids.into_iter();
        assert!(iterator.next().is_some());
        assert_eq!(accounting.used_bytes(MemoryPool::QueryBuffers), retained);
        drop(iterator);
        assert_eq!(accounting.used_bytes(MemoryPool::QueryBuffers), before);
        drop(runtime.try_reserve_query_buffer(0).expect("sample"));
        let available = accounting.budget().resolved_bytes - accounting.total_used_bytes();
        let held = runtime
            .try_reserve_query_buffer((available - 16 * 1024) as usize)
            .expect("pressure");
        let error = runtime
            .index_store_ref()
            .hash
            .lookup_owned("hash_rows", "by_email", b"alpha")
            .expect_err("deny before allocating");
        assert!(error.to_string().contains("workspace exceeds"), "{error}");
        drop(held);
        assert_eq!(lookup(&runtime, "alpha").len(), 1);
    }

    #[test]
    fn paged_hash_digest_collisions_recheck_full_keys() {
        let (_directory, runtime) = fixture();
        let store = runtime.db().store();
        let projection = Arc::new(HashProjection {
            columns: vec!["email".into()],
            key_bytes_max: 4 * 1024 * 1024,
            schema: store
                .get_collection("hash_rows")
                .expect("hash_rows")
                .column_schema(),
            forced_digest: Some([7; 32]),
        });
        let identity = SecondaryIndexIdentity {
            codec_version: 1,
            definition: [9; 32],
        };
        store.unregister_secondary_index("hash_rows", "by_email");
        store
            .register_secondary_index(
                "hash_rows",
                "by_email",
                identity.clone(),
                projection.clone(),
            )
            .expect("colliding projection");
        store.persist().expect("publish collision fixture");
        runtime.index_store_ref().hash.install_paged(
            "hash_rows",
            "by_email",
            Arc::new(RuntimeHashIndex {
                runtime: Arc::downgrade(&runtime.inner),
                collection: "hash_rows".into(),
                name: "by_email".into(),
                identity,
                projection,
            }),
        );
        assert_eq!(lookup(&runtime, "alpha").len(), 1);
        assert_eq!(lookup(&runtime, "beta").len(), 1);
        assert_ne!(lookup(&runtime, "alpha")[0], lookup(&runtime, "beta")[0]);
        assert!(lookup(&runtime, "absent").is_empty());
    }

    #[test]
    fn paged_hash_page_errors_reach_select_update_delete_and_unique_admission() {
        use crate::storage::engine::{Page, PageType};
        let (_directory, runtime) = fixture();
        let store = runtime.db().store();
        let schema = store
            .get_collection("hash_rows")
            .expect("hash_rows")
            .column_schema();
        let identity = projection_identity(&["email".into()], schema.as_deref().map(Vec::as_slice));
        let native = store
            .secondary_index("hash_rows", "by_email", &identity)
            .expect("published native root");
        let pager = store.pager().expect("pager");
        let root = native.root_page_id();
        let original = pager.read_page(root).expect("original");
        pager
            .write_page(root, Page::new(PageType::Overflow, root))
            .expect("corrupt fixture index");
        for sql in [
            "SELECT id FROM hash_rows WHERE email = 'alpha'",
            "UPDATE hash_rows SET email = 'changed' WHERE email = 'alpha'",
            "DELETE FROM hash_rows WHERE email = 'alpha'",
            "INSERT INTO hash_rows (id, email) VALUES (3, 'alpha')",
        ] {
            let error = runtime
                .execute_query(sql)
                .err()
                .expect("index failure must propagate");
            assert!(
                error.to_string().contains("hash index read failed"),
                "{sql}: {error}"
            );
        }
        pager.write_page(root, original).expect("restore");
        assert_eq!(lookup(&runtime, "alpha").len(), 1);
    }
}
