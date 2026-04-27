//! S3-Compatible Storage Backend
//!
//! Works with: AWS S3, Cloudflare R2, DigitalOcean Spaces, Google Cloud Storage (S3 mode).
//!
//! # Configuration
//!
//! | Provider | Endpoint | Region |
//! |----------|----------|--------|
//! | AWS S3 | `https://s3.{region}.amazonaws.com` | us-east-1, etc. |
//! | Cloudflare R2 | `https://{account_id}.r2.cloudflarestorage.com` | auto |
//! | DigitalOcean Spaces | `https://{region}.digitaloceanspaces.com` | nyc3, sfo3, etc. |
//! | Google Cloud Storage | `https://storage.googleapis.com` | us, eu, etc. |
//!
//! # Transport
//!
//! Uses `curl` via `std::process::Command` for HTTP transport. This avoids pulling in a
//! TLS library while remaining universally available on all target platforms.
//!
//! Cross-platform note: the null device path differs per OS (`/dev/null` on
//! Unix, `NUL` on Windows). `null_device()` below picks the right one so
//! `curl -o` discards response bodies correctly on every platform.
//!
//! # Example
//! ```ignore
//! use reddb::storage::backend::s3::{S3Backend, S3Config};
//!
//! let backend = S3Backend::new(S3Config::aws(
//!     "my-bucket", "us-east-1", "AKIAIOSFODNN7EXAMPLE", "wJalrXUtnFEMI/K7MDENG..."
//! ));
//! ```

use super::{
    AtomicRemoteBackend, BackendError, BackendObjectVersion, ConditionalDelete, ConditionalPut,
    RemoteBackend,
};
use crate::crypto;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for an S3-compatible object storage endpoint.
#[derive(Debug, Clone)]
pub struct S3Config {
    /// S3-compatible endpoint URL (e.g., `"https://s3.us-east-1.amazonaws.com"`).
    /// Must include scheme (`https://`). No trailing slash.
    pub endpoint: String,
    /// Bucket name.
    pub bucket: String,
    /// Key prefix prepended to every `remote_key` (e.g., `"databases/"`).
    pub key_prefix: String,
    /// Access key ID.
    pub access_key: String,
    /// Secret access key.
    pub secret_key: String,
    /// AWS region (e.g., `"us-east-1"`, `"auto"` for R2).
    pub region: String,
    /// Use path-style addressing (`{endpoint}/{bucket}/{key}`) instead
    /// of virtual-host-style (`{scheme}://{bucket}.{endpoint_host}/{key}`).
    /// Required for MinIO, Ceph RGW, Garage, SeaweedFS, and any
    /// self-hosted S3-compatible store. AWS S3 / R2 / DO Spaces /
    /// GCS interop accept either style — true is the safe default
    /// for cloud-agnostic deployments. Override via env
    /// `RED_S3_PATH_STYLE=false` for providers that hard-require
    /// virtual-host (rare).
    pub path_style: bool,
}

impl S3Config {
    /// Create a config targeting **AWS S3**.
    pub fn aws(bucket: &str, region: &str, access_key: &str, secret_key: &str) -> Self {
        Self {
            endpoint: format!("https://s3.{region}.amazonaws.com"),
            bucket: bucket.into(),
            key_prefix: String::new(),
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            region: region.into(),
            path_style: true,
        }
    }

    /// Create a config targeting **Cloudflare R2**.
    pub fn r2(account_id: &str, bucket: &str, access_key: &str, secret_key: &str) -> Self {
        Self {
            endpoint: format!("https://{account_id}.r2.cloudflarestorage.com"),
            bucket: bucket.into(),
            key_prefix: String::new(),
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            region: "auto".into(),
            path_style: true,
        }
    }

    /// Create a config targeting **DigitalOcean Spaces**.
    pub fn digitalocean(region: &str, bucket: &str, access_key: &str, secret_key: &str) -> Self {
        Self {
            endpoint: format!("https://{region}.digitaloceanspaces.com"),
            bucket: bucket.into(),
            key_prefix: String::new(),
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            region: region.into(),
            path_style: true,
        }
    }

    /// Create a config targeting **Google Cloud Storage** (S3-compatible mode).
    pub fn gcs(bucket: &str, access_key: &str, secret_key: &str) -> Self {
        Self {
            endpoint: "https://storage.googleapis.com".into(),
            bucket: bucket.into(),
            key_prefix: String::new(),
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            region: "us".into(),
            path_style: true,
        }
    }

    /// Create a config targeting a generic S3-compatible endpoint
    /// (MinIO, Ceph, Garage, SeaweedFS, B2 S3-compat, IDrive,
    /// Storj, Wasabi, etc.). The operator passes the full endpoint
    /// URL; region defaults to `us-east-1` because most self-hosted
    /// stores ignore it.
    pub fn generic(endpoint: &str, bucket: &str, access_key: &str, secret_key: &str) -> Self {
        Self {
            endpoint: endpoint.trim_end_matches('/').into(),
            bucket: bucket.into(),
            key_prefix: String::new(),
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            region: "us-east-1".into(),
            path_style: true,
        }
    }

    /// Set a key prefix (e.g., `"databases/prod/"`).
    pub fn with_prefix(mut self, prefix: &str) -> Self {
        self.key_prefix = prefix.into();
        self
    }

    /// Override the addressing style (path-style vs virtual-host).
    pub fn with_path_style(mut self, path_style: bool) -> Self {
        self.path_style = path_style;
        self
    }

    /// Override the AWS region.
    pub fn with_region(mut self, region: &str) -> Self {
        self.region = region.into();
        self
    }
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

/// S3-compatible storage backend.
///
/// Implements AWS Signature Version 4 for request authentication and
/// delegates HTTP transport to `curl(1)`.
pub struct S3Backend {
    config: S3Config,
}

impl S3Backend {
    /// Create a new S3 backend with the given configuration.
    pub fn new(config: S3Config) -> Self {
        Self { config }
    }

    // -----------------------------------------------------------------------
    // Request signing (AWS Signature Version 4)
    // -----------------------------------------------------------------------

    /// Build a fully-signed set of HTTP headers for an S3 request.
    ///
    /// Returns a `BTreeMap` of header name -> value that must be sent with the
    /// HTTP request. The map includes `Host`, `x-amz-date`,
    /// `x-amz-content-sha256`, and `Authorization`.
    fn build_signed_request(
        &self,
        method: &str,
        object_key: &str,
        body: &[u8],
    ) -> Result<BTreeMap<String, String>, BackendError> {
        self.build_signed_request_with_query(method, object_key, "", body)
    }

    fn build_signed_request_with_query(
        &self,
        method: &str,
        object_key: &str,
        canonical_querystring: &str,
        body: &[u8],
    ) -> Result<BTreeMap<String, String>, BackendError> {
        self.build_signed_request_with_query_and_headers(
            method,
            object_key,
            canonical_querystring,
            body,
            &[],
        )
    }

    fn build_signed_request_with_query_and_headers(
        &self,
        method: &str,
        object_key: &str,
        canonical_querystring: &str,
        body: &[u8],
        extra_headers: &[(&str, &str)],
    ) -> Result<BTreeMap<String, String>, BackendError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| BackendError::Internal(format!("clock error: {e}")))?;

        let secs = now.as_secs();
        let (timestamp, datestamp) = format_iso8601(secs);

        let body_hash = sha256_hex(body);

        // Extract host from endpoint (strip scheme).
        let host = self
            .config
            .endpoint
            .trim_start_matches("https://")
            .trim_start_matches("http://");

        let mut headers = BTreeMap::new();
        headers.insert("host".into(), host.into());
        headers.insert("x-amz-content-sha256".into(), body_hash.clone());
        headers.insert("x-amz-date".into(), timestamp.clone());

        // For PUT requests, set content-type so S3 doesn't reject.
        if method == "PUT" {
            headers.insert("content-type".into(), "application/octet-stream".into());
        }
        for (name, value) in extra_headers {
            headers.insert(name.to_ascii_lowercase(), value.trim().to_string());
        }

        let auth = sign_s3v4(
            method,
            object_key,
            canonical_querystring,
            &headers,
            &body_hash,
            &self.config,
            &timestamp,
            &datestamp,
        );

        headers.insert("Authorization".into(), auth);

        Ok(headers)
    }

    fn split_status(stdout: &[u8]) -> (u16, Vec<u8>) {
        let s = String::from_utf8_lossy(stdout);
        if let Some(idx) = s.rfind("HTTPSTATUS:") {
            let body = stdout[..idx].to_vec();
            let code = s[idx + "HTTPSTATUS:".len()..].trim().parse().unwrap_or(0);
            (code, body)
        } else {
            (0, stdout.to_vec())
        }
    }

    fn header_value(headers: &[u8], name: &str) -> Option<String> {
        let needle = format!("{}:", name.to_ascii_lowercase());
        String::from_utf8_lossy(headers)
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim();
                let lower = trimmed.to_ascii_lowercase();
                lower
                    .starts_with(&needle)
                    .then(|| trimmed[needle.len()..].trim().to_string())
            })
            .last()
            .filter(|value| !value.is_empty())
    }

    /// Build the full URL for the given object key, honoring the
    /// configured addressing style.
    ///
    /// Path-style: `https://endpoint/bucket/key` — what MinIO, Ceph,
    /// Garage, SeaweedFS, and self-hosted S3-compatible stores
    /// require.
    ///
    /// Virtual-host style: `https://bucket.endpoint-host/key` — the
    /// modern AWS default. Switching is a one-line config change in
    /// case a provider rejects path-style.
    fn object_url(&self, object_key: &str) -> String {
        if self.config.path_style {
            return format!(
                "{}/{}/{}",
                self.config.endpoint, self.config.bucket, object_key
            );
        }
        // Virtual-host style: prepend bucket as a subdomain of the
        // endpoint host. Splits scheme + host carefully so an
        // endpoint with a non-default port (`https://host:9000`)
        // still produces a valid URL.
        let trimmed = self.config.endpoint.trim_end_matches('/');
        if let Some(rest) = trimmed.strip_prefix("https://") {
            return format!("https://{}.{}/{}", self.config.bucket, rest, object_key);
        }
        if let Some(rest) = trimmed.strip_prefix("http://") {
            return format!("http://{}.{}/{}", self.config.bucket, rest, object_key);
        }
        // Endpoint missing a scheme — fall back to path-style; safer
        // than producing a malformed URL the operator can't debug.
        format!("{}/{}/{}", trimmed, self.config.bucket, object_key)
    }

    /// Build the full object key by prepending the configured prefix.
    fn full_key(&self, remote_key: &str) -> String {
        format!("{}{}", self.config.key_prefix, remote_key)
    }

    /// Execute a curl command and return its output.
    fn exec_curl(cmd: &mut std::process::Command) -> Result<std::process::Output, BackendError> {
        cmd.output()
            .map_err(|e| BackendError::Transport(format!("curl not available: {e}")))
    }

    /// Platform-specific "null device" for `curl -o`. On Unix it's
    /// `/dev/null`; on Windows it's `NUL`. Callers that pass this to
    /// the `-o` flag get discarded response bodies on every platform.
    #[inline]
    fn null_device() -> &'static str {
        #[cfg(windows)]
        {
            "NUL"
        }
        #[cfg(not(windows))]
        {
            "/dev/null"
        }
    }
}

// ---------------------------------------------------------------------------
// RemoteBackend implementation
// ---------------------------------------------------------------------------

impl RemoteBackend for S3Backend {
    fn name(&self) -> &str {
        "s3"
    }

    fn download(&self, remote_key: &str, local_path: &Path) -> Result<bool, BackendError> {
        let object_key = self.full_key(remote_key);
        let url = self.object_url(&object_key);

        let headers = self.build_signed_request("GET", &object_key, &[])?;

        let mut cmd = std::process::Command::new("curl");
        cmd.arg("-sf").arg("-o").arg(local_path.as_os_str());
        for (k, v) in &headers {
            cmd.arg("-H").arg(format!("{k}: {v}"));
        }
        cmd.arg(&url);

        let output = Self::exec_curl(&mut cmd)?;

        if output.status.success() {
            Ok(true)
        } else if output.status.code() == Some(22) {
            // curl exit code 22 = HTTP 4xx error (typically 404 Not Found)
            Ok(false)
        } else {
            Err(BackendError::Transport(format!(
                "download failed (exit {}): {}",
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr)
            )))
        }
    }

    fn upload(&self, local_path: &Path, remote_key: &str) -> Result<(), BackendError> {
        let data = fs::read(local_path)
            .map_err(|e| BackendError::Transport(format!("read local file: {e}")))?;

        let object_key = self.full_key(remote_key);
        let url = self.object_url(&object_key);

        let headers = self.build_signed_request("PUT", &object_key, &data)?;

        let mut cmd = std::process::Command::new("curl");
        cmd.arg("-sf")
            .arg("-X")
            .arg("PUT")
            .arg("--data-binary")
            .arg(format!("@{}", local_path.display()));
        for (k, v) in &headers {
            cmd.arg("-H").arg(format!("{k}: {v}"));
        }
        cmd.arg(&url);

        let output = Self::exec_curl(&mut cmd)?;

        if output.status.success() {
            Ok(())
        } else {
            Err(BackendError::Transport(format!(
                "upload failed (exit {}): {}",
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr)
            )))
        }
    }

    fn exists(&self, remote_key: &str) -> Result<bool, BackendError> {
        let object_key = self.full_key(remote_key);
        let url = self.object_url(&object_key);

        let headers = self.build_signed_request("HEAD", &object_key, &[])?;

        let mut cmd = std::process::Command::new("curl");
        cmd.arg("-sf")
            .arg("-I") // HEAD request
            .arg("-o")
            .arg(Self::null_device());
        for (k, v) in &headers {
            cmd.arg("-H").arg(format!("{k}: {v}"));
        }
        cmd.arg(&url);

        let output = Self::exec_curl(&mut cmd)?;

        if output.status.success() {
            Ok(true)
        } else if output.status.code() == Some(22) {
            Ok(false)
        } else {
            Err(BackendError::Transport(format!(
                "exists check failed (exit {}): {}",
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr)
            )))
        }
    }

    fn delete(&self, remote_key: &str) -> Result<(), BackendError> {
        let object_key = self.full_key(remote_key);
        let url = self.object_url(&object_key);

        let headers = self.build_signed_request("DELETE", &object_key, &[])?;

        let mut cmd = std::process::Command::new("curl");
        cmd.arg("-sf")
            .arg("-X")
            .arg("DELETE")
            .arg("-o")
            .arg(Self::null_device());
        for (k, v) in &headers {
            cmd.arg("-H").arg(format!("{k}: {v}"));
        }
        cmd.arg(&url);

        let output = Self::exec_curl(&mut cmd)?;

        // S3 returns 204 on successful delete, or 404 if already gone -- both are fine.
        if output.status.success() || output.status.code() == Some(22) {
            Ok(())
        } else {
            Err(BackendError::Transport(format!(
                "delete failed (exit {}): {}",
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr)
            )))
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError> {
        let object_prefix = self.full_key(prefix);
        let canonical_querystring =
            format!("list-type=2&prefix={}", uri_encode(&object_prefix, true));
        let url = format!(
            "{}/{}?{}",
            self.config.endpoint, self.config.bucket, canonical_querystring
        );
        let headers =
            self.build_signed_request_with_query("GET", "", &canonical_querystring, &[])?;

        let mut cmd = std::process::Command::new("curl");
        cmd.arg("-sf");
        for (k, v) in &headers {
            cmd.arg("-H").arg(format!("{k}: {v}"));
        }
        cmd.arg(&url);

        let output = Self::exec_curl(&mut cmd)?;
        if !output.status.success() {
            return Err(BackendError::Transport(format!(
                "list failed (exit {}): {}",
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        let xml = String::from_utf8(output.stdout).map_err(|err| {
            BackendError::Transport(format!("list response was not utf-8: {err}"))
        })?;
        Ok(parse_list_objects_keys(&xml)
            .into_iter()
            .filter_map(|key| {
                key.strip_prefix(&self.config.key_prefix)
                    .map(|k| k.to_string())
            })
            .collect())
    }
}

impl AtomicRemoteBackend for S3Backend {
    fn object_version(
        &self,
        remote_key: &str,
    ) -> Result<Option<BackendObjectVersion>, BackendError> {
        let object_key = self.full_key(remote_key);
        let url = self.object_url(&object_key);
        let headers = self.build_signed_request("HEAD", &object_key, &[])?;

        let mut cmd = std::process::Command::new("curl");
        cmd.arg("-sS")
            .arg("-D")
            .arg("-")
            .arg("-o")
            .arg(Self::null_device())
            .arg("-w")
            .arg("HTTPSTATUS:%{http_code}")
            .arg("-X")
            .arg("HEAD");
        for (k, v) in &headers {
            cmd.arg("-H").arg(format!("{k}: {v}"));
        }
        cmd.arg(&url);

        let output = Self::exec_curl(&mut cmd)?;
        if !output.status.success() {
            return Err(BackendError::Transport(format!(
                "s3 HEAD {url}: curl failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        let (code, body) = Self::split_status(&output.stdout);
        match code {
            200..=299 => Self::header_value(&body, "etag")
                .map(BackendObjectVersion::new)
                .map(Some)
                .ok_or_else(|| BackendError::Internal(format!("s3 HEAD {url} missing ETag"))),
            404 => Ok(None),
            401 | 403 => Err(BackendError::Auth(format!(
                "s3 HEAD {url} returned status {code}"
            ))),
            other => Err(BackendError::Transport(format!(
                "s3 HEAD {url} returned status {other}"
            ))),
        }
    }

    fn upload_conditional(
        &self,
        local_path: &Path,
        remote_key: &str,
        condition: ConditionalPut,
    ) -> Result<BackendObjectVersion, BackendError> {
        let data = fs::read(local_path)
            .map_err(|e| BackendError::Transport(format!("read local file: {e}")))?;
        let object_key = self.full_key(remote_key);
        let url = self.object_url(&object_key);
        let condition_header = match &condition {
            ConditionalPut::IfAbsent => ("if-none-match", "*"),
            ConditionalPut::IfVersion(version) => ("if-match", version.token.as_str()),
        };
        let headers = self.build_signed_request_with_query_and_headers(
            "PUT",
            &object_key,
            "",
            &data,
            &[condition_header],
        )?;

        let mut cmd = std::process::Command::new("curl");
        cmd.arg("-sS")
            .arg("-o")
            .arg(Self::null_device())
            .arg("-w")
            .arg("HTTPSTATUS:%{http_code}")
            .arg("-X")
            .arg("PUT")
            .arg("--data-binary")
            .arg(format!("@{}", local_path.display()));
        for (k, v) in &headers {
            cmd.arg("-H").arg(format!("{k}: {v}"));
        }
        cmd.arg(&url);

        let output = Self::exec_curl(&mut cmd)?;
        if !output.status.success() {
            return Err(BackendError::Transport(format!(
                "s3 conditional PUT {url}: curl failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        let (code, _) = Self::split_status(&output.stdout);
        match code {
            200..=299 => self.object_version(remote_key)?.ok_or_else(|| {
                BackendError::Internal(format!("s3 object '{}' missing after upload", remote_key))
            }),
            404 | 409 | 412 => Err(BackendError::PreconditionFailed(format!(
                "s3 conditional PUT {url} returned status {code}"
            ))),
            401 | 403 => Err(BackendError::Auth(format!(
                "s3 conditional PUT {url} returned status {code}"
            ))),
            other => Err(BackendError::Transport(format!(
                "s3 conditional PUT {url} returned status {other}"
            ))),
        }
    }

    fn delete_conditional(
        &self,
        remote_key: &str,
        condition: ConditionalDelete,
    ) -> Result<(), BackendError> {
        let object_key = self.full_key(remote_key);
        let url = self.object_url(&object_key);
        let ConditionalDelete::IfVersion(version) = condition;
        let headers = self.build_signed_request_with_query_and_headers(
            "DELETE",
            &object_key,
            "",
            &[],
            &[("if-match", version.token.as_str())],
        )?;

        let mut cmd = std::process::Command::new("curl");
        cmd.arg("-sS")
            .arg("-o")
            .arg(Self::null_device())
            .arg("-w")
            .arg("HTTPSTATUS:%{http_code}")
            .arg("-X")
            .arg("DELETE");
        for (k, v) in &headers {
            cmd.arg("-H").arg(format!("{k}: {v}"));
        }
        cmd.arg(&url);

        let output = Self::exec_curl(&mut cmd)?;
        if !output.status.success() {
            return Err(BackendError::Transport(format!(
                "s3 conditional DELETE {url}: curl failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        let (code, _) = Self::split_status(&output.stdout);
        match code {
            200..=299 => Ok(()),
            404 | 409 | 412 => Err(BackendError::PreconditionFailed(format!(
                "s3 conditional DELETE {url} returned status {code}"
            ))),
            401 | 403 => Err(BackendError::Auth(format!(
                "s3 conditional DELETE {url} returned status {code}"
            ))),
            other => Err(BackendError::Transport(format!(
                "s3 conditional DELETE {url} returned status {other}"
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// AWS Signature Version 4 implementation
// ---------------------------------------------------------------------------

/// Compute the `Authorization` header value for AWS Signature Version 4.
fn sign_s3v4(
    method: &str,
    object_key: &str,
    canonical_querystring: &str,
    headers: &BTreeMap<String, String>,
    body_hash: &str,
    config: &S3Config,
    timestamp: &str,
    datestamp: &str,
) -> String {
    let service = "s3";

    // -- Step 1: Canonical request ----------------------------------------

    // Canonical URI: /{bucket}/{key} — URI-encode each path component.
    let canonical_uri = format!(
        "/{}{}",
        uri_encode(&config.bucket, false),
        if object_key.is_empty() {
            String::new()
        } else {
            format!("/{}", uri_encode_path(object_key))
        }
    );

    // Canonical headers (sorted, lower-cased keys, trimmed values).
    let mut canonical_headers = String::new();
    let mut signed_header_names: Vec<&str> = Vec::new();
    for (k, v) in headers {
        canonical_headers.push_str(&format!("{}:{}\n", k.to_lowercase(), v.trim()));
        signed_header_names.push(k.as_str());
    }
    let signed_headers = signed_header_names
        .iter()
        .map(|s| s.to_lowercase())
        .collect::<Vec<_>>()
        .join(";");

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method, canonical_uri, canonical_querystring, canonical_headers, signed_headers, body_hash
    );

    // -- Step 2: String to sign -------------------------------------------

    let credential_scope = format!("{datestamp}/{}/{service}/aws4_request", config.region);

    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        timestamp,
        credential_scope,
        sha256_hex(canonical_request.as_bytes()),
    );

    // -- Step 3: Signing key derivation -----------------------------------
    //
    // HMAC-SHA256 chain:
    //   kDate    = HMAC("AWS4" + secret_key, datestamp)
    //   kRegion  = HMAC(kDate, region)
    //   kService = HMAC(kRegion, service)
    //   kSigning = HMAC(kService, "aws4_request")

    let k_date = hmac_sha256(
        format!("AWS4{}", config.secret_key).as_bytes(),
        datestamp.as_bytes(),
    );
    let k_region = hmac_sha256(&k_date, config.region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"aws4_request");

    // -- Step 4: Signature ------------------------------------------------

    let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()));

    format!(
        "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
        config.access_key, credential_scope, signed_headers, signature
    )
}

fn parse_list_objects_keys(xml: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut rest = xml;
    let open = "<Key>";
    let close = "</Key>";
    while let Some(start) = rest.find(open) {
        let after_start = &rest[start + open.len()..];
        let Some(end) = after_start.find(close) else {
            break;
        };
        keys.push(after_start[..end].to_string());
        rest = &after_start[end + close.len()..];
    }
    keys
}

// ---------------------------------------------------------------------------
// Crypto helpers (delegate to crate::crypto)
// ---------------------------------------------------------------------------

/// HMAC-SHA256 returning raw bytes.
fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    crypto::hmac::hmac_sha256(key, data).to_vec()
}

/// SHA-256 returning lower-case hex string.
fn sha256_hex(data: &[u8]) -> String {
    hex::encode(crypto::sha256::sha256(data))
}

// ---------------------------------------------------------------------------
// URI encoding helpers (per AWS Sig v4 spec)
// ---------------------------------------------------------------------------

/// URI-encode a string per RFC 3986, plus AWS quirks:
///   - Unreserved characters (A-Z a-z 0-9 - _ . ~) are NOT encoded.
///   - Everything else is percent-encoded.
///   - If `encode_slash` is false, '/' is kept literal (for path components).
fn uri_encode(input: &str, encode_slash: bool) -> String {
    let mut encoded = String::with_capacity(input.len() * 2);
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            b'/' if !encode_slash => {
                encoded.push('/');
            }
            _ => {
                encoded.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    encoded
}

/// URI-encode a full object key path, keeping '/' separators literal.
fn uri_encode_path(path: &str) -> String {
    uri_encode(path, false)
}

// ---------------------------------------------------------------------------
// Time formatting
// ---------------------------------------------------------------------------

/// Produce ISO 8601 timestamp and datestamp from Unix epoch seconds.
///
/// Returns `("20260409T120000Z", "20260409")`.
fn format_iso8601(epoch_secs: u64) -> (String, String) {
    // Manual UTC calendar conversion (no chrono dependency).
    let secs_per_day: u64 = 86400;
    let days = epoch_secs / secs_per_day;
    let day_secs = epoch_secs % secs_per_day;

    let hours = day_secs / 3600;
    let minutes = (day_secs % 3600) / 60;
    let seconds = day_secs % 60;

    // Days since Unix epoch (1970-01-01) to (year, month, day).
    let (year, month, day) = civil_from_days(days as i64);

    let timestamp = format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        year, month, day, hours, minutes, seconds
    );
    let datestamp = format!("{:04}{:02}{:02}", year, month, day);

    (timestamp, datestamp)
}

/// Convert a day count from the Unix epoch to a (year, month, day) triple.
///
/// Adapted from Howard Hinnant's `civil_from_days` algorithm.
fn civil_from_days(mut days: i64) -> (i32, u32, u32) {
    days += 719_468; // shift epoch from 1970-01-01 to 0000-03-01
    let era = if days >= 0 {
        days / 146_097
    } else {
        (days - 146_096) / 146_097
    };
    let doe = (days - era * 146_097) as u32; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // year of era [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month index [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // day [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_hex_empty() {
        // SHA-256 of empty input is the well-known constant.
        let hash = sha256_hex(b"");
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_hmac_sha256_rfc4231_case1() {
        // RFC 4231 Test Case 1
        let key = hex::decode("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b").unwrap();
        let data = b"Hi There";
        let expected = "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7";
        assert_eq!(hex::encode(hmac_sha256(&key, data)), expected);
    }

    #[test]
    fn test_format_iso8601() {
        // 2023-01-15 12:30:45 UTC = 1673785845
        let (ts, ds) = format_iso8601(1_673_785_845);
        assert_eq!(ts, "20230115T123045Z");
        assert_eq!(ds, "20230115");
    }

    #[test]
    fn test_format_iso8601_epoch() {
        let (ts, ds) = format_iso8601(0);
        assert_eq!(ts, "19700101T000000Z");
        assert_eq!(ds, "19700101");
    }

    #[test]
    fn test_uri_encode_simple() {
        assert_eq!(uri_encode("hello world", true), "hello%20world");
        assert_eq!(uri_encode("foo/bar", true), "foo%2Fbar");
        assert_eq!(uri_encode("foo/bar", false), "foo/bar");
    }

    #[test]
    fn test_uri_encode_unreserved() {
        // Unreserved characters should pass through.
        let unreserved = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.~";
        assert_eq!(uri_encode(unreserved, true), unreserved);
    }

    #[test]
    fn test_civil_from_days() {
        // 1970-01-01 = day 0
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2000-01-01 = day 10957
        assert_eq!(civil_from_days(10957), (2000, 1, 1));
        // 2024-02-29 (leap year) = day 19782
        assert_eq!(civil_from_days(19782), (2024, 2, 29));
    }

    #[test]
    fn test_s3v4_signature_known_vector() {
        // Verify signature against a known AWS test vector.
        // This uses the AWS example values from the Sig V4 documentation.
        let config = S3Config {
            endpoint: "https://s3.us-east-1.amazonaws.com".into(),
            bucket: "examplebucket".into(),
            key_prefix: String::new(),
            access_key: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            region: "us-east-1".into(),
            path_style: true,
        };

        let mut headers = BTreeMap::new();
        headers.insert("host".into(), "s3.us-east-1.amazonaws.com".into());
        headers.insert(
            "x-amz-content-sha256".into(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
        );
        headers.insert("x-amz-date".into(), "20130524T000000Z".into());

        let auth = sign_s3v4(
            "GET",
            "test.txt",
            "",
            &headers,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            &config,
            "20130524T000000Z",
            "20130524",
        );

        // Verify structure of the Authorization header.
        assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/"));
        assert!(auth.contains("20130524/us-east-1/s3/aws4_request"));
        assert!(auth.contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date"));
        assert!(auth.contains("Signature="));

        // Ensure the signature is 64 hex chars.
        let sig = auth.split("Signature=").nth(1).unwrap();
        assert_eq!(sig.len(), 64);
        assert!(sig.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_config_constructors() {
        let aws = S3Config::aws("mybucket", "eu-west-1", "AK", "SK");
        assert_eq!(aws.endpoint, "https://s3.eu-west-1.amazonaws.com");
        assert_eq!(aws.region, "eu-west-1");
        assert_eq!(aws.bucket, "mybucket");
        assert!(aws.key_prefix.is_empty());

        let r2 = S3Config::r2("abc123", "mybucket", "AK", "SK");
        assert_eq!(r2.endpoint, "https://abc123.r2.cloudflarestorage.com");
        assert_eq!(r2.region, "auto");

        let do_spaces = S3Config::digitalocean("nyc3", "mybucket", "AK", "SK");
        assert_eq!(do_spaces.endpoint, "https://nyc3.digitaloceanspaces.com");
        assert_eq!(do_spaces.region, "nyc3");

        let gcs = S3Config::gcs("mybucket", "AK", "SK");
        assert_eq!(gcs.endpoint, "https://storage.googleapis.com");
        assert_eq!(gcs.region, "us");
    }

    #[test]
    fn test_config_with_prefix() {
        let config = S3Config::aws("b", "us-east-1", "AK", "SK").with_prefix("databases/prod/");
        assert_eq!(config.key_prefix, "databases/prod/");
    }

    #[test]
    fn test_build_signed_request_structure() {
        let backend = S3Backend::new(S3Config::aws("mybucket", "us-east-1", "AK", "SK"));
        let headers = backend
            .build_signed_request("GET", "test.rdb", &[])
            .unwrap();

        assert!(headers.contains_key("host"));
        assert!(headers.contains_key("x-amz-date"));
        assert!(headers.contains_key("x-amz-content-sha256"));
        assert!(headers.contains_key("Authorization"));

        let auth = &headers["Authorization"];
        assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential=AK/"));
        assert!(auth.contains("/us-east-1/s3/aws4_request"));
    }

    #[test]
    fn test_build_signed_request_put_has_content_type() {
        let backend = S3Backend::new(S3Config::aws("mybucket", "us-east-1", "AK", "SK"));
        let headers = backend
            .build_signed_request("PUT", "test.rdb", b"hello")
            .unwrap();

        assert_eq!(
            headers.get("content-type").unwrap(),
            "application/octet-stream"
        );
    }

    #[test]
    fn test_full_key() {
        let backend =
            S3Backend::new(S3Config::aws("b", "us-east-1", "AK", "SK").with_prefix("db/"));
        assert_eq!(backend.full_key("mydb.rdb"), "db/mydb.rdb");
    }

    #[test]
    fn test_object_url() {
        let backend = S3Backend::new(S3Config::aws("mybucket", "us-east-1", "AK", "SK"));
        assert_eq!(
            backend.object_url("data/test.rdb"),
            "https://s3.us-east-1.amazonaws.com/mybucket/data/test.rdb"
        );
    }

    #[test]
    fn test_object_url_virtual_host_style() {
        let backend = S3Backend::new(
            S3Config::aws("mybucket", "us-east-1", "AK", "SK").with_path_style(false),
        );
        assert_eq!(
            backend.object_url("data/test.rdb"),
            "https://mybucket.s3.us-east-1.amazonaws.com/data/test.rdb"
        );
    }

    #[test]
    fn test_object_url_path_style_minio() {
        // MinIO/Ceph self-hosted endpoint with port. Path-style is
        // the only supported addressing — verify it works for an
        // endpoint that includes a non-default port.
        let backend = S3Backend::new(S3Config::generic(
            "http://minio:9000",
            "bench",
            "minio",
            "minio123",
        ));
        assert_eq!(
            backend.object_url("snapshots/1.snap"),
            "http://minio:9000/bench/snapshots/1.snap"
        );
    }
}
