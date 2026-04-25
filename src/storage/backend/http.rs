//! Generic HTTP `RemoteBackend` (PLAN.md Phase 2.3).
//!
//! Speaks plain `PUT` / `GET` / `DELETE` against a configurable
//! base URL. The intent is maximum portability: any custom service
//! that exposes object storage over HTTP — IPFS gateways, in-house
//! storage proxies, ad-hoc backup hosts, anything that takes a body
//! on PUT and serves it back on GET — can serve as RedDB's backup
//! target without writing a new backend.
//!
//! Wire contract:
//!   - `PUT  {base}/{prefix}{key}` — body = file bytes
//!   - `GET  {base}/{prefix}{key}` — 200 returns body, 404 means
//!     "doesn't exist" (treated as `Ok(false)` by `download`)
//!   - `DELETE {base}/{prefix}{key}` — 200/204 ok, 404 ignored
//!   - `GET {base}/{prefix}?list=<sub-prefix>` — newline-delimited
//!     list of keys, one per line
//!
//! Auth: every request adds the `Authorization` header from
//! `HttpBackendConfig::auth_header`. The factory in service_cli
//! reads it from `RED_HTTP_AUTH_HEADER_FILE` so the actual token
//! never appears in env (Kubernetes Secrets / Vault Agent friendly).
//!
//! Transport: shells out to `curl(1)`, matching the S3 backend's
//! choice. No TLS crate baked in, no async runtime requirement,
//! universally available on every Linux/macOS/BSD distro.

use std::path::Path;
use std::process::Command;

use super::{BackendError, RemoteBackend};

/// Configuration for the generic HTTP backend.
#[derive(Debug, Clone)]
pub struct HttpBackendConfig {
    /// Base URL (e.g. `https://storage.example.com`). No trailing slash.
    pub base_url: String,
    /// Prefix prepended to every key (e.g. `databases/prod/`).
    /// Empty string means "no prefix".
    pub prefix: String,
    /// Optional `Authorization: <value>` header. `None` means no auth.
    pub auth_header: Option<String>,
}

impl HttpBackendConfig {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            prefix: String::new(),
            auth_header: None,
        }
    }

    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        let mut p = prefix.into();
        if !p.is_empty() && !p.ends_with('/') {
            p.push('/');
        }
        self.prefix = p;
        self
    }

    pub fn with_auth_header(mut self, value: impl Into<String>) -> Self {
        self.auth_header = Some(value.into());
        self
    }
}

pub struct HttpBackend {
    config: HttpBackendConfig,
}

impl HttpBackend {
    pub fn new(config: HttpBackendConfig) -> Self {
        Self { config }
    }

    fn url_for(&self, key: &str) -> String {
        format!(
            "{}/{}{}",
            self.config.base_url,
            self.config.prefix,
            key.trim_start_matches('/')
        )
    }

    /// Run `curl` with the configured auth header and return its
    /// process output. The caller decides what to do with non-zero
    /// exit codes — for `download` a 404 is success-with-false, for
    /// `upload` any non-2xx is an error.
    fn curl(&self, args: &[&str]) -> Result<std::process::Output, BackendError> {
        let mut cmd = Command::new("curl");
        cmd.arg("-sS"); // silent + show errors
        cmd.arg("-w").arg("HTTPSTATUS:%{http_code}");
        for &a in args {
            cmd.arg(a);
        }
        if let Some(ref auth) = self.config.auth_header {
            cmd.arg("-H").arg(format!("Authorization: {}", auth));
        }
        cmd.output()
            .map_err(|e| BackendError::Transport(format!("curl not available: {e}")))
    }

    /// Parse the trailing `HTTPSTATUS:NNN` token curl emits and
    /// return `(http_code, body_without_status)`.
    fn split_status(stdout: &[u8]) -> (u16, Vec<u8>) {
        let s = String::from_utf8_lossy(stdout);
        if let Some(idx) = s.rfind("HTTPSTATUS:") {
            let body = stdout[..idx].to_vec();
            let code: u16 = s[idx + "HTTPSTATUS:".len()..]
                .trim()
                .parse()
                .unwrap_or(0);
            (code, body)
        } else {
            (0, stdout.to_vec())
        }
    }
}

impl RemoteBackend for HttpBackend {
    fn name(&self) -> &str {
        "http"
    }

    fn download(
        &self,
        remote_key: &str,
        local_path: &Path,
    ) -> Result<bool, BackendError> {
        let url = self.url_for(remote_key);
        // Stream body to a temp file via -o; we still want HTTPSTATUS
        // in stdout for the success/404 distinction.
        let local_path_str = local_path.to_string_lossy().to_string();
        let output = self.curl(&["-o", &local_path_str, "-X", "GET", &url])?;
        if !output.status.success() {
            // curl exits non-zero on transport errors (DNS, connection
            // reset). Treat that as a hard failure regardless of HTTP
            // code, since stdout may be empty.
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(BackendError::Transport(format!(
                "http GET {url}: curl failed: {stderr}"
            )));
        }
        let (code, _body) = Self::split_status(&output.stdout);
        match code {
            200..=299 => Ok(true),
            404 => {
                // Make sure we don't leave a zero-byte file behind
                // that downstream code mistakes for a real download.
                let _ = std::fs::remove_file(local_path);
                Ok(false)
            }
            _ => Err(BackendError::Transport(format!(
                "http GET {url} returned status {code}"
            ))),
        }
    }

    fn upload(&self, local_path: &Path, remote_key: &str) -> Result<(), BackendError> {
        let url = self.url_for(remote_key);
        let local_path_str = local_path.to_string_lossy().to_string();
        let output = self.curl(&[
            "-X",
            "PUT",
            "--data-binary",
            &format!("@{}", local_path_str),
            &url,
        ])?;
        if !output.status.success() {
            return Err(BackendError::Transport(format!(
                "http PUT {url}: curl failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        let (code, body) = Self::split_status(&output.stdout);
        if !(200..=299).contains(&code) {
            return Err(BackendError::Transport(format!(
                "http PUT {url} returned status {code}: {}",
                String::from_utf8_lossy(&body)
            )));
        }
        Ok(())
    }

    fn exists(&self, remote_key: &str) -> Result<bool, BackendError> {
        let url = self.url_for(remote_key);
        let output = self.curl(&["-I", "-X", "HEAD", &url])?;
        if !output.status.success() {
            return Err(BackendError::Transport(format!(
                "http HEAD {url}: curl failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        let (code, _) = Self::split_status(&output.stdout);
        match code {
            200..=299 => Ok(true),
            404 => Ok(false),
            other => Err(BackendError::Transport(format!(
                "http HEAD {url} returned status {other}"
            ))),
        }
    }

    fn delete(&self, remote_key: &str) -> Result<(), BackendError> {
        let url = self.url_for(remote_key);
        let output = self.curl(&["-X", "DELETE", &url])?;
        if !output.status.success() {
            return Err(BackendError::Transport(format!(
                "http DELETE {url}: curl failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        let (code, _) = Self::split_status(&output.stdout);
        match code {
            200..=299 | 404 => Ok(()),
            other => Err(BackendError::Transport(format!(
                "http DELETE {url} returned status {other}"
            ))),
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError> {
        // Convention: GET base/?list=<sub-prefix> returns
        // newline-delimited keys. Servers that don't implement this
        // can still serve the rest of the API; list will return an
        // empty vec and PITR / archiver code will treat it as "no
        // archived segments".
        let url = format!(
            "{}/{}?list={}",
            self.config.base_url,
            self.config.prefix.trim_end_matches('/'),
            urlencode_simple(prefix)
        );
        let output = self.curl(&["-X", "GET", &url])?;
        if !output.status.success() {
            return Ok(Vec::new());
        }
        let (code, body) = Self::split_status(&output.stdout);
        if !(200..=299).contains(&code) {
            return Ok(Vec::new());
        }
        let text = String::from_utf8_lossy(&body);
        Ok(text
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect())
    }
}

/// Minimal RFC3986 percent-encoder for the query-string `list=` value.
/// Doesn't pull in `url` or `percent-encoding` to keep the engine's
/// dependency surface flat.
fn urlencode_simple(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char);
            }
            other => {
                use std::fmt::Write;
                let _ = write!(out, "%{:02X}", other);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_for_strips_leading_slash() {
        let backend = HttpBackend::new(HttpBackendConfig::new("https://store.example/").with_prefix("dbs/prod"));
        assert_eq!(
            backend.url_for("/snapshots/1.snap"),
            "https://store.example/dbs/prod/snapshots/1.snap"
        );
    }

    #[test]
    fn url_for_with_no_prefix() {
        let backend = HttpBackend::new(HttpBackendConfig::new("https://store.example"));
        assert_eq!(backend.url_for("a/b"), "https://store.example/a/b");
    }

    #[test]
    fn split_status_parses_curl_output() {
        let stdout = b"hello world\nHTTPSTATUS:200";
        let (code, body) = HttpBackend::split_status(stdout);
        assert_eq!(code, 200);
        assert_eq!(body, b"hello world\n");
    }

    #[test]
    fn split_status_handles_404() {
        let stdout = b"HTTPSTATUS:404";
        let (code, body) = HttpBackend::split_status(stdout);
        assert_eq!(code, 404);
        assert!(body.is_empty());
    }

    #[test]
    fn urlencode_keeps_path_separators() {
        // We use `/` in list prefixes; encoding it would break
        // server-side prefix matching.
        assert_eq!(urlencode_simple("snapshots/2026"), "snapshots/2026");
    }

    #[test]
    fn urlencode_escapes_spaces() {
        assert_eq!(urlencode_simple("hello world"), "hello%20world");
    }
}
