//! Turso/libSQL Storage Backend
//!
//! Stores RedDB database snapshots as base64-encoded BLOBs in a Turso/libSQL
//! database via the HTTP Pipeline API (`/v2/pipeline`).
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
//! # Example
//! ```ignore
//! use reddb::storage::backend::turso::{TursoBackend, TursoConfig};
//!
//! let backend = TursoBackend::new(TursoConfig::new(
//!     "https://mydb-myorg.turso.io",
//!     "eyJhbGciOi...",
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

/// Configuration for Turso/libSQL HTTP backend.
#[derive(Debug, Clone)]
pub struct TursoConfig {
    /// Database URL (e.g., `"https://mydb-myorg.turso.io"`).
    pub url: String,
    /// Auth token for the Turso database.
    pub auth_token: String,
    /// Table name for storing database snapshots (default: `"reddb_snapshots"`).
    pub table_name: String,
}

impl TursoConfig {
    /// Create a new Turso config with default table name.
    pub fn new(url: impl Into<String>, auth_token: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            auth_token: auth_token.into(),
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

/// Turso/libSQL storage backend.
///
/// Stores database snapshots as BLOBs in a Turso-hosted SQLite database
/// via the HTTP Pipeline API. Uses `curl(1)` for HTTP transport.
pub struct TursoBackend {
    config: TursoConfig,
}

impl TursoBackend {
    /// Create a new Turso backend with the given configuration.
    pub fn new(config: TursoConfig) -> Self {
        Self { config }
    }

    /// Pipeline API endpoint URL.
    fn pipeline_url(&self) -> String {
        let base = self.config.url.trim_end_matches('/');
        format!("{base}/v2/pipeline")
    }

    /// Execute a pipeline request via curl. Returns the raw JSON response body.
    fn exec_pipeline(&self, json_body: &str) -> Result<String, BackendError> {
        let url = self.pipeline_url();

        let mut cmd = std::process::Command::new("curl");
        cmd.arg("-sf")
            .arg("-X")
            .arg("POST")
            .arg("-H")
            .arg(format!("Authorization: Bearer {}", self.config.auth_token))
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
                    "turso HTTP error (exit {code}): {stderr}"
                )));
            }

            return Err(BackendError::Transport(format!(
                "turso pipeline failed (exit {code}): {stderr}"
            )));
        }

        String::from_utf8(output.stdout)
            .map_err(|e| BackendError::Internal(format!("turso response is not valid UTF-8: {e}")))
    }

    /// Ensure the snapshot table exists.
    fn ensure_table(&self) -> Result<(), BackendError> {
        let sql = format!(
            "CREATE TABLE IF NOT EXISTS {} (key TEXT PRIMARY KEY, data BLOB, size INTEGER, updated_at INTEGER)",
            self.config.table_name
        );
        let body = format!(r#"{{"requests":[{{"type":"execute","stmt":{{"sql":"{sql}"}}}}]}}"#,);
        self.exec_pipeline(&body)?;
        Ok(())
    }

    /// Build a pipeline request body with a parameterised statement.
    fn build_stmt(sql: &str, args: &[&str]) -> String {
        let args_json: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        let args_str = args_json.join(",");

        format!(
            r#"{{"requests":[{{"type":"execute","stmt":{{"sql":"{}","args":[{}]}}}}]}}"#,
            sql, args_str
        )
    }

    /// Extract a string value from the first row, first column of a Turso
    /// pipeline response. Returns `None` if there are no rows.
    ///
    /// The Turso response shape (simplified):
    /// ```json
    /// {
    ///   "results": [{
    ///     "type": "ok",
    ///     "response": {
    ///       "type": "execute",
    ///       "result": {
    ///         "rows": [[ { "type": "blob", "base64": "..." } ]]
    ///       }
    ///     }
    ///   }]
    /// }
    /// ```
    ///
    /// We use a minimal JSON scanner rather than pulling in serde.
    fn extract_first_blob(response: &str) -> Result<Option<String>, BackendError> {
        // Look for "rows":[[...]] -- if empty, return None.
        let rows_start = match response.find("\"rows\"") {
            Some(pos) => pos,
            None => {
                return Err(BackendError::Internal(
                    "turso response missing \"rows\" field".into(),
                ))
            }
        };

        // Find the opening of rows array.
        let after_rows = &response[rows_start..];
        let arr_start = match after_rows.find("[[") {
            Some(pos) => pos,
            None => return Ok(None), // "rows": [] -- empty result set
        };

        // Check for empty rows: `"rows":[]`
        if let Some(bracket_pos) = after_rows.find('[') {
            let after_bracket = after_rows[bracket_pos + 1..].trim_start();
            if after_bracket.starts_with(']') {
                return Ok(None);
            }
        }

        // Find "base64" value in the first row element.
        let row_data = &after_rows[arr_start..];
        if let Some(b64_key) = row_data.find("\"base64\"") {
            let after_key = &row_data[b64_key + 8..]; // skip `"base64"`
                                                      // Find the opening quote of the value.
            if let Some(quote_start) = after_key.find('"') {
                let value_start = quote_start + 1;
                let rest = &after_key[value_start..];
                if let Some(quote_end) = rest.find('"') {
                    return Ok(Some(rest[..quote_end].to_string()));
                }
            }
        }

        // Might be a text value instead of blob.
        if let Some(val_key) = row_data.find("\"value\"") {
            let after_key = &row_data[val_key + 7..]; // skip `"value"`
            if let Some(quote_start) = after_key.find('"') {
                let value_start = quote_start + 1;
                let rest = &after_key[value_start..];
                if let Some(quote_end) = rest.find('"') {
                    return Ok(Some(rest[..quote_end].to_string()));
                }
            }
        }

        Ok(None)
    }

    /// Extract an integer value from the first row, first column.
    /// Returns `None` if there are no rows.
    fn extract_first_integer(response: &str) -> Result<Option<i64>, BackendError> {
        match Self::extract_first_blob(response)? {
            Some(val) => val
                .parse::<i64>()
                .map(Some)
                .map_err(|e| BackendError::Internal(format!("turso integer parse error: {e}"))),
            None => Ok(None),
        }
    }

    fn extract_text_values(response: &str) -> Vec<String> {
        let mut values = Vec::new();
        let mut rest = response;
        while let Some(value_key) = rest.find("\"value\"") {
            let after_key = &rest[value_key + 7..];
            let Some(quote_start) = after_key.find('"') else {
                break;
            };
            let value_rest = &after_key[quote_start + 1..];
            let Some(quote_end) = value_rest.find('"') else {
                break;
            };
            values.push(value_rest[..quote_end].to_string());
            rest = &value_rest[quote_end + 1..];
        }
        values
    }
}

// ---------------------------------------------------------------------------
// RemoteBackend implementation
// ---------------------------------------------------------------------------

impl RemoteBackend for TursoBackend {
    fn name(&self) -> &str {
        "turso"
    }

    fn download(&self, remote_key: &str, local_path: &Path) -> Result<bool, BackendError> {
        self.ensure_table()?;

        let sql = format!("SELECT data FROM {} WHERE key = ?", self.config.table_name);
        let body = Self::build_stmt(
            &sql,
            &[&format!(r#"{{"type":"text","value":"{}"}}"#, remote_key)],
        );

        let response = self.exec_pipeline(&body)?;

        match Self::extract_first_blob(&response)? {
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

        let sql = format!(
            "INSERT OR REPLACE INTO {} (key, data, size, updated_at) VALUES (?, ?, ?, ?)",
            self.config.table_name
        );
        let body = Self::build_stmt(
            &sql,
            &[
                &format!(r#"{{"type":"text","value":"{}"}}"#, remote_key),
                &format!(r#"{{"type":"blob","base64":"{}"}}"#, b64_data),
                &format!(r#"{{"type":"integer","value":"{}"}}"#, size),
                &format!(r#"{{"type":"integer","value":"{}"}}"#, now),
            ],
        );

        self.exec_pipeline(&body)?;
        Ok(())
    }

    fn exists(&self, remote_key: &str) -> Result<bool, BackendError> {
        self.ensure_table()?;

        let sql = format!("SELECT 1 FROM {} WHERE key = ?", self.config.table_name);
        let body = Self::build_stmt(
            &sql,
            &[&format!(r#"{{"type":"text","value":"{}"}}"#, remote_key)],
        );

        let response = self.exec_pipeline(&body)?;

        match Self::extract_first_integer(&response)? {
            Some(_) => Ok(true),
            None => Ok(false),
        }
    }

    fn delete(&self, remote_key: &str) -> Result<(), BackendError> {
        self.ensure_table()?;

        let sql = format!("DELETE FROM {} WHERE key = ?", self.config.table_name);
        let body = Self::build_stmt(
            &sql,
            &[&format!(r#"{{"type":"text","value":"{}"}}"#, remote_key)],
        );

        self.exec_pipeline(&body)?;
        Ok(())
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError> {
        self.ensure_table()?;

        let sql = format!(
            "SELECT key FROM {} WHERE key LIKE ? ORDER BY key",
            self.config.table_name
        );
        let body = Self::build_stmt(
            &sql,
            &[&format!(r#"{{"type":"text","value":"{}%"}}"#, prefix)],
        );

        let response = self.exec_pipeline(&body)?;
        Ok(Self::extract_text_values(&response))
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
        // Test padding for lengths 0..=5
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
    fn test_turso_config_new() {
        let config = TursoConfig::new("https://mydb.turso.io", "tok123");
        assert_eq!(config.url, "https://mydb.turso.io");
        assert_eq!(config.auth_token, "tok123");
        assert_eq!(config.table_name, "reddb_snapshots");
    }

    #[test]
    fn test_turso_config_with_table() {
        let config =
            TursoConfig::new("https://mydb.turso.io", "tok123").with_table("custom_snapshots");
        assert_eq!(config.table_name, "custom_snapshots");
    }

    #[test]
    fn test_pipeline_url() {
        let backend = TursoBackend::new(TursoConfig::new("https://mydb-myorg.turso.io", "tok"));
        assert_eq!(
            backend.pipeline_url(),
            "https://mydb-myorg.turso.io/v2/pipeline"
        );
    }

    #[test]
    fn test_pipeline_url_trailing_slash() {
        let backend = TursoBackend::new(TursoConfig::new("https://mydb-myorg.turso.io/", "tok"));
        assert_eq!(
            backend.pipeline_url(),
            "https://mydb-myorg.turso.io/v2/pipeline"
        );
    }

    #[test]
    fn test_extract_first_blob_with_data() {
        let response = r#"{"results":[{"type":"ok","response":{"type":"execute","result":{"cols":[{"name":"data"}],"rows":[[{"type":"blob","base64":"SGVsbG8="}]],"affected_row_count":0}}}]}"#;
        let result = TursoBackend::extract_first_blob(response).unwrap();
        assert_eq!(result, Some("SGVsbG8=".into()));
    }

    #[test]
    fn test_extract_first_blob_empty_rows() {
        let response = r#"{"results":[{"type":"ok","response":{"type":"execute","result":{"cols":[{"name":"data"}],"rows":[],"affected_row_count":0}}}]}"#;
        let result = TursoBackend::extract_first_blob(response).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_first_integer() {
        let response = r#"{"results":[{"type":"ok","response":{"type":"execute","result":{"cols":[{"name":"1"}],"rows":[[{"type":"integer","value":"1"}]],"affected_row_count":0}}}]}"#;
        let result = TursoBackend::extract_first_integer(response).unwrap();
        assert_eq!(result, Some(1));
    }

    #[test]
    fn test_extract_first_integer_empty() {
        let response = r#"{"results":[{"type":"ok","response":{"type":"execute","result":{"cols":[{"name":"1"}],"rows":[],"affected_row_count":0}}}]}"#;
        let result = TursoBackend::extract_first_integer(response).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_build_stmt_no_args() {
        let stmt = TursoBackend::build_stmt("SELECT 1", &[]);
        assert!(stmt.contains("\"sql\":\"SELECT 1\""));
        assert!(stmt.contains("\"args\":[]"));
    }

    #[test]
    fn test_build_stmt_with_args() {
        let stmt = TursoBackend::build_stmt(
            "SELECT data FROM t WHERE key = ?",
            &[r#"{"type":"text","value":"mykey"}"#],
        );
        assert!(stmt.contains("\"sql\":\"SELECT data FROM t WHERE key = ?\""));
        assert!(stmt.contains(r#"{"type":"text","value":"mykey"}"#));
    }

    #[test]
    fn test_backend_name() {
        let backend = TursoBackend::new(TursoConfig::new("https://x.turso.io", "t"));
        assert_eq!(backend.name(), "turso");
    }
}
