//! Client-certificate authentication (Phase 3.4 PG parity).
//!
//! Validates mTLS client certificates against a trust store and extracts
//! the user identity + role from the cert's subject / extensions. Used
//! by TLS-terminated listeners (wire, gRPC, PG wire) to authenticate
//! callers without sending passwords.
//!
//! # Identity mapping
//!
//! Two mapping modes, configured per deployment:
//!
//! * **`CommonName`** — take the subject CN ("CN=alice") as the RedDB
//!   username. Matches PG's `cert` auth default. Simplest option but
//!   conflates identity with naming.
//! * **`SanRfc822Name`** — take the first rfc822Name (email) SAN entry
//!   as the username. Works well with corporate PKIs that encode email
//!   in the cert subject alternative name.
//!
//! Additional extension-based mapping (custom OIDs for role tags) lives
//! behind `CertAuthConfig::role_oid` — when set, the validator extracts
//! the role string from that OID; otherwise the role defaults to
//! `CertAuthConfig::default_role`.
//!
//! # Trust store
//!
//! For Phase 3.4 the trust store is a file path holding one or more
//! PEM-encoded CA certificates. Any leaf cert signed by any of those
//! CAs validates. Chain verification delegates to the underlying TLS
//! stack (`rustls`) — we only consume the already-validated
//! certificate at the handler layer.

use std::path::PathBuf;

use super::{Role, User};

/// Per-deployment cert-auth policy. Enabled on a per-listener basis
/// (the TLS listeners inject this into their accept loop).
#[derive(Debug, Clone)]
pub struct CertAuthConfig {
    /// Whether cert auth is active for this listener. When false the
    /// validator is skipped entirely.
    pub enabled: bool,
    /// Path to a PEM file containing trusted CA certificates. Client
    /// certs must chain to one of these.
    pub trust_bundle: PathBuf,
    /// Identity extraction mode.
    pub identity_mode: CertIdentityMode,
    /// Optional X.509 extension OID (dotted notation) that carries the
    /// role string. When unset, `default_role` is used.
    pub role_oid: Option<String>,
    /// Role assigned when the cert does not carry an explicit role.
    pub default_role: Role,
    /// When `true`, a cert whose CN / email matches an existing RedDB
    /// user maps to that user (and inherits the user's stored role).
    /// When `false`, the cert-derived role is always authoritative.
    pub map_to_existing_users: bool,
}

impl Default for CertAuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            trust_bundle: PathBuf::from("./certs/client-ca.pem"),
            identity_mode: CertIdentityMode::CommonName,
            role_oid: None,
            default_role: Role::Read,
            map_to_existing_users: true,
        }
    }
}

/// How to derive the RedDB username from a client certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertIdentityMode {
    /// Subject CN field ("CN=alice").
    CommonName,
    /// First rfc822Name (email) Subject Alternative Name.
    SanRfc822Name,
}

/// Parsed identity extracted from a validated client certificate.
///
/// The auth store consumes this to either look up a matching persisted
/// user or treat it as an ephemeral identity (no entry in the user
/// table, role is cert-derived).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertIdentity {
    pub username: String,
    pub role: Role,
    /// Subject DN — preserved for audit logging even when we don't map
    /// it back to a persisted user.
    pub subject_dn: String,
    /// Certificate serial number in uppercase hex — audit identifier.
    pub serial_hex: String,
    /// Unix-seconds expiry of the certificate. Auth middleware rejects
    /// requests once this passes even if the cert is still in cache.
    pub not_after_unix_secs: i64,
}

/// Errors raised while validating a client certificate.
#[derive(Debug, Clone)]
pub enum CertAuthError {
    /// TLS layer validated the chain but the cert does not carry the
    /// identity field required by `identity_mode`.
    MissingIdentity(CertIdentityMode),
    /// `role_oid` was configured but the cert does not carry that
    /// extension.
    MissingRoleExtension(String),
    /// `map_to_existing_users` is on but no stored user matches.
    UnknownUser(String),
    /// Cert expired (wall-clock beyond `not_after`).
    Expired { not_after_unix_secs: i64 },
    /// Trust-bundle configuration failure (file missing / malformed).
    TrustBundle(String),
    /// Arbitrary parse failure in the cert surface bytes.
    Parse(String),
}

impl std::fmt::Display for CertAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CertAuthError::MissingIdentity(mode) => {
                write!(f, "client cert missing {:?} identity field", mode)
            }
            CertAuthError::MissingRoleExtension(oid) => {
                write!(f, "client cert missing role extension {oid}")
            }
            CertAuthError::UnknownUser(u) => write!(f, "cert user '{u}' not in auth store"),
            CertAuthError::Expired {
                not_after_unix_secs,
            } => write!(f, "client cert expired at unix {not_after_unix_secs}"),
            CertAuthError::TrustBundle(m) => write!(f, "trust bundle error: {m}"),
            CertAuthError::Parse(m) => write!(f, "cert parse error: {m}"),
        }
    }
}

impl std::error::Error for CertAuthError {}

/// Subset of the cert surface the validator consumes. TLS listeners
/// construct this from their `rustls::Certificate` payload via an
/// ASN.1 parser (`x509-parser` or similar); we model the fields we
/// actually look at so tests don't need a real PEM.
#[derive(Debug, Clone)]
pub struct ParsedClientCert {
    pub subject_dn: String,
    pub common_name: Option<String>,
    pub san_rfc822: Vec<String>,
    pub serial_hex: String,
    pub not_after_unix_secs: i64,
    /// Map of X.509 extension OID → raw bytes. Populated for any
    /// extension the parser saw; the validator only looks at
    /// `role_oid` when configured.
    pub extensions: std::collections::HashMap<String, Vec<u8>>,
}

/// Stateless validator. Holds the config + lookup closure; TLS
/// listeners wrap it in an Arc and call `validate` on every accepted
/// connection.
pub struct CertAuthenticator {
    config: CertAuthConfig,
}

impl CertAuthenticator {
    pub fn new(config: CertAuthConfig) -> Self {
        Self { config }
    }

    /// Validate a parsed client cert and extract the RedDB identity.
    ///
    /// `lookup_user` is invoked when `map_to_existing_users=true` so the
    /// caller can consult the auth store (any closure returning
    /// `Option<User>` works — tests inject a fake).
    pub fn validate<F>(
        &self,
        cert: &ParsedClientCert,
        now_unix_secs: i64,
        lookup_user: F,
    ) -> Result<CertIdentity, CertAuthError>
    where
        F: Fn(&str) -> Option<User>,
    {
        if !self.config.enabled {
            return Err(CertAuthError::Parse(
                "cert auth disabled on this listener".into(),
            ));
        }

        if cert.not_after_unix_secs < now_unix_secs {
            return Err(CertAuthError::Expired {
                not_after_unix_secs: cert.not_after_unix_secs,
            });
        }

        let username = match self.config.identity_mode {
            CertIdentityMode::CommonName => cert
                .common_name
                .clone()
                .ok_or(CertAuthError::MissingIdentity(CertIdentityMode::CommonName))?,
            CertIdentityMode::SanRfc822Name => {
                cert.san_rfc822
                    .first()
                    .cloned()
                    .ok_or(CertAuthError::MissingIdentity(
                        CertIdentityMode::SanRfc822Name,
                    ))?
            }
        };

        // Prefer persisted user role; fall back to cert-derived role.
        let role = if self.config.map_to_existing_users {
            match lookup_user(&username) {
                Some(user) => user.role,
                None => self.derive_role_from_cert(cert)?,
            }
        } else {
            self.derive_role_from_cert(cert)?
        };

        Ok(CertIdentity {
            username,
            role,
            subject_dn: cert.subject_dn.clone(),
            serial_hex: cert.serial_hex.clone(),
            not_after_unix_secs: cert.not_after_unix_secs,
        })
    }

    fn derive_role_from_cert(&self, cert: &ParsedClientCert) -> Result<Role, CertAuthError> {
        let Some(oid) = &self.config.role_oid else {
            return Ok(self.config.default_role);
        };
        let bytes = cert
            .extensions
            .get(oid)
            .ok_or_else(|| CertAuthError::MissingRoleExtension(oid.clone()))?;
        // Extension payload is expected to be a DER-encoded UTF-8 string.
        // For Phase 3.4 we accept raw bytes interpreted as UTF-8 to keep
        // the validator independent of an ASN.1 dependency.
        let name = std::str::from_utf8(bytes)
            .map_err(|e| CertAuthError::Parse(format!("role extension not valid UTF-8: {e}")))?;
        Role::from_str(name.trim())
            .ok_or_else(|| CertAuthError::Parse(format!("unknown role '{name}'")))
    }

    pub fn config(&self) -> &CertAuthConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn base_cert() -> ParsedClientCert {
        ParsedClientCert {
            subject_dn: "CN=alice,O=reddb,C=BR".to_string(),
            common_name: Some("alice".to_string()),
            san_rfc822: vec!["alice@example.com".to_string()],
            serial_hex: "ABCDEF".to_string(),
            not_after_unix_secs: 2_000_000_000,
            extensions: HashMap::new(),
        }
    }

    fn cfg(mode: CertIdentityMode) -> CertAuthConfig {
        CertAuthConfig {
            enabled: true,
            identity_mode: mode,
            ..CertAuthConfig::default()
        }
    }

    #[test]
    fn common_name_maps_to_username() {
        let auth = CertAuthenticator::new(cfg(CertIdentityMode::CommonName));
        let id = auth
            .validate(&base_cert(), 1_000_000_000, |_| None)
            .unwrap();
        assert_eq!(id.username, "alice");
        assert_eq!(id.role, Role::Read);
    }

    #[test]
    fn san_rfc822_maps_to_email() {
        let auth = CertAuthenticator::new(cfg(CertIdentityMode::SanRfc822Name));
        let id = auth
            .validate(&base_cert(), 1_000_000_000, |_| None)
            .unwrap();
        assert_eq!(id.username, "alice@example.com");
    }

    #[test]
    fn missing_cn_field_rejected() {
        let mut cert = base_cert();
        cert.common_name = None;
        let auth = CertAuthenticator::new(cfg(CertIdentityMode::CommonName));
        let err = auth.validate(&cert, 1_000_000_000, |_| None).unwrap_err();
        assert!(matches!(err, CertAuthError::MissingIdentity(_)));
    }

    #[test]
    fn expired_cert_rejected() {
        let mut cert = base_cert();
        cert.not_after_unix_secs = 500;
        let auth = CertAuthenticator::new(cfg(CertIdentityMode::CommonName));
        let err = auth.validate(&cert, 1_000, |_| None).unwrap_err();
        assert!(matches!(err, CertAuthError::Expired { .. }));
    }

    #[test]
    fn role_extension_overrides_default_role() {
        let mut cert = base_cert();
        cert.extensions
            .insert("1.3.6.1.4.1.99999.1".to_string(), b"admin".to_vec());
        let mut config = cfg(CertIdentityMode::CommonName);
        config.role_oid = Some("1.3.6.1.4.1.99999.1".to_string());
        config.map_to_existing_users = false;
        let auth = CertAuthenticator::new(config);
        let id = auth.validate(&cert, 1_000_000_000, |_| None).unwrap();
        assert_eq!(id.role, Role::Admin);
    }

    #[test]
    fn missing_role_extension_errors_when_configured() {
        let mut config = cfg(CertIdentityMode::CommonName);
        config.role_oid = Some("1.2.3".to_string());
        config.map_to_existing_users = false;
        let auth = CertAuthenticator::new(config);
        let err = auth
            .validate(&base_cert(), 1_000_000_000, |_| None)
            .unwrap_err();
        assert!(matches!(err, CertAuthError::MissingRoleExtension(_)));
    }
}
