//! Auth middleware helpers.
//!
//! Provides the [`AuthResult`] type and [`check_permission`] function used by
//! the gRPC and HTTP layers to decide whether an incoming request is allowed.

use super::{CertIdentity, OAuthIdentity, Role};

// ---------------------------------------------------------------------------
// AuthResult
// ---------------------------------------------------------------------------

/// How the caller's identity was established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthSource {
    /// Classic password / API-key / session cookie path.
    Password,
    /// mTLS client certificate (Phase 3.4 PG parity).
    ClientCert,
    /// OAuth/OIDC Bearer token (Phase 3.4 PG parity).
    Oauth,
}

/// Outcome of auth validation for an incoming request.
#[derive(Debug, Clone)]
pub enum AuthResult {
    /// Fully authenticated with RBAC.
    Authenticated {
        username: String,
        role: Role,
        /// Which auth path produced this identity. Defaults to
        /// `Password` for callers that haven't been updated to set it
        /// explicitly, keeping backwards compatibility.
        source: AuthSource,
    },
    /// No credentials provided.
    Anonymous,
    /// Credentials were provided but rejected.
    Denied(String),
}

impl AuthResult {
    /// Back-compat constructor for password auth — callers that predate
    /// the `AuthSource` field can keep passing `(user, role)`.
    pub fn password(username: impl Into<String>, role: Role) -> Self {
        Self::Authenticated {
            username: username.into(),
            role,
            source: AuthSource::Password,
        }
    }

    /// Build an `AuthResult` from a validated client certificate.
    pub fn from_cert(id: CertIdentity) -> Self {
        Self::Authenticated {
            username: id.username,
            role: id.role,
            source: AuthSource::ClientCert,
        }
    }

    /// Build an `AuthResult` from a validated OAuth/OIDC token.
    pub fn from_oauth(id: OAuthIdentity) -> Self {
        Self::Authenticated {
            username: id.username,
            role: id.role,
            source: AuthSource::Oauth,
        }
    }

    /// Short description suitable for logging.
    pub fn summary(&self) -> String {
        match self {
            Self::Authenticated {
                username,
                role,
                source,
            } => {
                let src = match source {
                    AuthSource::Password => "pwd",
                    AuthSource::ClientCert => "cert",
                    AuthSource::Oauth => "oauth",
                };
                format!("user={username} role={role} via={src}")
            }
            Self::Anonymous => "anonymous".to_string(),
            Self::Denied(reason) => format!("denied: {reason}"),
        }
    }

    /// Whether this result represents a successfully identified caller.
    pub fn is_authenticated(&self) -> bool {
        matches!(self, Self::Authenticated { .. })
    }
}

// ---------------------------------------------------------------------------
// Permission check
// ---------------------------------------------------------------------------

/// Check whether the given [`AuthResult`] has sufficient privileges.
///
/// * `requires_write` -- the operation mutates data.
/// * `requires_admin` -- the operation requires admin privileges (user
///   management, index ops, etc.).
pub fn check_permission(
    auth: &AuthResult,
    requires_write: bool,
    requires_admin: bool,
) -> Result<(), String> {
    match auth {
        AuthResult::Authenticated { role, .. } => {
            if requires_admin && !role.can_admin() {
                return Err("admin role required".into());
            }
            if requires_write && !role.can_write() {
                return Err("write permission required".into());
            }
            Ok(())
        }
        AuthResult::Anonymous => {
            if requires_admin {
                return Err("admin authentication required".into());
            }
            // When auth is disabled, anonymous can read AND write
            Ok(())
        }
        AuthResult::Denied(reason) => Err(reason.clone()),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_admin_can_do_everything() {
        let auth = AuthResult::Authenticated {
            username: "root".into(),
            role: Role::Admin,
            source: AuthSource::Password,
        };
        assert!(check_permission(&auth, false, false).is_ok());
        assert!(check_permission(&auth, true, false).is_ok());
        assert!(check_permission(&auth, false, true).is_ok());
        assert!(check_permission(&auth, true, true).is_ok());
    }

    #[test]
    fn test_write_role_cannot_admin() {
        let auth = AuthResult::Authenticated {
            username: "writer".into(),
            role: Role::Write,
            source: AuthSource::Password,
        };
        assert!(check_permission(&auth, false, false).is_ok());
        assert!(check_permission(&auth, true, false).is_ok());
        assert!(check_permission(&auth, false, true).is_err());
    }

    #[test]
    fn test_read_role_cannot_write() {
        let auth = AuthResult::Authenticated {
            username: "reader".into(),
            role: Role::Read,
            source: AuthSource::Password,
        };
        assert!(check_permission(&auth, false, false).is_ok());
        assert!(check_permission(&auth, true, false).is_err());
        assert!(check_permission(&auth, false, true).is_err());
    }

    #[test]
    fn test_anonymous_access() {
        let auth = AuthResult::Anonymous;
        // When auth disabled: anonymous can read and write, but not admin
        assert!(check_permission(&auth, false, false).is_ok());
        assert!(check_permission(&auth, true, false).is_ok());
        assert!(check_permission(&auth, false, true).is_err());
    }

    #[test]
    fn test_denied_always_fails() {
        let auth = AuthResult::Denied("bad token".into());
        assert!(check_permission(&auth, false, false).is_err());
        assert!(check_permission(&auth, true, true).is_err());
    }

    #[test]
    fn test_auth_result_summary() {
        let auth = AuthResult::Authenticated {
            username: "alice".into(),
            role: Role::Admin,
            source: AuthSource::Password,
        };
        assert!(auth.summary().contains("alice"));
        assert!(auth.is_authenticated());

        let anon = AuthResult::Anonymous;
        assert_eq!(anon.summary(), "anonymous");
        assert!(!anon.is_authenticated());
    }
}
