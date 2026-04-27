//! Test-only RSA keypair + JWT minting.
//!
//! Embeds a static 2048-bit RSA keypair (generated once with
//! `openssl genpkey`) so the smoke tests don't need a runtime
//! keygen dep. The keypair is throwaway — it lives only in the
//! test binary and signs JWTs that never leave the test process.
//!
//! Public surface mirrors what an OAuth provider exposes:
//!   - [`KID`]               — kid baked into both the JWK and the JWT header.
//!   - [`mint_rs256`]        — sign a `Claims` struct with the test private key.
//!   - [`build_jwks`]        — return the JWKS JSON the discovery endpoint serves.
//!   - [`build_verifier`]    — closure that verifies RS256 signatures against the test key,
//!                             plugged into `OAuthValidator::with_verifier`.

use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};

/// Stable kid baked into the JWK + JWT header. Matches the entry
/// the JWKS endpoint serves.
pub const KID: &str = "test-kid";

/// PKCS#8 RSA-2048 private key, generated once for these tests.
/// Throwaway — embedded only to avoid pulling a runtime keygen
/// dependency. Never reuse outside this test crate.
pub const TEST_RSA_PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQCiYyHy8BHB5mBV
txAiYvOU8sVxJlNBsBmvqY8nyaeP8Hf17Pz9BJO2IV/vJODZpgymYtjmGlS/fa3/
hT/8aAMQbOk9llDErcbcuNqcOhY+4IRCeA7ovfUbAd3MMMty3Z/HuWLy+sjKMb9y
Bs/WeSXqNv/Zz870Xv2B5XImBroKkybrEwjyYEhioxDLwFEm/whl/Ep2HcJjxgPr
+OpcD84ZCRVWO8ibR9A4BZAgDszO6d5H9KjgGw/FlwAcp1r3ADj8m/uSBxv4pzpd
ACIwkVYMay7/6c7+hKBEPcQuP4Ej+hLbdWm82LsBrmNuBNt+YTvJ7MhseebBowvY
SGfoGxbpAgMBAAECggEABFtxmUGs0E2cryAa3DlYfNooxxj2qfAOOGLt1uz3xIp4
xY4G2ckqJ3xsxQ9xwxVMCJjlZgM12++E4DLUnTKzRlkNxxvF7gkVqW2CXCfI2gYP
NnNfPwp9zaw2pdh3VQ0yUNseFxP4mEhOcUJSiFg21rqEEfWcAX2dAsPD1NZgXpE6
Oku3Zg0qbeHJ/cFI9En4LhJLFEbbu+UVG0H9D79xctXvHnU1BucsevLKgB6Jjo/H
H1NnKcvaRMpnR6RGRTTkhXu6JFoJRQG2CMZljww/Tq1Cy98phxbqgRrI6e2hCRhN
O7Lf3XXWbo54F0rCYnSEOX5mLd9gYq9WUAoKGx4SGwKBgQDkgTK+PMLTZTyhVFiW
BDDUTYk2W0xtAlsX/k6bWl55+fSsRT9jhQRpd0R0Pt5/8zUZmmhq3HF6/8MSyJhT
KmOYcgJQoLdJkc64wv2n70gTTGktgLVctObvpssNItk9uwUAGZDMRXQOhF7OUkfL
Ru7fv/HBdck+tU6T2LNeb8X7MwKBgQC17UTbvVJEuTzRgdi+CWnCP0AIAvOwQe02
2hI0jLg3CbJerFf3Tf2dCTaGh28XfT/qJjqTANJCyoi5ttNtx6NvpSHWms/p1Wpe
vNAhrbKbowca4Yyb4usfwuR7ZilYDK8l1AB77z9r3jU2FI3ewuzpGmm27bqXTDTg
59/X4K/FcwKBgF2LGofQff1ma0ysJ9u5+XdgCnTrKT1TApGu9OUaOKT8k5JWgt2t
3aGDRs3D0vhUSv+hO2/LsNUmkOhGoD0jlEQbICF7uazveM4gXRD7nujvlfsfvp8m
G4guIt/MzVw9DI3+6U0Gfb1XqSwTePqZnj6Q6FpHasw2EuXph3x4i3cLAoGBAK9f
cimBb3TgPEiaKx3GZTTjVA5lChS2+L0Pqs0NeedUaaXp7UJw5DIlV3KHzAeQrbRB
9eUPvaC1LOgZ3ebNtDdDsEL4KcT3/foleV1929c8aPT4yFrdfFq5vRdXfDNsxspo
e679Ct4o7pKbbcd3kHmFBLNap6yBwdesrpOj/M0RAoGAJRayi/XN7rU41OkVOwcF
QK9V1zwKRDYU6bZRyHHmd++yp1w86Hr5zytqqWH5s6Gwcrj67OopRLLFAf75mTo+
JJG0153BPTEl87d6fm0OHUdMeypO3Y7mrVg8VC5Fhrjf8TbgjqVHWq32z4l9qT8l
s9mdObHlvBle/104fdejzJM=
-----END PRIVATE KEY-----
";

/// SubjectPublicKeyInfo (SPKI) PEM matching `TEST_RSA_PRIVATE_PEM`.
pub const TEST_RSA_PUBLIC_PEM: &str = "-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAomMh8vARweZgVbcQImLz
lPLFcSZTQbAZr6mPJ8mnj/B39ez8/QSTtiFf7yTg2aYMpmLY5hpUv32t/4U//GgD
EGzpPZZQxK3G3LjanDoWPuCEQngO6L31GwHdzDDLct2fx7li8vrIyjG/cgbP1nkl
6jb/2c/O9F79geVyJga6CpMm6xMI8mBIYqMQy8BRJv8IZfxKdh3CY8YD6/jqXA/O
GQkVVjvIm0fQOAWQIA7MzuneR/So4BsPxZcAHKda9wA4/Jv7kgcb+Kc6XQAiMJFW
DGsu/+nO/oSgRD3ELj+BI/oS23VpvNi7Aa5jbgTbfmE7yezIbHnmwaML2Ehn6BsW
6QIDAQAB
-----END PUBLIC KEY-----
";

/// JWK `n` (modulus, base64url, no padding) of `TEST_RSA_PUBLIC_PEM`.
pub const TEST_RSA_N_B64URL: &str = "omMh8vARweZgVbcQImLzlPLFcSZTQbAZr6mPJ8mnj_B39ez8_QSTtiFf7yTg2aYMpmLY5hpUv32t_4U__GgDEGzpPZZQxK3G3LjanDoWPuCEQngO6L31GwHdzDDLct2fx7li8vrIyjG_cgbP1nkl6jb_2c_O9F79geVyJga6CpMm6xMI8mBIYqMQy8BRJv8IZfxKdh3CY8YD6_jqXA_OGQkVVjvIm0fQOAWQIA7MzuneR_So4BsPxZcAHKda9wA4_Jv7kgcb-Kc6XQAiMJFWDGsu_-nO_oSgRD3ELj-BI_oS23VpvNi7Aa5jbgTbfmE7yezIbHnmwaML2Ehn6BsW6Q";

/// JWK `e` (publicExponent 65537 = 0x010001, base64url no padding).
pub const TEST_RSA_E_B64URL: &str = "AQAB";

/// JWT claims the smoke tests sign + serve. Mirrors the
/// fields RedDB's `OAuthValidator` reads. We model claims as a
/// JSON object so we don't pay the cost of pulling `serde_derive`
/// for a single test struct.
#[derive(Debug, Clone)]
pub struct Claims {
    pub iss: String,
    pub sub: String,
    pub aud: String,
    pub exp: i64,
    pub nbf: i64,
    pub iat: i64,
    /// Custom claim mapped onto a RedDB role.
    pub role: Option<String>,
}

impl Claims {
    fn to_json(&self) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert("iss".into(), serde_json::Value::String(self.iss.clone()));
        obj.insert("sub".into(), serde_json::Value::String(self.sub.clone()));
        obj.insert("aud".into(), serde_json::Value::String(self.aud.clone()));
        obj.insert("exp".into(), serde_json::Value::Number(self.exp.into()));
        obj.insert("nbf".into(), serde_json::Value::Number(self.nbf.into()));
        obj.insert("iat".into(), serde_json::Value::Number(self.iat.into()));
        if let Some(role) = &self.role {
            obj.insert("role".into(), serde_json::Value::String(role.clone()));
        }
        serde_json::Value::Object(obj)
    }
}

/// Mint an RS256 JWT with the test key and `KID`.
pub fn mint_rs256(claims: &Claims) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(KID.to_string());
    let key = EncodingKey::from_rsa_pem(TEST_RSA_PRIVATE_PEM.as_bytes())
        .expect("test private PEM should parse");
    jsonwebtoken::encode(&header, &claims.to_json(), &key).expect("JWT mint should succeed")
}

/// Mint an RS256 JWT with an attacker-controlled key (still valid
/// JWT shape, but signature won't verify against the test public
/// key). Used to drive the "tampered signature" negative case.
pub fn mint_rs256_with_bogus_key(claims: &Claims) -> String {
    // A different RSA-2048 keypair so the signature math is
    // identical but the resulting bytes won't match.
    const BOGUS_PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDOhMkO+gQ06qou
J6oR9LJxKAS9lAuNnDp7ohpu1Z+xRNZFsdWPjHmf4FXQIgQu/r9ic7VDaUlH0Iqj
k/6xBe4TF3dUbcZsqOKi04Lx2eaoDyiUnRrmjsvTnk6TINCUg2XJsEqU7TjMnYb6
SO1iczXTl5bb8sOBe65NzS7Dq9+x9iFbsIZ1aAYFdwKXOmVk9BtfqkPq3bxMvWAi
qnK7p3VWLhB5K5p1kqd7wpFaVqDgrXbSFJ3CAH+yBhyBN+ml/WMYOhiCOIu7MuU2
35yOWrmkGgUkX2W6h3XQiTRoZSrebhP4gfnmLA8HKjC5IrV/JUbdsJ7CzS5O7Zrn
GgUMFY9PAgMBAAECggEAD9XF1+KeFbBpZWMmJ4ITT+UIPFfiUSfAtvf//KaG4fhX
9vXKzcRgyGuW3/qtTZEsgKMEr6xJg1GUI+PN/mRJV/NkM0tj1ZSnD4qC4CfCApN9
0KRnJaMkUJZ4kZ0L9j+l0tCQVCBVmOlTlmfNk1bkLJ0V5bNOf/htEaTfRMlrjBdv
OfpOOyZuwOmt+1EKQyMpc69MmQFS9gPp2tn6+rGFZ4B+QcuPtUMvLyo9Su2jApkQ
A0Pxv8KXgPILWpZSOhStxDvCt1pbYKvITpV5yhd7nlIfJKKhwRAj5JJzbiBlHCar
RbHCp7SOaH0AlHe1eGlOe/aN4i9C2KGZD6lyAiwTeQKBgQD2Mw9PaB0ANrWtFRFp
GWpRwRiCNuJSF4HMXZ9HWlxCQzuI4WkOAjnpBSiR0Du6xKJgYOqp1zPgTk0r+7B7
BGxhf60AOe/+DeqQwI3ZyEXTvpA2XOSPb1aKWmzmIjoqUxCa4EW6h+Mnp1zMTpPx
nbSG2j+NSYvE45tlBGMAfwl9cwKBgQDWh8/CXVytgcUg5/kBZN/U4tfk6vBXMc3D
iOX4RRbbbUjtpIjxSk6KzWjzqV4WKNXp/i0rmrDBsXm7Pj4DxLRRcRLoMRJl4gHn
ux/qSL5QkRQ/Kkxs7PDxvr8Jiz+CqoBqUrZjKoGkVS0KtpnAKsiTLjL0kmNzYNKB
4ITpgzO/9QKBgFa2kwsj6xBmJNAyPN7yNX2N0fjDlTAR4Lu+jVjfTDTPGd/ZOOgA
gNoY/Hzh7fK/q7gFm1cPnlT0PE4kIM3UzUIcVRchRvwrbgRVxJggkM+qhHSRibNS
WS5lGQc+S0X+VZTrUyOSv6FsnCRJWN1kTM8yJ8C8E5YHjiQ4MVErgr7/AoGAFqe7
qOtKSk6X9Wpy9F0wxRZWgAeOdFrR3+yBI/SrFHqeFVZAdwOxr3O+OEFxtP3oN3Vd
nMl+Ol9SHt4iuKxlNVuZnEf5GYVaY8pYCYuD7B0H0bRjTHvRKDBPgHzr0sdsP8gC
3Bw43PUmPvuxNDcG/eMt/0AyKtoN2bzwLSzZWGECgYEAjfb3cAhWP/gO92ANCHg/
TrAjp2ZK+KIGu7t/5GO+TTRtb7C0WoVvqyW5OBb5LpMmvTpZflNSGu5yVaBzzlrA
b1++4p57wSGbxkNgfdFyDiWG8Pgn/FIzepuIZj8jwyqWDxZf6iOomPVKzu0Bsp3w
YHsbQGEPfAbVkqNomS3Q6fI=
-----END PRIVATE KEY-----
";
    // Try to mint with the bogus key — if for whatever reason the
    // PEM is malformed we degrade gracefully to a structural tamper
    // (flip a byte in the signature) rather than panic the test.
    if let Ok(key) = EncodingKey::from_rsa_pem(BOGUS_PRIVATE_PEM.as_bytes()) {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(KID.to_string());
        if let Ok(jwt) = jsonwebtoken::encode(&header, &claims.to_json(), &key) {
            return jwt;
        }
    }
    // Fallback: take a real JWT and tamper the signature bytes.
    let real = mint_rs256(claims);
    let mut parts: Vec<String> = real.split('.').map(String::from).collect();
    if let Some(sig) = parts.get_mut(2) {
        // Flip the first character — base64url charset stays valid.
        let mut chars = sig.chars();
        let first = chars.next().unwrap_or('A');
        let flipped = if first == 'A' { 'B' } else { 'A' };
        let rest: String = chars.collect();
        *sig = format!("{flipped}{rest}");
    }
    parts.join(".")
}

/// JSON body of the `/jwks.json` endpoint. One key, kid = `KID`,
/// alg = RS256, kty = RSA.
pub fn build_jwks() -> serde_json::Value {
    serde_json::json!({
        "keys": [
            {
                "kty": "RSA",
                "use": "sig",
                "alg": "RS256",
                "kid": KID,
                "n": TEST_RSA_N_B64URL,
                "e": TEST_RSA_E_B64URL,
            }
        ]
    })
}

/// Build the engine-side `JwtVerifier` closure. Validates the RS256
/// signature with `jsonwebtoken::DecodingKey` over the test public
/// key. The closure is `Send + Sync + 'static` as the validator
/// requires.
pub fn build_verifier() -> reddb::auth::oauth::JwtVerifier {
    let decoding_key = DecodingKey::from_rsa_pem(TEST_RSA_PUBLIC_PEM.as_bytes())
        .expect("test public PEM should parse");
    Box::new(move |_jwk, signing_input, signature| {
        // jsonwebtoken expects the compact-serialized form; we have
        // signing_input + signature as bytes, so reconstruct the
        // base64url-encoded compact form and run a low-level verify.
        // `jsonwebtoken` exposes verify for that exact shape via
        // `jsonwebtoken::crypto::verify`.
        let signing_input_str = std::str::from_utf8(signing_input)
            .map_err(|_| "signing_input not utf-8".to_string())?;
        let sig_b64 = base64_url_no_pad(signature);
        let mut validation = Validation::new(Algorithm::RS256);
        // We're only validating the signature here; the auth module
        // will recheck iss/aud/exp/nbf with `validate(...)`. Disable
        // every claim check to avoid double-rejection on time skew.
        validation.required_spec_claims.clear();
        validation.validate_aud = false;
        validation.validate_exp = false;
        validation.validate_nbf = false;
        let ok = jsonwebtoken::crypto::verify(
            &sig_b64,
            signing_input_str.as_bytes(),
            &decoding_key,
            Algorithm::RS256,
        )
        .map_err(|e| format!("RS256 verify error: {e}"))?;
        if ok {
            Ok(())
        } else {
            Err("RS256 signature did not verify".to_string())
        }
    })
}

/// Build a `Jwk` matching the test public key, ready to feed into
/// `OAuthValidator::set_jwks`. The validator looks the JWK up by
/// `kid` + `alg` before invoking the verifier — `key_bytes` is
/// opaque to the auth module so we leave it empty.
pub fn build_jwk_for_validator() -> reddb::auth::oauth::Jwk {
    reddb::auth::oauth::Jwk {
        kid: KID.to_string(),
        alg: "RS256".to_string(),
        key_bytes: Vec::new(),
    }
}

/// Current unix seconds — both validator and JWT mint share this
/// clock so happy-path tokens land within the validity window.
pub fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Standard base64url (no padding) — matches what JWTs use.
fn base64_url_no_pad(input: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    let chunks = input.chunks_exact(3);
    let rem = chunks.remainder();
    for c in chunks {
        let n = ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | (c[2] as u32);
        out.push(A[((n >> 18) & 0x3F) as usize] as char);
        out.push(A[((n >> 12) & 0x3F) as usize] as char);
        out.push(A[((n >> 6) & 0x3F) as usize] as char);
        out.push(A[(n & 0x3F) as usize] as char);
    }
    match rem {
        [a] => {
            let n = (*a as u32) << 16;
            out.push(A[((n >> 18) & 0x3F) as usize] as char);
            out.push(A[((n >> 12) & 0x3F) as usize] as char);
        }
        [a, b] => {
            let n = ((*a as u32) << 16) | ((*b as u32) << 8);
            out.push(A[((n >> 18) & 0x3F) as usize] as char);
            out.push(A[((n >> 12) & 0x3F) as usize] as char);
            out.push(A[((n >> 6) & 0x3F) as usize] as char);
        }
        _ => {}
    }
    out
}
