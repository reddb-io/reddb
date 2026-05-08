//! Hot-reload watcher for the server config file.
//!
//! Watches the config file (default `/etc/reddb/config.json`, override
//! via `REDDB_CONFIG_FILE`) for changes and applies hot-reloadable keys
//! to `red_config` without a server restart.
//!
//! **Hot-reloadable:** `red.logging.*`, `slow_query.*`, `disk_space.critical_pct`.
//!
//! **Restart-required:** everything else. Detected changes to non-hot-reloadable
//! fields emit `OperatorEvent::ConfigChangeRequiresRestart` and are NOT applied.
//! Hot-reloadable fields in the same reload ARE applied.
//!
//! Atomicity: if JSON parse fails, nothing is applied (parse-then-apply).
//!
//! Linux: inotify on the parent directory catches atomic rename-swaps
//! (vim's default save pattern: write temp → `rename(2)` → `IN_MOVED_TO`).
//! Non-Linux: falls back to a 5-second poll loop.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::serde_json::Value as JsonValue;
use crate::storage::UnifiedStore;

const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Fields updatable in a live process without restart.
const HOT_RELOAD_WHITELIST: &[&str] = &[
    "red.logging.level",
    "red.logging.format",
    "red.logging.keep_days",
    "red.logging.dir",
    "red.logging.file_prefix",
    "slow_query.threshold_ms",
    "slow_query.sample_pct",
    "disk_space.critical_pct",
];

/// Background watcher that hot-reloads the server config file on change.
pub struct ConfigWatcher {
    path: PathBuf,
    store: Arc<UnifiedStore>,
}

impl ConfigWatcher {
    pub fn new(path: impl Into<PathBuf>, store: Arc<UnifiedStore>) -> Self {
        Self {
            path: path.into(),
            store,
        }
    }

    /// Spawn the watcher as a detached background thread (lives until
    /// process exit — no cancellation handle needed).
    pub fn spawn(self) -> std::thread::JoinHandle<()> {
        std::thread::Builder::new()
            .name("reddb-config-watcher".into())
            .spawn(move || run(self.path, self.store))
            .expect("config watcher thread spawn")
    }
}

fn run(path: PathBuf, store: Arc<UnifiedStore>) {
    #[cfg(target_os = "linux")]
    {
        if run_inotify(&path, &store) {
            return;
        }
        // inotify_init1 or inotify_add_watch failed — fall through to poll.
    }
    run_poll(&path, &store);
}

// ---------------------------------------------------------------------------
// Linux: inotify path (catches atomic rename-swaps)
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn run_inotify(path: &std::path::Path, store: &UnifiedStore) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let fd = unsafe { libc::inotify_init1(libc::O_CLOEXEC) };
    if fd < 0 {
        return false;
    }

    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| d.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let file_name = match path.file_name() {
        Some(n) => n.to_os_string(),
        None => {
            unsafe { libc::close(fd) };
            return false;
        }
    };

    let dir_cstr = match CString::new(dir.as_os_str().as_bytes()) {
        Ok(s) => s,
        Err(_) => {
            unsafe { libc::close(fd) };
            return false;
        }
    };

    // Watch for both direct writes and atomic renames (vim).
    let mask = libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO;
    let wd = unsafe { libc::inotify_add_watch(fd, dir_cstr.as_ptr(), mask) };
    if wd < 0 {
        unsafe { libc::close(fd) };
        return false;
    }

    // Blocking read loop — owns the fd until process exit or read error.
    let mut buf = vec![0u8; 4096];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            break;
        }
        let mut offset = 0usize;
        let n = n as usize;
        while offset + 16 <= n {
            // inotify_event layout: wd(4) mask(4) cookie(4) len(4) name(len)
            let len = u32::from_ne_bytes([
                buf[offset + 12],
                buf[offset + 13],
                buf[offset + 14],
                buf[offset + 15],
            ]) as usize;
            let name_end = offset + 16 + len;
            if name_end > n {
                break;
            }
            if len > 0 {
                let name_bytes = &buf[offset + 16..name_end];
                let nul = name_bytes
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(name_bytes.len());
                let name = std::ffi::OsStr::from_bytes(&name_bytes[..nul]);
                if name == file_name.as_os_str() {
                    apply_hot_reload(path, store);
                }
            }
            offset = name_end;
        }
    }

    unsafe { libc::close(fd) };
    true
}

// ---------------------------------------------------------------------------
// Polling fallback (non-Linux or when inotify is unavailable)
// ---------------------------------------------------------------------------

fn run_poll(path: &std::path::Path, store: &UnifiedStore) {
    let mut last_mtime: Option<std::time::SystemTime> = None;
    loop {
        std::thread::sleep(POLL_INTERVAL);
        let mtime = path.metadata().ok().and_then(|m| m.modified().ok());
        if mtime.is_some() && mtime != last_mtime {
            if last_mtime.is_some() {
                // Only reload on actual changes, not the first observation.
                apply_hot_reload(path, store);
            }
            last_mtime = mtime;
        }
    }
}

// ---------------------------------------------------------------------------
// Hot-reload logic
// ---------------------------------------------------------------------------

fn apply_hot_reload(path: &std::path::Path, store: &UnifiedStore) {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "config watcher: read failed");
            return;
        }
    };

    // Parse-then-apply: if parse fails, nothing is applied.
    let parsed: JsonValue = match crate::serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "config watcher: JSON parse failed — not applying"
            );
            return;
        }
    };
    let JsonValue::Object(_) = &parsed else {
        tracing::warn!(
            path = %path.display(),
            "config watcher: root must be JSON object — not applying"
        );
        return;
    };

    let mut flat: Vec<(String, JsonValue)> = Vec::new();
    flatten_json("", &parsed, &mut flat);

    let changed_by = format!("config_watcher::{}", path.display());
    let mut non_hot: Vec<String> = Vec::new();

    for (key, new_json) in flat {
        let new_str = json_to_str(&new_json);
        if HOT_RELOAD_WHITELIST.contains(&key.as_str()) {
            let old_str = store
                .get_config(&key)
                .as_ref()
                .map(schema_value_to_str)
                .unwrap_or_default();
            if old_str == new_str {
                continue;
            }
            store.set_config_tree(&key, &new_json);
            crate::telemetry::operator_event::OperatorEvent::ConfigChanged {
                key,
                old_value: old_str,
                new_value: new_str,
                changed_by: changed_by.clone(),
            }
            .emit_global();
        } else {
            // Non-hot-reloadable: warn if the value actually changed.
            let old_str = store
                .get_config(&key)
                .as_ref()
                .map(schema_value_to_str)
                .unwrap_or_default();
            if old_str != new_str {
                non_hot.push(key);
            }
        }
    }

    if !non_hot.is_empty() {
        crate::telemetry::operator_event::OperatorEvent::ConfigChangeRequiresRestart {
            fields_changed: non_hot.join(", "),
        }
        .emit_global();
    }
}

fn json_to_str(v: &JsonValue) -> String {
    match v {
        JsonValue::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn schema_value_to_str(v: &crate::storage::schema::Value) -> String {
    format!("{v}")
}

/// Flatten a JSON object to dotted key-value pairs (mirrors `config_overlay`).
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
        _ if !prefix.is_empty() => out.push((prefix.to_string(), value.clone())),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hot_reload_whitelist_contains_expected_keys() {
        assert!(HOT_RELOAD_WHITELIST.contains(&"red.logging.level"));
        assert!(HOT_RELOAD_WHITELIST.contains(&"red.logging.format"));
        assert!(HOT_RELOAD_WHITELIST.contains(&"slow_query.threshold_ms"));
        assert!(HOT_RELOAD_WHITELIST.contains(&"slow_query.sample_pct"));
        assert!(HOT_RELOAD_WHITELIST.contains(&"disk_space.critical_pct"));
    }

    #[test]
    fn flatten_json_produces_dotted_keys() {
        let raw = r#"{"red":{"logging":{"level":"info","format":"json"}},"slow_query":{"threshold_ms":500}}"#;
        let json: JsonValue = crate::serde_json::from_str(raw).unwrap();
        let mut flat = Vec::new();
        flatten_json("", &json, &mut flat);
        let keys: Vec<&str> = flat.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"red.logging.level"));
        assert!(keys.contains(&"red.logging.format"));
        assert!(keys.contains(&"slow_query.threshold_ms"));
    }

    #[test]
    fn flatten_json_nested_object() {
        let raw = r#"{"a":{"b":1}}"#;
        let json: JsonValue = crate::serde_json::from_str(raw).unwrap();
        let mut out = Vec::new();
        flatten_json("", &json, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "a.b");
    }

    #[test]
    fn json_to_str_unquotes_strings() {
        assert_eq!(json_to_str(&JsonValue::String("info".into())), "info");
        // Numbers and bools render without quotes
        let raw = r#"{"n":42,"b":true}"#;
        let v: JsonValue = crate::serde_json::from_str(raw).unwrap();
        if let JsonValue::Object(map) = v {
            assert_eq!(json_to_str(map.get("n").unwrap()), "42");
            assert_eq!(json_to_str(map.get("b").unwrap()), "true");
        } else {
            panic!("expected object");
        }
    }

    #[test]
    fn schema_value_to_str_uses_display() {
        use crate::storage::schema::Value;
        assert_eq!(schema_value_to_str(&Value::Integer(-1)), "-1");
        assert_eq!(schema_value_to_str(&Value::UnsignedInteger(42)), "42");
        assert_eq!(schema_value_to_str(&Value::Boolean(true)), "true");
    }
}
