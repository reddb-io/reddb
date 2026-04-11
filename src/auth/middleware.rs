//! Auth middleware helpers.
//!
//! Provides the [`AuthResult`] type and [`check_permission`] function used by
//! the gRPC and HTTP layers to decide whether an incoming request is allowed.

use super::Role;

// ---------------------------------------------------------------------------
// AuthResult
// ---------------------------------------------------------------------------

/// Outcome of auth validation for an incoming request.
#[derive(Debug, Clone)]
pub enum AuthResult {
    /// Fully authenticated with RBAC.
    Authenticated { username: String, role: Role },
    /// No credentials provided.
    Anonymous,
    /// Credentials were provided but rejected.
    Denied(String),
}

impl AuthResult {
    /// Short description suitable for logging.
    pub fn summary(&self) -> String {
        match self {
            Self::Authenticated { username, role } => {
                format!("user={username} role={role}")
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
        };
        assert!(check_permission(&auth, false, false).is_ok());
        assert!(check_permission(&auth, true, false).is_err());
        assert!(check_permission(&auth, false, true).is_err());
    }

    #[test]
    fn test_anonymous_read_only() {
        let auth = AuthResult::Anonymous;
        assert!(check_permission(&auth, false, false).is_ok());
        assert!(check_permission(&auth, true, false).is_err());
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
        };
        assert!(auth.summary().contains("alice"));
        assert!(auth.is_authenticated());

        let anon = AuthResult::Anonymous;
        assert_eq!(anon.summary(), "anonymous");
        assert!(!anon.is_authenticated());
    }
}
