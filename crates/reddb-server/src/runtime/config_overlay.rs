//! Boot-time config overlay.
//!
//! Resolves perf / durability / concurrency config in the following
//! precedence (highest wins):
//!
//! 1. `REDDB_<MATRIX_KEY_UPPERCASED_WITH_DOTS_AS_UNDERSCORES>` env vars
//!    — in-memory only, re-read every boot, never persisted to
//!    red_config. Designed for hot-fix ("restart with
//!    `REDDB_DURABILITY_MODE=async` to trade safety for speed").
//!
//! 2. Mounted config file at `/etc/reddb/config.json` (override via
//!    `REDDB_CONFIG_FILE=<path>`) — parsed once on boot, values
//!    written into red_config with write-if-absent semantics so a
//!    later `SET CONFIG` by the user always wins.
//!
//! 3. Persisted `red_config` rows — values the user set via `SET
//!    CONFIG` in a previous session.
//!
//! 4. Hard-coded defaults from the `config_matrix::MATRIX`.
//!
//! Tiers 2, 3, 4 are all read through the same red_config collection
//! (tiers 2 + 4 are seeded there on boot). Tier 1 sits in an
//! in-memory map; readers must consult that map first.
//!
//! Env var mapping is restricted to keys declared in the matrix:
//! `durability.mode` → `REDDB_DURABILITY_MODE`. Unknown env vars are
//! ignored — prevents typos from silently leaking into the runtime.

use std::collections::HashMap;
use std::path::Path;

use crate::serde_json::Value as JsonValue;
use crate::storage::UnifiedStore;

use super::config_matrix::{default_for, MATRIX};

/// Scan the process environment for every matrix-declared key and
/// return the `{dotted_key → raw string value}` overrides. Values are
/// kept as strings; the reader coerces to the target type the same
/// way `config_bool` / `config_u64` / `config_string` already do.
pub fn collect_env_overrides() -> HashMap<String, String> {
    let mut out = HashMap::new();
    for entry in MATRIX {
        let env_name = env_name_for(entry.key);
        if let Ok(raw) = std::env::var(&env_name) {
            if !raw.is_empty() {
                out.insert(entry.key.to_string(), raw);
            }
        }
    }
    out
}

/// Construct the env-var name for a matrix key.
/// `storage.btree.lehman_yao` → `REDDB_STORAGE_BTREE_LEHMAN_YAO`.
pub fn env_name_for(key: &str) -> String {
    format!("REDDB_{}", key.to_ascii_uppercase().replace('.', "_"))
}

/// Resolve the config-file path. `REDDB_CONFIG_FILE` wins, else
/// `/etc/reddb/config.json` (the container convention).
pub fn config_file_path() -> String {
    std::env::var("REDDB_CONFIG_FILE").unwrap_or_else(|_| "/etc/reddb/config.json".to_string())
}

/// Read the mounted config file (if present) and seed its keys into
/// red_config with write-if-absent semantics. Returns `Ok(n)` with
/// the number of keys written. Missing file = silent 0. Malformed
/// file logs a warning and returns 0 — boot never fails on a bad
/// overlay file.
pub fn apply_config_file(store: &UnifiedStore, path: &str) -> usize {
    let p = Path::new(path);
    if !p.exists() {
        return 0;
    }
    let raw = match std::fs::read_to_string(p) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(path = %path, error = %err, "reading config overlay file");
            return 0;
        }
    };
    let parsed: JsonValue = match crate::serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(
                path = %path,
                error = %err,
                "parsing config overlay file as JSON — ignoring"
            );
            return 0;
        }
    };
    // Accept only a top-level object. Other shapes (array, scalar)
    // make no sense for a config overlay.
    let JsonValue::Object(_) = &parsed else {
        tracing::warn!(
            path = %path,
            "config overlay must be a JSON object — ignoring"
        );
        return 0;
    };

    let mut written = 0;
    let mut flat: Vec<(String, JsonValue)> = Vec::new();
    flatten_json("", &parsed, &mut flat);
    for (key, value) in flat {
        if key_already_present(store, &key) {
            continue;
        }
        store.set_config_tree(&key, &value);
        written += 1;
    }
    written
}

/// Single-pass presence check so `apply_config_file` doesn't rescan
/// red_config for every key.
fn key_already_present(store: &UnifiedStore, key: &str) -> bool {
    let Some(manager) = store.get_collection("red_config") else {
        return false;
    };
    let mut found = false;
    manager.for_each_entity(|entity| {
        if let Some(row) = entity.data.as_row() {
            if let Some(crate::storage::schema::Value::Text(s)) = row.get_field("key") {
                if s.as_ref() == key {
                    found = true;
                    return false;
                }
            }
        }
        true
    });
    found
}

/// Flatten a JSON object into `{dotted_key → leaf_value}` pairs.
/// Mirrors the `flatten_config_json` helper in the storage crate.
fn flatten_json(prefix: &str, value: &JsonValue, out: &mut Vec<(String, JsonValue)>) {
    match value {
        JsonValue::Object(map) => {
            for (k, v) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten_json(&key, v, out);
            }
        }
        _ if !prefix.is_empty() => {
            out.push((prefix.to_string(), value.clone()));
        }
        _ => {
            // A root-level non-object was rejected upstream; this arm
            // shouldn't be reached.
        }
    }
}

/// Coerce a raw env-var string into the matrix-declared default's
/// type. Returns `None` when the key is unknown to the matrix or the
/// coercion fails; the caller falls back to the persisted value.
pub fn coerce_env_value(key: &str, raw: &str) -> Option<crate::storage::schema::Value> {
    use crate::storage::schema::Value;

    let default = default_for(key)?;
    match default {
        JsonValue::Bool(_) => match raw.to_ascii_lowercase().as_str() {
            "true" | "1" | "on" | "yes" => Some(Value::Boolean(true)),
            "false" | "0" | "off" | "no" => Some(Value::Boolean(false)),
            _ => None,
        },
        JsonValue::Number(n) => {
            if n.fract().abs() < f64::EPSILON {
                raw.parse::<u64>().ok().map(Value::UnsignedInteger)
            } else {
                raw.parse::<f64>().ok().map(Value::Float)
            }
        }
        JsonValue::String(_) => Some(Value::text(raw.to_string())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_name_follows_convention() {
        assert_eq!(env_name_for("durability.mode"), "REDDB_DURABILITY_MODE");
        assert_eq!(
            env_name_for("storage.btree.lehman_yao"),
            "REDDB_STORAGE_BTREE_LEHMAN_YAO"
        );
        assert_eq!(
            env_name_for("storage.bulk_insert.max_buffered_rows"),
            "REDDB_STORAGE_BULK_INSERT_MAX_BUFFERED_ROWS"
        );
    }

    #[test]
    fn coerce_bool_accepts_common_forms() {
        use crate::storage::schema::Value;
        assert_eq!(
            coerce_env_value("concurrency.locking.enabled", "true"),
            Some(Value::Boolean(true))
        );
        assert_eq!(
            coerce_env_value("concurrency.locking.enabled", "FALSE"),
            Some(Value::Boolean(false))
        );
        assert_eq!(
            coerce_env_value("concurrency.locking.enabled", "1"),
            Some(Value::Boolean(true))
        );
        assert_eq!(
            coerce_env_value("concurrency.locking.enabled", "off"),
            Some(Value::Boolean(false))
        );
        assert!(coerce_env_value("concurrency.locking.enabled", "maybe").is_none());
    }

    #[test]
    fn coerce_number_rejects_garbage() {
        use crate::storage::schema::Value;
        assert_eq!(
            coerce_env_value("storage.wal.max_interval_ms", "25"),
            Some(Value::UnsignedInteger(25))
        );
        assert!(coerce_env_value("storage.wal.max_interval_ms", "fast").is_none());
    }

    #[test]
    fn unknown_key_returns_none() {
        assert!(coerce_env_value("nonexistent.key", "42").is_none());
    }
}
