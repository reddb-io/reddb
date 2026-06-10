//! `red ui` bundle resolver — pin→URL resolution, HTTPS download,
//! SHA-256 verification, tgz extraction, and local cache management.
//!
//! Implements the runtime download path described in ADR 0050:
//!
//!   1. The pinned `red-ui` version and its bundle SHA-256 are build-time
//!      constants set by CI (`RED_UI_PINNED_VERSION` / `RED_UI_PINNED_SHA256`).
//!   2. [`resolve_ui_bundle`] checks `~/.cache/reddb/ui/<version>/` for a
//!      cached bundle whose manifest matches the pin. Cache hit → returns
//!      the directory immediately (no network call).
//!   3. On a cache miss it downloads the GitHub release asset, verifies the
//!      SHA-256 (refuses on mismatch), extracts the tgz into a staging
//!      directory, writes a manifest, and atomically promotes the staging
//!      directory to the live version directory.
//!   4. Offline first-run with no cached bundle fails with a clear,
//!      actionable `io::Error` message.
//!
//! The [`UiBundleFetcher`] trait makes the network layer injectable for
//! tests.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hex;
use sha2::{Digest, Sha256};

use reddb_file::{
    decode_ui_bundle_manifest_json, encode_ui_bundle_manifest_json, promote_ui_bundle_staging,
    ui_bundle_cache_root, ui_bundle_manifest_path, ui_bundle_staging_dir, ui_bundle_version_dir,
    write_ui_bundle_manifest, UiBundleManifest,
};

// ---------------------------------------------------------------------------
// Build-time pin (CI replaces these)
// ---------------------------------------------------------------------------

/// Exact `red-ui` version this `red` binary was tested against. CI that
/// validates the `red`↔`red-ui` pair sets this before the release build.
pub const RED_UI_PINNED_VERSION: &str = "0.0.0-dev";

/// SHA-256 (lower-case hex) of the `ui-dist.tgz` for the pinned version.
/// CI sets this alongside `RED_UI_PINNED_VERSION`.
pub const RED_UI_PINNED_SHA256: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

// ---------------------------------------------------------------------------
// URL resolution
// ---------------------------------------------------------------------------

/// GitHub release asset URL for a given `red-ui` version.
///
/// Maps `"1.2.3"` → `https://github.com/reddb-io/red-ui/releases/download/v1.2.3/ui-dist.tgz`
pub fn release_asset_url(version: &str) -> String {
    format!("https://github.com/reddb-io/red-ui/releases/download/v{version}/ui-dist.tgz")
}

// ---------------------------------------------------------------------------
// Fetcher abstraction
// ---------------------------------------------------------------------------

/// Abstraction over the network layer so tests can inject a fake without
/// making real HTTP calls.
pub trait UiBundleFetcher: Send + Sync {
    /// Fetch the URL and return the raw response bytes. Returns an
    /// `io::Error` on any network or HTTP-level failure.
    fn fetch_bytes(&self, url: &str) -> io::Result<Vec<u8>>;
}

/// Production fetcher — HTTPS via `ureq` with rustls, per ADR 0050.
pub struct HttpFetcher;

impl UiBundleFetcher for HttpFetcher {
    fn fetch_bytes(&self, url: &str) -> io::Result<Vec<u8>> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_send_request(Some(Duration::from_secs(60)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .timeout_recv_body(Some(Duration::from_secs(600)))
            .build()
            .into();

        let mut resp = agent
            .get(url)
            .call()
            .map_err(|err| io::Error::other(format!("HTTP GET {url}: {err}")))?;

        let status = resp.status().as_u16();
        if status != 200 {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("HTTP GET {url}: status {status}"),
            ));
        }

        resp.body_mut()
            .read_to_vec()
            .map_err(|err| io::Error::other(format!("read response body from {url}: {err}")))
    }
}

// ---------------------------------------------------------------------------
// Cache root
// ---------------------------------------------------------------------------

/// Compute `~/.cache/reddb` (XDG-aware on Linux, standard on macOS/Windows).
///
/// The returned path is not created; callers are responsible for
/// `fs::create_dir_all` as needed.
pub fn reddb_user_cache_root() -> io::Result<PathBuf> {
    let base = if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        PathBuf::from(xdg)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".cache")
    } else if cfg!(target_os = "windows") {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            PathBuf::from(local)
        } else {
            std::env::temp_dir()
        }
    } else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "cannot determine home directory: HOME and XDG_CACHE_HOME are both unset",
        ));
    };
    Ok(base.join("reddb"))
}

// ---------------------------------------------------------------------------
// Main resolver
// ---------------------------------------------------------------------------

/// Ensure the pinned `red-ui` bundle is cached and return its directory.
///
/// # Behaviour
///
/// 1. **Cache hit**: if `<reddb_cache_root>/ui/<version>/manifest.json`
///    exists and records the pinned version and SHA-256, return the
///    directory immediately — no network call.
/// 2. **Cache miss**: download the release asset via `fetcher`, verify
///    SHA-256 (reject on mismatch), extract the tgz into a staging
///    directory, write the manifest, and atomically promote to the live
///    version directory.
/// 3. **Offline first-run**: if `fetcher` returns an error and no cached
///    bundle exists, propagate a clear `io::Error` with an actionable
///    message.
///
/// `reddb_cache_root` is the `~/.cache/reddb` base (use
/// [`reddb_user_cache_root`] to derive it). In tests, pass a `TempDir`
/// path to keep caches isolated.
pub fn resolve_ui_bundle(
    reddb_cache_root: &Path,
    fetcher: &dyn UiBundleFetcher,
) -> io::Result<PathBuf> {
    let version = RED_UI_PINNED_VERSION;
    let expected_sha256 = RED_UI_PINNED_SHA256;

    let cache_root = ui_bundle_cache_root(reddb_cache_root);
    let version_dir = ui_bundle_version_dir(&cache_root, version);
    let manifest_path = ui_bundle_manifest_path(&version_dir);

    // Cache hit: manifest present, version and checksum match the pin.
    if manifest_path.exists() {
        if let Ok(bytes) = fs::read(&manifest_path) {
            if let Ok(manifest) = decode_ui_bundle_manifest_json(&bytes) {
                if manifest.version == version && manifest.sha256_hex == expected_sha256 {
                    return Ok(version_dir);
                }
            }
        }
        // Manifest exists but is stale or corrupt — fall through to re-download.
    }

    // Download.
    let url = release_asset_url(version);
    let tgz_bytes = fetcher.fetch_bytes(&url).map_err(|err| {
        // Distinguish between "never cached" and "cached but stale":
        // both produce an actionable offline message, but only "never
        // cached" has no fallback, so we surface the download URL.
        io::Error::new(
            err.kind(),
            format!(
                "could not download red-ui bundle v{version} from {url}: {err}\n\
                 hint: run `red ui` while online to populate the cache, \
                 or pass --ui-dir to serve a local bundle directory"
            ),
        )
    })?;

    // Verify SHA-256 before writing anything to disk.
    let actual_sha256 = sha256_hex(&tgz_bytes);
    if actual_sha256 != expected_sha256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "red-ui bundle SHA-256 mismatch: expected {expected_sha256}, \
                 got {actual_sha256} — refusing to serve a potentially tampered bundle"
            ),
        ));
    }

    // Unique token for staging/purge directory names; avoids a collision
    // if two processes race to download the same version.
    let unique = format!(
        "{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    );

    let staging_dir = ui_bundle_staging_dir(&cache_root, version, &unique);

    // Clean up any leftover staging directory from a prior crashed download.
    if staging_dir.exists() {
        let _ = fs::remove_dir_all(&staging_dir);
    }

    // Extract into staging.
    extract_tgz(&tgz_bytes, &staging_dir)?;

    // Write the manifest atomically inside staging before promotion.
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let manifest = UiBundleManifest {
        version: version.to_string(),
        sha256_hex: expected_sha256.to_string(),
        tgz_size_bytes: tgz_bytes.len() as u64,
        cached_at_unix_ms: now_ms,
    };
    let manifest_bytes = encode_ui_bundle_manifest_json(&manifest)?;
    write_ui_bundle_manifest(&staging_dir, &manifest_bytes)?;

    // Atomically promote staging → live version directory.
    fs::create_dir_all(&cache_root)?;
    promote_ui_bundle_staging(&cache_root, version, &unique, &staging_dir, &version_dir)?;

    Ok(version_dir)
}

// ---------------------------------------------------------------------------
// tgz extraction
// ---------------------------------------------------------------------------

/// Extract `tgz_bytes` into `dest`, creating `dest` if needed.
///
/// Path traversal safety: any archive entry whose path contains a `..`
/// component is rejected. Symlinks and other special entry types are
/// skipped (only regular files and directories are extracted).
///
/// Top-level directory stripping: if every non-empty path in the archive
/// shares a common first component (e.g. `dist/`), that component is
/// stripped so files land directly inside `dest`.
fn extract_tgz(tgz_bytes: &[u8], dest: &Path) -> io::Result<()> {
    fs::create_dir_all(dest)?;

    // Two-pass: first decide whether to strip the common root directory,
    // then extract. Re-reading from bytes is cheap (already in memory).
    let strip_prefix = detect_common_root(tgz_bytes)?;

    let cursor = std::io::Cursor::new(tgz_bytes);
    let gz = flate2::read::GzDecoder::new(cursor);
    let mut archive = tar::Archive::new(gz);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let raw_path = entry.path()?.into_owned();

        // Compute the relative path, optionally stripping the common root.
        let rel: PathBuf = if strip_prefix {
            raw_path.components().skip(1).collect()
        } else {
            raw_path.components().collect()
        };

        // Skip the (now-empty) root directory itself.
        if rel.as_os_str().is_empty() {
            continue;
        }

        // Reject path traversal.
        if rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unsafe path in red-ui bundle archive: {}",
                    raw_path.display()
                ),
            ));
        }

        let out_path = dest.join(&rel);

        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                fs::create_dir_all(&out_path)?;
            }
            tar::EntryType::Regular => {
                if let Some(parent) = out_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut file = fs::File::create(&out_path)?;
                std::io::copy(&mut entry, &mut file)?;
            }
            _ => {} // skip symlinks, hard links, etc. — security boundary
        }
    }
    Ok(())
}

/// Return `true` if every non-empty path in the archive shares a single
/// common first component (implying the archive was created with a
/// top-level wrapper directory that should be stripped).
///
/// A single-component **directory** entry (e.g. `dist/`) is the root
/// directory itself and does NOT signal "file at the archive root". A
/// single-component **regular-file** entry (e.g. `index.html`) does —
/// there is no prefix to strip in that case.
fn detect_common_root(tgz_bytes: &[u8]) -> io::Result<bool> {
    let cursor = std::io::Cursor::new(tgz_bytes);
    let gz = flate2::read::GzDecoder::new(cursor);
    let mut archive = tar::Archive::new(gz);

    let mut common: Option<String> = None;
    for entry in archive.entries()? {
        let entry = entry?;
        let path = entry.path()?.into_owned();
        let is_dir = matches!(entry.header().entry_type(), tar::EntryType::Directory);

        let first = match path.components().next() {
            Some(c) => match c.as_os_str().to_str() {
                Some(s) => s.to_string(),
                None => continue,
            },
            None => continue,
        };

        let component_count = path.components().count();

        // A single-component, non-directory entry is a regular file placed
        // directly at the archive root — no common prefix to strip.
        if component_count == 1 && !is_dir {
            return Ok(false);
        }

        match &common {
            None => common = Some(first),
            Some(prev) if prev != &first => return Ok(false),
            _ => {}
        }
    }
    Ok(common.is_some())
}

// ---------------------------------------------------------------------------
// SHA-256 helper
// ---------------------------------------------------------------------------

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ------------------------------------------------------------------
    // Test helpers
    // ------------------------------------------------------------------

    /// Build a minimal `.tgz` archive in memory.
    ///
    /// Names ending in `/` are written as directory entries
    /// (`EntryType::Directory`); all others are regular files.
    fn make_tgz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let buf = Vec::new();
        let gz = flate2::write::GzEncoder::new(buf, flate2::Compression::default());
        let mut tb = tar::Builder::new(gz);
        for (name, content) in files {
            let mut header = tar::Header::new_gnu();
            if name.ends_with('/') {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_size(0);
                header.set_mode(0o755);
            } else {
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
            }
            header.set_cksum();
            tb.append_data(&mut header, *name, std::io::Cursor::new(*content))
                .unwrap();
        }
        let gz = tb.into_inner().unwrap();
        gz.finish().unwrap()
    }

    struct FakeFetcher {
        bytes: Vec<u8>,
        call_count: std::sync::Mutex<usize>,
    }

    impl FakeFetcher {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                bytes,
                call_count: std::sync::Mutex::new(0),
            }
        }

        fn calls(&self) -> usize {
            *self.call_count.lock().unwrap()
        }
    }

    impl UiBundleFetcher for FakeFetcher {
        fn fetch_bytes(&self, _url: &str) -> io::Result<Vec<u8>> {
            *self.call_count.lock().unwrap() += 1;
            Ok(self.bytes.clone())
        }
    }

    struct OfflineFetcher;

    impl UiBundleFetcher for OfflineFetcher {
        fn fetch_bytes(&self, url: &str) -> io::Result<Vec<u8>> {
            Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                format!("no network: {url}"),
            ))
        }
    }

    // Resolve using a known tgz + SHA-256, bypassing the build-time pin.
    fn resolve_with_pin(
        reddb_cache_root: &Path,
        fetcher: &dyn UiBundleFetcher,
        version: &str,
        expected_sha256: &str,
    ) -> io::Result<PathBuf> {
        let cache_root = ui_bundle_cache_root(reddb_cache_root);
        let version_dir = ui_bundle_version_dir(&cache_root, version);
        let manifest_path = ui_bundle_manifest_path(&version_dir);

        if manifest_path.exists() {
            if let Ok(bytes) = fs::read(&manifest_path) {
                if let Ok(manifest) = decode_ui_bundle_manifest_json(&bytes) {
                    if manifest.version == version && manifest.sha256_hex == expected_sha256 {
                        return Ok(version_dir);
                    }
                }
            }
        }

        let url = release_asset_url(version);
        let tgz_bytes = fetcher.fetch_bytes(&url)?;

        let actual = sha256_hex(&tgz_bytes);
        if actual != expected_sha256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("red-ui bundle SHA-256 mismatch: expected {expected_sha256}, got {actual}"),
            ));
        }

        let unique = format!("{}", tgz_bytes.len());
        let staging_dir = ui_bundle_staging_dir(&cache_root, version, &unique);
        if staging_dir.exists() {
            let _ = fs::remove_dir_all(&staging_dir);
        }
        extract_tgz(&tgz_bytes, &staging_dir)?;

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let manifest = UiBundleManifest {
            version: version.to_string(),
            sha256_hex: expected_sha256.to_string(),
            tgz_size_bytes: tgz_bytes.len() as u64,
            cached_at_unix_ms: now_ms,
        };
        let manifest_bytes = encode_ui_bundle_manifest_json(&manifest)?;
        write_ui_bundle_manifest(&staging_dir, &manifest_bytes)?;
        fs::create_dir_all(&cache_root)?;
        promote_ui_bundle_staging(&cache_root, version, &unique, &staging_dir, &version_dir)?;

        Ok(version_dir)
    }

    // ------------------------------------------------------------------
    // Tests
    // ------------------------------------------------------------------

    #[test]
    fn pin_to_url_resolution() {
        assert_eq!(
            release_asset_url("1.2.3"),
            "https://github.com/reddb-io/red-ui/releases/download/v1.2.3/ui-dist.tgz"
        );
        assert_eq!(
            release_asset_url("0.0.0-dev"),
            "https://github.com/reddb-io/red-ui/releases/download/v0.0.0-dev/ui-dist.tgz"
        );
    }

    #[test]
    fn checksum_match_produces_cached_path() {
        let dir = tempfile::tempdir().unwrap();
        let tgz = make_tgz(&[
            ("index.html", b"<html></html>"),
            ("app.js", b"console.log(1)"),
        ]);
        let sha256 = sha256_hex(&tgz);
        let fetcher = FakeFetcher::new(tgz);

        let bundle_dir = resolve_with_pin(dir.path(), &fetcher, "1.0.0", &sha256).expect("resolve");

        assert!(bundle_dir.exists());
        assert!(bundle_dir.join("index.html").exists());
        assert!(bundle_dir.join("app.js").exists());
        assert_eq!(
            std::fs::read_to_string(bundle_dir.join("index.html")).unwrap(),
            "<html></html>"
        );
    }

    #[test]
    fn checksum_mismatch_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let tgz = make_tgz(&[("index.html", b"<html></html>")]);
        let wrong_sha256 = "aaaa000000000000000000000000000000000000000000000000000000000000";
        let fetcher = FakeFetcher::new(tgz);

        let err = resolve_with_pin(dir.path(), &fetcher, "1.0.0", wrong_sha256)
            .expect_err("should reject mismatched checksum");

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("SHA-256 mismatch"), "{err}");
        assert!(err.to_string().contains(wrong_sha256), "{err}");
    }

    #[test]
    fn cache_hit_skips_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let tgz = make_tgz(&[("index.html", b"<html></html>")]);
        let sha256 = sha256_hex(&tgz);
        let fetcher = FakeFetcher::new(tgz);

        // First call populates the cache.
        resolve_with_pin(dir.path(), &fetcher, "2.0.0", &sha256).unwrap();
        assert_eq!(fetcher.calls(), 1);

        // Second call must return immediately — no fetch.
        resolve_with_pin(dir.path(), &fetcher, "2.0.0", &sha256).unwrap();
        assert_eq!(fetcher.calls(), 1, "cache hit must not call the fetcher");
    }

    #[test]
    fn offline_first_run_fails_with_clear_message() {
        let dir = tempfile::tempdir().unwrap();
        let fetcher = OfflineFetcher;

        let err = resolve_with_pin(
            dir.path(),
            &fetcher,
            "1.0.0",
            "0000000000000000000000000000000000000000000000000000000000000000",
        )
        .expect_err("offline should fail");

        let msg = err.to_string();
        assert!(
            msg.contains("no network"),
            "error should name the cause: {msg}"
        );
    }

    #[test]
    fn tgz_with_top_level_dir_is_stripped() {
        let dir = tempfile::tempdir().unwrap();
        // Simulate a GitHub release archive: dist/index.html, dist/app.js
        let tgz = make_tgz(&[
            ("dist/", b""),
            ("dist/index.html", b"<html></html>"),
            ("dist/app.js", b"console.log(1)"),
        ]);
        let sha256 = sha256_hex(&tgz);
        let fetcher = FakeFetcher::new(tgz);

        let bundle_dir = resolve_with_pin(dir.path(), &fetcher, "3.0.0", &sha256).expect("resolve");

        // After stripping "dist/", files should be at the root.
        assert!(
            bundle_dir.join("index.html").exists(),
            "index.html should be at bundle root after stripping"
        );
        assert!(bundle_dir.join("app.js").exists());
    }

    #[test]
    fn extracted_file_set_matches_archive() {
        let dir = tempfile::tempdir().unwrap();
        let files: &[(&str, &[u8])] = &[
            ("index.html", b"<html>"),
            ("assets/main.js", b"const x=1"),
            ("assets/style.css", b"body{}"),
        ];
        let tgz = make_tgz(files);
        let sha256 = sha256_hex(&tgz);
        let fetcher = FakeFetcher::new(tgz);

        let bundle_dir = resolve_with_pin(dir.path(), &fetcher, "4.0.0", &sha256).expect("resolve");

        let expected: HashSet<&str> = files.iter().map(|(n, _)| *n).collect();
        for name in &expected {
            let p = bundle_dir.join(name);
            assert!(p.exists(), "expected {name} at {}", p.display());
        }
    }
}
