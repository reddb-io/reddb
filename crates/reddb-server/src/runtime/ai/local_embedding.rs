//! Local embedding routing (#680).
//!
//! Wires the `local` AI provider into HTTP and gRPC embedding surfaces.
//! Resolves a registered+installed local model (registry from #678,
//! cache from #679), routes through a swappable
//! [`LocalEmbeddingBackend`], and returns deterministic, provider-tagged
//! embeddings.
//!
//! The default backend is the in-process [`DeterministicFakeBackend`],
//! which produces stable f32 vectors from `SHA-256(model || \0 || text)`.
//! It exists so the end-to-end contract (registry lookup → backend
//! dispatch → response shape) can be exercised without downloading a
//! real model. Real candle/onnx engines slot in by calling
//! [`install_local_embedding_backend`] at server boot.
//!
//! Errors are intentionally distinct so callers can disambiguate:
//!
//! * `FeatureNotEnabled` — `local-models` is off and no backend is
//!   installed (routes through HTTP 501 / gRPC `feature_not_enabled`).
//! * `NotFound` — the named model is not in the registry, or it is
//!   registered but not installed in the cache.
//! * `Query` — the registered task is not `embedding`, or the backend
//!   produced a shape that disagrees with the registered dimensions.

use std::sync::{Arc, OnceLock, RwLock};

use crate::crypto::sha256::Sha256;
use crate::json::{parse_json, Value as JsonValue};
use crate::runtime::RedDBRuntime;
use crate::storage::schema::Value;
use crate::storage::unified::RedDB;
use crate::{RedDBError, RedDBResult};

const RED_CONFIG_COLLECTION: &str = "red_config";
const AI_MODEL_KEY_PREFIX: &str = "red.config.ai.models.";
const STATUS_INSTALLED: &str = "installed";
const TASK_EMBEDDING: &str = "embedding";
const PROVIDER_LOCAL: &str = "local";

/// Canonical pull-policy names mirrored from the model-registry contract
/// (`crate::server::handlers_ai`). The embed path is read-side and does
/// not depend on the handler module, so these constants are duplicated
/// deliberately to keep the runtime crate free of HTTP-layer coupling.
const PULL_POLICY_NEVER: &str = "never";
const PULL_POLICY_IF_MISSING: &str = "if_missing";
const PULL_POLICY_ALWAYS: &str = "always";

/// Normalise a stored `pull_policy` value to its canonical form. Old
/// registry entries written before the rename still carry
/// `manual`/`on_demand`/`eager`; those continue to resolve to the
/// matching canonical name so existing installs keep working.
fn normalize_stored_pull_policy(raw: &str) -> &'static str {
    match raw.trim().to_ascii_lowercase().as_str() {
        "never" | "manual" => PULL_POLICY_NEVER,
        "always" | "eager" => PULL_POLICY_ALWAYS,
        // Default — anything else, including the legacy `on_demand`, is
        // treated as `if_missing` (the safest default for query-time
        // routing: never auto-acquire silently, but allow operator
        // pulls).
        _ => PULL_POLICY_IF_MISSING,
    }
}

const LOCAL_MODELS_DISABLED_MESSAGE: &str =
    "local embeddings require the `local-models` feature flag at engine build time. \
     Build with: cargo build --features local-models. Alternatively, install a backend \
     via runtime::ai::local_embedding::install_local_embedding_backend, or use the \
     'ollama' provider with a local Ollama server.";

/// Materialised view of a single embedding request handed to a backend.
#[derive(Debug, Clone)]
pub struct LocalEmbeddingRequest {
    /// Registered model name (registry key under `red.config.ai.models.{name}`).
    pub name: String,
    /// HuggingFace repo id or other source identifier (from the registry).
    pub source: String,
    /// Pinned git revision/tag from the registry.
    pub revision: String,
    /// Engine identifier from the registry (e.g. `candle`).
    pub engine: String,
    /// Output dimensionality declared at registration time.
    pub dimensions: u32,
    /// Texts to embed. Empty entries are rejected by the caller.
    pub inputs: Vec<String>,
}

/// Backend abstraction so HTTP/gRPC routing does not depend on a
/// specific model engine. A future candle/onnx backend implements this
/// trait and is installed via [`install_local_embedding_backend`].
pub trait LocalEmbeddingBackend: Send + Sync {
    fn embed(&self, request: &LocalEmbeddingRequest) -> RedDBResult<Vec<Vec<f32>>>;
}

/// Resolved local-embedding response. Carries provider/model metadata
/// the wire encoders surface to HTTP and gRPC clients.
#[derive(Debug, Clone)]
pub struct LocalEmbeddingResponse {
    pub provider: &'static str,
    pub name: String,
    pub source: String,
    pub revision: String,
    pub engine: String,
    pub dimensions: u32,
    pub embeddings: Vec<Vec<f32>>,
}

/// Deterministic, dependency-free backend used to prove the wire
/// contract end-to-end. The output of `embed(model, text, dim)` is a
/// pure function of `(model, text, dim)` — no I/O, no clocks, no RNGs
/// — so tests get byte-identical embeddings across runs.
#[derive(Debug, Default, Clone, Copy)]
pub struct DeterministicFakeBackend;

impl LocalEmbeddingBackend for DeterministicFakeBackend {
    fn embed(&self, request: &LocalEmbeddingRequest) -> RedDBResult<Vec<Vec<f32>>> {
        let dim = request.dimensions as usize;
        let mut out = Vec::with_capacity(request.inputs.len());
        for text in &request.inputs {
            out.push(deterministic_embedding(&request.name, text, dim));
        }
        Ok(out)
    }
}

fn deterministic_embedding(model: &str, text: &str, dim: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(dim);
    let mut counter: u32 = 0;
    while out.len() < dim {
        let mut hasher = Sha256::new();
        hasher.update(model.as_bytes());
        hasher.update(&[0u8]);
        hasher.update(text.as_bytes());
        hasher.update(&[0u8]);
        hasher.update(&counter.to_le_bytes());
        let digest = hasher.finalize();
        for chunk in digest.chunks(4) {
            if out.len() >= dim {
                break;
            }
            let mut bytes = [0u8; 4];
            bytes.copy_from_slice(chunk);
            let raw = u32::from_le_bytes(bytes) as f32 / u32::MAX as f32;
            // Map [0, 1] → [-1, 1) so the fake produces sign-mixed
            // vectors (the property tests look for both signs).
            out.push(raw * 2.0 - 1.0);
        }
        counter = counter.wrapping_add(1);
    }
    out
}

type BackendSlot = Arc<dyn LocalEmbeddingBackend>;

fn backend_slot() -> &'static RwLock<Option<BackendSlot>> {
    static SLOT: OnceLock<RwLock<Option<BackendSlot>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// Install (or replace) the process-global local embedding backend.
///
/// Production servers built with `--features local-models` should call
/// this once at boot with their real engine. Tests use it to swap in
/// a deterministic stub. Safe to call from any thread; the most recent
/// install wins.
pub fn install_local_embedding_backend(backend: Arc<dyn LocalEmbeddingBackend>) {
    let mut guard = backend_slot().write().expect("backend slot poisoned");
    *guard = Some(backend);
}

/// Test-only: clear the installed backend so a subsequent call exercises
/// the `FeatureNotEnabled` path again.
#[doc(hidden)]
pub fn clear_local_embedding_backend_for_tests() {
    let mut guard = backend_slot().write().expect("backend slot poisoned");
    *guard = None;
}

fn current_backend() -> Option<BackendSlot> {
    backend_slot()
        .read()
        .expect("backend slot poisoned")
        .as_ref()
        .map(Arc::clone)
}

/// Return the deterministic feature-disabled error before callers do
/// request-shape validation. Tests may install a backend without the
/// Cargo feature, so a present backend still means local embeddings are
/// available for that process.
pub fn ensure_local_embedding_available() -> RedDBResult<()> {
    if current_backend().is_none() && !cfg!(feature = "local-models") {
        return Err(RedDBError::FeatureNotEnabled(
            LOCAL_MODELS_DISABLED_MESSAGE.to_string(),
        ));
    }
    Ok(())
}

/// Resolve and run a local embedding request end-to-end.
///
/// Performs, in order:
/// 1. Backend availability gate (or feature-off error).
/// 2. Registry lookup for `name` in `red_config`.
/// 3. Task / status / engine validation.
/// 4. Backend dispatch.
/// 5. Shape validation against the registered dimensions.
pub fn embed_local(
    runtime: &RedDBRuntime,
    name: &str,
    inputs: Vec<String>,
) -> RedDBResult<LocalEmbeddingResponse> {
    embed_local_with_db(&runtime.db(), name, inputs)
}

/// Validate that a local embedding request for `name` would resolve a
/// registered+installed model and an available backend, without sending
/// any inputs.
///
/// Used by write paths (e.g. INSERT ... WITH AUTO EMBED) that need a
/// deterministic pre-flight to fail the statement before any side
/// effect on the target collection, satisfying the
/// "embedding failures leave the target collection unchanged" contract
/// for the failure modes the local provider owns: feature disabled,
/// missing model, uninstalled artifacts, unsupported task, wrong
/// provider tag, missing dimensions, corrupted registry entry.
///
/// Returns the resolved descriptor's `dimensions` so callers can pin
/// the expected output shape before any backend round-trip.
pub fn preflight_local_embedding(db: &RedDB, name: &str) -> RedDBResult<u32> {
    let name = name.trim();
    if name.is_empty() {
        return Err(RedDBError::Query(
            "local embedding 'model' field cannot be empty; pass the registered local model name"
                .to_string(),
        ));
    }

    // Mirror the backend-availability gate from `embed_local_with_db`
    // so a feature-off build fails before the write phase rather than
    // after we have already inserted rows.
    ensure_local_embedding_available()?;

    let descriptor = read_model_descriptor(db, name)?;
    if descriptor.provider != PROVIDER_LOCAL {
        return Err(RedDBError::Query(format!(
            "model '{name}' has provider '{}'; only '{PROVIDER_LOCAL}' is supported by local embedding routing",
            descriptor.provider
        )));
    }
    if descriptor.task != TASK_EMBEDDING {
        return Err(RedDBError::Query(format!(
            "model '{name}' has task '{}'; only '{TASK_EMBEDDING}' is supported by the local provider \
             (prompt/generation are out of scope)",
            descriptor.task
        )));
    }
    if descriptor.status != STATUS_INSTALLED {
        let message = match descriptor.pull_policy {
            PULL_POLICY_NEVER => format!(
                "local model '{name}' is registered (status='{}') but its artifacts are not installed; \
                 pull_policy='never' forbids runtime acquisition. An operator must explicitly install \
                 the model via `POST /ai/models/{name}/pull`.",
                descriptor.status
            ),
            PULL_POLICY_ALWAYS => format!(
                "local model '{name}' is registered (status='{}') but its artifacts are not installed; \
                 pull_policy='always' is configured but query-time auto-pull is not implemented in this slice. \
                 Trigger a refresh via `POST /ai/models/{name}/pull` before requesting embeddings.",
                descriptor.status
            ),
            _ => format!(
                "local model '{name}' is registered (status='{}') but its artifacts are not installed; \
                 pull_policy='if_missing' permits acquisition only via the explicit pull endpoint \
                 (query-time auto-pull is not implemented). Run `POST /ai/models/{name}/pull` to install.",
                descriptor.status
            ),
        };
        return Err(RedDBError::NotFound(message));
    }
    if descriptor.dimensions == 0 {
        return Err(RedDBError::Query(format!(
            "model '{name}' registry entry has dimensions=0; re-register with the model's true output width"
        )));
    }
    Ok(descriptor.dimensions)
}

/// Variant of [`embed_local`] that operates against a `RedDB` handle
/// directly. The runtime query executor only carries `&RedDB`, so the
/// text-vector-search routing path calls this rather than the runtime
/// wrapper above.
pub fn embed_local_with_db(
    db: &RedDB,
    name: &str,
    inputs: Vec<String>,
) -> RedDBResult<LocalEmbeddingResponse> {
    if inputs.is_empty() {
        return Err(RedDBError::Query(
            "at least one input is required for local embeddings".to_string(),
        ));
    }
    let name = name.trim();
    if name.is_empty() {
        return Err(RedDBError::Query(
            "local embedding 'model' field cannot be empty; pass the registered local model name"
                .to_string(),
        ));
    }

    let backend = match current_backend() {
        Some(b) => b,
        None => {
            if cfg!(feature = "local-models") {
                // Feature is on but no engine was installed by the
                // server boot path — fall back to the deterministic
                // fake so the surface stays usable in dev builds.
                let fake: Arc<dyn LocalEmbeddingBackend> = Arc::new(DeterministicFakeBackend);
                install_local_embedding_backend(Arc::clone(&fake));
                fake
            } else {
                return Err(RedDBError::FeatureNotEnabled(
                    LOCAL_MODELS_DISABLED_MESSAGE.to_string(),
                ));
            }
        }
    };

    let descriptor = read_model_descriptor(db, name)?;

    if descriptor.provider != PROVIDER_LOCAL {
        return Err(RedDBError::Query(format!(
            "model '{name}' has provider '{}'; only '{PROVIDER_LOCAL}' is supported by local embedding routing",
            descriptor.provider
        )));
    }
    if descriptor.task != TASK_EMBEDDING {
        return Err(RedDBError::Query(format!(
            "model '{name}' has task '{}'; only '{TASK_EMBEDDING}' is supported by the local provider \
             (prompt/generation are out of scope)",
            descriptor.task
        )));
    }
    if descriptor.status != STATUS_INSTALLED {
        // Operator-safe contract: query-time routing never silently
        // acquires artifacts and never falls back to a remote provider.
        // Each policy surfaces a clear, distinct error so the operator
        // knows which knob to turn.
        let message = match descriptor.pull_policy {
            PULL_POLICY_NEVER => format!(
                "local model '{name}' is registered (status='{}') but its artifacts are not installed; \
                 pull_policy='never' forbids runtime acquisition. An operator must explicitly install \
                 the model via `POST /ai/models/{name}/pull`.",
                descriptor.status
            ),
            PULL_POLICY_ALWAYS => format!(
                "local model '{name}' is registered (status='{}') but its artifacts are not installed; \
                 pull_policy='always' is configured but query-time auto-pull is not implemented in this slice. \
                 Trigger a refresh via `POST /ai/models/{name}/pull` before requesting embeddings.",
                descriptor.status
            ),
            // PULL_POLICY_IF_MISSING (default)
            _ => format!(
                "local model '{name}' is registered (status='{}') but its artifacts are not installed; \
                 pull_policy='if_missing' permits acquisition only via the explicit pull endpoint \
                 (query-time auto-pull is not implemented). Run `POST /ai/models/{name}/pull` to install.",
                descriptor.status
            ),
        };
        return Err(RedDBError::NotFound(message));
    }

    let request = LocalEmbeddingRequest {
        name: descriptor.name.clone(),
        source: descriptor.source.clone(),
        revision: descriptor.revision.clone(),
        engine: descriptor.engine.clone(),
        dimensions: descriptor.dimensions,
        inputs,
    };
    let embeddings = backend.embed(&request)?;

    if embeddings.len() != request.inputs.len() {
        return Err(RedDBError::Query(format!(
            "local backend returned {} embeddings for {} inputs",
            embeddings.len(),
            request.inputs.len()
        )));
    }
    for (idx, row) in embeddings.iter().enumerate() {
        if row.len() != descriptor.dimensions as usize {
            return Err(RedDBError::Query(format!(
                "local backend returned embedding[{idx}] of length {} but model '{name}' \
                 was registered with dimensions={}",
                row.len(),
                descriptor.dimensions
            )));
        }
    }

    Ok(LocalEmbeddingResponse {
        provider: PROVIDER_LOCAL,
        name: descriptor.name,
        source: descriptor.source,
        revision: descriptor.revision,
        engine: descriptor.engine,
        dimensions: descriptor.dimensions,
        embeddings,
    })
}

#[derive(Debug, Clone)]
struct ModelDescriptor {
    name: String,
    provider: String,
    source: String,
    revision: String,
    engine: String,
    task: String,
    status: String,
    dimensions: u32,
    /// Canonical pull policy (`never` / `if_missing` / `always`),
    /// normalised at read time so the gate logic does not need to know
    /// about legacy alias spellings.
    pull_policy: &'static str,
}

fn read_model_descriptor(db: &RedDB, name: &str) -> RedDBResult<ModelDescriptor> {
    let key = format!("{AI_MODEL_KEY_PREFIX}{name}");
    let raw = match db.get_kv(RED_CONFIG_COLLECTION, &key) {
        Some((Value::Text(text), _)) => text.to_string(),
        Some(_) => {
            return Err(RedDBError::Query(format!(
                "local model registry entry for '{name}' is not a JSON text payload"
            )));
        }
        None => {
            return Err(RedDBError::NotFound(format!(
                "local model '{name}' is not registered; POST /ai/models to register it first"
            )));
        }
    };
    let parsed = parse_json(&raw).map_err(|err| {
        RedDBError::Query(format!(
            "local model registry entry for '{name}' is corrupted: {err}"
        ))
    })?;
    let value = JsonValue::from(parsed);
    let object = value
        .as_object()
        .ok_or_else(|| RedDBError::Query(format!("model entry for '{name}' is not an object")))?;

    let pick = |key: &str| -> Option<String> {
        object
            .get(key)
            .and_then(JsonValue::as_str)
            .map(str::to_string)
    };

    let provider = pick("provider").unwrap_or_else(|| PROVIDER_LOCAL.to_string());
    let source = pick("source").unwrap_or_default();
    let revision = pick("revision").unwrap_or_default();
    let engine = pick("engine").unwrap_or_default();
    let task = pick("task").unwrap_or_default();
    let status = pick("status").unwrap_or_default();
    let dimensions = object
        .get("dimensions")
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| {
            RedDBError::Query(format!("model entry for '{name}' is missing 'dimensions'"))
        })? as u32;
    let pull_policy = normalize_stored_pull_policy(
        pick("pull_policy")
            .as_deref()
            .unwrap_or(PULL_POLICY_IF_MISSING),
    );

    Ok(ModelDescriptor {
        name: pick("name").unwrap_or_else(|| name.to_string()),
        provider,
        source,
        revision,
        engine,
        task,
        status,
        dimensions,
        pull_policy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_fake_is_pure_and_correct_length() {
        let backend = DeterministicFakeBackend;
        let req = LocalEmbeddingRequest {
            name: "mini".to_string(),
            source: "sentence-transformers/all-MiniLM-L6-v2".to_string(),
            revision: "main".to_string(),
            engine: "candle".to_string(),
            dimensions: 16,
            inputs: vec!["hello".to_string(), "world".to_string()],
        };
        let a = backend.embed(&req).expect("embed");
        let b = backend.embed(&req).expect("embed twice");
        assert_eq!(a, b, "deterministic backend must be pure");
        assert_eq!(a.len(), 2);
        assert!(a.iter().all(|v| v.len() == 16));
        assert_ne!(
            a[0], a[1],
            "different inputs must produce different vectors"
        );
    }

    #[test]
    fn deterministic_fake_changes_with_model_name() {
        let backend = DeterministicFakeBackend;
        let mk = |name: &str| LocalEmbeddingRequest {
            name: name.to_string(),
            source: String::new(),
            revision: String::new(),
            engine: String::new(),
            dimensions: 8,
            inputs: vec!["x".to_string()],
        };
        let a = backend.embed(&mk("alpha")).unwrap();
        let b = backend.embed(&mk("beta")).unwrap();
        assert_ne!(a[0], b[0]);
    }
}
