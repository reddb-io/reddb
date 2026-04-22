//! Model registry — catalogues models, their immutable versions, and
//! which version is currently active.
//!
//! Versions are never mutated in place. Every `CREATE MODEL` /
//! `ALTER MODEL` allocates a new version number (monotonic per model
//! name) and stores its weights + metadata. The `active_version`
//! pointer is the only mutable piece — rollback is a single pointer
//! swap.
//!
//! Persistence: the registry is a pure in-memory structure in this
//! sprint. A future sprint wires it to `red_config` KV so state
//! survives restart. Keeping the storage backend behind this façade
//! means the runtime API won't change when we plug persistence in.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use super::jobs::now_ms;
use super::persist::{key, ns, MlPersistence};
use crate::json::{Map, Value as JsonValue};

/// Error surface for registry operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelRegistryError {
    /// The model name is not registered.
    UnknownModel(String),
    /// The (model, version) pair is not registered.
    UnknownVersion { model: String, version: u32 },
    /// `USE VERSION` pointed at an archived version.
    VersionArchived { model: String, version: u32 },
    /// Internal mutex poisoned — should not happen in practice, but
    /// the surface needs a variant because we never panic on poison.
    LockPoisoned,
    /// Wrapped error from the persistence backend.
    Backend(String),
}

impl std::fmt::Display for ModelRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelRegistryError::UnknownModel(name) => {
                write!(f, "unknown model '{name}'")
            }
            ModelRegistryError::UnknownVersion { model, version } => {
                write!(f, "unknown version {version} for model '{model}'")
            }
            ModelRegistryError::VersionArchived { model, version } => {
                write!(
                    f,
                    "version {version} of model '{model}' is archived; unarchive before use"
                )
            }
            ModelRegistryError::LockPoisoned => write!(f, "ml registry lock poisoned"),
            ModelRegistryError::Backend(msg) => write!(f, "ml registry backend: {msg}"),
        }
    }
}

impl std::error::Error for ModelRegistryError {}

impl<T> From<PoisonError<T>> for ModelRegistryError {
    fn from(_: PoisonError<T>) -> Self {
        ModelRegistryError::LockPoisoned
    }
}

/// Snapshot of a single model version.
///
/// The `weights_blob` and `metrics_json` / `hyperparams_json` are
/// opaque to the registry — the caller decides encoding. SHA-256
/// fingerprints let callers detect drift / corruption without
/// re-materialising the full blob.
#[derive(Debug, Clone)]
pub struct ModelVersion {
    pub model: String,
    pub version: u32,
    pub weights_blob: Vec<u8>,
    pub hyperparams_json: String,
    pub metrics_json: String,
    /// SHA-256 of the training dataset's bytes — reproducibility check.
    pub training_data_hash: Option<String>,
    /// Raw SQL used to produce the training dataset.
    pub training_sql: Option<String>,
    /// `Some(n)` when this version was fine-tuned from version `n`.
    pub parent_version: Option<u32>,
    /// Epoch millis.
    pub created_at_ms: u64,
    /// Free-form caller identifier (user name, session id, etc.).
    pub created_by: Option<String>,
    pub archived: bool,
}

/// Summary row suitable for `SELECT * FROM ML_MODELS_DASHBOARD`.
#[derive(Debug, Clone)]
pub struct ModelSummary {
    pub name: String,
    pub active_version: Option<u32>,
    pub total_versions: usize,
    pub archived_versions: usize,
}

#[derive(Debug)]
struct ModelState {
    versions: Vec<ModelVersion>,
    active_version: Option<u32>,
}

/// Thread-safe registry with optional durable backend.
///
/// Cloning is cheap — the `Arc`s shared. Mutations flush to the
/// attached [`MlPersistence`] (when present) so state survives
/// restart. Without a backend the registry is pure in-memory, which
/// is what the unit tests use.
#[derive(Clone)]
pub struct ModelRegistry {
    inner: Arc<Mutex<HashMap<String, ModelState>>>,
    backend: Option<Arc<dyn MlPersistence>>,
}

impl Default for ModelRegistry {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            backend: None,
        }
    }
}

impl std::fmt::Debug for ModelRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelRegistry")
            .field("has_backend", &self.backend.is_some())
            .finish()
    }
}

impl ModelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a registry that persists mutations to `backend` and
    /// rehydrates from any existing entries. Callers typically
    /// construct this once at startup via the runtime helper.
    pub fn with_backend(backend: Arc<dyn MlPersistence>) -> Self {
        let registry = Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            backend: Some(backend),
        };
        // Best-effort rehydrate — persistence errors become `Default`
        // state rather than failing startup. Operators see empty
        // registry + a logged error on next mutation.
        let _ = registry.load_from_backend();
        registry
    }

    /// Read every persisted model + version back into memory. Called
    /// implicitly by [`Self::with_backend`]; also exposed so tests
    /// can assert round-trip.
    pub fn load_from_backend(&self) -> Result<(), ModelRegistryError> {
        let Some(backend) = self.backend.as_ref() else {
            return Ok(());
        };
        let model_rows = backend
            .list(ns::MODELS)
            .map_err(|e| ModelRegistryError::Backend(e.to_string()))?;
        let version_rows = backend
            .list(ns::MODEL_VERSIONS)
            .map_err(|e| ModelRegistryError::Backend(e.to_string()))?;
        let mut guard = self.inner.lock()?;
        guard.clear();
        for (key, raw) in model_rows {
            let active = decode_model_active(&raw);
            guard.insert(
                key,
                ModelState {
                    versions: Vec::new(),
                    active_version: active,
                },
            );
        }
        for (k, raw) in version_rows {
            let Some((model, _)) = key::parse_model_version(&k) else {
                continue;
            };
            let Some(version) = ModelVersion::from_json(&raw) else {
                continue;
            };
            let state = guard.entry(model).or_insert_with(|| ModelState {
                versions: Vec::new(),
                active_version: None,
            });
            state.versions.push(version);
        }
        Ok(())
    }

    fn persist_model(&self, name: &str, active: Option<u32>) {
        if let Some(backend) = self.backend.as_ref() {
            let raw = encode_model_active(active);
            let _ = backend.put(ns::MODELS, &key::model(name), &raw);
        }
    }

    fn persist_version(&self, version: &ModelVersion) {
        if let Some(backend) = self.backend.as_ref() {
            let raw = version.to_json();
            let _ = backend.put(
                ns::MODEL_VERSIONS,
                &key::model_version(&version.model, version.version),
                &raw,
            );
        }
    }

    /// Register a new version of `model`. Returns the new version
    /// number, which is always `max(existing) + 1`.
    ///
    /// The registered version becomes the active one automatically
    /// unless `make_active = false` — callers who want to validate
    /// before publishing pass `false` and later call
    /// [`Self::set_active_version`].
    pub fn register_version(
        &self,
        model: impl Into<String>,
        mut version: ModelVersion,
        make_active: bool,
    ) -> Result<u32, ModelRegistryError> {
        let name = model.into();
        let mut guard = self.inner.lock()?;
        let state = guard.entry(name.clone()).or_insert_with(|| ModelState {
            versions: Vec::new(),
            active_version: None,
        });
        let next_version = state
            .versions
            .iter()
            .map(|v| v.version)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        version.model = name.clone();
        version.version = next_version;
        version.archived = false;
        if version.created_at_ms == 0 {
            version.created_at_ms = now_ms();
        }
        state.versions.push(version.clone());
        if make_active {
            state.active_version = Some(next_version);
        }
        let active_snapshot = state.active_version;
        drop(guard);
        self.persist_version(&version);
        self.persist_model(&name, active_snapshot);
        Ok(next_version)
    }

    /// Point the `active_version` at `version`. Errors if the version
    /// does not exist or is archived.
    pub fn set_active_version(&self, model: &str, version: u32) -> Result<(), ModelRegistryError> {
        let mut guard = self.inner.lock()?;
        let state = guard
            .get_mut(model)
            .ok_or_else(|| ModelRegistryError::UnknownModel(model.to_string()))?;
        let found = state.versions.iter().find(|v| v.version == version).ok_or(
            ModelRegistryError::UnknownVersion {
                model: model.to_string(),
                version,
            },
        )?;
        if found.archived {
            return Err(ModelRegistryError::VersionArchived {
                model: model.to_string(),
                version,
            });
        }
        state.active_version = Some(version);
        drop(guard);
        self.persist_model(model, Some(version));
        Ok(())
    }

    /// Flag a version as archived. Archived versions remain readable
    /// (so offline audits work) but cannot be made active. If the
    /// archived version was the active one, `active_version` is
    /// cleared — operator must explicitly pick a new active version.
    pub fn archive_version(&self, model: &str, version: u32) -> Result<(), ModelRegistryError> {
        let mut guard = self.inner.lock()?;
        let state = guard
            .get_mut(model)
            .ok_or_else(|| ModelRegistryError::UnknownModel(model.to_string()))?;
        let entry = state
            .versions
            .iter_mut()
            .find(|v| v.version == version)
            .ok_or(ModelRegistryError::UnknownVersion {
                model: model.to_string(),
                version,
            })?;
        entry.archived = true;
        let entry_snapshot = entry.clone();
        if state.active_version == Some(version) {
            state.active_version = None;
        }
        let active_snapshot = state.active_version;
        drop(guard);
        self.persist_version(&entry_snapshot);
        self.persist_model(model, active_snapshot);
        Ok(())
    }

    /// Fetch a specific version. Clones because callers typically
    /// need an owned blob.
    pub fn get_version(
        &self,
        model: &str,
        version: u32,
    ) -> Result<ModelVersion, ModelRegistryError> {
        let guard = self.inner.lock()?;
        let state = guard
            .get(model)
            .ok_or_else(|| ModelRegistryError::UnknownModel(model.to_string()))?;
        state
            .versions
            .iter()
            .find(|v| v.version == version)
            .cloned()
            .ok_or(ModelRegistryError::UnknownVersion {
                model: model.to_string(),
                version,
            })
    }

    /// Fetch the currently-active version of `model`, if one is set.
    pub fn get_active(&self, model: &str) -> Result<Option<ModelVersion>, ModelRegistryError> {
        let guard = self.inner.lock()?;
        let Some(state) = guard.get(model) else {
            return Err(ModelRegistryError::UnknownModel(model.to_string()));
        };
        let Some(active) = state.active_version else {
            return Ok(None);
        };
        Ok(state.versions.iter().find(|v| v.version == active).cloned())
    }

    /// List every version of `model`, oldest first.
    pub fn list_versions(&self, model: &str) -> Result<Vec<ModelVersion>, ModelRegistryError> {
        let guard = self.inner.lock()?;
        let state = guard
            .get(model)
            .ok_or_else(|| ModelRegistryError::UnknownModel(model.to_string()))?;
        let mut out = state.versions.clone();
        out.sort_by_key(|v| v.version);
        Ok(out)
    }

    /// One-line summary per model.
    pub fn summaries(&self) -> Result<Vec<ModelSummary>, ModelRegistryError> {
        let guard = self.inner.lock()?;
        let mut out: Vec<ModelSummary> = guard
            .iter()
            .map(|(name, state)| ModelSummary {
                name: name.clone(),
                active_version: state.active_version,
                total_versions: state.versions.len(),
                archived_versions: state.versions.iter().filter(|v| v.archived).count(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }
}

// ---- JSON serialisation --------------------------------------------------
//
// Each model version is persisted as a compact JSON object. The
// `weights_blob` ships as lowercase hex so the KV backend only ever
// handles UTF-8 text — avoids base64 edge cases and keeps logs
// human-readable for small models.

impl ModelVersion {
    pub fn to_json(&self) -> String {
        let mut obj = Map::new();
        obj.insert("model".to_string(), JsonValue::String(self.model.clone()));
        obj.insert(
            "version".to_string(),
            JsonValue::Number(self.version as f64),
        );
        obj.insert(
            "weights_hex".to_string(),
            JsonValue::String(hex_encode(&self.weights_blob)),
        );
        obj.insert(
            "hyperparams".to_string(),
            JsonValue::String(self.hyperparams_json.clone()),
        );
        obj.insert(
            "metrics".to_string(),
            JsonValue::String(self.metrics_json.clone()),
        );
        obj.insert(
            "training_data_hash".to_string(),
            self.training_data_hash
                .as_ref()
                .map(|s| JsonValue::String(s.clone()))
                .unwrap_or(JsonValue::Null),
        );
        obj.insert(
            "training_sql".to_string(),
            self.training_sql
                .as_ref()
                .map(|s| JsonValue::String(s.clone()))
                .unwrap_or(JsonValue::Null),
        );
        obj.insert(
            "parent_version".to_string(),
            self.parent_version
                .map(|v| JsonValue::Number(v as f64))
                .unwrap_or(JsonValue::Null),
        );
        obj.insert(
            "created_at".to_string(),
            JsonValue::Number(self.created_at_ms as f64),
        );
        obj.insert(
            "created_by".to_string(),
            self.created_by
                .as_ref()
                .map(|s| JsonValue::String(s.clone()))
                .unwrap_or(JsonValue::Null),
        );
        obj.insert("archived".to_string(), JsonValue::Bool(self.archived));
        JsonValue::Object(obj).to_string_compact()
    }

    pub fn from_json(raw: &str) -> Option<Self> {
        let parsed = crate::json::parse_json(raw).ok()?;
        let value = JsonValue::from(parsed);
        let obj = value.as_object()?;
        let model = obj.get("model")?.as_str()?.to_string();
        let version = obj.get("version")?.as_i64()? as u32;
        let weights_blob = hex_decode(obj.get("weights_hex")?.as_str()?)?;
        let hyperparams_json = obj.get("hyperparams")?.as_str()?.to_string();
        let metrics_json = obj.get("metrics")?.as_str()?.to_string();
        let training_data_hash = match obj.get("training_data_hash") {
            Some(JsonValue::String(s)) => Some(s.clone()),
            _ => None,
        };
        let training_sql = match obj.get("training_sql") {
            Some(JsonValue::String(s)) => Some(s.clone()),
            _ => None,
        };
        let parent_version = match obj.get("parent_version") {
            Some(JsonValue::Number(n)) => Some(*n as u32),
            _ => None,
        };
        let created_at_ms = obj.get("created_at")?.as_i64()? as u64;
        let created_by = match obj.get("created_by") {
            Some(JsonValue::String(s)) => Some(s.clone()),
            _ => None,
        };
        let archived = match obj.get("archived") {
            Some(JsonValue::Bool(b)) => *b,
            _ => false,
        };
        Some(ModelVersion {
            model,
            version,
            weights_blob,
            hyperparams_json,
            metrics_json,
            training_data_hash,
            training_sql,
            parent_version,
            created_at_ms,
            created_by,
            archived,
        })
    }
}

fn encode_model_active(active: Option<u32>) -> String {
    let mut obj = Map::new();
    obj.insert(
        "active".to_string(),
        active
            .map(|v| JsonValue::Number(v as f64))
            .unwrap_or(JsonValue::Null),
    );
    JsonValue::Object(obj).to_string_compact()
}

fn decode_model_active(raw: &str) -> Option<u32> {
    let parsed = crate::json::parse_json(raw).ok()?;
    let value = JsonValue::from(parsed);
    match value.as_object()?.get("active") {
        Some(JsonValue::Number(n)) => Some(*n as u32),
        _ => None,
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for chunk in s.as_bytes().chunks(2) {
        let hi = hex_nibble(chunk[0])?;
        let lo = hex_nibble(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_version() -> ModelVersion {
        ModelVersion {
            model: String::new(),
            version: 0,
            weights_blob: vec![1, 2, 3],
            hyperparams_json: "{}".into(),
            metrics_json: "{\"f1\":0.9}".into(),
            training_data_hash: None,
            training_sql: None,
            parent_version: None,
            created_at_ms: 0,
            created_by: None,
            archived: false,
        }
    }

    #[test]
    fn register_assigns_monotonic_versions() {
        let reg = ModelRegistry::new();
        let v1 = reg.register_version("m", fresh_version(), true).unwrap();
        let v2 = reg.register_version("m", fresh_version(), true).unwrap();
        let v3 = reg.register_version("m", fresh_version(), true).unwrap();
        assert_eq!((v1, v2, v3), (1, 2, 3));
    }

    #[test]
    fn new_version_becomes_active_by_default() {
        let reg = ModelRegistry::new();
        reg.register_version("m", fresh_version(), true).unwrap();
        reg.register_version("m", fresh_version(), true).unwrap();
        let active = reg.get_active("m").unwrap().unwrap();
        assert_eq!(active.version, 2);
    }

    #[test]
    fn unpublished_training_keeps_old_active_version() {
        let reg = ModelRegistry::new();
        reg.register_version("m", fresh_version(), true).unwrap();
        reg.register_version("m", fresh_version(), false).unwrap();
        assert_eq!(reg.get_active("m").unwrap().unwrap().version, 1);
    }

    #[test]
    fn set_active_version_rolls_back() {
        let reg = ModelRegistry::new();
        reg.register_version("m", fresh_version(), true).unwrap();
        reg.register_version("m", fresh_version(), true).unwrap();
        reg.set_active_version("m", 1).unwrap();
        assert_eq!(reg.get_active("m").unwrap().unwrap().version, 1);
    }

    #[test]
    fn set_active_rejects_unknown_version() {
        let reg = ModelRegistry::new();
        reg.register_version("m", fresh_version(), true).unwrap();
        let err = reg.set_active_version("m", 99).unwrap_err();
        assert!(matches!(err, ModelRegistryError::UnknownVersion { .. }));
    }

    #[test]
    fn archived_version_cannot_become_active() {
        let reg = ModelRegistry::new();
        reg.register_version("m", fresh_version(), true).unwrap();
        reg.register_version("m", fresh_version(), false).unwrap();
        reg.archive_version("m", 1).unwrap();
        let err = reg.set_active_version("m", 1).unwrap_err();
        assert!(matches!(err, ModelRegistryError::VersionArchived { .. }));
    }

    #[test]
    fn archiving_active_version_clears_pointer() {
        let reg = ModelRegistry::new();
        reg.register_version("m", fresh_version(), true).unwrap();
        reg.archive_version("m", 1).unwrap();
        assert!(reg.get_active("m").unwrap().is_none());
    }

    #[test]
    fn list_versions_returns_in_order() {
        let reg = ModelRegistry::new();
        for _ in 0..5 {
            reg.register_version("m", fresh_version(), true).unwrap();
        }
        let versions: Vec<u32> = reg
            .list_versions("m")
            .unwrap()
            .into_iter()
            .map(|v| v.version)
            .collect();
        assert_eq!(versions, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn summaries_count_archived_separately() {
        let reg = ModelRegistry::new();
        reg.register_version("m", fresh_version(), true).unwrap();
        reg.register_version("m", fresh_version(), true).unwrap();
        reg.register_version("m", fresh_version(), true).unwrap();
        reg.archive_version("m", 1).unwrap();
        let s = &reg.summaries().unwrap()[0];
        assert_eq!(s.total_versions, 3);
        assert_eq!(s.archived_versions, 1);
        assert_eq!(s.active_version, Some(3));
    }

    #[test]
    fn unknown_model_lookups_error_cleanly() {
        let reg = ModelRegistry::new();
        assert!(matches!(
            reg.get_active("nope").unwrap_err(),
            ModelRegistryError::UnknownModel(_)
        ));
        assert!(matches!(
            reg.list_versions("nope").unwrap_err(),
            ModelRegistryError::UnknownModel(_)
        ));
    }

    #[test]
    fn model_version_json_round_trips() {
        let v = ModelVersion {
            model: "spam".to_string(),
            version: 7,
            weights_blob: vec![0xde, 0xad, 0xbe, 0xef],
            hyperparams_json: "{\"lr\":0.01}".to_string(),
            metrics_json: "{\"f1\":0.93}".to_string(),
            training_data_hash: Some("abcd".to_string()),
            training_sql: Some("SELECT * FROM t".to_string()),
            parent_version: Some(6),
            created_at_ms: 1_700_000_000_000,
            created_by: Some("alice".to_string()),
            archived: false,
        };
        let round = ModelVersion::from_json(&v.to_json()).unwrap();
        assert_eq!(round.model, v.model);
        assert_eq!(round.version, v.version);
        assert_eq!(round.weights_blob, v.weights_blob);
        assert_eq!(round.hyperparams_json, v.hyperparams_json);
        assert_eq!(round.metrics_json, v.metrics_json);
        assert_eq!(round.training_data_hash, v.training_data_hash);
        assert_eq!(round.training_sql, v.training_sql);
        assert_eq!(round.parent_version, v.parent_version);
        assert_eq!(round.created_at_ms, v.created_at_ms);
        assert_eq!(round.created_by, v.created_by);
        assert_eq!(round.archived, v.archived);
    }

    #[test]
    fn backend_persists_versions_and_active_pointer() {
        use super::super::persist::InMemoryMlPersistence;
        let backend = Arc::new(InMemoryMlPersistence::new());
        let reg = ModelRegistry::with_backend(backend.clone());
        reg.register_version("m", fresh_version(), true).unwrap();
        reg.register_version("m", fresh_version(), true).unwrap();

        // Fresh registry sharing the same backend should rehydrate
        // exactly the same state.
        let reg2 = ModelRegistry::with_backend(backend);
        let active = reg2.get_active("m").unwrap().unwrap();
        assert_eq!(active.version, 2);
        let versions: Vec<u32> = reg2
            .list_versions("m")
            .unwrap()
            .into_iter()
            .map(|v| v.version)
            .collect();
        assert_eq!(versions, vec![1, 2]);
    }

    #[test]
    fn backend_rehydrate_survives_archive_then_rollback() {
        use super::super::persist::InMemoryMlPersistence;
        let backend = Arc::new(InMemoryMlPersistence::new());
        let reg = ModelRegistry::with_backend(backend.clone());
        reg.register_version("m", fresh_version(), true).unwrap();
        reg.register_version("m", fresh_version(), true).unwrap();
        reg.archive_version("m", 1).unwrap();
        reg.set_active_version("m", 2).unwrap();

        let reg2 = ModelRegistry::with_backend(backend);
        let versions = reg2.list_versions("m").unwrap();
        assert_eq!(versions.len(), 2);
        assert!(versions.iter().find(|v| v.version == 1).unwrap().archived);
        assert_eq!(reg2.get_active("m").unwrap().unwrap().version, 2);
    }

    #[test]
    fn hex_helpers_round_trip() {
        let bytes = vec![0u8, 1, 2, 3, 255, 128, 64];
        assert_eq!(hex_decode(&hex_encode(&bytes)).unwrap(), bytes);
    }

    #[test]
    fn hex_decode_rejects_odd_length_or_non_hex() {
        assert!(hex_decode("abc").is_none());
        assert!(hex_decode("zz").is_none());
    }
}
