//! Runtime index registry persistence.
//!
//! Extracted verbatim from `impl_core.rs` (impl_core slice 9/10, issue #1630).
//! Houses `rebuild_runtime_indexes_for_table`, `persist_runtime_index_descriptor`,
//! `persist_runtime_index_drop`, and `rehydrate_runtime_index_registry`.
//!
//! The private `named_*` / `index_method_kind_*` free helpers and the
//! `RUNTIME_INDEX_REGISTRY_COLLECTION` const moved here alongside their only
//! callers.
use super::impl_lifecycle::table_row_index_fields;
use super::*;

const RUNTIME_INDEX_REGISTRY_COLLECTION: &str = "red_index_registry";

fn named_text(
    named: &std::collections::HashMap<String, crate::storage::schema::Value>,
    key: &str,
) -> Option<String> {
    match named.get(key) {
        Some(crate::storage::schema::Value::Text(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn named_bool(
    named: &std::collections::HashMap<String, crate::storage::schema::Value>,
    key: &str,
) -> Option<bool> {
    match named.get(key) {
        Some(crate::storage::schema::Value::Boolean(value)) => Some(*value),
        _ => None,
    }
}

fn named_i64(
    named: &std::collections::HashMap<String, crate::storage::schema::Value>,
    key: &str,
) -> Option<i64> {
    match named.get(key) {
        Some(crate::storage::schema::Value::Integer(value)) => Some(*value),
        _ => None,
    }
}

fn index_method_kind_as_str(method: super::index_store::IndexMethodKind) -> &'static str {
    match method {
        super::index_store::IndexMethodKind::Hash => "hash",
        super::index_store::IndexMethodKind::Bitmap => "bitmap",
        super::index_store::IndexMethodKind::Spatial => "spatial",
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
        "spatial" | "rtree" => Some(super::index_store::IndexMethodKind::Spatial),
        "btree" => Some(super::index_store::IndexMethodKind::BTree),
        "h3" => Some(super::index_store::IndexMethodKind::H3 { resolution }),
        _ => None,
    }
}

impl RedDBRuntime {
    pub(crate) fn rebuild_runtime_indexes_for_table(&self, table: &str) -> RedDBResult<()> {
        let registered = self.inner.index_store.list_indices(table);
        if registered.is_empty() {
            return Ok(());
        }
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(table) else {
            return Ok(());
        };
        let entity_fields = manager
            .query_all(|entity| matches!(entity.kind, crate::storage::EntityKind::TableRow { .. }))
            .into_iter()
            .map(|entity| (entity.id, table_row_index_fields(&entity)))
            .collect::<Vec<_>>();

        for index in registered {
            self.inner.index_store.drop_index(&index.name, table);
            self.inner
                .index_store
                .create_index(
                    &index.name,
                    table,
                    &index.columns,
                    index.method,
                    index.unique,
                    &entity_fields,
                )
                .map_err(RedDBError::Internal)?;
            self.inner.index_store.register(index);
        }
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
                            crate::storage::schema::Value::text(index.collection.clone()),
                        ),
                        (
                            "name".to_string(),
                            crate::storage::schema::Value::text(index.name.clone()),
                        ),
                        (
                            "columns".to_string(),
                            crate::storage::schema::Value::text(index.columns.join("\u{1f}")),
                        ),
                        (
                            "method".to_string(),
                            crate::storage::schema::Value::text(index_method_kind_as_str(
                                index.method,
                            )),
                        ),
                        (
                            "resolution".to_string(),
                            crate::storage::schema::Value::Integer(i64::from(
                                index_method_kind_resolution(index.method),
                            )),
                        ),
                        (
                            "unique".to_string(),
                            crate::storage::schema::Value::Boolean(index.unique),
                        ),
                        (
                            "dropped".to_string(),
                            crate::storage::schema::Value::Boolean(false),
                        ),
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
                            crate::storage::schema::Value::text(collection.to_string()),
                        ),
                        (
                            "name".to_string(),
                            crate::storage::schema::Value::text(name.to_string()),
                        ),
                        (
                            "dropped".to_string(),
                            crate::storage::schema::Value::Boolean(true),
                        ),
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

    pub(crate) fn rehydrate_runtime_index_registry(&self) -> RedDBResult<()> {
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(RUNTIME_INDEX_REGISTRY_COLLECTION) else {
            return Ok(());
        };
        let mut rows = manager.query_all(|_| true);
        rows.sort_by_key(|entity| entity.id.raw());

        let mut latest = std::collections::HashMap::<
            (String, String),
            Option<super::index_store::RegisteredIndex>,
        >::new();
        for entity in rows {
            let crate::storage::EntityData::Row(row) = &entity.data else {
                continue;
            };
            let Some(named) = &row.named else {
                continue;
            };
            let Some(collection) = named_text(named, "collection") else {
                continue;
            };
            let Some(name) = named_text(named, "name") else {
                continue;
            };
            let dropped = named_bool(named, "dropped").unwrap_or(false);
            let key = (collection.clone(), name.clone());
            if dropped {
                latest.insert(key, None);
                continue;
            }
            let columns = named_text(named, "columns")
                .map(|raw| {
                    raw.split('\u{1f}')
                        .filter(|part| !part.is_empty())
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let resolution = named_i64(named, "resolution")
                .and_then(|v| u8::try_from(v).ok())
                .unwrap_or(0);
            let Some(method) = named_text(named, "method")
                .and_then(|raw| index_method_kind_from_str(&raw, resolution))
            else {
                continue;
            };
            latest.insert(
                key,
                Some(super::index_store::RegisteredIndex {
                    name,
                    collection,
                    columns,
                    method,
                    unique: named_bool(named, "unique").unwrap_or(false),
                }),
            );
        }

        for index in latest.into_values().flatten() {
            let Some(manager) = store.get_collection(&index.collection) else {
                continue;
            };
            let entity_fields = manager
                .query_all(|entity| {
                    matches!(entity.kind, crate::storage::EntityKind::TableRow { .. })
                })
                .into_iter()
                .map(|entity| (entity.id, table_row_index_fields(&entity)))
                .collect::<Vec<_>>();
            self.inner
                .index_store
                .create_index(
                    &index.name,
                    &index.collection,
                    &index.columns,
                    index.method,
                    index.unique,
                    &entity_fields,
                )
                .map_err(RedDBError::Internal)?;
            self.inner.index_store.register(index);
        }
        self.invalidate_plan_cache();
        Ok(())
    }
}
