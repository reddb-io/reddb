//! Runtime index registry persistence.
//!
//! Extracted verbatim from `impl_core.rs` (impl_core slice 9/10, issue #1630).
//! Houses `rebuild_runtime_indexes_for_table`, `persist_runtime_index_descriptor`,
//! `persist_runtime_index_drop`, and `rehydrate_runtime_index_registry`.
//!
//! The private `named_*` / `index_method_kind_*` free helpers and the
//! `RUNTIME_INDEX_REGISTRY_COLLECTION` const moved here alongside their only
//! callers.
use super::*;

const RUNTIME_INDEX_REGISTRY_COLLECTION: &str = "red_index_registry";

/// Borrow one source row, projecting only indexed fields and document bodies.
/// Neither callback may re-enter storage: the visitor holds publication locks.
fn for_each_index_row(
    manager: &crate::storage::unified::SegmentManager,
    columns: &std::collections::HashSet<String>,
    read_bytes_max: usize,
    mut visit: impl FnMut(crate::storage::EntityId, &[(String, reddb_types::Value)]) -> RedDBResult<()>,
) -> RedDBResult<()> {
    let schema = manager.column_schema();
    let mut failure = None;
    manager.try_for_each_entity(read_bytes_max, |entity| {
        if !matches!(entity.kind, crate::storage::EntityKind::TableRow { .. }) {
            return true;
        }
        let required_bytes = crate::storage::unified::memory_size::entity_bytes(entity);
        if required_bytes > read_bytes_max {
            failure = Some(RedDBError::InvalidOperation(format!(
                "index backfill row needs {required_bytes} bytes; workspace row limit is {read_bytes_max}"
            )));
            return false;
        }
        let crate::storage::EntityData::Row(row) = &entity.data else {
            return true;
        };
        let selected = |name: &str| columns.contains(name) || name == "body";
        let fields: Vec<_> = if let Some(named) = &row.named {
            named.iter().filter(|(name, _)| selected(name))
                .map(|(name, value)| (name.clone(), value.clone())).collect()
        } else if let Some(schema) = row.schema.as_ref().or(schema.as_ref()) {
            schema.iter().zip(&row.columns).filter(|(name, _)| selected(name))
                .map(|(name, value)| (name.clone(), value.clone())).collect()
        } else {
            Vec::new()
        };
        match visit(entity.id, &fields) {
            Ok(()) => true,
            Err(error) => { failure = Some(error); false }
        }
    })?;
    failure.map_or(Ok(()), Err)
}

type IndexDescriptor = (
    (String, String),
    Option<super::index_store::RegisteredIndex>,
);

fn index_descriptor(entity: &crate::storage::UnifiedEntity) -> Option<IndexDescriptor> {
    let crate::storage::EntityData::Row(row) = &entity.data else {
        return None;
    };
    let named = row.named.as_ref()?;
    let collection = named_text(named, "collection")?;
    let name = named_text(named, "name")?;
    let key = (collection.clone(), name.clone());
    if named_bool(named, "dropped").unwrap_or(false) {
        return Some((key, None));
    }
    let columns = named_text(named, "columns")
        .map(|raw| {
            raw.split('\u{1f}')
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let resolution = named_i64(named, "resolution")
        .and_then(|v| u8::try_from(v).ok())
        .unwrap_or(0);
    let raw_method = named_text(named, "method")?;
    if matches!(raw_method.as_str(), "spatial" | "rtree") {
        tracing::warn!(collection = %collection, index = %name, method = %raw_method,
            "dropping retired RTREE index descriptor during load; the in-RAM R-tree indexed nothing and served no queries");
        return Some((key, None));
    }
    let method = index_method_kind_from_str(&raw_method, resolution)?;
    Some((
        key,
        Some(super::index_store::RegisteredIndex {
            name,
            collection,
            columns,
            method,
            unique: named_bool(named, "unique").unwrap_or(false),
        }),
    ))
}

fn named_text(
    named: &std::collections::HashMap<String, reddb_types::Value>,
    key: &str,
) -> Option<String> {
    match named.get(key) {
        Some(reddb_types::Value::Text(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn named_bool(
    named: &std::collections::HashMap<String, reddb_types::Value>,
    key: &str,
) -> Option<bool> {
    match named.get(key) {
        Some(reddb_types::Value::Boolean(value)) => Some(*value),
        _ => None,
    }
}

fn named_i64(
    named: &std::collections::HashMap<String, reddb_types::Value>,
    key: &str,
) -> Option<i64> {
    match named.get(key) {
        Some(reddb_types::Value::Integer(value)) => Some(*value),
        _ => None,
    }
}

fn index_method_kind_as_str(method: super::index_store::IndexMethodKind) -> &'static str {
    match method {
        super::index_store::IndexMethodKind::Hash => "hash",
        super::index_store::IndexMethodKind::Bitmap => "bitmap",
        super::index_store::IndexMethodKind::BTree => "btree",
        // The H3 resolution rides in a sibling `resolution` column of the
        // persisted descriptor; the method tag itself is just "h3".
        super::index_store::IndexMethodKind::H3 { .. } => "h3",
    }
}

/// The H3 resolution to persist for an index. Non-H3 kinds persist `0`,
/// which the rehydrate path ignores.
fn index_method_kind_resolution(method: super::index_store::IndexMethodKind) -> u8 {
    match method {
        super::index_store::IndexMethodKind::H3 { resolution } => resolution,
        _ => 0,
    }
}

fn index_method_kind_from_str(
    raw: &str,
    resolution: u8,
) -> Option<super::index_store::IndexMethodKind> {
    match raw {
        "hash" => Some(super::index_store::IndexMethodKind::Hash),
        "bitmap" => Some(super::index_store::IndexMethodKind::Bitmap),
        "btree" => Some(super::index_store::IndexMethodKind::BTree),
        "h3" => Some(super::index_store::IndexMethodKind::H3 { resolution }),
        _ => None,
    }
}

impl RedDBRuntime {
    pub(crate) fn rebuild_runtime_indexes_for_table(&self, table: &str) -> RedDBResult<()> {
        let topology_lock = self.inner.index_store.collection_topology_lock(table);
        let _topology_guard = topology_lock.write();
        let registered = self.inner.index_store.list_indices(table);
        if registered.is_empty() {
            return Ok(());
        }
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(table) else {
            return Ok(());
        };
        self.restore_index_collection(table, &manager, registered)?;
        self.invalidate_plan_cache();
        Ok(())
    }

    pub(crate) fn persist_runtime_index_descriptor(
        &self,
        index: super::index_store::RegisteredIndex,
    ) -> RedDBResult<()> {
        let store = self.inner.db.store();
        let _ = store.get_or_create_collection(RUNTIME_INDEX_REGISTRY_COLLECTION);
        let entity = crate::storage::UnifiedEntity::new(
            crate::storage::EntityId::new(0),
            crate::storage::EntityKind::TableRow {
                table: std::sync::Arc::from(RUNTIME_INDEX_REGISTRY_COLLECTION),
                row_id: 0,
            },
            crate::storage::EntityData::Row(crate::storage::RowData {
                columns: Vec::new(),
                named: Some(
                    [
                        (
                            "collection".to_string(),
                            reddb_types::Value::text(index.collection.clone()),
                        ),
                        (
                            "name".to_string(),
                            reddb_types::Value::text(index.name.clone()),
                        ),
                        (
                            "columns".to_string(),
                            reddb_types::Value::text(index.columns.join("\u{1f}")),
                        ),
                        (
                            "method".to_string(),
                            reddb_types::Value::text(index_method_kind_as_str(index.method)),
                        ),
                        (
                            "resolution".to_string(),
                            reddb_types::Value::Integer(i64::from(index_method_kind_resolution(
                                index.method,
                            ))),
                        ),
                        (
                            "unique".to_string(),
                            reddb_types::Value::Boolean(index.unique),
                        ),
                        ("dropped".to_string(), reddb_types::Value::Boolean(false)),
                    ]
                    .into_iter()
                    .collect(),
                ),
                schema: None,
            }),
        );
        store
            .insert_auto(RUNTIME_INDEX_REGISTRY_COLLECTION, entity)
            .map(|_| ())
            .map_err(|err| RedDBError::Internal(format!("{err:?}")))
    }

    pub(crate) fn persist_runtime_index_drop(
        &self,
        collection: &str,
        name: &str,
    ) -> RedDBResult<()> {
        let store = self.inner.db.store();
        let _ = store.get_or_create_collection(RUNTIME_INDEX_REGISTRY_COLLECTION);
        let entity = crate::storage::UnifiedEntity::new(
            crate::storage::EntityId::new(0),
            crate::storage::EntityKind::TableRow {
                table: std::sync::Arc::from(RUNTIME_INDEX_REGISTRY_COLLECTION),
                row_id: 0,
            },
            crate::storage::EntityData::Row(crate::storage::RowData {
                columns: Vec::new(),
                named: Some(
                    [
                        (
                            "collection".to_string(),
                            reddb_types::Value::text(collection.to_string()),
                        ),
                        (
                            "name".to_string(),
                            reddb_types::Value::text(name.to_string()),
                        ),
                        ("dropped".to_string(), reddb_types::Value::Boolean(true)),
                    ]
                    .into_iter()
                    .collect(),
                ),
                schema: None,
            }),
        );
        store
            .insert_auto(RUNTIME_INDEX_REGISTRY_COLLECTION, entity)
            .map(|_| ())
            .map_err(|err| RedDBError::Internal(format!("{err:?}")))
    }

    fn reserve_index_scan(
        &self,
        manager: &crate::storage::unified::SegmentManager,
    ) -> RedDBResult<(super::memory_admission::QueryBufferReservation, usize)> {
        let mut read_bytes_max =
            usize::try_from(self.memory_accounting().budget().resolved_bytes / 16)
                .unwrap_or(usize::MAX)
                .max(1);
        // Decode, indexed projection and descriptor reduction share this owned
        // scratch. Resident-only visitors borrow rows already charged to the
        // arena. Disk-backed visitors also retain overlay identities; reserve
        // for bounded stores even before a checkpoint installs their backing.
        let overlay_bytes = if manager.has_backing()
            || self
                .inner
                .db
                .store()
                .config()
                .resident_memory_bytes_max
                .is_some()
        {
            usize::try_from(manager.resident_bytes()).unwrap_or(usize::MAX)
        } else {
            0
        };
        // The configured allowance is a ceiling, not a minimum allocation.
        // Reduce owned scratch under pressure, but reserve it before scanning
        // and pass the effective limit to every reader and projection. Keep a
        // 64 KiB floor (or the smaller configured ceiling) to bound retries.
        let read_bytes_min = read_bytes_max.min(64 * 1024);
        loop {
            let scratch = read_bytes_max
                .saturating_mul(4)
                .saturating_add(overlay_bytes);
            if let Some(reservation) = self.try_reserve_query_buffer(scratch) {
                return Ok((reservation, read_bytes_max));
            }
            if read_bytes_max == read_bytes_min {
                let accounting = self.memory_accounting();
                return Err(RedDBError::InvalidOperation(format!(
                    "index restoration workspace exceeds the shared memory budget: requested {scratch} bytes (overlay {overlay_bytes}), used {}, budget {}",
                    accounting.total_used_bytes(), accounting.budget().resolved_bytes
                )));
            }
            read_bytes_max = (read_bytes_max / 2).max(read_bytes_min);
        }
    }

    /// Two bounded passes: estimate the final resident indexes, admit their
    /// growth, then build privately. Failure never publishes an incomplete index.
    /// This is still a resident secondary-index provider, not lazy disk indexing.
    fn restore_index_collection(
        &self,
        collection: &str,
        manager: &crate::storage::unified::SegmentManager,
        registered: Vec<super::index_store::RegisteredIndex>,
    ) -> RedDBResult<()> {
        let (_scratch, read_bytes_max) = self.reserve_index_scan(manager)?;
        let _metadata = self.admit_non_evictable_growth(
            crate::storage::memory_pools::MemoryPool::IndexMemory,
            "index restoration metadata",
            (registered.len() as u64).saturating_mul(4096),
        )?;
        let prepared = super::index_store::IndexStore::new();
        for index in registered {
            prepared
                .create_index(
                    &index.name,
                    collection,
                    &index.columns,
                    index.method,
                    // HASH postings include superseded committed versions.
                    // Snapshot-aware runtime admission enforces logical UNIQUE,
                    // as it does for BTREE's multi-valued equality pocket.
                    index.unique
                        && !matches!(index.method, super::index_store::IndexMethodKind::Hash),
                    &[],
                )
                .map_err(RedDBError::Internal)?;
            prepared.register(index);
        }
        let columns = prepared.indexed_columns_set_with_parents(collection);
        let mut growth = 0u64;
        for_each_index_row(manager, &columns, read_bytes_max, |_, fields| {
            growth = growth.saturating_add(prepared.estimate_insert_growth(
                collection,
                std::iter::once(fields),
                false,
            ));
            Ok(())
        })?;
        let _entries = self.admit_non_evictable_growth(
            crate::storage::memory_pools::MemoryPool::IndexMemory,
            "index restoration entries",
            growth,
        )?;
        for_each_index_row(manager, &columns, read_bytes_max, |id, fields| {
            prepared
                .index_entity_insert(collection, id, fields)
                .map_err(RedDBError::Internal)
        })?;
        self.inner
            .index_store
            .replace_collection_from(collection, prepared);
        Ok(())
    }

    pub(crate) fn rehydrate_runtime_index_registry(&self) -> RedDBResult<()> {
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(RUNTIME_INDEX_REGISTRY_COLLECTION) else {
            return Ok(());
        };
        // A cold registry is physical data too. Reduce by durable entity ID,
        // without copying or sorting its append-only descriptor history.
        let (mut registry_workspace, read_bytes_max) = self.reserve_index_scan(&manager)?;
        let mut latest = std::collections::HashMap::<
            _,
            (u64, Option<super::index_store::RegisteredIndex>),
        >::new();
        let mut retained_bytes = 0usize;
        let mut failure = None;
        manager.try_for_each_entity(read_bytes_max, |entity| {
            if crate::storage::unified::memory_size::entity_bytes(entity) > read_bytes_max {
                failure = Some(RedDBError::InvalidOperation(
                    "index descriptor exceeds the reserved restoration workspace".into(),
                ));
                return false;
            }
            // A short delimited string can expand to many owned column names.
            // Bound that expansion before collecting the descriptor's Vec.
            if let crate::storage::EntityData::Row(row) = &entity.data {
                if let Some(reddb_types::Value::Text(columns)) = row.get_field("columns") {
                    let expanded = columns
                        .split('\u{1f}')
                        .filter(|part| !part.is_empty())
                        .map(|column| column.len().saturating_add(64))
                        .fold(0usize, usize::saturating_add);
                    if expanded > read_bytes_max {
                        failure = Some(RedDBError::InvalidOperation(
                            "index descriptor columns exceed the reserved restoration workspace"
                                .into(),
                        ));
                        return false;
                    }
                }
            }
            let Some((key, descriptor)) = index_descriptor(entity) else {
                return true;
            };
            if latest
                .get(&key)
                .is_some_and(|(id, _)| *id >= entity.id.raw())
            {
                return true;
            }
            let descriptor_bytes =
                |key: &(String, String), index: &Option<super::index_store::RegisteredIndex>| {
                    256usize
                        .saturating_add(key.0.len())
                        .saturating_add(key.1.len())
                        .saturating_add(index.as_ref().map_or(0, |index| {
                            index.name.len()
                                + index.collection.len()
                                + index
                                    .columns
                                    .iter()
                                    .map(|column| column.len() + 64)
                                    .sum::<usize>()
                        }))
                };
            let before = latest
                .get(&key)
                .map_or(0, |(_, index)| descriptor_bytes(&key, index));
            let after = descriptor_bytes(&key, &descriptor);
            retained_bytes = retained_bytes.saturating_sub(before).saturating_add(after);
            if retained_bytes > read_bytes_max {
                failure = Some(RedDBError::InvalidOperation(
                    "index descriptor registry exceeds the reserved restoration workspace".into(),
                ));
                return false;
            }
            latest.insert(key, (entity.id.raw(), descriptor));
            true
        })?;
        if let Some(error) = failure {
            return Err(error);
        }
        let mut collections = std::collections::BTreeMap::<String, Vec<_>>::new();
        for (_, index) in latest.into_values() {
            if let Some(index) = index {
                collections
                    .entry(index.collection.clone())
                    .or_default()
                    .push(index);
            }
        }
        // The visitor, decoder and overlay identities are gone. Keep the
        // conservative descriptor/grouping allowance, but return unused scan
        // scratch before admitting each collection's replacement indexes.
        registry_workspace.shrink_to(retained_bytes.saturating_mul(4));
        for (collection, indices) in collections {
            let topology_lock = self.inner.index_store.collection_topology_lock(&collection);
            let _topology_guard = topology_lock.write();
            let Some(manager) = store.get_collection(&collection) else {
                continue;
            };
            self.restore_index_collection(&collection, &manager, indices)?;
        }
        self.invalidate_plan_cache();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{EntityData, EntityId, RowData, UnifiedEntity};
    use reddb_types::Value;

    #[test]
    fn resident_index_backfill_borrows_payloads_and_still_enforces_workspace_budget() {
        const MIB: u64 = 1024 * 1024;
        let runtime = RedDBRuntime::with_options(
            crate::api::RedDBOptions::in_memory().with_memory_budget(64 * MIB),
        )
        .expect("resident runtime");
        runtime
            .execute_query("CREATE TABLE eager_scan_payloads (id INTEGER, payload BLOB)")
            .expect("table");
        runtime
            .execute_query("CREATE INDEX eager_scan_id ON eager_scan_payloads (id) USING BTREE")
            .expect("index");
        let manager = runtime
            .db()
            .store()
            .get_collection("eager_scan_payloads")
            .expect("source manager");
        let schema = std::sync::Arc::new(vec!["id".into(), "payload".into()]);
        for id in 1..=32 {
            let mut entity = UnifiedEntity::table_row(
                EntityId::new(1000 + id),
                "eager_scan_payloads",
                id,
                vec![],
            );
            entity.data = EntityData::Row(RowData {
                columns: vec![
                    Value::Integer(id as i64),
                    Value::Blob(vec![id as u8; MIB as usize]),
                ],
                named: None,
                schema: Some(std::sync::Arc::clone(&schema)),
            });
            manager.insert(entity).expect("source row");
        }
        assert!(!manager.has_backing(), "resident-only source");
        let resident_bytes = manager.resident_bytes();
        assert!(resident_bytes >= 32 * MIB);
        assert!(
            resident_bytes < 40 * MIB,
            "compact recovered row payloads plus zone bounds: {resident_bytes}"
        );
        runtime
            .rebuild_runtime_indexes_for_table("eager_scan_payloads")
            .expect("backfill must not reserve a second copy of borrowed source payloads");

        let held = runtime
            .try_reserve_query_buffer(16 * MIB as usize)
            .expect("independent retained query workspace fits");
        runtime
            .rebuild_runtime_indexes_for_table("eager_scan_payloads")
            .expect("backfill adapts its owned read workspace to remaining headroom");
        drop(held);
        drop(
            runtime
                .try_reserve_query_buffer(0)
                .expect("refresh accounting"),
        );
        let available = runtime.memory_accounting().budget().resolved_bytes
            - runtime.memory_accounting().total_used_bytes();
        // The registry's preferred decode scratch plus one minimum backfill
        // scratch fits, but retaining both leaves no room for index metadata.
        // Once decoding finishes, only the owned descriptors need to stay
        // charged while the collection's complete replacement is built.
        let registry_headroom = 16 * MIB + 256 * 1024 + 1;
        assert!(available > registry_headroom);
        let held = runtime
            .try_reserve_query_buffer((available - registry_headroom) as usize)
            .expect("independent workspace leaves room for a complete registry rebuild");
        runtime
            .rehydrate_runtime_index_registry()
            .expect("completed registry decoding must release unused scratch before backfill");
        drop(held);
        drop(
            runtime
                .try_reserve_query_buffer(0)
                .expect("refresh after registry rebuild"),
        );
        let available = runtime.memory_accounting().budget().resolved_bytes
            - runtime.memory_accounting().total_used_bytes();
        assert!(available > 32 * 1024);
        let held = runtime
            .try_reserve_query_buffer((available - 32 * 1024) as usize)
            .expect("retain all but a small amount of actual headroom");
        let error = runtime
            .rebuild_runtime_indexes_for_table("eager_scan_payloads")
            .expect_err("minimum scan scratch still competes with retained query workspace");
        assert!(error.to_string().contains("workspace exceeds"), "{error}");
        drop(held);
        let result = runtime
            .execute_query("SELECT id FROM eager_scan_payloads WHERE id = 17")
            .expect("prior complete index survives denied rebuild");
        assert_eq!(result.result.records.len(), 1);
        assert_eq!(
            result.result.records[0].get("id"),
            Some(&Value::Integer(17))
        );
        runtime
            .rebuild_runtime_indexes_for_table("eager_scan_payloads")
            .expect("retry after the owned query workspace is released");
    }
}
