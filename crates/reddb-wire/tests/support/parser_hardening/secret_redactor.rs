//! Slim wire-side snapshot secret redactor (issue #98).
//!
//! Mirror of `crates/reddb-server/tests/support/parser_hardening/
//! secret_redactor.rs`. The wire crate's parser-hardening harness is
//! a deliberate duplicate of the server-side one (#90 keeps the
//! crates' test trees independent), so the redactor lives here too.
//!
//! Patterns and placeholders MUST stay byte-for-byte identical to
//! the server-side module — both versions feed the same lint test
//! that walks every `*.snap` file in the workspace. If you tweak a
//! pattern here, mirror the change in the server crate (and vice
//! versa) before merging.
//!
//! Usage from a wire-side snapshot test:
//!
//! ```ignore
//! let _guard = secret_redactor::install_redactions();
//! insta::assert_snapshot!(name, formatted);
//! ```

#![allow(dead_code)]

use insta::internals::SettingsBindDropGuard;
use insta::Settings;

/// Placeholder used for `sk_/rs_/reddb_`-prefixed API keys (and any
/// future generic-secret pattern that lacks its own placeholder).
pub const TOKEN_PLACEHOLDER: &str = "<TOKEN-REDACTED>";

/// Placeholder used for JWT-shaped strings (three base64url
/// segments separated by `.`).
pub const JWT_PLACEHOLDER: &str = "<JWT-REDACTED>";

/// Placeholder used for the body of an `Authorization: Bearer …`
/// header or any standalone `Bearer …` substring.
pub const BEARER_PLACEHOLDER: &str = "Bearer <REDACTED>";

/// Placeholder used for the value of a credential-bearing
/// connection-string query parameter (`token=`, `cert=`, `key=`,
/// `ca=`).
pub const PARAM_PLACEHOLDER: &str = "<REDACTED>";

const JWT_PATTERN: &str = r"eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+";
const BEARER_PATTERN: &str = r"(?i)Bearer\s+[A-Za-z0-9._\-+/=]+";
const PARAM_PATTERN: &str = r"(?i)(?P<sep>[?&])(?P<name>token|cert|key|ca)=[^&#\s]+";
const API_KEY_PATTERN: &str = r"(?:sk|rs|reddb)_[A-Za-z0-9_]{16,}";

/// Build an `insta::Settings` pre-loaded with the four redaction
/// filters. Filter order matches the server-side module.
pub fn settings_with_redactions() -> Settings {
    let mut settings = Settings::clone_current();

    settings.add_filter(JWT_PATTERN, JWT_PLACEHOLDER);
    settings.add_filter(BEARER_PATTERN, BEARER_PLACEHOLDER);
    settings.add_filter(
        PARAM_PATTERN,
        format!("$sep$name={}", PARAM_PLACEHOLDER).as_str(),
    );
    settings.add_filter(API_KEY_PATTERN, TOKEN_PLACEHOLDER);

    settings
}

/// Install the redaction filters for the lifetime of the returned
/// guard. Drop the guard at the end of the test scope.
#[must_use = "drop the guard at the end of the test scope"]
pub fn install_redactions() -> SettingsBindDropGuard {
    settings_with_redactions().bind_to_scope()
}

/// Apply the same regex chain insta installs, but to an arbitrary
/// string. Used by the wire-side snapshot lint and by the unit
/// tests below.
pub fn redact(input: &str) -> String {
    use regex::Regex;
    let mut out = input.to_string();

    let jwt = Regex::new(JWT_PATTERN).expect("jwt regex compiles");
    out = jwt.replace_all(&out, JWT_PLACEHOLDER).into_owned();

    let bearer = Regex::new(BEARER_PATTERN).expect("bearer regex compiles");
    out = bearer.replace_all(&out, BEARER_PLACEHOLDER).into_owned();

    let param = Regex::new(PARAM_PATTERN).expect("param regex compiles");
    let param_replacement = format!("$sep$name={}", PARAM_PLACEHOLDER);
    out = param
        .replace_all(&out, param_replacement.as_str())
        .into_owned();

    let api = Regex::new(API_KEY_PATTERN).expect("api-key regex compiles");
    out = api.replace_all(&out, TOKEN_PLACEHOLDER).into_owned();

    out
}

/// Return every secret-shaped substring `s` still contains after
/// the redaction filter chain. Empty vec means clean. Matches whose
/// payload is a documented placeholder are skipped so the lint
/// doesn't re-flag the redactor's own output.
pub fn find_unmasked_secrets(s: &str) -> Vec<UnmaskedHit> {
    use regex::Regex;
    let patterns: &[(&str, &str)] = &[
        ("jwt", JWT_PATTERN),
        ("bearer", BEARER_PATTERN),
        ("conn-string-credential-param", PARAM_PATTERN),
        ("api-key", API_KEY_PATTERN),
    ];

    let mut hits = Vec::new();
    for (label, pat) in patterns {
        let re = Regex::new(pat).expect("redactor regex compiles");
        for m in re.find_iter(s) {
            if is_known_placeholder_match(m.as_str()) {
                continue;
            }
            hits.push(UnmaskedHit {
                pattern: label,
                matched: m.as_str().to_string(),
                offset: m.start(),
            });
        }
    }
    hits
}

fn is_known_placeholder_match(matched: &str) -> bool {
    matched.contains("<REDACTED>")
        || matched.contains("<JWT-REDACTED>")
        || matched.contains("<TOKEN-REDACTED>")
}

#[derive(Debug, Clone)]
pub struct UnmaskedHit {
    pub pattern: &'static str,
    pub matched: String,
    pub offset: usize,
}

#[cfg(test)]
mod tests {
    //! Wire-side unit tests pinning each redaction pattern. Mirror
    //! of the server-side suite — keep the four `_is_masked` test
    //! names identical so a regression in either crate trips a
    //! recognisable failure.

    use super::*;
    use crate::support::parser_hardening::secret_fixture_gen as gen;

    #[test]
    fn bearer_in_url_is_masked() {
        let bearer = gen::bearer_header(0x1001);
        let input = format!("GET /v1/keys with Authorization: {}", bearer);
        let redacted = redact(&input);
        let body = bearer.split_whitespace().nth(1).expect("bearer body");
        assert!(redacted.contains(BEARER_PLACEHOLDER), "got: {}", redacted);
        assert!(!redacted.contains(body), "leaked: {}", redacted);
        assert!(find_unmasked_secrets(&redacted).is_empty());
    }

    #[test]
    fn jwt_in_error_message_is_masked() {
        let jwt = gen::jwt(0x1002);
        let input = format!("parse error: token={}", jwt);
        let redacted = redact(&input);
        let header = jwt.split('.').next().expect("jwt header");
        assert!(redacted.contains(JWT_PLACEHOLDER), "got: {}", redacted);
        assert!(!redacted.contains(header), "leaked: {}", redacted);
        assert!(find_unmasked_secrets(&redacted).is_empty());
    }

    #[test]
    fn conn_string_token_param_is_masked() {
        let input =
            "red://primary.svc:5050?token=hunter2&cert=/etc/ssl/cert.pem&key=secretkey&ca=root";
        let redacted = redact(input);
        for name in ["token", "cert", "key", "ca"] {
            assert!(
                redacted.contains(&format!("{}={}", name, PARAM_PLACEHOLDER)),
                "{} not masked: {}",
                name,
                redacted
            );
        }
        assert!(!redacted.contains("hunter2"), "leaked: {}", redacted);
        assert!(find_unmasked_secrets(&redacted).is_empty());
    }

    #[test]
    fn sk_style_api_key_is_masked() {
        let sk_token = gen::api_key_token(&["sk", "live"], 24, 0x2001);
        let sk_body = sk_token.rsplit('_').next().expect("body");
        let input_sk = format!("Auth header carried {}", sk_token);
        let red_sk = redact(&input_sk);
        assert!(
            red_sk.contains(TOKEN_PLACEHOLDER),
            "sk_ not masked: {}",
            red_sk
        );
        assert!(!red_sk.contains(sk_body), "leaked: {}", red_sk);

        let rs_token = gen::api_key_token(&["rs"], 22, 0x2002);
        let input_rs = format!("issued {} for tenant", rs_token);
        let red_rs = redact(&input_rs);
        assert!(
            red_rs.contains(TOKEN_PLACEHOLDER),
            "rs_ not masked: {}",
            red_rs
        );

        let reddb_token = gen::api_key_token(&["reddb"], 20, 0x2003);
        let input_reddb = format!("key {}", reddb_token);
        let red_reddb = redact(&input_reddb);
        assert!(
            red_reddb.contains(TOKEN_PLACEHOLDER),
            "reddb_ not masked: {}",
            red_reddb
        );

        assert!(find_unmasked_secrets(&red_sk).is_empty());
        assert!(find_unmasked_secrets(&red_rs).is_empty());
        assert!(find_unmasked_secrets(&red_reddb).is_empty());
    }
}
