//! Shared keyed-version selection for KV-like runtime models.
//!
//! Config and Vault keep append-only rows keyed by `key`, where the
//! highest `version` is the current state and older rows form history.
//! This Module owns that common latest-version selection rule while
//! model-specific parsing, encryption, capabilities, and watch payloads
//! stay in their caller modules.

use std::collections::BTreeMap;

use super::execution_context::SnapshotContext;
use crate::storage::{EntityId, Metadata, RowData, SegmentManager, UnifiedEntity};
use crate::{RedDBError, RedDBResult};
use reddb_engine::storage::segment_key_lookup::SegmentKeyLookup;
use reddb_types::Value;

pub(super) trait KeyedVersion {
    fn key(&self) -> &str;
    fn version(&self) -> i64;
}

#[derive(Clone)]
pub(super) struct KeyedRowVersion {
    pub id: EntityId,
    pub key: String,
    pub version: i64,
    pub value: Value,
    pub tombstone: bool,
    pub created_at_ms: i64,
    pub op: String,
}

impl KeyedVersion for KeyedRowVersion {
    fn key(&self) -> &str {
        &self.key
    }

    fn version(&self) -> i64 {
        self.version
    }
}

pub(super) fn row_version(
    id: EntityId,
    row: &RowData,
    fallback_version: i64,
) -> Option<KeyedRowVersion> {
    let Some(Value::Text(key)) = row.get_field("key") else {
        return None;
    };
    Some(KeyedRowVersion {
        id,
        key: key.to_string(),
        version: value_i64(row.get_field("version")).unwrap_or(fallback_version),
        value: row.get_field("value").cloned().unwrap_or(Value::Null),
        tombstone: matches!(row.get_field("tombstone"), Some(Value::Boolean(true))),
        created_at_ms: value_i64(row.get_field("created_at_ms")).unwrap_or(0),
        op: match row.get_field("op") {
            Some(Value::Text(value)) => value.to_string(),
            _ => "put".to_string(),
        },
    })
}

/// Point reads preserve the last entry among equal highest versions.
pub(super) fn select_latest_version<T: KeyedVersion>(latest: &mut Option<T>, version: T) {
    if latest
        .as_ref()
        .is_none_or(|previous| version.version() >= previous.version())
    {
        *latest = Some(version);
    }
}

/// Lists preserve the first entry among equal highest versions. Point reads
/// use their existing last-equal selection rule independently.
pub(super) fn retain_latest_version<T: KeyedVersion>(by_key: &mut BTreeMap<String, T>, version: T) {
    let key = version.key();
    if by_key
        .get(key)
        .is_none_or(|existing| version.version() > existing.version())
    {
        by_key.insert(key.to_string(), version);
    }
}

pub(super) fn history_versions<T>(mut versions: Vec<T>) -> Vec<T>
where
    T: KeyedVersion,
{
    versions.sort_by_key(|version| version.version());
    versions
}

pub(super) fn value_i64(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Integer(value)) => Some(*value),
        Some(Value::UnsignedInteger(value)) => i64::try_from(*value).ok(),
        Some(Value::Timestamp(value)) => Some(*value),
        Some(Value::Duration(value)) => Some(*value),
        _ => None,
    }
}

/// The caller fences publication for the cursor capture and iteration. A cold
/// source yields one decoded physical record with its frozen metadata. Resident
/// reads retain the existing segment scan; filter before cloning metadata.
/// `None` requests physical history, not the current-visibility fallback.
type KeyedRecordCursor<'manager> =
    Box<dyn Iterator<Item = RedDBResult<(UnifiedEntity, Option<Metadata>)>> + 'manager>;

pub(super) fn records_with_metadata<'manager>(
    manager: &'manager SegmentManager,
    snapshot: Option<SnapshotContext>,
    key: Option<&'manager str>,
    filter: impl Fn(&UnifiedEntity) -> bool + Sync + 'manager,
) -> RedDBResult<KeyedRecordCursor<'manager>> {
    let cold = match (key, snapshot.as_ref()) {
        (Some(key), Some(_)) => manager.try_paged_key_entity_cursor(key, snapshot.clone())?,
        (Some(key), None) => manager.try_paged_key_physical_cursor(key)?,
        (None, Some(_)) => manager.try_paged_entity_cursor(snapshot.clone())?,
        (None, None) => manager.try_paged_physical_cursor()?,
    };
    if let Some(cursor) = cold {
        return Ok(Box::new(cursor.filter_map(move |record| match record {
            Ok((entity, metadata)) if filter(&entity) => Some(Ok((entity, metadata))),
            Ok(_) => None,
            Err(error) => Some(Err(RedDBError::from(error))),
        })));
    }
    let filter = move |entity: &UnifiedEntity| {
        key.is_none_or(|key| SegmentKeyLookup::key(entity) == Some(key)) && filter(entity)
    };
    let entities = match snapshot.as_ref() {
        Some(snapshot) => manager.scan(Some(snapshot), filter),
        None => manager.query_all(filter),
    };
    Ok(Box::new(entities.into_iter().map(move |entity| {
        let metadata = manager.get_metadata(entity.id);
        Ok((entity, metadata))
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Debug, PartialEq, Eq)]
    struct TestVersion {
        key: &'static str,
        version: i64,
        value: &'static str,
    }

    impl KeyedVersion for TestVersion {
        fn key(&self) -> &str {
            self.key
        }

        fn version(&self) -> i64 {
            self.version
        }
    }

    #[test]
    fn latest_version_picks_highest_version_for_one_key() {
        let mut latest = None;
        for version in [
            TestVersion {
                key: "a",
                version: 1,
                value: "old",
            },
            TestVersion {
                key: "a",
                version: 3,
                value: "new",
            },
            TestVersion {
                key: "a",
                version: 2,
                value: "middle",
            },
            TestVersion {
                key: "a",
                version: 3,
                value: "last-equal",
            },
        ] {
            select_latest_version(&mut latest, version);
        }
        let latest = latest.expect("latest");

        assert_eq!(latest.value, "last-equal");
    }

    #[test]
    fn retain_latest_version_groups_by_key_and_keeps_first_equal() {
        let mut latest = BTreeMap::new();
        for version in [
            TestVersion {
                key: "app.a",
                version: 1,
                value: "old-a",
            },
            TestVersion {
                key: "app.a",
                version: 2,
                value: "new-a",
            },
            TestVersion {
                key: "app.a",
                version: 2,
                value: "equal-must-keep-first",
            },
            TestVersion {
                key: "app.b",
                version: 1,
                value: "only-b",
            },
            TestVersion {
                key: "app.a",
                version: 1,
                value: "older-after-current",
            },
        ] {
            retain_latest_version(&mut latest, version);
        }
        let values: Vec<_> = latest.into_values().map(|version| version.value).collect();
        assert_eq!(values, vec!["new-a", "only-b"]);
    }

    #[test]
    fn row_version_extracts_common_keyed_fields() {
        let mut row = RowData::new(Vec::new());
        row.named = Some(HashMap::from([
            ("key".to_string(), Value::text("app.secret")),
            ("value".to_string(), Value::text("sealed")),
            ("version".to_string(), Value::Integer(7)),
            ("tombstone".to_string(), Value::Boolean(true)),
            ("op".to_string(), Value::text("delete")),
            ("created_at_ms".to_string(), Value::Integer(42)),
        ]));

        let version = row_version(EntityId::new(99), &row, 0).expect("keyed row");

        assert_eq!(version.id.raw(), 99);
        assert_eq!(version.key, "app.secret");
        assert_eq!(version.version, 7);
        assert_eq!(version.value, Value::text("sealed"));
        assert!(version.tombstone);
        assert_eq!(version.op, "delete");
        assert_eq!(version.created_at_ms, 42);
    }

    #[test]
    fn history_versions_sorts_by_version() {
        let versions = history_versions(vec![
            TestVersion {
                key: "a",
                version: 3,
                value: "new",
            },
            TestVersion {
                key: "a",
                version: 1,
                value: "old",
            },
            TestVersion {
                key: "a",
                version: 2,
                value: "middle",
            },
        ]);

        let values: Vec<_> = versions.into_iter().map(|version| version.value).collect();
        assert_eq!(values, vec!["old", "middle", "new"]);
    }
}
