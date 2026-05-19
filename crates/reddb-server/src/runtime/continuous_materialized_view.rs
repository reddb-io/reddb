//! Continuous (materialized) view descriptor + catalog persistence
//! (issue #593, slice 9a of #575).
//!
//! Today the view registry on `RuntimeInner` is purely in-memory:
//! every `CREATE [MATERIALIZED] VIEW` lands in
//! `inner.views: RwLock<HashMap<String, Arc<CreateViewQuery>>>` and a
//! restart loses every definition. This module introduces the
//! descriptor type and the persistence layer that backs the registry
//! onto the system collection [`CATALOG_COLLECTION`]. Read / write /
//! refresh code paths are unchanged — the rehydrate hook at boot
//! repopulates the in-memory registry from the persisted rows before
//! the API opens.

use crate::api::{RedDBError, RedDBResult};
use crate::storage::schema::Value;
use crate::storage::unified::entity::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};

/// Name of the system collection that stores one row per
/// materialized-view definition. Bootstrapped alongside the other
/// keyed system collections (`red.config`, `red.vault`) at boot.
pub const CATALOG_COLLECTION: &str = "red_materialized_view_defs";

/// Persisted shape of a single `CREATE MATERIALIZED VIEW`.
///
/// The descriptor stores the original SQL source so the body AST can
/// be recovered by re-parsing at boot — this avoids embedding a
/// version-dependent AST serialization in the on-disk catalog, and
/// keeps the rehydrate path symmetric with the user-facing CREATE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedViewDescriptor {
    /// View name as declared in `CREATE MATERIALIZED VIEW <name>`.
    pub name: String,
    /// Verbatim SQL source of the `CREATE MATERIALIZED VIEW`
    /// statement. The rehydrate path re-parses this string to recover
    /// the body AST.
    pub source_sql: String,
    /// Source collections referenced by the view body — populated
    /// from `collect_table_refs(&q.query)` at creation time.
    pub source_collections: Vec<String>,
    /// `REFRESH EVERY <duration>` clause in milliseconds, or `None`
    /// for refresh-on-demand views.
    pub refresh_every_ms: Option<u64>,
    /// `WITH RETENTION <duration>` clause in milliseconds, or `None`
    /// when no retention policy was declared on the view.
    pub retention_duration_ms: Option<u64>,
}

impl MaterializedViewDescriptor {
    /// Build a row entity suitable for `insert_auto(CATALOG_COLLECTION, …)`.
    /// Each descriptor field maps to one named column on the row, in
    /// the same shape `red_config` uses for its key/value entries —
    /// keeps the storage layout introspectable from SQL without a
    /// JSON parser.
    fn to_row_entity(&self) -> UnifiedEntity {
        let mut named = std::collections::HashMap::new();
        named.insert("name".to_string(), Value::text(self.name.clone()));
        named.insert(
            "source_sql".to_string(),
            Value::text(self.source_sql.clone()),
        );
        named.insert(
            "source_collections".to_string(),
            Value::Array(
                self.source_collections
                    .iter()
                    .map(|s| Value::text(s.clone()))
                    .collect(),
            ),
        );
        named.insert(
            "refresh_every_ms".to_string(),
            match self.refresh_every_ms {
                Some(ms) => Value::UnsignedInteger(ms),
                None => Value::Null,
            },
        );
        named.insert(
            "retention_duration_ms".to_string(),
            match self.retention_duration_ms {
                Some(ms) => Value::UnsignedInteger(ms),
                None => Value::Null,
            },
        );
        UnifiedEntity::new(
            EntityId::new(0),
            EntityKind::TableRow {
                table: std::sync::Arc::from(CATALOG_COLLECTION),
                row_id: 0,
            },
            EntityData::Row(RowData {
                columns: Vec::new(),
                named: Some(named),
                schema: None,
            }),
        )
    }
}

/// Decode a stored row back into a descriptor. Returns `None` for
/// rows that are missing the required `name` / `source_sql` columns
/// — boot-time rehydrate logs and continues so a single malformed
/// entry does not block startup.
pub(crate) fn decode_row(entity: &UnifiedEntity) -> Option<MaterializedViewDescriptor> {
    let EntityData::Row(row) = &entity.data else {
        return None;
    };
    let named = row.named.as_ref()?;
    let name = match named.get("name")? {
        Value::Text(s) => s.to_string(),
        _ => return None,
    };
    let source_sql = match named.get("source_sql")? {
        Value::Text(s) => s.to_string(),
        _ => return None,
    };
    let source_collections = match named.get("source_collections") {
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(|v| match v {
                Value::Text(s) => Some(s.to_string()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let refresh_every_ms = match named.get("refresh_every_ms") {
        Some(Value::UnsignedInteger(ms)) => Some(*ms),
        Some(Value::Integer(ms)) if *ms >= 0 => Some(*ms as u64),
        _ => None,
    };
    let retention_duration_ms = match named.get("retention_duration_ms") {
        Some(Value::UnsignedInteger(ms)) => Some(*ms),
        Some(Value::Integer(ms)) if *ms >= 0 => Some(*ms as u64),
        _ => None,
    };
    Some(MaterializedViewDescriptor {
        name,
        source_sql,
        source_collections,
        refresh_every_ms,
        retention_duration_ms,
    })
}

/// Persist a descriptor to the catalog collection, replacing any
/// prior rows with the same `name`. Idempotent: re-persisting an
/// existing view (e.g. `CREATE OR REPLACE`) leaves exactly one row
/// behind, so the catalog never accumulates duplicates across
/// repeated definition churn.
pub(crate) fn persist_descriptor(
    store: &crate::storage::unified::UnifiedStore,
    descriptor: &MaterializedViewDescriptor,
) -> RedDBResult<()> {
    let _ = store.get_or_create_collection(CATALOG_COLLECTION);
    remove_by_name(store, &descriptor.name)?;
    let entity = descriptor.to_row_entity();
    store
        .insert_auto(CATALOG_COLLECTION, entity)
        .map_err(|err| {
            RedDBError::Internal(format!(
                "persist materialized-view descriptor {}: {err}",
                descriptor.name
            ))
        })?;
    Ok(())
}

/// Remove every row whose `name` column matches `name`. Used on
/// `DROP MATERIALIZED VIEW` and as the first half of
/// [`persist_descriptor`]'s upsert semantics.
pub(crate) fn remove_by_name(
    store: &crate::storage::unified::UnifiedStore,
    name: &str,
) -> RedDBResult<()> {
    let Some(manager) = store.get_collection(CATALOG_COLLECTION) else {
        return Ok(());
    };
    let mut to_delete: Vec<EntityId> = Vec::new();
    for entity in manager.query_all(|_| true) {
        let EntityData::Row(row) = &entity.data else {
            continue;
        };
        let Some(named) = &row.named else { continue };
        if let Some(Value::Text(stored)) = named.get("name") {
            if stored.as_ref() == name {
                to_delete.push(entity.id);
            }
        }
    }
    for id in to_delete {
        store.delete(CATALOG_COLLECTION, id).map_err(|err| {
            RedDBError::Internal(format!(
                "delete materialized-view descriptor row for {name}: {err}"
            ))
        })?;
    }
    Ok(())
}

/// Read every persisted descriptor. Returns an empty vector when the
/// catalog collection doesn't exist (fresh datadir / first boot).
pub(crate) fn load_all(
    store: &crate::storage::unified::UnifiedStore,
) -> Vec<MaterializedViewDescriptor> {
    let Some(manager) = store.get_collection(CATALOG_COLLECTION) else {
        return Vec::new();
    };
    manager
        .query_all(|_| true)
        .iter()
        .filter_map(decode_row)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_entity_roundtrips_through_decode() {
        let d = MaterializedViewDescriptor {
            name: "v".into(),
            source_sql: "CREATE MATERIALIZED VIEW v AS SELECT * FROM t".into(),
            source_collections: vec!["t".into(), "u".into()],
            refresh_every_ms: Some(60_000),
            retention_duration_ms: Some(7 * 24 * 3_600_000),
        };
        let entity = d.to_row_entity();
        let back = decode_row(&entity).expect("decode");
        assert_eq!(d, back);
    }

    #[test]
    fn null_options_decode_to_none() {
        let d = MaterializedViewDescriptor {
            name: "v".into(),
            source_sql: "CREATE MATERIALIZED VIEW v AS SELECT 1".into(),
            source_collections: vec![],
            refresh_every_ms: None,
            retention_duration_ms: None,
        };
        let entity = d.to_row_entity();
        let back = decode_row(&entity).expect("decode");
        assert!(back.refresh_every_ms.is_none());
        assert!(back.retention_duration_ms.is_none());
    }
}
