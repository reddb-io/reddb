//! Authentication & Authorization
//!
//! Provides user management, RBAC, and token-based auth for RedDB.
//!
//! # Roles
//! - `admin`: Full access (user management, index ops, read, write)
//! - `write`: Read + write data
//! - `read`: Read-only access
//!
//! # Auth Methods
//! - User/Password login -> session token
//! - API key -> direct auth with assigned role

pub mod middleware;
pub mod store;
pub mod vault;

use std::fmt;

// ---------------------------------------------------------------------------
// Role
// ---------------------------------------------------------------------------

/// Access role within the RedDB authorization model.
///
/// Roles form an ordered hierarchy: `Read < Write < Admin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    Read,
    Write,
    Admin,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Admin => "admin",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "read" => Some(Self::Read),
            "write" => Some(Self::Write),
            "admin" => Some(Self::Admin),
            _ => None,
        }
    }

    pub fn can_read(&self) -> bool {
        true
    }

    pub fn can_write(&self) -> bool {
        matches!(self, Self::Write | Self::Admin)
    }

    pub fn can_admin(&self) -> bool {
        matches!(self, Self::Admin)
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// User
// ---------------------------------------------------------------------------

/// A registered user in the RedDB auth system.
#[derive(Debug, Clone)]
pub struct User {
    pub username: String,
    pub password_hash: String,
    pub role: Role,
    pub api_keys: Vec<ApiKey>,
    pub created_at: u128,
    pub updated_at: u128,
    pub enabled: bool,
}

// ---------------------------------------------------------------------------
// ApiKey
// ---------------------------------------------------------------------------

/// A persistent API key bound to a user.
#[derive(Debug, Clone)]
pub struct ApiKey {
    /// Token value: `"rk_<hex32>"`
    pub key: String,
    /// Human-readable label.
    pub name: String,
    /// Role granted by this key (cannot exceed user's role).
    pub role: Role,
    pub created_at: u128,
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// An ephemeral session created by login.
#[derive(Debug, Clone)]
pub struct Session {
    /// Token value: `"rs_<hex32>"`
    pub token: String,
    pub username: String,
    pub role: Role,
    pub created_at: u128,
    /// Absolute expiry (ms since epoch).
    pub expires_at: u128,
}

// ---------------------------------------------------------------------------
// AuthConfig
// ---------------------------------------------------------------------------

/// Configuration knobs for the auth subsystem.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// Master switch -- when `false` auth is completely bypassed.
    pub enabled: bool,
    /// Session time-to-live in seconds (default 3600 = 1 h).
    pub session_ttl_secs: u64,
    /// When `true`, unauthenticated requests are rejected even for reads.
    pub require_auth: bool,
    /// When `true`, storage files are encrypted when auth is active.
    pub auto_encrypt_storage: bool,
    /// When `true`, auth state (users, api keys, bootstrap flag) is persisted
    /// to reserved vault pages inside the main `.rdb` database file using
    /// AES-256-GCM encryption.  The encryption key is read from
    /// `REDDB_VAULT_KEY` env var or a passphrase.
    pub vault_enabled: bool,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            session_ttl_secs: 3600,
            require_auth: false,
            auto_encrypt_storage: false,
            vault_enabled: false,
        }
    }
}

// ---------------------------------------------------------------------------
// AuthError
// ---------------------------------------------------------------------------

/// Errors produced by auth operations.
#[derive(Debug, Clone)]
pub enum AuthError {
    UserExists(String),
    UserNotFound(String),
    InvalidCredentials,
    KeyNotFound(String),
    RoleExceeded { requested: Role, ceiling: Role },
    Disabled,
    Forbidden(String),
    Internal(String),
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UserExists(u) => write!(f, "user already exists: {u}"),
            Self::UserNotFound(u) => write!(f, "user not found: {u}"),
            Self::InvalidCredentials => write!(f, "invalid credentials"),
            Self::KeyNotFound(k) => write!(f, "api key not found: {k}"),
            Self::RoleExceeded { requested, ceiling } => {
                write!(
                    f,
                    "requested role '{requested}' exceeds ceiling '{ceiling}'"
                )
            }
            Self::Disabled => write!(f, "authentication is disabled"),
            Self::Forbidden(msg) => write!(f, "forbidden: {msg}"),
            Self::Internal(msg) => write!(f, "internal auth error: {msg}"),
        }
    }
}

impl std::error::Error for AuthError {}

// ---------------------------------------------------------------------------
// Helpers -- timestamp
// ---------------------------------------------------------------------------

/// Current time in milliseconds since the UNIX epoch.
pub(crate) fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_role_ordering() {
        assert!(Role::Read < Role::Write);
        assert!(Role::Write < Role::Admin);
    }

    #[test]
    fn test_role_roundtrip() {
        for role in [Role::Read, Role::Write, Role::Admin] {
            assert_eq!(Role::from_str(role.as_str()), Some(role));
        }
        assert_eq!(Role::from_str("unknown"), None);
    }

    #[test]
    fn test_role_permissions() {
        assert!(Role::Read.can_read());
        assert!(!Role::Read.can_write());
        assert!(!Role::Read.can_admin());

        assert!(Role::Write.can_read());
        assert!(Role::Write.can_write());
        assert!(!Role::Write.can_admin());

        assert!(Role::Admin.can_read());
        assert!(Role::Admin.can_write());
        assert!(Role::Admin.can_admin());
    }

    #[test]
    fn test_auth_config_default() {
        let cfg = AuthConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.session_ttl_secs, 3600);
        assert!(!cfg.require_auth);
        assert!(!cfg.auto_encrypt_storage);
    }

    #[test]
    fn test_auth_error_display() {
        let err = AuthError::UserExists("alice".into());
        assert!(err.to_string().contains("alice"));

        let err = AuthError::InvalidCredentials;
        assert!(err.to_string().contains("invalid"));
    }
}
