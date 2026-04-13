//! Cloudflare D1 Storage Backend
//!
//! Stores RedDB database snapshots as base64-encoded BLOBs in a Cloudflare D1
//! database via the REST API.
//!
//! # Schema
//!
//! The backend automatically creates a table (default `reddb_snapshots`) with:
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS reddb_snapshots (
//!     key        TEXT PRIMARY KEY,
//!     data       BLOB,
//!     size       INTEGER,
//!     updated_at INTEGER
//! )
//! ```
//!
//! # Transport
//!
//! Uses `curl` via `std::process::Command` for HTTP transport, consistent with
//! the S3 backend approach. No TLS library dependency required.
//!
//! # API Reference
//!
//! Cloudflare D1 REST API:
//! `POST https://api.cloudflare.com/client/v4/accounts/{account_id}/d1/database/{database_id}/query`
//!
//! # Example
//! ```ignore
//! use reddb::storage::backend::d1::{D1Backend, D1Config};
//!
//! let backend = D1Backend::new(D1Config::new(
//!     "account-id-here",
//!     "database-id-here",
//!     "cf-api-token-here",
//! ));
//! ```

use super::{BackendError, RemoteBackend};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Base64 encode / decode
// ---------------------------------------------------------------------------

const BASE64_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut result = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;

        result.push(BASE64_CHARS[((n >> 18) & 63) as usize] as char);
        result.push(BASE64_CHARS[((n >> 12) & 63) as usize] as char);

        if chunk.len() > 1 {
            result.push(BASE64_CHARS[((n >> 6) & 63) as usize] as char);
        } else {
            result.push('=');
        }

        if chunk.len() > 2 {
            result.push(BASE64_CHARS[(n & 63) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

fn base64_decode(input: &str) -> Result<Vec<u8>, BackendError> {
    let input = input.trim_end_matches('=');
    let mut buf = Vec::with_capacity(input.len() * 3 / 4);

    let lookup = |c: u8| -> Result<u32, BackendError> {
        match c {
            b'A'..=b'Z' => Ok((c - b'A') as u32),
            b'a'..=b'z' => Ok((c - b'a' + 26) as u32),
            b'0'..=b'9' => Ok((c - b'0' + 52) as u32),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err(BackendError::Internal(format!(
                "invalid base64 character: {}",
                c as char,
            ))),
        }
    };

    let bytes: Vec<u8> = input.bytes().collect();
    let chunks = bytes.chunks(4);

    for chunk in chunks {
        let vals: Vec<u32> = chunk
            .iter()
            .map(|&b| lookup(b))
            .collect::<Result<Vec<_>, _>>()?;

        let n = vals
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, &v)| acc | (v << (6 * (3 - i))));

        buf.push((n >> 16) as u8);
        if vals.len() > 2 {
            buf.push((n >> 8) as u8);
        }
        if vals.len() > 3 {
            buf.push(n as u8);
        }
    }

    Ok(buf)
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for Cloudflare D1 HTTP backend.
#[derive(Debug, Clone)]
pub struct D1Config {
    /// Cloudflare account ID.
    pub account_id: String,
    /// D1 database ID.
    pub database_id: String,
    /// Cloudflare API token (Bearer token).
    pub api_token: String,
    /// Table name for storing database snapshots (default: `"reddb_snapshots"`).
    pub table_name: String,
}

impl D1Config {
    /// Create a new D1 config with default table name.
    pub fn new(
        account_id: impl Into<String>,
        database_id: impl Into<String>,
        api_token: impl Into<String>,
    ) -> Self {
        Self {
            account_id: account_id.into(),
            database_id: database_id.into(),
            api_token: api_token.into(),
            table_name: "reddb_snapshots".into(),
        }
    }

    /// Override the table name used for snapshot storage.
    pub fn with_table(mut self, table: impl Into<String>) -> Self {
        self.table_name = table.into();
        self
    }
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

/// Cloudflare D1 storage backend.
///
/// Stores database snapshots as BLOBs in a Cloudflare D1 SQLite database
/// via the REST API. Uses `curl(1)` for HTTP transport.
pub struct D1Backend {
    config: D1Config,
}

impl D1Backend {
    /// Create a new D1 backend with the given configuration.
    pub fn new(config: D1Config) -> Self {
        Self { config }
    }

    /// D1 query API endpoint URL.
    fn query_url(&self) -> String {
        format!(
            "https://api.cloudflare.com/client/v4/accounts/{}/d1/database/{}/query",
            self.config.account_id, self.config.database_id
        )
    }

    /// Execute a D1 query via curl. Returns the raw JSON response body.
    fn exec_query(&self, json_body: &str) -> Result<String, BackendError> {
        let url = self.query_url();

        let mut cmd = std::process::Command::new("curl");
        cmd.arg("-sf")
            .arg("-X")
            .arg("POST")
            .arg("-H")
            .arg(format!("Authorization: Bearer {}", self.config.api_token))
            .arg("-H")
            .arg("Content-Type: application/json")
            .arg("-d")
            .arg(json_body)
            .arg(&url);

        let output = cmd
            .output()
            .map_err(|e| BackendError::Transport(format!("curl not available: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let code = output.status.code().unwrap_or(-1);

            if code == 22 {
                return Err(BackendError::Auth(format!(
                    "d1 HTTP error (exit {code}): {stderr}"
                )));
            }

            return Err(BackendError::Transport(format!(
                "d1 query failed (exit {code}): {stderr}"
            )));
        }

        String::from_utf8(output.stdout)
            .map_err(|e| BackendError::Internal(format!("d1 response is not valid UTF-8: {e}")))
    }

    /// Ensure the snapshot table exists.
    fn ensure_table(&self) -> Result<(), BackendError> {
        let sql = format!(
            "CREATE TABLE IF NOT EXISTS {} (key TEXT PRIMARY KEY, data BLOB, size INTEGER, updated_at INTEGER)",
            self.config.table_name
        );
        let body = format!(r#"{{"sql":"{}"}}"#, sql);
        self.exec_query(&body)?;
        Ok(())
    }

    /// Build a D1 query request body with parameters.
    fn build_query(sql: &str, params: &[&str]) -> String {
        if params.is_empty() {
            return format!(r#"{{"sql":"{}"}}"#, sql);
        }

        let params_json: Vec<String> = params.iter().map(|p| (*p).to_string()).collect();
        let params_str = params_json.join(",");

        format!(r#"{{"sql":"{}","params":[{}]}}"#, sql, params_str)
    }

    /// Extract a string value from the first row, first column of a D1
    /// query response. Returns `None` if there are no rows.
    ///
    /// The D1 response shape (simplified):
    /// ```json
    /// {
    ///   "result": [{
    ///     "results": [{ "data": "<base64>" }],
    ///     "success": true
    ///   }],
    ///   "success": true
    /// }
    /// ```
    ///
    /// We use a minimal JSON scanner rather than pulling in serde.
    fn extract_first_value(response: &str, field: &str) -> Result<Option<String>, BackendError> {
        // D1 wraps results in "result":[{"results":[...]}]
        // Look for the inner "results" array.
        let results_key = "\"results\"";
        let results_start = match response.find(results_key) {
            Some(pos) => pos,
            None => {
                return Err(BackendError::Internal(
                    "d1 response missing \"results\" field".into(),
                ))
            }
        };

        let after_results = &response[results_start + results_key.len()..];

        // Find the opening bracket of results array.
        let arr_start = match after_results.find('[') {
            Some(pos) => pos,
            None => {
                return Err(BackendError::Internal(
                    "d1 response: malformed results array".into(),
                ))
            }
        };

        let arr_content = &after_results[arr_start + 1..].trim_start();

        // Empty results array?
        if arr_content.starts_with(']') {
            return Ok(None);
        }

        // Look for the field name in the first result object.
        let field_key = format!("\"{}\"", field);
        let field_start = match arr_content.find(&field_key) {
            Some(pos) => pos,
            None => return Ok(None), // Field not present in results
        };

        let after_field = &arr_content[field_start + field_key.len()..];

        // Skip past : and whitespace to find the value.
        let after_colon = match after_field.find(':') {
            Some(pos) => &after_field[pos + 1..],
            None => return Ok(None),
        };

        let trimmed = after_colon.trim_start();

        // Value could be a string (quoted) or null.
        if trimmed.starts_with('"') {
            let value_start = 1;
            let rest = &trimmed[value_start..];
            // Find the closing quote, handling escaped quotes.
            let mut end = 0;
            let bytes = rest.as_bytes();
            while end < bytes.len() {
                if bytes[end] == b'"' && (end == 0 || bytes[end - 1] != b'\\') {
                    break;
                }
                end += 1;
            }
            if end < bytes.len() {
                return Ok(Some(rest[..end].to_string()));
            }
            Ok(None)
        } else if trimmed.starts_with("null") {
            Ok(None)
        } else {
            // Numeric or boolean -- read until comma/bracket/brace.
            let end = trimmed.find([',', '}', ']']).unwrap_or(trimmed.len());
            let val = trimmed[..end].trim();
            if val.is_empty() || val == "null" {
                Ok(None)
            } else {
                Ok(Some(val.to_string()))
            }
        }
    }

    /// Check if the D1 response contains any result rows.
    fn has_results(response: &str) -> bool {
        // D1 returns `"results":[]` when no rows match.
        let results_key = "\"results\"";
        if let Some(pos) = response.find(results_key) {
            let after = &response[pos + results_key.len()..];
            if let Some(arr_start) = after.find('[') {
                let content = after[arr_start + 1..].trim_start();
                return !content.starts_with(']');
            }
        }
        false
    }
}

// ---------------------------------------------------------------------------
// RemoteBackend implementation
// ---------------------------------------------------------------------------

impl RemoteBackend for D1Backend {
    fn name(&self) -> &str {
        "d1"
    }

    fn download(&self, remote_key: &str, local_path: &Path) -> Result<bool, BackendError> {
        self.ensure_table()?;

        let sql = format!("SELECT data FROM {} WHERE key = ?1", self.config.table_name);
        let body = Self::build_query(&sql, &[&format!("\"{}\"", remote_key)]);

        let response = self.exec_query(&body)?;

        match Self::extract_first_value(&response, "data")? {
            Some(b64_data) => {
                let bytes = base64_decode(&b64_data)?;
                fs::write(local_path, &bytes)
                    .map_err(|e| BackendError::Transport(format!("write local file: {e}")))?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn upload(&self, local_path: &Path, remote_key: &str) -> Result<(), BackendError> {
        self.ensure_table()?;

        let data = fs::read(local_path)
            .map_err(|e| BackendError::Transport(format!("read local file: {e}")))?;

        let b64_data = base64_encode(&data);
        let size = data.len();

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| BackendError::Internal(format!("clock error: {e}")))?
            .as_secs();

        // D1 does not support blob base64 syntax directly in params the way
        // Turso does. We pass the base64 string and use a CAST or store as text.
        // However, D1 *does* accept raw blob hex via X'...' in SQL, but that
        // requires inlining. Instead, we use the standard parameter binding
        // approach -- D1 will store the blob as a base64 text string, and we
        // decode it on download.
        let sql = format!(
            "INSERT OR REPLACE INTO {} (key, data, size, updated_at) VALUES (?1, ?2, ?3, ?4)",
            self.config.table_name
        );
        let body = Self::build_query(
            &sql,
            &[
                &format!("\"{}\"", remote_key),
                &format!("\"{}\"", b64_data),
                &size.to_string(),
                &now.to_string(),
            ],
        );

        self.exec_query(&body)?;
        Ok(())
    }

    fn exists(&self, remote_key: &str) -> Result<bool, BackendError> {
        self.ensure_table()?;

        let sql = format!("SELECT 1 FROM {} WHERE key = ?1", self.config.table_name);
        let body = Self::build_query(&sql, &[&format!("\"{}\"", remote_key)]);

        let response = self.exec_query(&body)?;
        Ok(Self::has_results(&response))
    }

    fn delete(&self, remote_key: &str) -> Result<(), BackendError> {
        self.ensure_table()?;

        let sql = format!("DELETE FROM {} WHERE key = ?1", self.config.table_name);
        let body = Self::build_query(&sql, &[&format!("\"{}\"", remote_key)]);

        self.exec_query(&body)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_encode_empty() {
        assert_eq!(base64_encode(b""), "");
    }

    #[test]
    fn test_base64_encode_hello() {
        assert_eq!(base64_encode(b"Hello"), "SGVsbG8=");
    }

    #[test]
    fn test_base64_encode_hello_world() {
        assert_eq!(base64_encode(b"Hello, World!"), "SGVsbG8sIFdvcmxkIQ==");
    }

    #[test]
    fn test_base64_roundtrip() {
        let data = b"RedDB snapshot data with binary \x00\xff\x80 content";
        let encoded = base64_encode(data);
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_base64_roundtrip_all_lengths() {
        for len in 0..=5 {
            let data: Vec<u8> = (0..len).map(|i| (i * 37 + 13) as u8).collect();
            let encoded = base64_encode(&data);
            let decoded = base64_decode(&encoded).unwrap();
            assert_eq!(decoded, data, "roundtrip failed for len={len}");
        }
    }

    #[test]
    fn test_base64_decode_invalid_char() {
        assert!(base64_decode("SGVs!!8=").is_err());
    }

    #[test]
    fn test_d1_config_new() {
        let config = D1Config::new("acc123", "db456", "tok789");
        assert_eq!(config.account_id, "acc123");
        assert_eq!(config.database_id, "db456");
        assert_eq!(config.api_token, "tok789");
        assert_eq!(config.table_name, "reddb_snapshots");
    }

    #[test]
    fn test_d1_config_with_table() {
        let config = D1Config::new("acc", "db", "tok").with_table("my_snapshots");
        assert_eq!(config.table_name, "my_snapshots");
    }

    #[test]
    fn test_query_url() {
        let backend = D1Backend::new(D1Config::new("acc123", "db456", "tok"));
        assert_eq!(
            backend.query_url(),
            "https://api.cloudflare.com/client/v4/accounts/acc123/d1/database/db456/query"
        );
    }

    #[test]
    fn test_build_query_no_params() {
        let q = D1Backend::build_query("SELECT 1", &[]);
        assert_eq!(q, r#"{"sql":"SELECT 1"}"#);
    }

    #[test]
    fn test_build_query_with_params() {
        let q = D1Backend::build_query("SELECT data FROM t WHERE key = ?1", &[r#""mykey""#]);
        assert!(q.contains(r#""sql":"SELECT data FROM t WHERE key = ?1""#));
        assert!(q.contains(r#""params":["mykey"]"#));
    }

    #[test]
    fn test_extract_first_value_with_data() {
        let response = r#"{"result":[{"results":[{"data":"SGVsbG8="}],"success":true,"meta":{}}],"success":true}"#;
        let result = D1Backend::extract_first_value(response, "data").unwrap();
        assert_eq!(result, Some("SGVsbG8=".into()));
    }

    #[test]
    fn test_extract_first_value_empty_results() {
        let response = r#"{"result":[{"results":[],"success":true,"meta":{}}],"success":true}"#;
        let result = D1Backend::extract_first_value(response, "data").unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_has_results_true() {
        let response = r#"{"result":[{"results":[{"1":1}],"success":true}],"success":true}"#;
        assert!(D1Backend::has_results(response));
    }

    #[test]
    fn test_has_results_false() {
        let response = r#"{"result":[{"results":[],"success":true}],"success":true}"#;
        assert!(!D1Backend::has_results(response));
    }

    #[test]
    fn test_backend_name() {
        let backend = D1Backend::new(D1Config::new("a", "d", "t"));
        assert_eq!(backend.name(), "d1");
    }

    #[test]
    fn test_extract_first_value_numeric() {
        let response = r#"{"result":[{"results":[{"size":12345}],"success":true}],"success":true}"#;
        let result = D1Backend::extract_first_value(response, "size").unwrap();
        assert_eq!(result, Some("12345".into()));
    }

    #[test]
    fn test_extract_first_value_null() {
        let response = r#"{"result":[{"results":[{"data":null}],"success":true}],"success":true}"#;
        let result = D1Backend::extract_first_value(response, "data").unwrap();
        assert_eq!(result, None);
    }
}
