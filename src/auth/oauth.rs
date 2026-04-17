//! OAuth / OIDC token validation (Phase 3.4 PG parity).
//!
//! Accepts `Authorization: Bearer <jwt>` headers, validates the JWT
//! against a trusted issuer's JWKS, and maps the verified claims onto
//! a RedDB identity. Lives alongside password auth — either is
//! sufficient on its own, and the server config picks which one (or
//! both) a listener accepts.
//!
//! # Supported flows
//!
//! * **OIDC Authorization Code → access token** — the identity provider
//!   issues a JWT signed with RS256 / ES256. RedDB validates:
//!   - issuer (`iss`) matches config
//!   - audience (`aud`) contains config-audience
//!   - expiry (`exp`) hasn't passed
//!   - not-before (`nbf`) is in the past
//!   - signature verifies against a JWK from the configured JWKS
//! * **Client credentials** — same JWT shape, `sub` = client_id.
//!
//! The JWKS fetch + caching lives on `OAuthValidator`. Phase 3.4 uses
//! an in-memory TTL cache keyed on `kid`; a background refresh loop
//! is a 3.4.2 follow-up.
//!
//! # Identity mapping
//!
//! Two modes:
//!
//! * **`SubClaim`** — the JWT `sub` is the RedDB username. Fastest;
//!   good when the identity provider subject matches our user store.
//! * **`ClaimField(name)`** — read any string claim (e.g. `preferred_username`,
//!   `email`) as the RedDB username. Covers the case where the issuer
//!   carries both a stable `sub` and a human-friendly handle.
//!
//! Role mapping works the same way: either consult the RedDB user
//! store with the extracted username (`map_to_existing_users=true`),
//! or read a claim (`role_claim`) whose value matches `Role::from_str`.

use std::collections::HashMap;

use super::{Role, User};

/// Configuration for OAuth/OIDC auth. Multiple issuers can be
/// registered in parallel — the validator tries each until one's
/// signature verification succeeds.
#[derive(Debug, Clone)]
pub struct OAuthConfig {
    /// Master switch. When false the validator is bypassed.
    pub enabled: bool,
    /// Expected `iss` claim. Tokens with a different issuer are
    /// rejected even when the signature validates against a shared key.
    pub issuer: String,
    /// Required `aud` claim. The JWT's audience (string or array) must
    /// contain this value.
    pub audience: String,
    /// JWKS endpoint URL (e.g. `https://id.example.com/.well-known/jwks.json`).
    /// Fetched lazily on first token + periodically by the refresh task.
    pub jwks_url: String,
    /// How to turn JWT claims into a RedDB username.
    pub identity_mode: OAuthIdentityMode,
    /// Optional claim whose string value maps to `Role::from_str`.
    pub role_claim: Option<String>,
    pub default_role: Role,
    pub map_to_existing_users: bool,
    /// Accept `Bearer`-prefixed Authorization headers. Always true in
    /// Phase 3.4; kept as a knob so custom auth schemes can bolt on
    /// without duplicating the validator.
    pub accept_bearer: bool,
}

impl Default for OAuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            issuer: String::new(),
            audience: String::new(),
            jwks_url: String::new(),
            identity_mode: OAuthIdentityMode::SubClaim,
            role_claim: None,
            default_role: Role::Read,
            map_to_existing_users: true,
            accept_bearer: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthIdentityMode {
    /// Use `sub` verbatim as the RedDB username.
    SubClaim,
    /// Read a specific string claim (e.g. `preferred_username`, `email`).
    ClaimField(String),
}

/// Parsed JWT header + payload that the validator consumes. Listeners
/// wire an actual JWT parser (e.g. `jsonwebtoken` crate) and produce
/// this struct; tests build one directly.
#[derive(Debug, Clone)]
pub struct DecodedJwt {
    pub header: JwtHeader,
    pub claims: JwtClaims,
    /// Raw signature bytes. The validator hands these to the JWKS
    /// verifier; tests can leave them empty when injecting trust.
    pub signature: Vec<u8>,
    /// The `header.payload` compact-serialization bytes signature was
    /// computed over. Required by the verifier.
    pub signing_input: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct JwtHeader {
    pub alg: String,
    /// Key ID — matches an entry in the JWKS.
    pub kid: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct JwtClaims {
    pub iss: Option<String>,
    pub sub: Option<String>,
    /// Audience: may be a single string or an array of strings.
    pub aud: Vec<String>,
    pub exp: Option<i64>,
    pub nbf: Option<i64>,
    pub iat: Option<i64>,
    /// Extra string claims (email, preferred_username, role, etc.).
    pub extra: HashMap<String, String>,
}

impl JwtClaims {
    pub fn claim(&self, key: &str) -> Option<&str> {
        match key {
            "iss" => self.iss.as_deref(),
            "sub" => self.sub.as_deref(),
            _ => self.extra.get(key).map(|s| s.as_str()),
        }
    }
}

/// Identity produced after successful token validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthIdentity {
    pub username: String,
    pub role: Role,
    pub issuer: String,
    pub subject: Option<String>,
    pub expires_at_unix_secs: Option<i64>,
}

#[derive(Debug, Clone)]
pub enum OAuthError {
    Disabled,
    /// No Bearer token in the Authorization header.
    MissingToken,
    /// Header or claim structure was malformed.
    Malformed(String),
    /// `iss` did not match the configured issuer.
    WrongIssuer {
        expected: String,
        actual: String,
    },
    /// `aud` did not contain the configured audience.
    WrongAudience {
        expected: String,
        actual: Vec<String>,
    },
    /// `exp` has passed.
    Expired {
        exp: i64,
    },
    /// `nbf` is in the future.
    NotYetValid {
        nbf: i64,
    },
    /// Signature verification failed or no JWK matched the `kid`.
    BadSignature(String),
    /// Identity-mode configured but the claim isn't present.
    MissingIdentityClaim(OAuthIdentityMode),
    /// `role_claim` configured but absent or unparseable.
    MissingOrInvalidRole(String),
    /// `map_to_existing_users=true` and no user matches.
    UnknownUser(String),
    /// Wraps a transport error during JWKS fetch (when implemented).
    JwksFetch(String),
}

impl std::fmt::Display for OAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OAuthError::Disabled => write!(f, "OAuth disabled on this listener"),
            OAuthError::MissingToken => write!(f, "no Bearer token"),
            OAuthError::Malformed(m) => write!(f, "malformed JWT: {m}"),
            OAuthError::WrongIssuer { expected, actual } => {
                write!(f, "issuer mismatch: expected {expected}, got {actual}")
            }
            OAuthError::WrongAudience { expected, actual } => {
                write!(
                    f,
                    "audience mismatch: expected {expected}, got {:?}",
                    actual
                )
            }
            OAuthError::Expired { exp } => write!(f, "token expired at unix {exp}"),
            OAuthError::NotYetValid { nbf } => {
                write!(f, "token not valid before unix {nbf}")
            }
            OAuthError::BadSignature(m) => write!(f, "signature verification failed: {m}"),
            OAuthError::MissingIdentityClaim(mode) => {
                write!(f, "identity claim missing for mode {:?}", mode)
            }
            OAuthError::MissingOrInvalidRole(c) => {
                write!(f, "role claim '{c}' missing or not a valid Role")
            }
            OAuthError::UnknownUser(u) => write!(f, "OAuth user '{u}' not in auth store"),
            OAuthError::JwksFetch(m) => write!(f, "JWKS fetch failed: {m}"),
        }
    }
}

impl std::error::Error for OAuthError {}

/// A single key from the JWKS endpoint. Phase 3.4 keeps the JWK in
/// pre-parsed form (algorithm-specific byte buffers) so the validator
/// can delegate verification to any signing library the deployment
/// chooses (`ring`, `rsa`, etc.) without tying the core module to a
/// crypto dependency.
#[derive(Debug, Clone)]
pub struct Jwk {
    pub kid: String,
    pub alg: String,
    /// Opaque pre-parsed key bytes (SPKI DER for RSA, raw point for EC).
    /// The closure passed to `OAuthValidator::with_verifier` knows how
    /// to interpret them.
    pub key_bytes: Vec<u8>,
}

/// Signature-verification callback. Owns the crypto dependency so the
/// auth module stays pure Rust. Returns `Ok(())` when the signature
/// over `signing_input` verifies with `jwk`, `Err` otherwise.
pub type JwtVerifier = Box<dyn Fn(&Jwk, &[u8], &[u8]) -> Result<(), String> + Send + Sync>;

pub struct OAuthValidator {
    config: OAuthConfig,
    jwks: parking_lot::RwLock<Vec<Jwk>>,
    verifier: JwtVerifier,
}

impl OAuthValidator {
    /// Construct a validator with an explicit signature verifier.
    /// Tests pass a closure that always returns `Ok(())`; production
    /// code plugs a real RS256 / ES256 verifier.
    pub fn with_verifier(config: OAuthConfig, verifier: JwtVerifier) -> Self {
        Self {
            config,
            jwks: parking_lot::RwLock::new(Vec::new()),
            verifier,
        }
    }

    /// Replace the JWKS cache. Called by the initial fetch + the
    /// periodic refresh loop. Tests seed known keys directly.
    pub fn set_jwks(&self, keys: Vec<Jwk>) {
        *self.jwks.write() = keys;
    }

    pub fn config(&self) -> &OAuthConfig {
        &self.config
    }

    /// Extract a bearer token from an `Authorization` header value, or
    /// `None` when the header isn't bearer-style.
    pub fn extract_bearer(&self, header_value: &str) -> Option<String> {
        if !self.config.accept_bearer {
            return None;
        }
        let trimmed = header_value.trim();
        let prefix = "Bearer ";
        if trimmed.len() > prefix.len() && trimmed[..prefix.len()].eq_ignore_ascii_case(prefix) {
            Some(trimmed[prefix.len()..].trim().to_string())
        } else {
            None
        }
    }

    /// Validate a decoded token. `now_unix_secs` is injected so the
    /// caller controls the clock (tests freeze time; production passes
    /// `SystemTime::now`).
    pub fn validate<F>(
        &self,
        token: &DecodedJwt,
        now_unix_secs: i64,
        lookup_user: F,
    ) -> Result<OAuthIdentity, OAuthError>
    where
        F: Fn(&str) -> Option<User>,
    {
        if !self.config.enabled {
            return Err(OAuthError::Disabled);
        }

        // 1. Signature — look up the key by kid, hand bytes to verifier.
        let jwk = {
            let jwks = self.jwks.read();
            let kid = token.header.kid.as_deref();
            jwks.iter()
                .find(|j| kid.map(|k| k == j.kid).unwrap_or(false) && j.alg == token.header.alg)
                .cloned()
        };
        let Some(jwk) = jwk else {
            return Err(OAuthError::BadSignature(format!(
                "no JWK for kid {:?} alg {}",
                token.header.kid, token.header.alg
            )));
        };
        (self.verifier)(&jwk, &token.signing_input, &token.signature)
            .map_err(OAuthError::BadSignature)?;

        // 2. Standard claims.
        match &token.claims.iss {
            Some(iss) if iss == &self.config.issuer => {}
            Some(iss) => {
                return Err(OAuthError::WrongIssuer {
                    expected: self.config.issuer.clone(),
                    actual: iss.clone(),
                });
            }
            None => {
                return Err(OAuthError::Malformed("missing iss".into()));
            }
        }
        if !token.claims.aud.iter().any(|a| a == &self.config.audience) {
            return Err(OAuthError::WrongAudience {
                expected: self.config.audience.clone(),
                actual: token.claims.aud.clone(),
            });
        }
        if let Some(exp) = token.claims.exp {
            if exp <= now_unix_secs {
                return Err(OAuthError::Expired { exp });
            }
        }
        if let Some(nbf) = token.claims.nbf {
            if nbf > now_unix_secs {
                return Err(OAuthError::NotYetValid { nbf });
            }
        }

        // 3. Identity extraction.
        let username = match &self.config.identity_mode {
            OAuthIdentityMode::SubClaim => token
                .claims
                .sub
                .clone()
                .ok_or_else(|| OAuthError::MissingIdentityClaim(OAuthIdentityMode::SubClaim))?,
            OAuthIdentityMode::ClaimField(name) => token
                .claims
                .claim(name)
                .map(|s| s.to_string())
                .ok_or_else(|| {
                    OAuthError::MissingIdentityClaim(OAuthIdentityMode::ClaimField(name.clone()))
                })?,
        };

        // 4. Role derivation — user store wins when configured.
        let role = if self.config.map_to_existing_users {
            match lookup_user(&username) {
                Some(user) => user.role,
                None => self.derive_role_from_claims(&token.claims)?,
            }
        } else {
            self.derive_role_from_claims(&token.claims)?
        };

        Ok(OAuthIdentity {
            username,
            role,
            issuer: self.config.issuer.clone(),
            subject: token.claims.sub.clone(),
            expires_at_unix_secs: token.claims.exp,
        })
    }

    fn derive_role_from_claims(&self, claims: &JwtClaims) -> Result<Role, OAuthError> {
        let Some(name) = &self.config.role_claim else {
            return Ok(self.config.default_role);
        };
        let raw = claims
            .claim(name)
            .ok_or_else(|| OAuthError::MissingOrInvalidRole(name.clone()))?;
        Role::from_str(raw.trim()).ok_or_else(|| OAuthError::MissingOrInvalidRole(name.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_verifier() -> JwtVerifier {
        Box::new(|_jwk, _input, _sig| Ok(()))
    }

    fn base_config() -> OAuthConfig {
        OAuthConfig {
            enabled: true,
            issuer: "https://id.example.com".to_string(),
            audience: "reddb".to_string(),
            jwks_url: String::new(),
            identity_mode: OAuthIdentityMode::SubClaim,
            role_claim: None,
            default_role: Role::Read,
            map_to_existing_users: false,
            accept_bearer: true,
        }
    }

    fn base_token(now: i64) -> DecodedJwt {
        DecodedJwt {
            header: JwtHeader {
                alg: "RS256".to_string(),
                kid: Some("k1".to_string()),
            },
            claims: JwtClaims {
                iss: Some("https://id.example.com".to_string()),
                sub: Some("alice".to_string()),
                aud: vec!["reddb".to_string()],
                exp: Some(now + 3600),
                nbf: Some(now - 60),
                iat: Some(now),
                extra: HashMap::new(),
            },
            signature: vec![0u8; 8],
            signing_input: b"header.payload".to_vec(),
        }
    }

    fn seeded_validator() -> OAuthValidator {
        let v = OAuthValidator::with_verifier(base_config(), noop_verifier());
        v.set_jwks(vec![Jwk {
            kid: "k1".to_string(),
            alg: "RS256".to_string(),
            key_bytes: Vec::new(),
        }]);
        v
    }

    #[test]
    fn extract_bearer_case_insensitive() {
        let v = seeded_validator();
        assert_eq!(
            v.extract_bearer("Bearer abc.def.ghi").as_deref(),
            Some("abc.def.ghi")
        );
        assert_eq!(v.extract_bearer("bearer xyz").as_deref(), Some("xyz"));
        assert!(v.extract_bearer("Basic QQ==").is_none());
    }

    #[test]
    fn valid_token_yields_sub_identity() {
        let v = seeded_validator();
        let token = base_token(1_700_000_000);
        let id = v.validate(&token, 1_700_000_000, |_| None).unwrap();
        assert_eq!(id.username, "alice");
        assert_eq!(id.role, Role::Read);
    }

    #[test]
    fn issuer_mismatch_rejected() {
        let v = seeded_validator();
        let mut token = base_token(1_700_000_000);
        token.claims.iss = Some("https://evil.example.com".to_string());
        assert!(matches!(
            v.validate(&token, 1_700_000_000, |_| None),
            Err(OAuthError::WrongIssuer { .. })
        ));
    }

    #[test]
    fn audience_mismatch_rejected() {
        let v = seeded_validator();
        let mut token = base_token(1_700_000_000);
        token.claims.aud = vec!["other".to_string()];
        assert!(matches!(
            v.validate(&token, 1_700_000_000, |_| None),
            Err(OAuthError::WrongAudience { .. })
        ));
    }

    #[test]
    fn expired_token_rejected() {
        let v = seeded_validator();
        let mut token = base_token(1_700_000_000);
        token.claims.exp = Some(1_600_000_000);
        assert!(matches!(
            v.validate(&token, 1_700_000_000, |_| None),
            Err(OAuthError::Expired { .. })
        ));
    }

    #[test]
    fn not_yet_valid_rejected() {
        let v = seeded_validator();
        let mut token = base_token(1_700_000_000);
        token.claims.nbf = Some(1_800_000_000);
        assert!(matches!(
            v.validate(&token, 1_700_000_000, |_| None),
            Err(OAuthError::NotYetValid { .. })
        ));
    }

    #[test]
    fn missing_jwk_fails_signature() {
        let v = OAuthValidator::with_verifier(base_config(), noop_verifier());
        // No JWKS seeded.
        let token = base_token(1_700_000_000);
        assert!(matches!(
            v.validate(&token, 1_700_000_000, |_| None),
            Err(OAuthError::BadSignature(_))
        ));
    }

    #[test]
    fn role_claim_parses_from_extra() {
        let mut config = base_config();
        config.role_claim = Some("role".to_string());
        let v = OAuthValidator::with_verifier(config, noop_verifier());
        v.set_jwks(vec![Jwk {
            kid: "k1".to_string(),
            alg: "RS256".to_string(),
            key_bytes: Vec::new(),
        }]);
        let mut token = base_token(1_700_000_000);
        token
            .claims
            .extra
            .insert("role".to_string(), "admin".to_string());
        let id = v.validate(&token, 1_700_000_000, |_| None).unwrap();
        assert_eq!(id.role, Role::Admin);
    }

    #[test]
    fn claim_field_identity_mode() {
        let mut config = base_config();
        config.identity_mode = OAuthIdentityMode::ClaimField("preferred_username".into());
        let v = OAuthValidator::with_verifier(config, noop_verifier());
        v.set_jwks(vec![Jwk {
            kid: "k1".to_string(),
            alg: "RS256".to_string(),
            key_bytes: Vec::new(),
        }]);
        let mut token = base_token(1_700_000_000);
        token
            .claims
            .extra
            .insert("preferred_username".into(), "alice.smith".into());
        let id = v.validate(&token, 1_700_000_000, |_| None).unwrap();
        assert_eq!(id.username, "alice.smith");
    }
}
