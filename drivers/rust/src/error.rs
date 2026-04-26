//! Error type for `reddb-client`.
//!
//! Mirrors the JSON-RPC error codes used by `red rpc --stdio` so that
//! client error mapping is consistent across all language drivers.
//! See `PLAN_DRIVERS.md` § "Spec do protocolo stdio".

use std::fmt;

/// Result alias for the entire crate.
pub type Result<T> = std::result::Result<T, ClientError>;

/// Stable, machine-readable error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// Caller passed an unsupported `connect()` URI scheme.
    UnsupportedScheme,
    /// URL parser rejected the connection string.
    InvalidUri,
    /// File backend reported an I/O failure.
    IoError,
    /// Engine query parser rejected the SQL.
    QueryError,
    /// Feature requested is gated behind a Cargo feature that wasn't enabled.
    FeatureDisabled,
    /// The user called a method on a closed connection.
    ClientClosed,
    /// Catch-all for unexpected engine failures.
    Internal,
    /// TCP / TLS / DNS layer reported a failure (RedWire client).
    Network,
    /// Wire-level decode failure or unexpected message kind.
    Protocol,
    /// Server refused the credentials the client supplied.
    AuthRefused,
    /// Engine returned an error string in response to a Query frame.
    Engine,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::UnsupportedScheme => "UNSUPPORTED_SCHEME",
            ErrorCode::InvalidUri => "INVALID_URI",
            ErrorCode::IoError => "IO_ERROR",
            ErrorCode::QueryError => "QUERY_ERROR",
            ErrorCode::FeatureDisabled => "FEATURE_DISABLED",
            ErrorCode::ClientClosed => "CLIENT_CLOSED",
            ErrorCode::Internal => "INTERNAL_ERROR",
            ErrorCode::Network => "NETWORK_ERROR",
            ErrorCode::Protocol => "PROTOCOL_ERROR",
            ErrorCode::AuthRefused => "AUTH_REFUSED",
            ErrorCode::Engine => "ENGINE_ERROR",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned by every fallible call in this crate.
#[derive(Debug, Clone)]
pub struct ClientError {
    pub code: ErrorCode,
    pub message: String,
}

impl ClientError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn unsupported_scheme(scheme: impl AsRef<str>) -> Self {
        Self::new(
            ErrorCode::UnsupportedScheme,
            format!(
                "unsupported URI scheme: '{}'. Expected 'file://', 'memory://' or 'grpc://'.",
                scheme.as_ref()
            ),
        )
    }

    pub fn feature_disabled(feature: &str) -> Self {
        Self::new(
            ErrorCode::FeatureDisabled,
            format!(
                "the '{feature}' feature is not enabled. Enable it in Cargo.toml: \
                 reddb-client = {{ version = \"…\", features = [\"{feature}\"] }}"
            ),
        )
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for ClientError {}
