//! Shared keyed-version selection for KV-like runtime models.
//!
//! Config and Vault keep append-only rows keyed by `key`, where the
//! highest `version` is the current state and older rows form history.
//! This Module owns that common latest-version selection rule while
//! model-specific parsing, encryption, capabilities, and watch payloads
//! stay in their caller modules.

use std::collections::BTreeMap;

use crate::storage::schema::Value;
use crate::storage::{EntityId, RowData};

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

pub(super) fn latest_version<T>(versions: impl IntoIterator<Item = T>) -> Option<T>
where
    T: KeyedVersion,
{
    versions.into_iter().max_by_key(|version| version.version())
}

pub(super) fn latest_versions<T>(
    versions: impl IntoIterator<Item = T>,
    prefix: Option<&str>,
) -> Vec<T>
where
    T: KeyedVersion,
{
    let mut by_key = BTreeMap::<String, T>::new();
    for version in versions {
        let key = version.key();
        if prefix.is_some_and(|prefix| !key.starts_with(prefix)) {
            continue;
        }
        let replace = by_key
            .get(key)
            .map(|existing| version.version() > existing.version())
            .unwrap_or(true);
        if replace {
            by_key.insert(key.to_string(), version);
        }
    }
    by_key.into_values().collect()
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
        let latest = latest_version([
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
        ])
        .expect("latest");

        assert_eq!(latest.value, "new");
    }

    #[test]
    fn latest_versions_groups_by_key_and_filters_prefix() {
        let latest = latest_versions(
            [
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
                    key: "app.b",
                    version: 1,
                    value: "only-b",
                },
                TestVersion {
                    key: "other.c",
                    version: 9,
                    value: "ignored",
                },
            ],
            Some("app."),
        );

        let values: Vec<_> = latest.into_iter().map(|version| version.value).collect();
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
