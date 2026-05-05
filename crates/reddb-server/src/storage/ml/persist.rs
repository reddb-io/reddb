//! Persistence abstraction for the ML subsystem.
//!
//! The registry and job queue call into a small [`MlPersistence`]
//! trait rather than touching the storage engine directly. That
//! keeps this module independent of `RedDBRuntime` / `StorageService`
//! so it stays unit-testable and so a runtime binding can be plugged
//! in later without reshaping callers.
//!
//! The default in-crate backend is [`InMemoryMlPersistence`] — a
//! thread-safe hashmap. A future sprint adds a `RedConfigMlPersistence`
//! that writes to the `red.ml.*` KV tree so state survives restart,
//! backup, and replica sync.
//!
//! The surface is intentionally small: three namespaces
//! (`"models"`, `"model_versions"`, `"jobs"`), CRUD by string key,
//! and a list operation per namespace. All values are encoded as
//! JSON strings; the registry / queue own the schema inside each
//! value.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Errors surfaced by a persistence backend. Intentionally small —
/// callers convert into their own error types.
#[derive(Debug, Clone)]
pub enum MlPersistenceError {
    /// Underlying store returned an error. Message is backend-specific.
    Backend(String),
    /// Value was expected to parse as JSON but did not.
    Corruption(String),
}

impl std::fmt::Display for MlPersistenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MlPersistenceError::Backend(msg) => write!(f, "ml persistence backend error: {msg}"),
            MlPersistenceError::Corruption(msg) => {
                write!(f, "ml persistence value corrupted: {msg}")
            }
        }
    }
}

impl std::error::Error for MlPersistenceError {}

pub type MlPersistenceResult<T> = Result<T, MlPersistenceError>;

/// Backend-agnostic storage surface the registry and job queue use.
///
/// Implementations must be `Send + Sync` because multiple worker
/// threads touch the same instance concurrently. Writes must be
/// at-least-once durable — the caller will re-issue a write on a
/// re-transition rather than relying on partial success semantics.
pub trait MlPersistence: Send + Sync + std::fmt::Debug {
    /// Store `value` under `(namespace, key)`. Overwrites any
    /// existing value.
    fn put(&self, namespace: &str, key: &str, value: &str) -> MlPersistenceResult<()>;

    /// Fetch the value at `(namespace, key)`, if any.
    fn get(&self, namespace: &str, key: &str) -> MlPersistenceResult<Option<String>>;

    /// Drop the entry at `(namespace, key)`. Returns `Ok(())` whether
    /// the key existed or not — callers do not distinguish.
    fn delete(&self, namespace: &str, key: &str) -> MlPersistenceResult<()>;

    /// Enumerate every `(key, value)` in `namespace`. Ordering is
    /// implementation-defined. Callers that need deterministic order
    /// sort the result themselves.
    fn list(&self, namespace: &str) -> MlPersistenceResult<Vec<(String, String)>>;
}

/// Test / default backend. Pure in-memory hashmap keyed by
/// `(namespace, key)`. Cloning the handle shares state.
#[derive(Debug, Default, Clone)]
pub struct InMemoryMlPersistence {
    inner: Arc<Mutex<HashMap<(String, String), String>>>,
}

impl InMemoryMlPersistence {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(
        &self,
    ) -> MlPersistenceResult<std::sync::MutexGuard<'_, HashMap<(String, String), String>>> {
        self.inner
            .lock()
            .map_err(|_| MlPersistenceError::Backend("mutex poisoned".to_string()))
    }
}

impl MlPersistence for InMemoryMlPersistence {
    fn put(&self, namespace: &str, key: &str, value: &str) -> MlPersistenceResult<()> {
        let mut guard = self.lock()?;
        guard.insert((namespace.to_string(), key.to_string()), value.to_string());
        Ok(())
    }

    fn get(&self, namespace: &str, key: &str) -> MlPersistenceResult<Option<String>> {
        let guard = self.lock()?;
        Ok(guard
            .get(&(namespace.to_string(), key.to_string()))
            .cloned())
    }

    fn delete(&self, namespace: &str, key: &str) -> MlPersistenceResult<()> {
        let mut guard = self.lock()?;
        guard.remove(&(namespace.to_string(), key.to_string()));
        Ok(())
    }

    fn list(&self, namespace: &str) -> MlPersistenceResult<Vec<(String, String)>> {
        let guard = self.lock()?;
        Ok(guard
            .iter()
            .filter(|((ns, _), _)| ns == namespace)
            .map(|((_, k), v)| (k.clone(), v.clone()))
            .collect())
    }
}

/// Namespace names — kept as `pub const` so the registry and queue
/// modules can share them and a future backend can map them
/// onto the `red.ml.*` KV tree.
pub mod ns {
    pub const MODELS: &str = "models";
    pub const MODEL_VERSIONS: &str = "model_versions";
    pub const JOBS: &str = "jobs";
}

/// Composite key helpers. Callers build keys via these helpers so a
/// future schema migration only needs to update one place.
pub mod key {
    pub fn model(name: &str) -> String {
        name.to_string()
    }

    pub fn model_version(model: &str, version: u32) -> String {
        format!("{model}@v{version}")
    }

    pub fn job(id: u128) -> String {
        // Zero-padded hex — sort order is deterministic without extra
        // allocations, and u128 fits in 32 hex characters exactly.
        format!("{id:032x}")
    }

    /// Parse a `job(id)` key back into an id. Returns `None` on any
    /// malformed key — callers skip rather than error out so a single
    /// poisoned record cannot poison a startup sweep.
    pub fn parse_job(raw: &str) -> Option<u128> {
        if raw.len() != 32 {
            return None;
        }
        u128::from_str_radix(raw, 16).ok()
    }

    /// Parse a `model_version` key back into `(model, version)`.
    pub fn parse_model_version(raw: &str) -> Option<(String, u32)> {
        let (model, rest) = raw.rsplit_once("@v")?;
        let version = rest.parse::<u32>().ok()?;
        Some((model.to_string(), version))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_put_then_get() {
        let p = InMemoryMlPersistence::new();
        p.put("jobs", "abc", "{\"status\":\"queued\"}").unwrap();
        assert_eq!(
            p.get("jobs", "abc").unwrap().as_deref(),
            Some("{\"status\":\"queued\"}")
        );
    }

    #[test]
    fn in_memory_get_missing_returns_none() {
        let p = InMemoryMlPersistence::new();
        assert!(p.get("jobs", "nope").unwrap().is_none());
    }

    #[test]
    fn in_memory_delete_is_idempotent() {
        let p = InMemoryMlPersistence::new();
        p.delete("jobs", "missing").unwrap();
        p.put("jobs", "k", "v").unwrap();
        p.delete("jobs", "k").unwrap();
        assert!(p.get("jobs", "k").unwrap().is_none());
    }

    #[test]
    fn in_memory_list_scopes_to_namespace() {
        let p = InMemoryMlPersistence::new();
        p.put("jobs", "j1", "a").unwrap();
        p.put("jobs", "j2", "b").unwrap();
        p.put("models", "spam", "{}").unwrap();
        let mut jobs = p.list("jobs").unwrap();
        jobs.sort();
        assert_eq!(
            jobs,
            vec![
                ("j1".to_string(), "a".to_string()),
                ("j2".to_string(), "b".to_string())
            ]
        );
        assert_eq!(p.list("models").unwrap().len(), 1);
    }

    #[test]
    fn job_key_round_trips() {
        let id = 0x0123_4567_89ab_cdef_0123_4567_89ab_cdef_u128;
        let raw = key::job(id);
        assert_eq!(raw.len(), 32);
        assert_eq!(key::parse_job(&raw), Some(id));
    }

    #[test]
    fn job_key_rejects_wrong_length() {
        assert!(key::parse_job("abc").is_none());
        assert!(key::parse_job(&"0".repeat(31)).is_none());
        assert!(key::parse_job(&"0".repeat(33)).is_none());
    }

    #[test]
    fn model_version_key_round_trips() {
        let raw = key::model_version("spam_classifier", 42);
        assert_eq!(raw, "spam_classifier@v42");
        assert_eq!(
            key::parse_model_version(&raw),
            Some(("spam_classifier".to_string(), 42))
        );
    }

    #[test]
    fn model_version_key_survives_at_in_name() {
        // Model names could in theory contain '@' — rsplit_once picks
        // the *last* occurrence, which is the `@v` prefix.
        let raw = "weird@name@v7";
        assert_eq!(
            key::parse_model_version(raw),
            Some(("weird@name".to_string(), 7))
        );
    }

    #[test]
    fn model_version_key_rejects_non_numeric_version() {
        assert!(key::parse_model_version("spam@vfoo").is_none());
    }
}
