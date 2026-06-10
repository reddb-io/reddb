//! Local `red ui` bundle cache file contracts.
//!
//! Parallel to `ai_model_cache`: the server owns download policy and
//! checksum verification; this module owns the persisted cache layout
//! and manifest JSON shape. ADR 0050.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value as JsonValue;

pub const UI_BUNDLE_CACHE_DIR_NAME: &str = "ui";
pub const UI_BUNDLE_STAGING_DIR_NAME: &str = ".staging";
pub const UI_BUNDLE_PURGE_DIR_NAME: &str = ".purge";
pub const UI_BUNDLE_MANIFEST_FILE: &str = "manifest.json";

/// Persisted record for a cached `red-ui` bundle version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiBundleManifest {
    pub version: String,
    /// SHA-256 of the original `.tgz` download, lower-case hex.
    pub sha256_hex: String,
    /// Size of the original `.tgz` download in bytes.
    pub tgz_size_bytes: u64,
    /// Unix epoch milliseconds when the bundle was cached.
    pub cached_at_unix_ms: u64,
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

pub fn ui_bundle_cache_root(base: &Path) -> PathBuf {
    base.join(UI_BUNDLE_CACHE_DIR_NAME)
}

pub fn ui_bundle_version_dir(cache_root: &Path, version: &str) -> PathBuf {
    cache_root.join(version)
}

pub fn ui_bundle_staging_root(cache_root: &Path) -> PathBuf {
    cache_root.join(UI_BUNDLE_STAGING_DIR_NAME)
}

pub fn ui_bundle_purge_root(cache_root: &Path) -> PathBuf {
    cache_root.join(UI_BUNDLE_PURGE_DIR_NAME)
}

pub fn ui_bundle_staging_dir(cache_root: &Path, version: &str, unique: &str) -> PathBuf {
    ui_bundle_staging_root(cache_root).join(format!("{version}-{unique}"))
}

pub fn ui_bundle_purge_dir(cache_root: &Path, version: &str, unique: &str) -> PathBuf {
    ui_bundle_purge_root(cache_root).join(format!("{version}-{unique}"))
}

pub fn ui_bundle_manifest_path(version_dir: &Path) -> PathBuf {
    version_dir.join(UI_BUNDLE_MANIFEST_FILE)
}

pub fn ui_bundle_manifest_temp_path(dir: &Path) -> PathBuf {
    dir.join(format!("{UI_BUNDLE_MANIFEST_FILE}.tmp"))
}

// ---------------------------------------------------------------------------
// I/O helpers
// ---------------------------------------------------------------------------

pub fn write_ui_bundle_manifest(dir: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = ui_bundle_manifest_temp_path(dir);
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, ui_bundle_manifest_path(dir))
}

/// Atomically promote a staging directory to the live version directory.
/// Rolls back if the rename fails; best-effort removes the purge directory
/// after a successful promotion.
pub fn promote_ui_bundle_staging(
    cache_root: &Path,
    version: &str,
    unique: &str,
    staging_dir: &Path,
    version_dir: &Path,
) -> io::Result<()> {
    let purge_root = ui_bundle_purge_root(cache_root);
    fs::create_dir_all(&purge_root)?;
    let purge_dir = ui_bundle_purge_dir(cache_root, version, unique);
    if version_dir.exists() {
        fs::rename(version_dir, &purge_dir)?;
    }
    if let Err(err) = fs::rename(staging_dir, version_dir) {
        if purge_dir.exists() {
            let _ = fs::rename(&purge_dir, version_dir);
        }
        let _ = fs::remove_dir_all(staging_dir);
        return Err(err);
    }
    if purge_dir.exists() {
        let _ = fs::remove_dir_all(&purge_dir);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Manifest JSON codec
// ---------------------------------------------------------------------------

pub fn encode_ui_bundle_manifest_json(manifest: &UiBundleManifest) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&manifest_to_json(manifest)).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("encode UI bundle manifest: {err}"),
        )
    })
}

pub fn decode_ui_bundle_manifest_json(bytes: &[u8]) -> io::Result<UiBundleManifest> {
    let value: JsonValue = serde_json::from_slice(bytes).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("UI bundle manifest is not valid JSON: {err}"),
        )
    })?;
    manifest_from_json(&value)
}

fn manifest_to_json(m: &UiBundleManifest) -> JsonValue {
    let mut obj = serde_json::Map::new();
    obj.insert("version".to_string(), JsonValue::String(m.version.clone()));
    obj.insert(
        "sha256".to_string(),
        JsonValue::String(m.sha256_hex.clone()),
    );
    obj.insert(
        "tgz_size_bytes".to_string(),
        JsonValue::Number(m.tgz_size_bytes.into()),
    );
    obj.insert(
        "cached_at_unix_ms".to_string(),
        JsonValue::Number(m.cached_at_unix_ms.into()),
    );
    JsonValue::Object(obj)
}

fn manifest_from_json(value: &JsonValue) -> io::Result<UiBundleManifest> {
    let obj = value
        .as_object()
        .ok_or_else(|| invalid("manifest is not an object"))?;
    Ok(UiBundleManifest {
        version: required_str(obj, "version")?,
        sha256_hex: required_str(obj, "sha256")?,
        tgz_size_bytes: required_u64(obj, "tgz_size_bytes")?,
        cached_at_unix_ms: required_u64(obj, "cached_at_unix_ms")?,
    })
}

fn required_str(obj: &serde_json::Map<String, JsonValue>, key: &str) -> io::Result<String> {
    obj.get(key)
        .and_then(JsonValue::as_str)
        .map(str::to_string)
        .ok_or_else(|| invalid(format!("manifest field '{key}' missing or not a string")))
}

fn required_u64(obj: &serde_json::Map<String, JsonValue>, key: &str) -> io::Result<u64> {
    obj.get(key)
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| invalid(format!("manifest field '{key}' missing or not a number")))
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_bundle_manifest_round_trips() {
        let m = UiBundleManifest {
            version: "1.2.3".to_string(),
            sha256_hex: "deadbeef".to_string(),
            tgz_size_bytes: 42_000,
            cached_at_unix_ms: 1_000_000,
        };
        let bytes = encode_ui_bundle_manifest_json(&m).expect("encode");
        let decoded = decode_ui_bundle_manifest_json(&bytes).expect("decode");
        assert_eq!(decoded, m);
        assert!(String::from_utf8(bytes)
            .unwrap()
            .contains("\"sha256\":\"deadbeef\""));
    }

    #[test]
    fn ui_bundle_cache_paths_are_canonical() {
        let root = Path::new("/tmp/reddb");
        assert_eq!(
            ui_bundle_cache_root(root),
            Path::new("/tmp/reddb").join("ui")
        );
        assert_eq!(
            ui_bundle_version_dir(&ui_bundle_cache_root(root), "1.2.3"),
            Path::new("/tmp/reddb/ui/1.2.3")
        );
        assert_eq!(
            ui_bundle_staging_dir(&ui_bundle_cache_root(root), "1.2.3", "abc"),
            Path::new("/tmp/reddb/ui/.staging/1.2.3-abc")
        );
        assert_eq!(
            ui_bundle_purge_dir(&ui_bundle_cache_root(root), "1.2.3", "abc"),
            Path::new("/tmp/reddb/ui/.purge/1.2.3-abc")
        );
    }
}
