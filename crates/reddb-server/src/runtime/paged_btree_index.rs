//! Ordered secondary pages for single-column BTREE indexes.

use super::index_store::{index_field_value, IndexMethodKind, IndexStore, RegisteredIndex};
use super::{RedDBRuntime, RuntimeInner};
use crate::storage::unified::hash_index::HashCandidates;
use crate::storage::unified::store::secondary_index::{
    SecondaryIndexIdentity, SecondaryIndexProjector, SECONDARY_KEY_BYTES_MAX,
};
use crate::storage::{EntityData, EntityId, EntityKind, StoreError, UnifiedEntity};
use reddb_engine::storage::ordered_secondary_key::encode_ordered_key;
use reddb_rql::ast::{CompareOp, FieldRef, Filter};
use reddb_types::value_to_canonical_key;
use sha2::{Digest, Sha256};
use std::ops::Bound::{self, Excluded, Included, Unbounded};
use std::sync::{Arc, Weak};

// Every valid family tag is below 255. An unrepresentable key retains a
// physical marker; the query then takes the existing authoritative scan.
// Never silently omit oversized or unsupported values from a complete index.
const SCAN_REQUIRED: u8 = 255;

struct BtreeProjection {
    column: String,
    schema: Option<Arc<Vec<String>>>,
}

impl SecondaryIndexProjector for BtreeProjection {
    fn project(
        &self,
        entity: &UnifiedEntity,
        output: &mut [u8; SECONDARY_KEY_BYTES_MAX],
    ) -> Result<Option<usize>, StoreError> {
        if !matches!(entity.kind, EntityKind::TableRow { .. }) {
            return Ok(None);
        }
        let EntityData::Row(row) = &entity.data else {
            return Ok(None);
        };
        let selected = |name: &str| {
            name == "body" || name == self.column || self.column.split('.').next() == Some(name)
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
        let Some(value) = index_field_value(&fields, &self.column) else {
            return Ok(None);
        };
        if let Some(key) = value_to_canonical_key(value.as_ref()) {
            match encode_ordered_key(&key, output) {
                Ok(length) => return Ok(Some(length)),
                Err(StoreError::ReadLimitExceeded { .. }) => {}
                Err(error) => return Err(error),
            }
        }
        output[0] = SCAN_REQUIRED;
        Ok(Some(1))
    }
}

pub(super) struct RuntimeBtreeIndex {
    runtime: Weak<RuntimeInner>,
    collection: String,
    name: String,
    identity: SecondaryIndexIdentity,
    projection: Arc<BtreeProjection>,
}

struct Range {
    lower: Bound<Vec<u8>>,
    upper: Bound<Vec<u8>>,
    family: u8,
}

fn encode_bound(value: &reddb_types::Value) -> Option<Vec<u8>> {
    let key = value_to_canonical_key(value)?;
    let mut output = [0; SECONDARY_KEY_BYTES_MAX];
    let length = encode_ordered_key(&key, &mut output).ok()?;
    Some(output[..length].to_vec())
}

impl Range {
    fn from_filter(filter: &Filter) -> Option<(&str, Self)> {
        let (field, lower, upper, family) = match filter {
            Filter::Between { field, low, high } => {
                let low = encode_bound(low)?;
                let high = encode_bound(high)?;
                if low[0] != high[0] {
                    return None;
                }
                let family = low[0];
                (field, Included(low), Included(high), family)
            }
            Filter::Compare { field, op, value } => {
                let key = encode_bound(value)?;
                let family = key[0];
                let (lower, upper) = match op {
                    CompareOp::Lt => (Unbounded, Excluded(key)),
                    CompareOp::Le => (Unbounded, Included(key)),
                    CompareOp::Gt => (Excluded(key), Unbounded),
                    CompareOp::Ge => (Included(key), Unbounded),
                    _ => return None,
                };
                (field, lower, upper, family)
            }
            _ => return None,
        };
        // Start with integral and text ranges. Other families keep the SQL scan
        // until their coercions (including floating signed zero) are qualified.
        if !matches!(family, 2 | 3 | 4 | 6) {
            return None;
        }
        let FieldRef::TableColumn { column, .. } = field else {
            return None;
        };
        Some((
            column,
            Self {
                lower,
                upper,
                family,
            },
        ))
    }

    fn contains(&self, key: &[u8]) -> bool {
        let above = match &self.lower {
            Included(low) => key >= low.as_slice(),
            Excluded(low) => key > low.as_slice(),
            Unbounded => true,
        };
        let below = match &self.upper {
            Included(high) => key <= high.as_slice(),
            Excluded(high) => key < high.as_slice(),
            Unbounded => true,
        };
        above && below
    }
}

impl RuntimeBtreeIndex {
    pub(super) fn memory_bytes(&self) -> u64 {
        (std::mem::size_of::<Self>()
            + std::mem::size_of::<BtreeProjection>()
            + 64
            + self.collection.len()
            + self.name.len()
            + self.projection.column.len()
            + self
                .projection
                .schema
                .as_ref()
                .map_or(0, |schema| schema.iter().map(|name| name.len() + 24).sum())) as u64
    }

    fn lookup(&self, range: &Range) -> crate::RedDBResult<Option<HashCandidates>> {
        let inner = self
            .runtime
            .upgrade()
            .ok_or_else(|| crate::RedDBError::Internal("BTREE runtime closed".into()))?;
        let runtime = RedDBRuntime {
            inner,
            background_tasks: None,
        };
        let ceiling = usize::try_from(runtime.memory_accounting().budget().resolved_bytes / 16)
            .unwrap_or(usize::MAX)
            .max(1);
        let mut limit = ceiling.min(64 * 1024);
        loop {
            match self.lookup_with_limit(&runtime, range, limit) {
                Err(StoreError::ReadLimitExceeded { required_bytes, .. })
                    if limit < ceiling && required_bytes <= ceiling =>
                {
                    limit = limit.saturating_mul(2).max(required_bytes).min(ceiling);
                }
                result => return result.map_err(crate::RedDBError::from),
            }
        }
    }

    fn lookup_with_limit(
        &self,
        runtime: &RedDBRuntime,
        range: &Range,
        limit: usize,
    ) -> Result<Option<HashCandidates>, StoreError> {
        let store = runtime.db().store();
        let manager = store
            .get_collection(&self.collection)
            .ok_or_else(|| StoreError::CollectionNotFound(self.collection.clone()))?;
        let (_scratch, read_bytes_max) = runtime
            .reserve_index_scan_with_limit(&manager, limit)
            .map_err(|error| StoreError::Serialization(error.to_string()))?;
        let output = runtime
            .try_reserve_query_buffer(read_bytes_max)
            .ok_or_else(|| {
                StoreError::Serialization("BTREE candidates exceed shared memory budget".into())
            })?;
        let ids_max = read_bytes_max / std::mem::size_of::<EntityId>();
        let mut ids = Vec::new();
        ids.try_reserve_exact(ids_max)
            .map_err(|error| StoreError::Serialization(error.to_string()))?;
        // Admission precedes publication/source locks; the returned owner keeps
        // candidate memory charged through result assembly and error paths.
        let _publication = store.vault_publication_lock.lock();
        let Some(index) = store.secondary_index(&self.collection, &self.name, &self.identity)
        else {
            return Ok(None);
        };
        let mut key = [0; SECONDARY_KEY_BYTES_MAX];
        // Existing SQL range semantics require a homogeneous canonical family.
        // Two seeks detect mixed families and overflow markers without reading
        // row payloads or hydrating the complete posting map.
        for (lower, upper) in [
            (Unbounded, Excluded([range.family].as_slice())),
            (Included([range.family + 1].as_slice()), Unbounded),
        ] {
            if index
                .ordered_range(lower, upper)?
                .next_posting(&mut key)?
                .is_some()
            {
                return Ok(None);
            }
        }
        let mut needs_scan = false;
        let mut accept = |entity: &UnifiedEntity| -> Result<(), StoreError> {
            let required_bytes = crate::storage::unified::memory_size::entity_bytes(entity);
            if required_bytes > read_bytes_max {
                return Err(StoreError::ReadLimitExceeded {
                    required_bytes,
                    limit_bytes: read_bytes_max,
                });
            }
            let Some(length) = self.projection.project(entity, &mut key)? else {
                return Ok(());
            };
            if key[0] != range.family {
                needs_scan = true;
                return Ok(());
            }
            if range.contains(&key[..length]) {
                if ids.len() == ids_max {
                    return Err(StoreError::ReadLimitExceeded {
                        required_bytes: read_bytes_max
                            .saturating_add(std::mem::size_of::<EntityId>()),
                        limit_bytes: read_bytes_max,
                    });
                }
                ids.push(entity.id);
            }
            Ok(())
        };
        let mut cursor = index.ordered_range(
            range.lower.as_ref().map(Vec::as_slice),
            range.upper.as_ref().map(Vec::as_slice),
        )?;
        let mut posting = [0; SECONDARY_KEY_BYTES_MAX];
        while let Some((_, id)) = cursor.next_posting(&mut posting)? {
            if let Some(entity) = manager.try_get(id, read_bytes_max)? {
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
        if needs_scan {
            return Ok(None);
        }
        ids.sort_unstable();
        ids.dedup();
        Ok(Some(HashCandidates::owned(ids, Box::new(output))))
    }
}

impl IndexStore {
    pub(crate) fn paged_btree_lookup(
        &self,
        collection: &str,
        filter: &Filter,
    ) -> crate::RedDBResult<Option<HashCandidates>> {
        if self.paged_btrees.read().is_empty() {
            return Ok(None);
        }
        let Some((column, range)) = Range::from_filter(filter) else {
            return Ok(None);
        };
        // Release the descriptor map before admission samples memory pools.
        let provider = self
            .paged_btrees
            .read()
            .values()
            .find(|provider| {
                provider.collection == collection && provider.projection.column == column
            })
            .cloned();
        match provider {
            Some(provider) => provider.lookup(&range),
            None => Ok(None),
        }
    }
}

impl RedDBRuntime {
    pub(super) fn prepare_paged_btree(
        &self,
        index: &RegisteredIndex,
        prepared: &IndexStore,
    ) -> crate::RedDBResult<()> {
        assert_eq!(index.method, IndexMethodKind::BTree);
        assert_eq!(index.columns.len(), 1);
        let schema = self
            .db()
            .store()
            .get_collection(&index.collection)
            .and_then(|manager| manager.column_schema());
        let projection = Arc::new(BtreeProjection {
            column: index.columns[0].clone(),
            schema,
        });
        let mut digest = Sha256::new();
        digest.update(b"runtime-btree-ordered-v1");
        digest.update([
            u8::from(index.unique),
            u8::from(projection.schema.is_some()),
        ]);
        for names in [
            index.columns.as_slice(),
            projection
                .schema
                .as_deref()
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        ] {
            digest.update((names.len() as u64).to_le_bytes());
            for name in names {
                digest.update((name.len() as u64).to_le_bytes());
                digest.update(name.as_bytes());
            }
        }
        let identity = SecondaryIndexIdentity {
            codec_version: 1,
            definition: digest.finalize().into(),
        };
        self.db().store().register_secondary_index(
            &index.collection,
            &index.name,
            identity.clone(),
            projection.clone(),
        )?;
        let mut equality = index.clone();
        equality.name = index.hash_lookup_name().into_owned();
        self.prepare_paged_hash_named(&equality, prepared, &equality_storage_name(&index.name))?;
        prepared.paged_btrees.write().insert(
            (index.collection.clone(), index.name.clone()),
            Arc::new(RuntimeBtreeIndex {
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

pub(super) fn equality_storage_name(name: &str) -> String {
    format!(
        "\0reddb/btree-equality-v1/{}",
        hex::encode(Sha256::digest(name.as_bytes()))
    )
}

#[cfg(test)]
mod tests {
    use crate::storage::{EntityId, StoreError};
    use crate::{RedDBOptions, RedDBRuntime};
    use reddb_engine::storage::collection_read::{
        CollectionCursor, CollectionRead, EncodedRecord, PagedCollection,
    };
    use reddb_types::Value;
    use std::sync::Arc;

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
                "unexpected BTREE source scan".into(),
            ))
        }
        fn owned_cursor(&self) -> Result<Box<dyn CollectionCursor>, StoreError> {
            Err(StoreError::Serialization(
                "unexpected BTREE source scan".into(),
            ))
        }
    }

    fn fixture() -> (tempfile::TempDir, RedDBRuntime, RedDBOptions) {
        let directory = tempfile::tempdir().expect("BTREE fixture");
        let options = RedDBOptions::persistent(directory.path().join("btree.rdb"))
            .with_memory_budget(64 * 1024 * 1024)
            .with_storage_profile(
                crate::storage::profile::StorageDeployPreset::PrimaryReplicaDev.selection(),
            )
            .expect("profile");
        let seed = RedDBRuntime::with_options(options.clone()).expect("seed");
        seed.execute_query("CREATE TABLE ordered_rows (id INTEGER, label TEXT, payload TEXT)")
            .expect("table");
        seed.execute_query("CREATE UNIQUE INDEX ordered_id ON ordered_rows (id) USING BTREE")
            .expect("numeric index");
        seed.execute_query("CREATE INDEX ordered_label ON ordered_rows (label) USING BTREE")
            .expect("text index");
        for id in 0..32 {
            seed.execute_query_with_params(
                "INSERT INTO ordered_rows (id, label, payload) VALUES ($1, $2, $3)",
                &[
                    Value::Integer(id),
                    Value::text(format!("key-{id:02}")),
                    Value::text("payload".repeat(2048)),
                ],
            )
            .expect("insert");
        }
        seed.flush().expect("seed checkpoint");
        drop(seed);
        let runtime = open(&options);
        (directory, runtime, options)
    }

    fn open(options: &RedDBOptions) -> RedDBRuntime {
        crate::storage::RedDB::with_disk_residency_for_qualification(|| {
            RedDBRuntime::with_options(options.clone())
        })
        .expect("cold runtime")
    }

    fn deny_scan(runtime: &RedDBRuntime) {
        let store = runtime.db().store();
        let manager = store.get_collection("ordered_rows").expect("collection");
        let root = manager.backing_root_page().expect("root");
        let count = manager.backing_count_at_root(root).expect("count");
        manager.install_backing(
            Arc::new(NoScan(Arc::new(PagedCollection::new(Arc::new(
                crate::storage::engine::BTree::with_root(
                    Arc::clone(store.pager().expect("pager")),
                    root,
                ),
            ))))),
            root,
            store.format_version(),
            1024 * 1024,
            count,
        );
    }

    fn ids(runtime: &RedDBRuntime, sql: &str) -> Vec<i64> {
        runtime
            .execute_query(sql)
            .expect(sql)
            .result
            .records
            .iter()
            .map(|record| match record.get("id").expect("id") {
                Value::Integer(id) => *id,
                other => panic!("unexpected id {other:?}"),
            })
            .collect()
    }

    #[test]
    fn paged_btree_prepared_restore_and_ranges_never_scan_source() {
        let (_directory, runtime, options) = fixture();
        drop(runtime);
        let runtime = open(&options);
        deny_scan(&runtime);
        runtime
            .rebuild_runtime_indexes_for_table("ordered_rows")
            .expect("prepared metadata-only BTREE restore");
        assert_eq!(
            runtime.index_store_ref().sorted.memory_bytes(),
            0,
            "no resident sorted postings"
        );
        for (predicate, expected) in [
            ("id BETWEEN 9 AND 12", vec![9, 10, 11, 12]),
            ("id > 28", vec![29, 30, 31]),
            ("id >= 30", vec![30, 31]),
            ("id < 2", vec![0, 1]),
            ("id <= 1", vec![0, 1]),
            ("label BETWEEN 'key-09' AND 'key-12'", vec![9, 10, 11, 12]),
        ] {
            assert_eq!(
                ids(
                    &runtime,
                    &format!("SELECT id FROM ordered_rows WHERE {predicate} ORDER BY id")
                ),
                expected
            );
        }
    }
    #[test]
    fn paged_btree_overlay_unique_rollback_and_checkpoint_preserve_results() {
        let (_directory, runtime, options) = fixture();
        for sql in [
            "UPDATE ordered_rows SET id = 40 WHERE id = 1",
            "DELETE FROM ordered_rows WHERE id = 2",
            "INSERT INTO ordered_rows (id, label, payload) VALUES (50, 'new', 'new')",
        ] {
            runtime.execute_query(sql).expect(sql);
        }
        let expected: Vec<_> = std::iter::once(0).chain(3..32).chain([40, 50]).collect();
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE id >= 0 ORDER BY id"
            ),
            expected
        );
        assert!(runtime
            .execute_query("INSERT INTO ordered_rows (id, label) VALUES (40, 'duplicate')")
            .is_err());
        runtime.execute_query("BEGIN").expect("begin");
        runtime
            .execute_query("UPDATE ordered_rows SET id = 60 WHERE id = 3")
            .expect("own update");
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE id >= 40 ORDER BY id"
            ),
            [40, 50, 60]
        );
        runtime
            .execute_query("SAVEPOINT nested")
            .expect("savepoint");
        runtime
            .execute_query("DELETE FROM ordered_rows WHERE id = 40")
            .expect("own delete");
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE id >= 40 ORDER BY id"
            ),
            [50, 60]
        );
        runtime
            .execute_query("ROLLBACK TO SAVEPOINT nested")
            .expect("rollback savepoint");
        runtime.execute_query("ROLLBACK").expect("rollback");
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE id >= 0 ORDER BY id"
            ),
            expected
        );
        runtime.flush().expect("rebase");
        drop(runtime);
        let runtime = open(&options);
        deny_scan(&runtime);
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE id >= 0 ORDER BY id"
            ),
            expected
        );
        assert_eq!(ids(&runtime, "SELECT id FROM ordered_rows WHERE id BETWEEN 9 AND 12 ORDER BY id DESC LIMIT 2 OFFSET 1"), [11, 10]);
        assert!(runtime
            .execute_query("INSERT INTO ordered_rows (id, label) VALUES (40, 'duplicate')")
            .is_err());
    }

    #[test]
    fn paged_btree_large_keys_fall_back_without_losing_rows() {
        use super::*;
        let (_directory, runtime, options) = fixture();
        runtime
            .execute_query_with_params(
                "INSERT INTO ordered_rows (id, label) VALUES (80, $1)",
                &[Value::text("z".repeat(2048))],
            )
            .expect("large label");
        let filter = Filter::Compare {
            field: FieldRef::column("ordered_rows", "label"),
            op: CompareOp::Ge,
            value: Value::text("key-30"),
        };
        assert!(runtime
            .index_store_ref()
            .paged_btree_lookup("ordered_rows", &filter)
            .expect("overlay overflow")
            .is_none());
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE label >= 'key-30' ORDER BY id"
            ),
            [30, 31, 80]
        );
        runtime.flush().expect("publish overflow marker");
        drop(runtime);
        let runtime = open(&options);
        assert!(runtime
            .index_store_ref()
            .paged_btree_lookup("ordered_rows", &filter)
            .expect("base overflow")
            .is_none());
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE label >= 'key-30' ORDER BY id"
            ),
            [30, 31, 80]
        );
    }

    #[test]
    fn paged_btree_page_errors_are_authoritative() {
        use crate::storage::engine::{Page, PageType};
        let (_directory, runtime, _options) = fixture();
        let store = runtime.db().store();
        let provider = runtime
            .index_store_ref()
            .paged_btrees
            .read()
            .get(&("ordered_rows".into(), "ordered_id".into()))
            .cloned()
            .expect("paged BTREE");
        let index = store
            .secondary_index("ordered_rows", "ordered_id", &provider.identity)
            .expect("root");
        let root = index.root_page_id();
        let pager = store.pager().expect("pager");
        let original = pager.read_page(root).expect("original");
        pager
            .write_page(root, Page::new(PageType::Overflow, root))
            .expect("corrupt fixture");
        runtime
            .execute_query("SELECT id FROM ordered_rows WHERE id BETWEEN 9 AND 12")
            .expect_err("an unreadable range must not become an empty result or fallback scan");
        pager.write_page(root, original).expect("restore");
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE id BETWEEN 9 AND 12 ORDER BY id"
            ),
            [9, 10, 11, 12]
        );
    }

    #[test]
    fn paged_btree_candidate_owner_retains_and_releases_admission() {
        use super::*;
        use crate::storage::memory_pools::MemoryPool;
        let (_directory, runtime, _options) = fixture();
        let filter = Filter::Between {
            field: FieldRef::column("ordered_rows", "id"),
            low: Value::Integer(9),
            high: Value::Integer(12),
        };
        let accounting = runtime.memory_accounting();
        let before = accounting.used_bytes(MemoryPool::QueryBuffers);
        let candidates = runtime
            .index_store_ref()
            .paged_btree_lookup("ordered_rows", &filter)
            .expect("range")
            .expect("provider");
        assert_eq!(candidates.len(), 4);
        let retained = accounting.used_bytes(MemoryPool::QueryBuffers);
        assert!(retained > before);
        assert!(retained - before <= 64 * 1024);
        let mut iterator = candidates.into_iter();
        assert!(iterator.next().is_some());
        assert_eq!(accounting.used_bytes(MemoryPool::QueryBuffers), retained);
        drop(iterator);
        assert_eq!(accounting.used_bytes(MemoryPool::QueryBuffers), before);
        drop(runtime.try_reserve_query_buffer(0).expect("sample"));
        let available = accounting.budget().resolved_bytes - accounting.total_used_bytes();
        let held = runtime
            .try_reserve_query_buffer((available - 16 * 1024) as usize)
            .expect("pressure");
        runtime
            .index_store_ref()
            .paged_btree_lookup("ordered_rows", &filter)
            .expect_err("deny before allocating");
        drop(held);
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE id BETWEEN 9 AND 12 ORDER BY id"
            ),
            [9, 10, 11, 12]
        );
    }

    #[test]
    fn paged_btree_sixteen_clients_retain_bounded_candidates() {
        use super::*;
        use crate::storage::memory_pools::MemoryPool;
        let (_directory, runtime, _options) = fixture();
        let accounting = runtime.memory_accounting();
        let before = accounting.used_bytes(MemoryPool::QueryBuffers);
        let barrier = Arc::new(std::sync::Barrier::new(17));
        let (send, receive) = std::sync::mpsc::channel();
        let mut workers = Vec::new();
        for _ in 0..16 {
            let runtime = runtime.clone();
            let barrier = Arc::clone(&barrier);
            let send = send.clone();
            workers.push(std::thread::spawn(move || {
                let filter = Filter::Between {
                    field: FieldRef::column("ordered_rows", "id"),
                    low: Value::Integer(9),
                    high: Value::Integer(12),
                };
                let candidates = runtime
                    .index_store_ref()
                    .paged_btree_lookup("ordered_rows", &filter);
                send.send(
                    candidates
                        .as_ref()
                        .map(|ids| ids.as_ref().map(|ids| ids.len()))
                        .map_err(ToString::to_string),
                )
                .expect("result");
                barrier.wait();
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
        barrier.wait();
        for worker in workers {
            worker.join().expect("client");
        }
        for result in results {
            assert_eq!(result.expect("admitted"), Some(4));
        }
        assert!(retained <= 16 * 64 * 1024, "{retained}");
        assert_eq!(accounting.used_bytes(MemoryPool::QueryBuffers), before);
    }

    #[test]
    fn paged_btree_drop_recreate_preserves_new_definition() {
        let (_directory, runtime, options) = fixture();
        runtime
            .execute_query("DROP INDEX ordered_id ON ordered_rows")
            .expect("drop");
        assert!(!runtime
            .index_store_ref()
            .paged_btrees
            .read()
            .contains_key(&("ordered_rows".into(), "ordered_id".into())));
        // Reuse the name with a different key; an old persisted numeric root
        // must never qualify this new definition.
        runtime
            .execute_query("CREATE INDEX ordered_id ON ordered_rows (label) USING BTREE")
            .expect("recreate");
        runtime
            .execute_query("INSERT INTO ordered_rows (id, label) VALUES (80, 'key-10')")
            .expect("write after recreate");
        runtime.flush().expect("checkpoint");
        drop(runtime);
        let runtime = open(&options);
        deny_scan(&runtime);
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE label BETWEEN 'key-09' AND 'key-11' ORDER BY id"
            ),
            [9, 10, 11, 80]
        );
    }

    #[test]
    fn paged_btree_null_family_and_numeric_coercion_keep_sql_fallback() {
        use super::*;
        let (_directory, runtime, options) = fixture();
        runtime
            .execute_query("INSERT INTO ordered_rows (id, label) VALUES (80, NULL)")
            .expect("null");
        let filter = Filter::Compare {
            field: FieldRef::column("ordered_rows", "label"),
            op: CompareOp::Ge,
            value: Value::text("key-30"),
        };
        assert!(runtime
            .index_store_ref()
            .paged_btree_lookup("ordered_rows", &filter)
            .expect("overlay mixed family")
            .is_none());
        runtime.flush().expect("publish mixed family");
        drop(runtime);
        let runtime = open(&options);
        assert!(runtime
            .index_store_ref()
            .paged_btree_lookup("ordered_rows", &filter)
            .expect("base mixed family")
            .is_none());
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE label >= 'key-30' ORDER BY id"
            ),
            [30, 31]
        );
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE id >= 29.0 ORDER BY id"
            ),
            [29, 30, 31, 80]
        );
    }
}
