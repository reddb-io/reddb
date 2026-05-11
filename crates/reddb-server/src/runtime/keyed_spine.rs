//! Shared keyed-version selection for KV-like runtime models.
//!
//! Config and Vault keep append-only rows keyed by `key`, where the
//! highest `version` is the current state and older rows form history.
//! This Module owns that common latest-version selection rule while
//! model-specific parsing, encryption, capabilities, and watch payloads
//! stay in their caller modules.

use std::collections::BTreeMap;

pub(super) trait KeyedVersion {
    fn key(&self) -> &str;
    fn version(&self) -> i64;
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
