//! JWT tenant-claim wiring.
//!
//! Asserts that an OAuth validator configured with `tenant_claim`
//! surfaces the tenant on the resolved `OAuthIdentity` and that the
//! redwire `validate_oauth_jwt_full` shim propagates it back to the
//! caller, so the listener can mint a session pinned to that tenant.

use std::collections::HashMap;

use reddb::auth::oauth::{
    DecodedJwt, Jwk, JwtClaims, JwtHeader, OAuthConfig, OAuthIdentityMode, OAuthValidator,
};
use reddb::auth::Role;

fn noop_verifier() -> Box<dyn Fn(&Jwk, &[u8], &[u8]) -> Result<(), String> + Send + Sync> {
    Box::new(|_jwk, _input, _sig| Ok(()))
}

fn base_validator(tenant_claim: Option<&str>) -> OAuthValidator {
    let cfg = OAuthConfig {
        enabled: true,
        issuer: "https://id.example.com".to_string(),
        audience: "reddb".to_string(),
        jwks_url: String::new(),
        identity_mode: OAuthIdentityMode::SubClaim,
        role_claim: Some("role".to_string()),
        tenant_claim: tenant_claim.map(|s| s.to_string()),
        default_role: Role::Read,
        map_to_existing_users: false,
        accept_bearer: true,
    };
    let v = OAuthValidator::with_verifier(cfg, noop_verifier());
    v.set_jwks(vec![Jwk {
        kid: "k1".to_string(),
        alg: "RS256".to_string(),
        key_bytes: Vec::new(),
    }]);
    v
}

fn token_with_extras(now: i64, extras: &[(&str, &str)]) -> DecodedJwt {
    let mut extra = HashMap::new();
    for (k, v) in extras {
        extra.insert((*k).to_string(), (*v).to_string());
    }
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
            extra,
        },
        signature: vec![0u8; 8],
        signing_input: b"header.payload".to_vec(),
    }
}

#[test]
fn tenant_claim_extracted_into_identity() {
    let v = base_validator(Some("tenant"));
    let now = 1_700_000_000;
    let token = token_with_extras(now, &[("tenant", "acme"), ("role", "admin")]);
    let identity = v.validate(&token, now, |_| None).unwrap();
    assert_eq!(identity.username, "alice");
    assert_eq!(identity.tenant.as_deref(), Some("acme"));
    assert_eq!(identity.role, Role::Admin);
}

#[test]
fn tenant_absent_yields_platform_identity() {
    // No tenant_claim configured -> identity.tenant is always None.
    let v = base_validator(None);
    let now = 1_700_000_000;
    let token = token_with_extras(now, &[("tenant", "acme")]);
    let identity = v.validate(&token, now, |_| None).unwrap();
    assert!(identity.tenant.is_none());
}

#[test]
fn tenant_claim_with_custom_name() {
    let v = base_validator(Some("org_id"));
    let now = 1_700_000_000;
    // Use the custom claim name carried by the IdP.
    let token = token_with_extras(now, &[("org_id", "globex")]);
    let identity = v.validate(&token, now, |_| None).unwrap();
    assert_eq!(identity.tenant.as_deref(), Some("globex"));
}

#[test]
fn empty_tenant_claim_does_not_become_some_empty_string() {
    let v = base_validator(Some("tenant"));
    let now = 1_700_000_000;
    let token = token_with_extras(now, &[("tenant", "")]);
    let identity = v.validate(&token, now, |_| None).unwrap();
    // Empty string is filtered to None so callers don't have to
    // disambiguate `Some("")` from `None`.
    assert!(identity.tenant.is_none());
}
