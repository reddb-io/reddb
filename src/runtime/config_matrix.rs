//! Performance / operational config matrix.
//!
//! Two tiers:
//!
//! - **Tier A (`Critical`)** — self-healing on boot. If the key is
//!   missing from `red_config`, the loader writes the default in.
//!   Operators always see these via `SHOW CONFIG` so they know what
//!   guarantees and tuning they have.
//! - **Tier B (`Optional`)** — in-memory default. Never self-populated.
//!   Appears in `SHOW CONFIG` only after an explicit `SET CONFIG`.
//!
//! The matrix is the single source of truth for perf / durability /
//! concurrency / storage keys introduced by the perf-parity push.
//! It intentionally does **not** cover the pre-existing `red.*`
//! trees (ai, server, storage, search, etc.) — those have their own
//! lifecycle in `impl_core`. Keys here live under the new
//! `durability.*`, `concurrency.*`, `storage.*` namespaces.

use crate::serde_json::Value as JsonValue;
use crate::storage::UnifiedStore;

#[inline]
fn num(v: f64) -> JsonValue {
    JsonValue::Number(v)
}

#[inline]
fn text(s: &str) -> JsonValue {
    JsonValue::String(s.to_string())
}

/// Default value encoded as JSON so the loader can delegate to
/// `set_config_tree` which already handles every `Value` variant.
#[derive(Debug, Clone)]
pub struct ConfigDefault {
    pub key: &'static str,
    pub tier: Tier,
    /// Lazily produced JSON default. A closure because `bgwriter.delay_ms`
    /// etc. are unsigned and `serde_json::Value::from(u64)` is fine, but
    /// we want the option of composing richer defaults later.
    pub default: fn() -> JsonValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Self-healing on boot. Always visible in `SHOW CONFIG`.
    Critical,
    /// In-memory default. Only visible in `SHOW CONFIG` after user writes.
    Optional,
}

/// The full matrix. Keep sorted by namespace for readability.
pub const MATRIX: &[ConfigDefault] = &[
    // durability.*
    ConfigDefault {
        key: "durability.mode",
        tier: Tier::Critical,
        default: || text("sync"),
    },
    // concurrency.*
    ConfigDefault {
        key: "concurrency.locking.enabled",
        tier: Tier::Critical,
        default: || JsonValue::Bool(true),
    },
    ConfigDefault {
        key: "concurrency.locking.deadlock_timeout_ms",
        tier: Tier::Optional,
        default: || num(5000.0),
    },
    // storage.wal.*
    ConfigDefault {
        key: "storage.wal.max_interval_ms",
        tier: Tier::Critical,
        default: || num(10.0),
    },
    ConfigDefault {
        key: "storage.wal.min_batch_size",
        tier: Tier::Optional,
        default: || num(4.0),
    },
    // storage.bgwriter.*
    ConfigDefault {
        key: "storage.bgwriter.delay_ms",
        tier: Tier::Critical,
        default: || num(200.0),
    },
    ConfigDefault {
        key: "storage.bgwriter.max_pages_per_round",
        tier: Tier::Optional,
        default: || num(100.0),
    },
    ConfigDefault {
        key: "storage.bgwriter.lru_multiplier",
        tier: Tier::Optional,
        default: || num(2.0),
    },
    // storage.bulk_insert.*
    ConfigDefault {
        key: "storage.bulk_insert.max_buffered_rows",
        tier: Tier::Optional,
        default: || num(1000.0),
    },
    ConfigDefault {
        key: "storage.bulk_insert.max_buffered_bytes",
        tier: Tier::Optional,
        default: || num(65536.0),
    },
    // storage.hot_update.*
    ConfigDefault {
        key: "storage.hot_update.max_chain_hops",
        tier: Tier::Optional,
        default: || num(32.0),
    },
    // storage.btree.*
    ConfigDefault {
        key: "storage.btree.lehman_yao",
        tier: Tier::Critical,
        default: || JsonValue::Bool(true),
    },
];

/// Fetch the JSON default for a matrix key. Returns `None` when the
/// key is not in the matrix (callers should treat that as a
/// programming error — unknown key, unknown tier, unknown semantics).
pub fn default_for(key: &str) -> Option<JsonValue> {
    MATRIX
        .iter()
        .find(|entry| entry.key == key)
        .map(|entry| (entry.default)())
}

/// Tier lookup — useful for tests and for introspection commands
/// that want to report whether a key is expected to self-heal.
pub fn tier_for(key: &str) -> Option<Tier> {
    MATRIX
        .iter()
        .find(|entry| entry.key == key)
        .map(|entry| entry.tier)
}

/// Boot-time self-healing pass: for every `Tier::Critical` key, if
/// `red_config` does not already contain the key, write the default
/// in. Idempotent — re-running produces no writes.
///
/// `Tier::Optional` keys are never touched here; they stay
/// transparent-default until a user `SET CONFIG` elevates them.
pub fn heal_critical_keys(store: &UnifiedStore) {
    // `set_config_tree` dot-splits the key and stores one row per
    // leaf, so we handle each matrix entry individually.
    for entry in MATRIX {
        if entry.tier != Tier::Critical {
            continue;
        }
        if is_key_present(store, entry.key) {
            continue;
        }
        store.set_config_tree(entry.key, &(entry.default)());
    }
}

/// Lightweight presence probe. Avoids loading the whole red_config
/// collection; scans until the first hit.
fn is_key_present(store: &UnifiedStore, key: &str) -> bool {
    let Some(manager) = store.get_collection("red_config") else {
        return false;
    };
    let mut found = false;
    manager.for_each_entity(|entity| {
        if let Some(row) = entity.data.as_row() {
            let entry_key = row.get_field("key").and_then(|v| match v {
                crate::storage::schema::Value::Text(s) => Some(s.as_ref()),
                _ => None,
            });
            if entry_key == Some(key) {
                found = true;
                return false; // short-circuit
            }
        }
        true
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_matrix_entry_has_a_default_that_resolves() {
        for entry in MATRIX {
            let value = (entry.default)();
            assert!(
                !matches!(value, JsonValue::Null),
                "matrix key {} has a null default, defeats self-heal",
                entry.key
            );
        }
    }

    #[test]
    fn critical_keys_cover_the_core_guarantees() {
        // This list is a tripwire — if someone drops one of these
        // from Tier A without updating callers, the test catches it.
        let required_critical = [
            "durability.mode",
            "concurrency.locking.enabled",
            "storage.wal.max_interval_ms",
            "storage.bgwriter.delay_ms",
            "storage.btree.lehman_yao",
        ];
        for key in required_critical {
            assert_eq!(
                tier_for(key),
                Some(Tier::Critical),
                "{key} must be a Tier A (Critical) key",
            );
        }
    }

    #[test]
    fn optional_keys_are_not_self_healed() {
        let must_be_optional = [
            "concurrency.locking.deadlock_timeout_ms",
            "storage.wal.min_batch_size",
            "storage.bgwriter.max_pages_per_round",
            "storage.bgwriter.lru_multiplier",
            "storage.bulk_insert.max_buffered_rows",
            "storage.bulk_insert.max_buffered_bytes",
            "storage.hot_update.max_chain_hops",
        ];
        for key in must_be_optional {
            assert_eq!(tier_for(key), Some(Tier::Optional), "{key} tier mismatch");
        }
    }

    #[test]
    fn unknown_key_returns_none() {
        assert!(default_for("nonexistent.key").is_none());
        assert!(tier_for("nonexistent.key").is_none());
    }
}
