//! Local AI embedding model artifact cache (#679).
//!
//! Builds on the registry from #678. Adds an explicit pull/inspect/drop
//! lifecycle for registered local HuggingFace embedding models so the
//! operator can:
//!
//! * Pull artifacts from a fixture or fake source into a RedDB-managed
//!   cache directory and write a validated manifest.
//! * Inspect cache status, footprint, and per-file checksums.
//! * Drop the cached artifacts without losing the registered metadata.
//!
//! Artifact acquisition is fixture-based here — the slice intentionally
//! does not call out to HuggingFace. A `fixture_dir` (per-request or via
//! `red.config.ai.local.fixture_dir`) supplies the bytes that simulate
//! a downloaded model, so tests stay deterministic and offline. Live
//! pull is a follow-up slice owned by the PRD.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use super::request_body::parse_json_body_allow_empty;
use super::transport::{json_error, json_response};
use crate::application::CreateKvInput;
use crate::json::{parse_json, to_string as json_to_string, Map, Value as JsonValue};
use crate::storage::schema::Value;
use crate::RedDBServer;
use reddb_file::{
    ai_model_cache_manifest_path, ai_model_cache_root, ai_model_cache_staging_dir,
    ai_model_cache_staging_root, copy_ai_model_cache_artifact, decode_ai_model_cache_manifest_json,
    drop_ai_model_cache_dir, encode_ai_model_cache_manifest_json, promote_ai_model_cache_staging,
    write_ai_model_cache_manifest, AiModelCacheManifest as Manifest,
    AiModelCacheManifestFile as ManifestFile, AI_MODEL_CACHE_MANIFEST_FILE,
};

const RED_CONFIG_COLLECTION: &str = "red_config";
const AI_MODEL_KEY_PREFIX: &str = "red.config.ai.models.";
const AI_LOCAL_CACHE_DIR_KEY: &str = "red.config.ai.local.cache_dir";
const AI_LOCAL_FIXTURE_DIR_KEY: &str = "red.config.ai.local.fixture_dir";

/// Body fields the pull endpoint must reject outright: the boundary
/// never accepts plaintext credentials. Operators stage them in the
/// vault and reference them by `credential_alias`.
const PULL_REJECTED_PLAINTEXT_FIELDS: &[&str] = &[
    "api_key",
    "apikey",
    "api_token",
    "token",
    "auth_token",
    "bearer_token",
    "password",
    "secret",
    "hf_token",
    "huggingface_token",
    "huggingface_api_key",
];

const STATUS_REGISTERED: &str = "registered";
const STATUS_INSTALLED: &str = "installed";
const STATUS_MISSING: &str = "missing";
const STATUS_UNHEALTHY: &str = "unhealthy";

impl RedDBServer {
    /// POST /ai/models/{name}/pull — install the artifact bundle into
    /// the managed cache. The request body may carry `fixture_dir` to
    /// override the configured fixture path. Returns 409 if the model
    /// is not registered, 400 if the fixture is missing, 500 on I/O.
    pub(crate) fn handle_ai_model_pull(&self, name: &str, body: Vec<u8>) -> HandlerResp {
        let name = match validate_path_name(name) {
            Ok(value) => value,
            Err(resp) => return resp,
        };

        let payload = match parse_json_body_allow_empty(&body) {
            Ok(p) => p,
            Err(resp) => return resp,
        };

        // The pull boundary must never accept a plaintext provider
        // credential. Operators stage HuggingFace tokens through the
        // vault and the model registry references them by alias.
        for field in PULL_REJECTED_PLAINTEXT_FIELDS {
            if payload.get(field).is_some() {
                return json_error(
                    400,
                    format!(
                        "field '{field}' is rejected: pull must not accept plaintext credentials. \
                         Store the secret in the vault at \
                         'red.secret.ai.providers.huggingface.tokens.{{alias}}' and \
                         reference it via the model's 'credential_alias' or pass 'credential_alias' \
                         on the pull request."
                    ),
                );
            }
        }

        let entry = match self.read_model_entry(&name) {
            Ok(Some(entry)) => entry,
            Ok(None) => {
                return json_error(404, format!("local AI model '{name}' is not registered"));
            }
            Err(err) => return json_error(500, err),
        };

        // Resolve provider credentials for the eventual live pull. The
        // alias falls back to whatever was registered on the model
        // entry; absence is allowed only for public sources. We do not
        // attach the resolved key to the response — it stays in
        // memory just long enough to authenticate the (future) HTTP
        // pull call.
        let credential_alias = payload
            .get("credential_alias")
            .and_then(JsonValue::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                entry
                    .get("credential_alias")
                    .and_then(JsonValue::as_str)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            });
        let _resolved_credential =
            match self.resolve_pull_credential(&entry, credential_alias.as_deref()) {
                Ok(value) => value,
                Err((status, message)) => return json_error(status, message),
            };

        let fixture_dir = match resolve_fixture_dir(&payload, |k| self.read_config_text(k)) {
            Ok(p) => p,
            Err(err) => return json_error(400, err),
        };
        if !fixture_dir.is_dir() {
            return json_error(
                400,
                format!(
                    "fixture_dir '{}' does not exist or is not a directory",
                    fixture_dir.display()
                ),
            );
        }

        let cache_root = match self.cache_root() {
            Ok(p) => p,
            Err(err) => return json_error(500, err),
        };
        let model_dir = cache_root.join(&name);

        let lock_key = lock_key(&cache_root, &name);
        let lock = acquire_model_lock(&lock_key);
        let _guard = lock.lock().expect("model lock poisoned");

        let manifest = match install_artifacts(&entry, &cache_root, &model_dir, &fixture_dir) {
            Ok(m) => m,
            Err(err) => return json_error(500, err),
        };

        if let Err(err) = self.stamp_installed(&name, &model_dir, &manifest) {
            return json_error(500, err);
        }

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert("name".to_string(), JsonValue::String(name));
        object.insert(
            "status".to_string(),
            JsonValue::String(STATUS_INSTALLED.into()),
        );
        object.insert(
            "cache_dir".to_string(),
            JsonValue::String(model_dir.display().to_string()),
        );
        object.insert("manifest".to_string(), manifest_to_json(&manifest));
        json_response(200, JsonValue::Object(object))
    }

    /// GET /ai/models/{name}/cache — inspect installed/missing/unhealthy.
    pub(crate) fn handle_ai_model_cache_status(&self, name: &str) -> HandlerResp {
        let name = match validate_path_name(name) {
            Ok(v) => v,
            Err(resp) => return resp,
        };
        if self.read_model_entry(&name).ok().flatten().is_none() {
            return json_error(404, format!("local AI model '{name}' is not registered"));
        }

        let cache_root = match self.cache_root() {
            Ok(p) => p,
            Err(err) => return json_error(500, err),
        };
        let model_dir = cache_root.join(&name);

        let report = inspect_cache(&model_dir);
        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert("name".to_string(), JsonValue::String(name));
        object.insert(
            "status".to_string(),
            JsonValue::String(report.status.to_string()),
        );
        object.insert(
            "cache_dir".to_string(),
            JsonValue::String(model_dir.display().to_string()),
        );
        if let Some(manifest) = report.manifest {
            object.insert("manifest".to_string(), manifest_to_json(&manifest));
        }
        if let Some(detail) = report.detail {
            object.insert("detail".to_string(), JsonValue::String(detail));
        }
        object.insert(
            "footprint_bytes".to_string(),
            JsonValue::Number(report.footprint_bytes as f64),
        );
        json_response(200, JsonValue::Object(object))
    }

    /// DELETE /ai/models/{name}/cache — drop the cached artifacts.
    /// The registry entry is preserved so the model stays registered.
    pub(crate) fn handle_ai_model_cache_drop(&self, name: &str) -> HandlerResp {
        let name = match validate_path_name(name) {
            Ok(v) => v,
            Err(resp) => return resp,
        };
        if self.read_model_entry(&name).ok().flatten().is_none() {
            return json_error(404, format!("local AI model '{name}' is not registered"));
        }

        let cache_root = match self.cache_root() {
            Ok(p) => p,
            Err(err) => return json_error(500, err),
        };
        let model_dir = cache_root.join(&name);

        let lock_key = lock_key(&cache_root, &name);
        let lock = acquire_model_lock(&lock_key);
        let _guard = lock.lock().expect("model lock poisoned");

        let removed = match drop_cache(&cache_root, &model_dir) {
            Ok(value) => value,
            Err(err) => return json_error(500, err),
        };
        if let Err(err) = self.stamp_registered(&name) {
            return json_error(500, err);
        }

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert("name".to_string(), JsonValue::String(name));
        object.insert("removed".to_string(), JsonValue::Bool(removed));
        object.insert(
            "status".to_string(),
            JsonValue::String(STATUS_REGISTERED.into()),
        );
        json_response(200, JsonValue::Object(object))
    }

    /// Resolve provider credentials for a pull request, when the
    /// registered source is a private remote (HuggingFace today). The
    /// resolver returns `Ok(None)` if no alias is set AND the source
    /// does not appear to need one; `Ok(Some(_))` when a key was
    /// successfully resolved via env/vault/legacy config; and
    /// `Err((status, message))` when the operator asked for credential
    /// resolution but the vault did not have a usable secret.
    ///
    /// The returned value is intentionally consumed by the caller and
    /// never persisted into the model entry or the HTTP response — the
    /// pull surface is the only code that should ever materialise the
    /// plaintext key.
    fn resolve_pull_credential(
        &self,
        _entry: &JsonValue,
        credential_alias: Option<&str>,
    ) -> Result<Option<String>, (u16, String)> {
        // Public HuggingFace repos do not need a key; only attempt
        // resolution when the operator opted in by setting an alias.
        // Private/gated repos still flow through this gate by virtue
        // of an alias on the model entry or the pull request body.
        let Some(alias) = credential_alias else {
            return Ok(None);
        };

        let result = crate::ai::resolve_api_key(
            &crate::ai::AiProvider::HuggingFace,
            Some(alias),
            |kv_key| {
                if kv_key.starts_with("red.secret.") {
                    return Ok(self.runtime().vault_kv_get(kv_key));
                }
                match self
                    .entity_use_cases()
                    .get_kv(RED_CONFIG_COLLECTION, kv_key)
                {
                    Ok(Some((Value::Text(secret), _))) => Ok(Some(secret.to_string())),
                    Ok(_) => Ok(None),
                    Err(err) => Err(crate::RedDBError::Query(format!(
                        "failed to read AI credential store: {err}"
                    ))),
                }
            },
        );
        match result {
            Ok(key) if !key.trim().is_empty() => Ok(Some(key)),
            Ok(_) => Err((
                400,
                format!(
                    "credential_alias '{alias}' resolved to an empty secret; store the \
                     HuggingFace token at \
                     'red.secret.ai.providers.huggingface.tokens.{alias}' before pulling"
                ),
            )),
            Err(err) => Err((
                400,
                format!(
                    "failed to resolve HuggingFace credentials for alias '{alias}': {err}. \
                     Store the token at \
                     'red.secret.ai.providers.huggingface.tokens.{alias}' in the vault."
                ),
            )),
        }
    }

    fn read_model_entry(&self, name: &str) -> Result<Option<JsonValue>, String> {
        let key = format!("{AI_MODEL_KEY_PREFIX}{name}");
        match self.entity_use_cases().get_kv(RED_CONFIG_COLLECTION, &key) {
            Ok(Some((Value::Text(text), _))) => match parse_json(&text) {
                Ok(parsed) => Ok(Some(JsonValue::from(parsed))),
                Err(err) => Err(format!("model entry for '{name}' is corrupted: {err}")),
            },
            Ok(_) => Ok(None),
            Err(err) => Err(format!("failed to read model registry: {err}")),
        }
    }

    fn read_config_text(&self, key: &str) -> Option<String> {
        match self.entity_use_cases().get_kv(RED_CONFIG_COLLECTION, key) {
            Ok(Some((Value::Text(s), _))) => {
                let trimmed = s.trim().to_string();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            }
            _ => None,
        }
    }

    fn cache_root(&self) -> Result<PathBuf, String> {
        if let Some(override_path) = self.read_config_text(AI_LOCAL_CACHE_DIR_KEY) {
            let p = PathBuf::from(override_path);
            ensure_dir(&p)?;
            return Ok(p);
        }
        let db = self.runtime().db();
        let store = db.store();
        let db_path = store.db_path();
        let base = match db_path {
            Some(p) => match p.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
                _ => std::env::temp_dir(),
            },
            None => std::env::temp_dir(),
        };
        let root = ai_model_cache_root(&base);
        ensure_dir(&root)?;
        Ok(root)
    }

    fn stamp_installed(
        &self,
        name: &str,
        cache_dir: &Path,
        manifest: &Manifest,
    ) -> Result<(), String> {
        self.rewrite_model_entry(name, |obj| {
            obj.insert(
                "status".to_string(),
                JsonValue::String(STATUS_INSTALLED.into()),
            );
            obj.insert(
                "cache_dir".to_string(),
                JsonValue::String(cache_dir.display().to_string()),
            );
            obj.insert(
                "installed_at_unix_ms".to_string(),
                JsonValue::Number(manifest.installed_at_unix_ms as f64),
            );
            obj.insert(
                "cache_size_bytes".to_string(),
                JsonValue::Number(manifest.total_size_bytes as f64),
            );
        })
    }

    fn stamp_registered(&self, name: &str) -> Result<(), String> {
        self.rewrite_model_entry(name, |obj| {
            obj.insert(
                "status".to_string(),
                JsonValue::String(STATUS_REGISTERED.into()),
            );
            obj.remove("cache_dir");
            obj.remove("installed_at_unix_ms");
            obj.remove("cache_size_bytes");
        })
    }

    fn rewrite_model_entry<F: FnOnce(&mut Map<String, JsonValue>)>(
        &self,
        name: &str,
        edit: F,
    ) -> Result<(), String> {
        let key = format!("{AI_MODEL_KEY_PREFIX}{name}");
        let raw = match self.entity_use_cases().get_kv(RED_CONFIG_COLLECTION, &key) {
            Ok(Some((Value::Text(s), _))) => s.to_string(),
            Ok(_) => return Err(format!("local AI model '{name}' is not registered")),
            Err(err) => return Err(format!("failed to read model registry: {err}")),
        };
        let parsed = parse_json(&raw)
            .map_err(|err| format!("model entry for '{name}' is corrupted: {err}"))?;
        let mut value = JsonValue::from(parsed);
        let JsonValue::Object(ref mut object) = value else {
            return Err(format!("model entry for '{name}' is not an object"));
        };
        object.insert(
            "updated_at_unix_ms".to_string(),
            JsonValue::Number(now_unix_ms() as f64),
        );
        edit(object);
        let encoded = json_to_string(&value)
            .map_err(|err| format!("failed to re-encode model entry: {err}"))?;
        let _ = self
            .entity_use_cases()
            .delete_kv(RED_CONFIG_COLLECTION, &key);
        self.entity_use_cases()
            .create_kv(CreateKvInput {
                collection: RED_CONFIG_COLLECTION.to_string(),
                key,
                value: Value::text(encoded),
                metadata: Vec::new(),
            })
            .map(|_| ())
            .map_err(|err| format!("failed to persist model update: {err}"))
    }
}

type HandlerResp = crate::server::transport::HttpResponse;

fn validate_path_name(name: &str) -> Result<String, HandlerResp> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(json_error(400, "model name path segment cannot be empty"));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(json_error(
            400,
            format!("model name '{trimmed}' contains unsupported characters"),
        ));
    }
    Ok(trimmed.to_string())
}

fn resolve_fixture_dir<F>(payload: &JsonValue, config_lookup: F) -> Result<PathBuf, String>
where
    F: FnOnce(&str) -> Option<String>,
{
    if let Some(value) = payload.get("fixture_dir").and_then(JsonValue::as_str) {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err("'fixture_dir' cannot be empty".to_string());
        }
        return Ok(PathBuf::from(trimmed));
    }
    if let Some(value) = config_lookup(AI_LOCAL_FIXTURE_DIR_KEY) {
        return Ok(PathBuf::from(value));
    }
    Err(format!(
        "no artifact source configured: provide 'fixture_dir' in the request body or set '{AI_LOCAL_FIXTURE_DIR_KEY}'; live HuggingFace pull is not implemented in this slice"
    ))
}

fn ensure_dir(path: &Path) -> Result<(), String> {
    if path.exists() {
        if !path.is_dir() {
            return Err(format!(
                "cache path '{}' exists but is not a directory",
                path.display()
            ));
        }
        return Ok(());
    }
    fs::create_dir_all(path)
        .map_err(|err| format!("failed to create directory '{}': {err}", path.display()))
}

fn install_artifacts(
    entry: &JsonValue,
    cache_root: &Path,
    model_dir: &Path,
    fixture_dir: &Path,
) -> Result<Manifest, String> {
    let staging_root = ai_model_cache_staging_root(cache_root);
    ensure_dir(&staging_root)?;
    let unique = unique_suffix();
    let name = entry
        .get("name")
        .and_then(JsonValue::as_str)
        .unwrap_or("model")
        .to_string();
    let staging_dir = ai_model_cache_staging_dir(cache_root, &name, &unique);
    if staging_dir.exists() {
        // Should not happen, but defend against suffix collision.
        let _ = fs::remove_dir_all(&staging_dir);
    }
    fs::create_dir_all(&staging_dir).map_err(|err| {
        format!(
            "failed to create staging dir '{}': {err}",
            staging_dir.display()
        )
    })?;

    // Roll back staging on any error after this point.
    let result = (|| -> Result<Manifest, String> {
        let mut files = Vec::new();
        let mut total: u64 = 0;
        let mut entries = collect_files_relative(fixture_dir)?;
        entries.sort_by(|a, b| a.relative.cmp(&b.relative));
        if entries.is_empty() {
            return Err(format!(
                "fixture_dir '{}' contains no files to install",
                fixture_dir.display()
            ));
        }
        for entry in entries {
            let src = entry.absolute;
            let dst = staging_dir.join(&entry.relative);
            copy_ai_model_cache_artifact(&src, &dst)
                .map_err(|err| format!("failed to copy '{}': {err}", src.display()))?;
            let (sha, size) = sha256_file(&dst)
                .map_err(|err| format!("failed to hash '{}': {err}", dst.display()))?;
            total = total.saturating_add(size);
            files.push(ManifestFile {
                path: entry.relative,
                sha256_hex: sha,
                size_bytes: size,
            });
        }

        let manifest = Manifest {
            name: entry
                .get("name")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_string(),
            source: entry
                .get("source")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_string(),
            revision: entry
                .get("revision")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .to_string(),
            task: entry
                .get("task")
                .and_then(JsonValue::as_str)
                .unwrap_or("embedding")
                .to_string(),
            engine: entry
                .get("engine")
                .and_then(JsonValue::as_str)
                .unwrap_or("candle")
                .to_string(),
            dimensions: entry
                .get("dimensions")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0) as u32,
            installed_at_unix_ms: now_unix_ms(),
            total_size_bytes: total,
            files,
        };

        let manifest_bytes = encode_ai_model_cache_manifest_json(&manifest)
            .map_err(|err| format!("failed to encode manifest: {err}"))?;
        write_ai_model_cache_manifest(&staging_dir, &manifest_bytes)
            .map_err(|err| format!("failed to finalize manifest: {err}"))?;
        Ok(manifest)
    })();

    let manifest = match result {
        Ok(m) => m,
        Err(err) => {
            let _ = fs::remove_dir_all(&staging_dir);
            return Err(err);
        }
    };

    // Promote staging → final atomically. If a previous install exists,
    // move it aside first into a purge dir, then promote, then nuke the
    // purge dir. That keeps the active path coherent at every step: a
    // crash mid-promotion leaves either the new or the old artifact
    // valid, never a half-merged tree.
    promote_ai_model_cache_staging(cache_root, &name, &unique, &staging_dir, model_dir)
        .map_err(|err| format!("failed to promote staging dir: {err}"))?;

    Ok(manifest)
}

#[derive(Debug)]
struct CollectedFile {
    absolute: PathBuf,
    relative: String,
}

fn collect_files_relative(root: &Path) -> Result<Vec<CollectedFile>, String> {
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let entries = fs::read_dir(&dir)
            .map_err(|err| format!("failed to read fixture dir '{}': {err}", dir.display()))?;
        for entry in entries {
            let entry = entry
                .map_err(|err| format!("fixture dir entry error in '{}': {err}", dir.display()))?;
            let file_type = entry
                .file_type()
                .map_err(|err| format!("fixture file type error: {err}"))?;
            let name = entry.file_name().to_string_lossy().to_string();
            // Skip dotfiles/staging artefacts and skip the manifest itself
            // if a caller includes one in the fixture — the cache owns
            // manifest.json.
            if name.starts_with('.') || name == AI_MODEL_CACHE_MANIFEST_FILE {
                continue;
            }
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if file_type.is_dir() {
                stack.push((entry.path(), rel));
            } else if file_type.is_file() {
                out.push(CollectedFile {
                    absolute: entry.path(),
                    relative: rel,
                });
            }
        }
    }
    Ok(out)
}

fn drop_cache(cache_root: &Path, model_dir: &Path) -> Result<bool, String> {
    let unique = unique_suffix();
    drop_ai_model_cache_dir(cache_root, model_dir, &unique)
        .map_err(|err| format!("failed to drop cache dir '{}': {err}", model_dir.display()))
}

#[derive(Debug)]
struct CacheReport {
    status: &'static str,
    manifest: Option<Manifest>,
    detail: Option<String>,
    footprint_bytes: u64,
}

fn inspect_cache(model_dir: &Path) -> CacheReport {
    if !model_dir.exists() {
        return CacheReport {
            status: STATUS_MISSING,
            manifest: None,
            detail: None,
            footprint_bytes: 0,
        };
    }
    let manifest_path = ai_model_cache_manifest_path(model_dir);
    let manifest_bytes = match fs::read(&manifest_path) {
        Ok(bytes) => bytes,
        Err(err) => {
            return CacheReport {
                status: STATUS_UNHEALTHY,
                manifest: None,
                detail: Some(format!("manifest unreadable: {err}")),
                footprint_bytes: directory_footprint(model_dir),
            };
        }
    };
    let manifest = match decode_ai_model_cache_manifest_json(&manifest_bytes) {
        Ok(m) => m,
        Err(err) => {
            return CacheReport {
                status: STATUS_UNHEALTHY,
                manifest: None,
                detail: Some(format!("manifest schema invalid: {err}")),
                footprint_bytes: directory_footprint(model_dir),
            };
        }
    };

    let mut footprint: u64 = 0;
    for entry in &manifest.files {
        let path = model_dir.join(&entry.path);
        let metadata = match fs::metadata(&path) {
            Ok(m) => m,
            Err(err) => {
                return CacheReport {
                    status: STATUS_UNHEALTHY,
                    manifest: Some(manifest.clone()),
                    detail: Some(format!("missing artifact file '{}': {err}", entry.path)),
                    footprint_bytes: directory_footprint(model_dir),
                };
            }
        };
        if metadata.len() != entry.size_bytes {
            return CacheReport {
                status: STATUS_UNHEALTHY,
                manifest: Some(manifest.clone()),
                detail: Some(format!(
                    "size mismatch for '{}': manifest={} actual={}",
                    entry.path,
                    entry.size_bytes,
                    metadata.len()
                )),
                footprint_bytes: directory_footprint(model_dir),
            };
        }
        footprint = footprint.saturating_add(metadata.len());
    }

    CacheReport {
        status: STATUS_INSTALLED,
        manifest: Some(manifest),
        detail: None,
        footprint_bytes: footprint,
    }
}

fn directory_footprint(path: &Path) -> u64 {
    let mut total: u64 = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

fn manifest_to_json(manifest: &Manifest) -> JsonValue {
    let Ok(bytes) = encode_ai_model_cache_manifest_json(manifest) else {
        return JsonValue::Null;
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return JsonValue::Null;
    };
    parse_json(text)
        .map(JsonValue::from)
        .unwrap_or(JsonValue::Null)
}

fn sha256_file(path: &Path) -> std::io::Result<(String, u64)> {
    let mut file = fs::File::open(path)?;
    let mut hasher = crate::crypto::sha256::Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut size: u64 = 0;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        hex.push_str(&format!("{:02x}", byte));
    }
    Ok((hex, size))
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn unique_suffix() -> String {
    let now_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}-{}-{}", std::process::id(), now_nanos, seq)
}

fn lock_key(cache_root: &Path, name: &str) -> String {
    format!("{}::{name}", cache_root.display())
}

fn model_lock_table() -> &'static Mutex<HashMap<String, Arc<Mutex<()>>>> {
    static TABLE: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn acquire_model_lock(key: &str) -> Arc<Mutex<()>> {
    let mut table = model_lock_table().lock().expect("lock table poisoned");
    table
        .entry(key.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tempdir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!(
            "reddb_cache_test_{label}_{}_{}",
            std::process::id(),
            nanos
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn sha256_file_hashes_known_payload() {
        let dir = tempdir("sha");
        let path = dir.join("a.bin");
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(b"hello world").unwrap();
        let (hex, size) = sha256_file(&path).unwrap();
        assert_eq!(size, 11);
        assert_eq!(
            hex,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn collect_files_relative_skips_dotfiles_and_manifest() {
        let dir = tempdir("collect");
        fs::write(dir.join("a.txt"), b"a").unwrap();
        fs::write(dir.join(".hidden"), b"h").unwrap();
        fs::write(dir.join(AI_MODEL_CACHE_MANIFEST_FILE), b"m").unwrap();
        fs::create_dir(dir.join("sub")).unwrap();
        fs::write(dir.join("sub").join("b.txt"), b"b").unwrap();
        let mut files = collect_files_relative(&dir).unwrap();
        files.sort_by(|a, b| a.relative.cmp(&b.relative));
        let names: Vec<_> = files.iter().map(|f| f.relative.clone()).collect();
        assert_eq!(names, vec!["a.txt".to_string(), "sub/b.txt".to_string()]);
    }
}
