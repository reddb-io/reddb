//! Issue #582 — Analytics slice 4: `BatchInsertEndpoint` HTTP support.
//!
//! Owns the config + in-memory idempotency cache that the
//! `POST /collections/:name/batch` handler uses to:
//!
//! * reject batches exceeding `red.batch.max_rows` with `413
//!   BatchTooLarge` before any storage work,
//! * deduplicate requests carrying the same `Idempotency-Key` within
//!   `red.batch.idempotency_window_secs`, returning the previously
//!   cached response body verbatim,
//! * report row-level validation failures by index so the caller can
//!   pinpoint the row that broke the all-or-nothing commit.
//!
//! The actual commit + AnalyticsSchemaRegistry validation runs in the
//! handler (`server::handlers_entity::handle_batch_insert`). This
//! module only owns the pieces that need to be unit-testable without
//! booting a full HTTP server.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const DEFAULT_MAX_ROWS: usize = 10_000;
const DEFAULT_IDEMPOTENCY_WINDOW_SECS: u64 = 300;
const ENV_MAX_ROWS: &str = "RED_BATCH_MAX_ROWS";
const ENV_IDEMPOTENCY_WINDOW_SECS: &str = "RED_BATCH_IDEMPOTENCY_WINDOW_SECS";

/// Knobs for the batch endpoint. Read from `RED_BATCH_*` env vars at
/// process start; the `red.batch.*` config keys in the brief route to
/// the same defaults until the broader config-overlay binding lands.
#[derive(Debug, Clone, Copy)]
pub struct BatchInsertConfig {
    pub max_rows: usize,
    pub idempotency_window: Duration,
}

impl BatchInsertConfig {
    pub fn from_env() -> Self {
        let max_rows = std::env::var(ENV_MAX_ROWS)
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_MAX_ROWS);
        let window_secs = std::env::var(ENV_IDEMPOTENCY_WINDOW_SECS)
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_IDEMPOTENCY_WINDOW_SECS);
        Self {
            max_rows,
            idempotency_window: Duration::from_secs(window_secs),
        }
    }
}

/// A response previously served for a given `(collection,
/// idempotency-key)`. Stored verbatim so a replay returns byte-for-byte
/// what the caller would have seen the first time, even if the
/// underlying state has since drifted.
#[derive(Debug, Clone)]
pub struct CachedResponse {
    pub status: u16,
    pub body: Vec<u8>,
    expires_at: Instant,
}

/// In-memory `(collection, idempotency-key) → CachedResponse` map.
/// Pruned lazily on every `lookup`/`store` rather than via a background
/// task — the working set is bounded by the `idempotency_window` and a
/// `Mutex` is cheap enough at the batch-insert call rate.
pub struct BatchInsertCache {
    inner: Mutex<HashMap<(String, String), CachedResponse>>,
}

impl Default for BatchInsertCache {
    fn default() -> Self {
        Self::new()
    }
}

impl BatchInsertCache {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    pub fn lookup(&self, collection: &str, key: &str, now: Instant) -> Option<CachedResponse> {
        let mut guard = self.inner.lock().ok()?;
        guard.retain(|_, v| v.expires_at > now);
        guard
            .get(&(collection.to_string(), key.to_string()))
            .cloned()
    }

    pub fn store(
        &self,
        collection: &str,
        key: &str,
        status: u16,
        body: Vec<u8>,
        window: Duration,
        now: Instant,
    ) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        guard.retain(|_, v| v.expires_at > now);
        guard.insert(
            (collection.to_string(), key.to_string()),
            CachedResponse {
                status,
                body,
                expires_at: now + window,
            },
        );
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }
}

/// Process-wide cache. The brief states the dedup window is "in-memory
/// per primary" — one process, one cache.
pub fn global_cache() -> &'static BatchInsertCache {
    static CACHE: OnceLock<BatchInsertCache> = OnceLock::new();
    CACHE.get_or_init(BatchInsertCache::new)
}

#[derive(Debug, Clone, PartialEq)]
pub enum BatchInsertError {
    /// Body did not deserialize to a JSON array.
    BodyNotJsonArray,
    /// `rows.len() > config.max_rows`.
    BatchTooLarge { limit: usize, got: usize },
    /// Parse / shape failure for row `index`.
    RowParseFailure { index: usize, reason: String },
    /// AnalyticsSchemaRegistry rejected the row at `index`.
    RowSchemaRejected { index: usize, reason: String },
}

impl BatchInsertError {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::BatchTooLarge { .. } => 413,
            _ => 400,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::BodyNotJsonArray => "BadRequest",
            Self::BatchTooLarge { .. } => "BatchTooLarge",
            Self::RowParseFailure { .. } => "RowParseFailure",
            Self::RowSchemaRejected { .. } => "RowSchemaRejected",
        }
    }

    pub fn row_index(&self) -> Option<usize> {
        match self {
            Self::RowParseFailure { index, .. } | Self::RowSchemaRejected { index, .. } => {
                Some(*index)
            }
            _ => None,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::BodyNotJsonArray => "request body must be a JSON array of rows".to_string(),
            Self::BatchTooLarge { limit, got } => {
                format!("batch size {got} exceeds red.batch.max_rows={limit}")
            }
            Self::RowParseFailure { index, reason } => {
                format!("row {index} failed validation: {reason}")
            }
            Self::RowSchemaRejected { index, reason } => {
                format!("row {index} rejected by AnalyticsSchemaRegistry: {reason}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_matches_brief() {
        let cfg = BatchInsertConfig {
            max_rows: DEFAULT_MAX_ROWS,
            idempotency_window: Duration::from_secs(DEFAULT_IDEMPOTENCY_WINDOW_SECS),
        };
        assert_eq!(cfg.max_rows, 10_000);
        assert_eq!(cfg.idempotency_window, Duration::from_secs(300));
    }

    #[test]
    fn cache_returns_stored_within_window() {
        let cache = BatchInsertCache::new();
        let now = Instant::now();
        cache.store(
            "events",
            "abc",
            200,
            b"{\"ok\":true,\"count\":3}".to_vec(),
            Duration::from_secs(60),
            now,
        );
        let hit = cache
            .lookup("events", "abc", now + Duration::from_secs(30))
            .expect("cached entry should still be live");
        assert_eq!(hit.status, 200);
        assert_eq!(hit.body, b"{\"ok\":true,\"count\":3}");
    }

    #[test]
    fn cache_evicts_after_window() {
        let cache = BatchInsertCache::new();
        let now = Instant::now();
        cache.store(
            "events",
            "abc",
            200,
            b"{}".to_vec(),
            Duration::from_secs(60),
            now,
        );
        assert!(cache
            .lookup("events", "abc", now + Duration::from_secs(61))
            .is_none());
        // Lookup after expiry also prunes the entry from the map.
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn cache_misses_for_unknown_key() {
        let cache = BatchInsertCache::new();
        assert!(cache.lookup("events", "never-stored", Instant::now()).is_none());
    }

    #[test]
    fn cache_namespaced_by_collection() {
        let cache = BatchInsertCache::new();
        let now = Instant::now();
        cache.store(
            "c1",
            "k",
            200,
            b"a".to_vec(),
            Duration::from_secs(60),
            now,
        );
        cache.store(
            "c2",
            "k",
            200,
            b"b".to_vec(),
            Duration::from_secs(60),
            now,
        );
        assert_eq!(cache.lookup("c1", "k", now).unwrap().body, b"a");
        assert_eq!(cache.lookup("c2", "k", now).unwrap().body, b"b");
    }

    #[test]
    fn batch_too_large_maps_to_413() {
        let err = BatchInsertError::BatchTooLarge {
            limit: 10,
            got: 11,
        };
        assert_eq!(err.http_status(), 413);
        assert_eq!(err.code(), "BatchTooLarge");
        assert!(err.row_index().is_none());
        assert!(err.message().contains("10"));
        assert!(err.message().contains("11"));
    }

    #[test]
    fn row_schema_rejected_carries_index() {
        let err = BatchInsertError::RowSchemaRejected {
            index: 3,
            reason: "AnalyticsSchemaError:TypeMismatch:click:v1:user_id".to_string(),
        };
        assert_eq!(err.http_status(), 400);
        assert_eq!(err.code(), "RowSchemaRejected");
        assert_eq!(err.row_index(), Some(3));
        assert!(err.message().contains("row 3"));
        assert!(err.message().contains("TypeMismatch"));
    }

    #[test]
    fn row_parse_failure_carries_index() {
        let err = BatchInsertError::RowParseFailure {
            index: 7,
            reason: "missing fields".to_string(),
        };
        assert_eq!(err.http_status(), 400);
        assert_eq!(err.row_index(), Some(7));
        assert!(err.message().contains("row 7"));
    }

    #[test]
    fn body_not_json_array_is_400() {
        let err = BatchInsertError::BodyNotJsonArray;
        assert_eq!(err.http_status(), 400);
        assert!(err.row_index().is_none());
    }
}
