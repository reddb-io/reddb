//! `red server --ui` static bundle surface (issue #1047, PRD #1041;
//! ADR 0050 distribution, ADR 0051 exposure model, ADR 0049 transport).
//!
//! When `red server … --ui` is given, the running server also serves the
//! pinned `red-ui` bundle (resolved/cached by the downloader slice, issue
//! #1043) as static assets on its existing HTTP surface, alongside the
//! API. A remote browser loads the bundle and connects back to the
//! server's RedWire-over-WS endpoint ([`super::ws_edge`]).
//!
//! Security model (ADR 0051):
//!   * **Inert assets.** The served bundle carries no embedded credential,
//!     so exposing it is safe by construction — auth lives on the RedWire
//!     data endpoint, not the asset path. The static paths are therefore
//!     served without authentication even on an authed database; the
//!     browser still authenticates against the data endpoint.
//!   * **Opt-in network reach.** This module never binds or widens a
//!     listener. Reach is governed entirely by the server's bind address
//!     (default localhost); reaching the UI from another host requires an
//!     explicit non-localhost bind, exactly like the API.
//!   * **Fallback, never a shadow.** Asset serving runs only after the
//!     full API routing table misses, so a bundle file can never shadow a
//!     real endpoint. `GET /` is the sole exception — with `--ui` it
//!     resolves to the bundle's `index.html` instead of the API discovery
//!     document.
//!
//! Path traversal is refused (no `..`/`.`/empty segments, no absolute
//! re-rooting), mirroring the loopback bridge's [`super::ui_bridge`].

use std::path::{Path, PathBuf};

use super::transport::HttpResponse;
use super::ui_bridge::content_type_for;

/// Resolve a request path to a bundle-relative file path, applying the
/// `/` → `index.html` default and refusing traversal.
///
/// Returns `None` when the path contains a `..`, `.`, or empty segment —
/// the only components served are plain, forward names. Kept separate from
/// the filesystem read so the policy is unit-tested without a real bundle.
fn safe_relative_path(request_path: &str) -> Option<PathBuf> {
    let rel = request_path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    if rel
        .split('/')
        .any(|seg| seg == ".." || seg == "." || seg.is_empty())
    {
        return None;
    }
    Some(PathBuf::from(rel))
}

/// Whether `request_path` resolves to a real file inside the bundle
/// directory. Used by the auth gate ([`super::RedDBServer::is_authorized`])
/// to allow exactly the inert asset paths through without authentication —
/// a `GET` to an authenticated API route is never opened, because it has
/// no matching file in the bundle.
pub(crate) fn bundle_asset_exists(ui_dir: &Path, request_path: &str) -> bool {
    match safe_relative_path(request_path) {
        Some(rel) => ui_dir.join(rel).is_file(),
        None => false,
    }
}

/// Serve `request_path` from the bundle directory as a buffered
/// [`HttpResponse`]. `/` maps to `index.html`. Returns `None` when the
/// path traverses or the file does not exist, so the caller falls through
/// to its own 404 — the static surface never invents a response for a
/// missing asset.
pub(crate) fn serve_bundle_asset(ui_dir: &Path, request_path: &str) -> Option<HttpResponse> {
    let rel = safe_relative_path(request_path)?;
    let content_type = content_type_for(&rel.to_string_lossy());
    let body = std::fs::read(ui_dir.join(&rel)).ok()?;
    Some(HttpResponse {
        status: 200,
        body,
        content_type,
        extra_headers: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn bundle() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        fs::write(dir.path().join("index.html"), b"<html>red-ui</html>").unwrap();
        fs::write(dir.path().join("app.js"), b"console.log('ui')").unwrap();
        fs::create_dir(dir.path().join("assets")).unwrap();
        fs::write(dir.path().join("assets/style.css"), b"body{}").unwrap();
        dir
    }

    #[test]
    fn root_serves_index_html() {
        let dir = bundle();
        let resp = serve_bundle_asset(dir.path(), "/").expect("index served");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.content_type, "text/html; charset=utf-8");
        assert_eq!(resp.body, b"<html>red-ui</html>");
    }

    #[test]
    fn named_asset_is_served_with_content_type() {
        let dir = bundle();
        let resp = serve_bundle_asset(dir.path(), "/app.js").expect("js served");
        assert_eq!(resp.content_type, "text/javascript; charset=utf-8");
        assert_eq!(resp.body, b"console.log('ui')");

        let css = serve_bundle_asset(dir.path(), "/assets/style.css").expect("css served");
        assert_eq!(css.content_type, "text/css; charset=utf-8");
    }

    #[test]
    fn missing_asset_returns_none() {
        let dir = bundle();
        assert!(serve_bundle_asset(dir.path(), "/nope.js").is_none());
    }

    #[test]
    fn traversal_is_refused() {
        let dir = bundle();
        assert!(serve_bundle_asset(dir.path(), "/../secret").is_none());
        assert!(serve_bundle_asset(dir.path(), "/assets/../../etc/passwd").is_none());
        assert!(safe_relative_path("/a/./b").is_none());
    }

    #[test]
    fn bundle_asset_exists_tracks_real_files_only() {
        let dir = bundle();
        assert!(bundle_asset_exists(dir.path(), "/"));
        assert!(bundle_asset_exists(dir.path(), "/index.html"));
        assert!(bundle_asset_exists(dir.path(), "/app.js"));
        assert!(bundle_asset_exists(dir.path(), "/assets/style.css"));
        // A real API route shape has no matching bundle file → stays gated.
        assert!(!bundle_asset_exists(dir.path(), "/query"));
        assert!(!bundle_asset_exists(
            dir.path(),
            "/collections/foo/documents/bar"
        ));
        assert!(!bundle_asset_exists(dir.path(), "/../secret"));
    }
}
