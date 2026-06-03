//! Browser credential layer — the hybrid token model (issue #936, PRD
//! #930, ADR 0036 §"Connection security", ADR 0029 §Authorization).
//!
//! A browser SPA cannot safely hold a long-lived bearer credential: any
//! token reachable from JavaScript is exfiltrable by an XSS payload. The
//! hybrid model splits the credential in two:
//!
//!   * a **short-lived access JWT** held only in memory (a JS variable),
//!     presented in the RedWire-over-WSS handshake (ADR 0036) exactly
//!     where native drivers present a bearer/OAuth-JWT, and
//!   * a **long-lived refresh token** delivered as an
//!     `HttpOnly; Secure; SameSite` cookie that JavaScript can never
//!     read. The browser silently mints a fresh access JWT from it at
//!     the `/auth/browser/refresh` endpoint.
//!
//! Both tokens are HS256 JWTs minted and verified by *this* server with
//! a single symmetric secret — RedDB is both issuer and verifier, so the
//! asymmetric RS256/JWKS machinery of [`super::oauth`] (which exists to
//! trust a *foreign* IdP) is unnecessary weight here. The vetted
//! `jsonwebtoken` crate owns signature construction and verification;
//! this module owns the issuer/audience/type/expiry policy on top.
//!
//! ## Why access-token rotation does not tear down in-flight streams
//!
//! ADR 0029 §Authorization makes the bearer token authenticate only the
//! *open* of a stream; an internal, unforwarded **stream lease** bound to
//! the MVCC snapshot pin is the credential consulted for every subsequent
//! chunk. So when a browser's access JWT expires and it mints a new one
//! at `/auth/browser/refresh`, the new token is used for the *next*
//! handshake — the streams already accepted on the live RedWire
//! connection keep flowing under their leases, untouched. The refresh
//! cadence is decoupled from result-set delivery time. This module mints
//! the tokens; that decoupling lives in the stream lease (see
//! `crate::server::output_stream`) and is exercised end-to-end by the
//! issue-#936 integration test.
//!
//! ## What this module deliberately does not do
//!
//! mTLS stays native-only (ADR 0036): browser client certificates are
//! hostile UX, so there is no browser mTLS path here. The access JWT /
//! refresh cookie pair is the browser's sole credential.

use std::collections::HashSet;

use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};

use super::Role;

/// Minimum HS256 secret length. RFC 7518 §3.2 requires a key at least as
/// long as the HMAC output (256 bits / 32 bytes); a shorter key is a
/// silent downgrade of the signature's security, so we reject it at
/// construction rather than mint weakly-keyed tokens.
pub const MIN_SECRET_BYTES: usize = 32;

/// `SameSite` cookie attribute for the refresh cookie. `Strict` is the
/// secure default — the refresh cookie is never attached to a
/// cross-site navigation, which is the cleanest CSRF posture for a
/// same-origin SPA. `Lax`/`None` exist for deployments that serve the
/// SPA from a different site; `None` *requires* `Secure` (enforced in
/// [`BrowserTokenConfig::sanitised`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SameSite {
    Strict,
    Lax,
    None,
}

impl SameSite {
    pub fn as_str(&self) -> &'static str {
        match self {
            SameSite::Strict => "Strict",
            SameSite::Lax => "Lax",
            SameSite::None => "None",
        }
    }
}

/// Configuration for the hybrid-token authority. Secure by default:
/// `Secure` cookies, `SameSite=Strict`, a short access TTL, and an
/// `HttpOnly` refresh cookie.
#[derive(Debug, Clone)]
pub struct BrowserTokenConfig {
    /// HS256 signing/verification secret. Must be ≥ [`MIN_SECRET_BYTES`].
    pub secret: Vec<u8>,
    /// `iss` claim stamped on every token and required on verify.
    pub issuer: String,
    /// `aud` claim stamped on every token and required on verify.
    pub audience: String,
    /// Access-JWT lifetime, seconds. Short by design (default 15 min):
    /// the blast radius of a leaked in-memory access token is one TTL.
    pub access_ttl_secs: i64,
    /// Refresh-cookie lifetime, seconds (default 30 days). Bounds how
    /// long a stolen refresh cookie is useful and sets the cookie's
    /// `Max-Age`.
    pub refresh_ttl_secs: i64,
    /// `Secure` attribute on the refresh cookie. Default true — the
    /// cookie must only ride HTTPS. Tests on a clear-text loopback set
    /// this false explicitly.
    pub cookie_secure: bool,
    /// `SameSite` attribute on the refresh cookie.
    pub same_site: SameSite,
    /// Cookie name. Default `reddb_refresh`.
    pub cookie_name: String,
    /// Cookie `Path` — scopes which requests carry the refresh cookie.
    /// Default `/auth/browser`, so it reaches `refresh`/`logout` but no
    /// other endpoint ever sees it.
    pub cookie_path: String,
}

impl BrowserTokenConfig {
    /// Build a config with secure defaults around an explicit secret.
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        Self {
            secret: secret.into(),
            issuer: "reddb-browser".to_string(),
            audience: "reddb-redwire".to_string(),
            access_ttl_secs: 15 * 60,
            refresh_ttl_secs: 30 * 24 * 60 * 60,
            cookie_secure: true,
            same_site: SameSite::Strict,
            cookie_name: "reddb_refresh".to_string(),
            cookie_path: "/auth/browser".to_string(),
        }
    }

    /// Apply cross-field invariants. `SameSite=None` is meaningless
    /// without `Secure` (modern browsers reject it), so we force `Secure`
    /// on rather than mint a cookie the browser will silently drop.
    fn sanitised(mut self) -> Self {
        if self.same_site == SameSite::None {
            self.cookie_secure = true;
        }
        self
    }
}

/// The identity carried by a validated browser token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserIdentity {
    pub username: String,
    pub tenant: Option<String>,
    pub role: Role,
}

/// Reasons a token is refused. Kept distinct so the WS handshake and the
/// refresh endpoint can log *why* without leaking detail to the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserTokenError {
    /// Signature, issuer, audience, or structural decode failure. The
    /// string is for server-side logs only.
    Decode(String),
    /// A refresh token was presented where an access token was required,
    /// or vice-versa. A refresh token must never authenticate a session
    /// directly — it only mints access tokens.
    WrongType { expected: TokenType, got: String },
    /// `exp` is at or before now.
    Expired,
    /// `nbf` is in the future.
    NotYetValid,
    /// The embedded `role` claim is not a known [`Role`].
    BadRole(String),
}

impl std::fmt::Display for BrowserTokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BrowserTokenError::Decode(m) => write!(f, "token decode failed: {m}"),
            BrowserTokenError::WrongType { expected, got } => {
                write!(
                    f,
                    "wrong token type: expected {}, got {got:?}",
                    expected.as_str()
                )
            }
            BrowserTokenError::Expired => write!(f, "token expired"),
            BrowserTokenError::NotYetValid => write!(f, "token not yet valid"),
            BrowserTokenError::BadRole(r) => write!(f, "token carries unknown role {r:?}"),
        }
    }
}

impl std::error::Error for BrowserTokenError {}

/// Which leg of the hybrid pair a token is. Stamped into the `typ` claim
/// and checked on verify so a refresh token can never be replayed as a
/// session credential (and an access token can never be replayed at the
/// refresh endpoint).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenType {
    Access,
    Refresh,
}

impl TokenType {
    pub fn as_str(&self) -> &'static str {
        match self {
            TokenType::Access => "access",
            TokenType::Refresh => "refresh",
        }
    }
}

/// JWT claim set. `exp`/`iat` are unix seconds. `typ` discriminates the
/// pair; `tenant` is omitted entirely when the identity is
/// platform-scoped so the wire form stays minimal.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Claims {
    iss: String,
    aud: String,
    sub: String,
    exp: i64,
    iat: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nbf: Option<i64>,
    typ: String,
    role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tenant: Option<String>,
}

/// The pair returned to the browser by a successful login / refresh: the
/// access JWT (held in memory) and the refresh JWT (set as a cookie).
#[derive(Debug, Clone)]
pub struct IssuedTokens {
    pub access_token: String,
    /// Seconds until the access token expires — the SPA schedules its
    /// silent refresh a little before this.
    pub access_expires_in: i64,
    pub refresh_token: String,
}

/// Mints and verifies the hybrid-token pair for the browser credential
/// layer. Cheap to clone the `Arc` the runtime holds; the keys inside are
/// derived once at construction.
pub struct BrowserTokenAuthority {
    config: BrowserTokenConfig,
    encoding: EncodingKey,
    decoding: DecodingKey,
}

impl std::fmt::Debug for BrowserTokenAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render the keys.
        f.debug_struct("BrowserTokenAuthority")
            .field("issuer", &self.config.issuer)
            .field("audience", &self.config.audience)
            .field("access_ttl_secs", &self.config.access_ttl_secs)
            .field("refresh_ttl_secs", &self.config.refresh_ttl_secs)
            .finish_non_exhaustive()
    }
}

impl BrowserTokenAuthority {
    /// Construct an authority. Fails if the secret is shorter than
    /// [`MIN_SECRET_BYTES`] — a weak key is rejected loudly rather than
    /// silently weakening every token.
    pub fn new(config: BrowserTokenConfig) -> Result<Self, String> {
        if config.secret.len() < MIN_SECRET_BYTES {
            return Err(format!(
                "browser-token secret must be at least {MIN_SECRET_BYTES} bytes, got {}",
                config.secret.len()
            ));
        }
        let config = config.sanitised();
        let encoding = EncodingKey::from_secret(&config.secret);
        let decoding = DecodingKey::from_secret(&config.secret);
        Ok(Self {
            config,
            encoding,
            decoding,
        })
    }

    pub fn access_ttl_secs(&self) -> i64 {
        self.config.access_ttl_secs
    }

    pub fn cookie_name(&self) -> &str {
        &self.config.cookie_name
    }

    /// Mint an access + refresh pair for an authenticated identity at
    /// `now` (unix seconds). Used by `/auth/browser/login`.
    pub fn issue(&self, identity: &BrowserIdentity, now: i64) -> Result<IssuedTokens, String> {
        let access_token = self.encode(
            identity,
            TokenType::Access,
            now,
            self.config.access_ttl_secs,
        )?;
        let refresh_token = self.encode(
            identity,
            TokenType::Refresh,
            now,
            self.config.refresh_ttl_secs,
        )?;
        Ok(IssuedTokens {
            access_token,
            access_expires_in: self.config.access_ttl_secs,
            refresh_token,
        })
    }

    /// Mint a fresh access token (only) for an identity recovered from a
    /// valid refresh token at `now`. Used by `/auth/browser/refresh`.
    pub fn issue_access(&self, identity: &BrowserIdentity, now: i64) -> Result<String, String> {
        self.encode(
            identity,
            TokenType::Access,
            now,
            self.config.access_ttl_secs,
        )
    }

    fn encode(
        &self,
        identity: &BrowserIdentity,
        typ: TokenType,
        now: i64,
        ttl: i64,
    ) -> Result<String, String> {
        let claims = Claims {
            iss: self.config.issuer.clone(),
            aud: self.config.audience.clone(),
            sub: identity.username.clone(),
            exp: now + ttl,
            iat: now,
            nbf: Some(now),
            typ: typ.as_str().to_string(),
            role: identity.role.as_str().to_string(),
            tenant: identity.tenant.clone(),
        };
        encode(&Header::new(Algorithm::HS256), &claims, &self.encoding)
            .map_err(|e| format!("encode browser token: {e}"))
    }

    /// Verify an access token presented in the RedWire WS handshake.
    pub fn validate_access(
        &self,
        token: &str,
        now: i64,
    ) -> Result<BrowserIdentity, BrowserTokenError> {
        self.validate(token, TokenType::Access, now)
    }

    /// Verify a refresh token presented (as a cookie) at the refresh
    /// endpoint.
    pub fn validate_refresh(
        &self,
        token: &str,
        now: i64,
    ) -> Result<BrowserIdentity, BrowserTokenError> {
        self.validate(token, TokenType::Refresh, now)
    }

    /// Signature + issuer + audience are verified by the vetted
    /// `jsonwebtoken` decode; the temporal checks (`exp`/`nbf`) run here
    /// against the injected `now` so the clock is testable — mirroring
    /// the injected-clock pattern of [`super::oauth::OAuthValidator`].
    fn validate(
        &self,
        token: &str,
        expected: TokenType,
        now: i64,
    ) -> Result<BrowserIdentity, BrowserTokenError> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_issuer(&[self.config.issuer.as_str()]);
        validation.set_audience(&[self.config.audience.as_str()]);
        // We own the clock: disable the library's own exp/nbf checks (it
        // reads the wall clock, which tests cannot freeze) and enforce
        // them below against the injected `now`. The signature, `iss`,
        // and `aud` checks the library *does* run are the security core.
        validation.validate_exp = false;
        validation.validate_nbf = false;
        validation.required_spec_claims = HashSet::new();

        let data = decode::<Claims>(token, &self.decoding, &validation)
            .map_err(|e| BrowserTokenError::Decode(e.to_string()))?;
        let claims = data.claims;

        if claims.typ != expected.as_str() {
            return Err(BrowserTokenError::WrongType {
                expected,
                got: claims.typ,
            });
        }
        if now >= claims.exp {
            return Err(BrowserTokenError::Expired);
        }
        if let Some(nbf) = claims.nbf {
            if now < nbf {
                return Err(BrowserTokenError::NotYetValid);
            }
        }
        let role = Role::from_str(&claims.role).ok_or(BrowserTokenError::BadRole(claims.role))?;
        Ok(BrowserIdentity {
            username: claims.sub,
            tenant: claims.tenant,
            role,
        })
    }

    /// `Set-Cookie` value that installs the refresh token. `HttpOnly`
    /// (unreadable from JS), plus the configured `Secure`/`SameSite`/
    /// `Path` and a `Max-Age` matching the refresh TTL.
    pub fn refresh_cookie(&self, refresh_token: &str) -> String {
        self.build_cookie(refresh_token, self.config.refresh_ttl_secs)
    }

    /// `Set-Cookie` value that clears the refresh cookie (logout). Empty
    /// value, `Max-Age=0`, same attributes so the browser matches and
    /// evicts it.
    pub fn clear_cookie(&self) -> String {
        self.build_cookie("", 0)
    }

    fn build_cookie(&self, value: &str, max_age: i64) -> String {
        let mut cookie = format!(
            "{}={}; HttpOnly; Path={}; Max-Age={}; SameSite={}",
            self.config.cookie_name,
            value,
            self.config.cookie_path,
            max_age,
            self.config.same_site.as_str()
        );
        if self.config.cookie_secure {
            cookie.push_str("; Secure");
        }
        cookie
    }
}

/// Extract a named cookie's value from a raw `Cookie:` request header.
/// Returns the first match. Cookie values are not URL-decoded — JWT
/// compact serialization is already cookie-safe (base64url + `.`).
pub fn cookie_value<'a>(cookie_header: &'a str, name: &str) -> Option<&'a str> {
    cookie_header.split(';').find_map(|pair| {
        let pair = pair.trim();
        let (k, v) = pair.split_once('=')?;
        if k.trim() == name {
            Some(v.trim())
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_750_000_000;

    fn authority() -> BrowserTokenAuthority {
        let secret = b"0123456789abcdef0123456789abcdef".to_vec();
        BrowserTokenAuthority::new(BrowserTokenConfig::new(secret)).unwrap()
    }

    fn identity() -> BrowserIdentity {
        BrowserIdentity {
            username: "alice".to_string(),
            tenant: Some("acme".to_string()),
            role: Role::Write,
        }
    }

    #[test]
    fn rejects_short_secret() {
        let err = BrowserTokenAuthority::new(BrowserTokenConfig::new(b"too-short".to_vec()));
        assert!(err.is_err());
    }

    #[test]
    fn issue_then_validate_access_roundtrip() {
        let auth = authority();
        let tokens = auth.issue(&identity(), NOW).unwrap();
        let id = auth
            .validate_access(&tokens.access_token, NOW + 60)
            .unwrap();
        assert_eq!(id, identity());
        assert_eq!(tokens.access_expires_in, 15 * 60);
    }

    #[test]
    fn platform_scoped_identity_has_no_tenant() {
        let auth = authority();
        let id = BrowserIdentity {
            username: "root".to_string(),
            tenant: None,
            role: Role::Admin,
        };
        let tokens = auth.issue(&id, NOW).unwrap();
        let got = auth.validate_access(&tokens.access_token, NOW + 1).unwrap();
        assert_eq!(got.tenant, None);
        assert_eq!(got.role, Role::Admin);
    }

    #[test]
    fn expired_access_token_rejected() {
        let auth = authority();
        let tokens = auth.issue(&identity(), NOW).unwrap();
        // 15-minute TTL; ask at now + 16 minutes.
        let err = auth
            .validate_access(&tokens.access_token, NOW + 16 * 60)
            .unwrap_err();
        assert_eq!(err, BrowserTokenError::Expired);
    }

    #[test]
    fn not_yet_valid_token_rejected() {
        let auth = authority();
        let tokens = auth.issue(&identity(), NOW).unwrap();
        // nbf = NOW; validate at NOW - 10.
        let err = auth
            .validate_access(&tokens.access_token, NOW - 10)
            .unwrap_err();
        assert_eq!(err, BrowserTokenError::NotYetValid);
    }

    #[test]
    fn refresh_token_cannot_authenticate_a_session() {
        // A refresh token presented where an access token is required
        // (the WS handshake) must be refused on type, even though its
        // signature is perfectly valid.
        let auth = authority();
        let tokens = auth.issue(&identity(), NOW).unwrap();
        let err = auth
            .validate_access(&tokens.refresh_token, NOW + 60)
            .unwrap_err();
        assert!(matches!(err, BrowserTokenError::WrongType { .. }));
    }

    #[test]
    fn access_token_cannot_be_used_at_refresh_endpoint() {
        let auth = authority();
        let tokens = auth.issue(&identity(), NOW).unwrap();
        let err = auth
            .validate_refresh(&tokens.access_token, NOW + 60)
            .unwrap_err();
        assert!(matches!(err, BrowserTokenError::WrongType { .. }));
    }

    #[test]
    fn refresh_validates_and_mints_new_access() {
        let auth = authority();
        let tokens = auth.issue(&identity(), NOW).unwrap();
        // Later, the cookie is replayed to mint a new access token.
        let later = NOW + 10 * 60;
        let id = auth.validate_refresh(&tokens.refresh_token, later).unwrap();
        let new_access = auth.issue_access(&id, later).unwrap();
        // The freshly-minted access token is valid well past the
        // original access token's expiry — refresh genuinely extends.
        let got = auth.validate_access(&new_access, NOW + 20 * 60).unwrap();
        assert_eq!(got, identity());
    }

    #[test]
    fn token_signed_by_a_different_secret_is_rejected() {
        let auth = authority();
        let other = BrowserTokenAuthority::new(BrowserTokenConfig::new(
            b"FEDCBA9876543210FEDCBA9876543210".to_vec(),
        ))
        .unwrap();
        let tokens = other.issue(&identity(), NOW).unwrap();
        let err = auth
            .validate_access(&tokens.access_token, NOW + 60)
            .unwrap_err();
        assert!(matches!(err, BrowserTokenError::Decode(_)));
    }

    #[test]
    fn wrong_audience_rejected() {
        let auth = authority();
        let mut cfg = BrowserTokenConfig::new(b"0123456789abcdef0123456789abcdef".to_vec());
        cfg.audience = "someone-else".to_string();
        let other = BrowserTokenAuthority::new(cfg).unwrap();
        let tokens = other.issue(&identity(), NOW).unwrap();
        let err = auth
            .validate_access(&tokens.access_token, NOW + 60)
            .unwrap_err();
        assert!(matches!(err, BrowserTokenError::Decode(_)));
    }

    #[test]
    fn refresh_cookie_carries_security_attributes() {
        let auth = authority();
        let cookie = auth.refresh_cookie("the.jwt.value");
        assert!(cookie.contains("reddb_refresh=the.jwt.value"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Path=/auth/browser"));
        assert!(cookie.contains("Max-Age=2592000"));
    }

    #[test]
    fn clear_cookie_expires_immediately() {
        let auth = authority();
        let cookie = auth.clear_cookie();
        assert!(cookie.contains("reddb_refresh=;"));
        assert!(cookie.contains("Max-Age=0"));
        assert!(cookie.contains("HttpOnly"));
    }

    #[test]
    fn samesite_none_forces_secure() {
        let mut cfg = BrowserTokenConfig::new(b"0123456789abcdef0123456789abcdef".to_vec());
        cfg.same_site = SameSite::None;
        cfg.cookie_secure = false; // should be overridden
        let auth = BrowserTokenAuthority::new(cfg).unwrap();
        let cookie = auth.refresh_cookie("x");
        assert!(cookie.contains("SameSite=None"));
        assert!(cookie.contains("Secure"));
    }

    #[test]
    fn cookie_value_extracts_named_cookie() {
        let header = "other=1; reddb_refresh=abc.def.ghi; theme=dark";
        assert_eq!(cookie_value(header, "reddb_refresh"), Some("abc.def.ghi"));
        assert_eq!(cookie_value(header, "missing"), None);
    }

    #[test]
    fn cookie_value_handles_single_cookie() {
        assert_eq!(
            cookie_value("reddb_refresh=solo", "reddb_refresh"),
            Some("solo")
        );
    }
}
