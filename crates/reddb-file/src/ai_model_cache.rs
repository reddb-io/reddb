//! Local AI model cache file contracts.
//!
//! The server owns registry policy, HTTP handlers, fixture acquisition, and
//! process locks. This module owns the persisted cache layout and manifest
//! JSON shape.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value as JsonValue;

pub const AI_MODEL_CACHE_DIR_NAME: &str = "ai_models_cache";
pub const AI_MODEL_CACHE_STAGING_DIR_NAME: &str = ".staging";
pub const AI_MODEL_CACHE_PURGE_DIR_NAME: &str = ".purge";
pub const AI_MODEL_CACHE_MANIFEST_FILE: &str = "manifest.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiModelCacheManifestFile {
    pub path: String,
    pub sha256_hex: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiModelCacheManifest {
    pub name: String,
    pub source: String,
    pub revision: String,
    pub task: String,
    pub engine: String,
    pub dimensions: u32,
    pub installed_at_unix_ms: u64,
    pub total_size_bytes: u64,
    pub files: Vec<AiModelCacheManifestFile>,
}

pub fn ai_model_cache_root(base: &Path) -> PathBuf {
    base.join(AI_MODEL_CACHE_DIR_NAME)
}

pub fn ai_model_cache_staging_root(cache_root: &Path) -> PathBuf {
    cache_root.join(AI_MODEL_CACHE_STAGING_DIR_NAME)
}

pub fn ai_model_cache_purge_root(cache_root: &Path) -> PathBuf {
    cache_root.join(AI_MODEL_CACHE_PURGE_DIR_NAME)
}

pub fn ai_model_cache_staging_dir(cache_root: &Path, name: &str, unique: &str) -> PathBuf {
    ai_model_cache_staging_root(cache_root).join(format!("{name}-{unique}"))
}

pub fn ai_model_cache_purge_dir(cache_root: &Path, name: &str, unique: &str) -> PathBuf {
    ai_model_cache_purge_root(cache_root).join(format!("{name}-{unique}"))
}

pub fn ai_model_cache_manifest_path(model_dir: &Path) -> PathBuf {
    model_dir.join(AI_MODEL_CACHE_MANIFEST_FILE)
}

pub fn ai_model_cache_manifest_temp_path(dir: &Path) -> PathBuf {
    dir.join(format!("{AI_MODEL_CACHE_MANIFEST_FILE}.tmp"))
}

pub fn copy_ai_model_cache_artifact(source: &Path, destination: &Path) -> io::Result<u64> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, destination)
}

pub fn encode_ai_model_cache_manifest_json(manifest: &AiModelCacheManifest) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&manifest_to_json(manifest)).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("encode AI model cache manifest: {err}"),
        )
    })
}

pub fn decode_ai_model_cache_manifest_json(bytes: &[u8]) -> io::Result<AiModelCacheManifest> {
    let value: JsonValue = serde_json::from_slice(bytes).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("AI model cache manifest is not valid JSON: {err}"),
        )
    })?;
    manifest_from_json(&value)
}

fn manifest_to_json(manifest: &AiModelCacheManifest) -> JsonValue {
    let mut object = serde_json::Map::new();
    object.insert("name".to_string(), JsonValue::String(manifest.name.clone()));
    object.insert(
        "source".to_string(),
        JsonValue::String(manifest.source.clone()),
    );
    object.insert(
        "revision".to_string(),
        JsonValue::String(manifest.revision.clone()),
    );
    object.insert("task".to_string(), JsonValue::String(manifest.task.clone()));
    object.insert(
        "engine".to_string(),
        JsonValue::String(manifest.engine.clone()),
    );
    object.insert(
        "dimensions".to_string(),
        JsonValue::Number(manifest.dimensions.into()),
    );
    object.insert(
        "installed_at_unix_ms".to_string(),
        JsonValue::Number(manifest.installed_at_unix_ms.into()),
    );
    object.insert(
        "total_size_bytes".to_string(),
        JsonValue::Number(manifest.total_size_bytes.into()),
    );
    let files = manifest
        .files
        .iter()
        .map(|file| {
            let mut object = serde_json::Map::new();
            object.insert("path".to_string(), JsonValue::String(file.path.clone()));
            object.insert(
                "sha256".to_string(),
                JsonValue::String(file.sha256_hex.clone()),
            );
            object.insert(
                "size_bytes".to_string(),
                JsonValue::Number(file.size_bytes.into()),
            );
            JsonValue::Object(object)
        })
        .collect();
    object.insert("files".to_string(), JsonValue::Array(files));
    JsonValue::Object(object)
}

fn manifest_from_json(value: &JsonValue) -> io::Result<AiModelCacheManifest> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("manifest is not an object"))?;
    let name = required_str(object, "name")?;
    let source = required_str(object, "source")?;
    let revision = required_str(object, "revision")?;
    let task = required_str(object, "task")?;
    let engine = required_str(object, "engine")?;
    let dimensions = required_u64(object, "dimensions")? as u32;
    let installed_at_unix_ms = required_u64(object, "installed_at_unix_ms")?;
    let total_size_bytes = required_u64(object, "total_size_bytes")?;
    let files_raw = object
        .get("files")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| invalid("manifest field 'files' must be an array"))?;
    let mut files = Vec::with_capacity(files_raw.len());
    for (idx, raw) in files_raw.iter().enumerate() {
        let entry = raw
            .as_object()
            .ok_or_else(|| invalid(format!("manifest files[{idx}] is not an object")))?;
        files.push(AiModelCacheManifestFile {
            path: required_str_at(entry, "path", idx)?,
            sha256_hex: required_str_at(entry, "sha256", idx)?,
            size_bytes: required_u64_at(entry, "size_bytes", idx)?,
        });
    }
    Ok(AiModelCacheManifest {
        name,
        source,
        revision,
        task,
        engine,
        dimensions,
        installed_at_unix_ms,
        total_size_bytes,
        files,
    })
}

fn required_str(object: &serde_json::Map<String, JsonValue>, key: &str) -> io::Result<String> {
    object
        .get(key)
        .and_then(JsonValue::as_str)
        .map(str::to_string)
        .ok_or_else(|| invalid(format!("manifest field '{key}' missing or not a string")))
}

fn required_u64(object: &serde_json::Map<String, JsonValue>, key: &str) -> io::Result<u64> {
    object
        .get(key)
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| invalid(format!("manifest field '{key}' missing or not a number")))
}

fn required_str_at(
    object: &serde_json::Map<String, JsonValue>,
    key: &str,
    index: usize,
) -> io::Result<String> {
    object
        .get(key)
        .and_then(JsonValue::as_str)
        .map(str::to_string)
        .ok_or_else(|| invalid(format!("manifest files[{index}].{key} missing")))
}

fn required_u64_at(
    object: &serde_json::Map<String, JsonValue>,
    key: &str,
    index: usize,
) -> io::Result<u64> {
    object
        .get(key)
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| invalid(format!("manifest files[{index}].{key} missing")))
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_model_cache_manifest_round_trips() {
        let manifest = AiModelCacheManifest {
            name: "mini".to_string(),
            source: "fixture".to_string(),
            revision: "abc".to_string(),
            task: "embedding".to_string(),
            engine: "candle".to_string(),
            dimensions: 384,
            installed_at_unix_ms: 42,
            total_size_bytes: 11,
            files: vec![AiModelCacheManifestFile {
                path: "model.bin".to_string(),
                sha256_hex: "00ff".to_string(),
                size_bytes: 11,
            }],
        };

        let bytes = encode_ai_model_cache_manifest_json(&manifest).expect("encode");
        let decoded = decode_ai_model_cache_manifest_json(&bytes).expect("decode");
        assert_eq!(decoded, manifest);
        assert!(String::from_utf8(bytes)
            .unwrap()
            .contains("\"sha256\":\"00ff\""));
    }

    #[test]
    fn ai_model_cache_paths_are_canonical() {
        let root = Path::new("/tmp/reddb");
        assert_eq!(
            ai_model_cache_root(root),
            Path::new("/tmp/reddb").join("ai_models_cache")
        );
        assert_eq!(
            ai_model_cache_staging_dir(root, "m", "u"),
            Path::new("/tmp/reddb").join(".staging").join("m-u")
        );
        assert_eq!(
            ai_model_cache_purge_dir(root, "m", "u"),
            Path::new("/tmp/reddb").join(".purge").join("m-u")
        );
        assert_eq!(
            ai_model_cache_manifest_temp_path(root),
            Path::new("/tmp/reddb").join("manifest.json.tmp")
        );
    }
}
