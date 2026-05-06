//! Shared snapshot secret redactor (issue #98).
//!
//! Every parser snapshot test in `reddb-server` flows its formatted
//! error string through `insta::assert_snapshot!`. Bad inputs may
//! contain secret-shaped substrings (bearer tokens, JWTs, conn-string
//! credential params, `sk_/rs_/reddb_` API keys). Without redaction,
//! a future test author can accidentally pin a real credential into
//! a `*.snap` file that ships in git history forever.
//!
//! This module ships an `insta::Settings` builder that installs four
//! regex-based filters covering the documented secret shapes. The
//! settings are bound to thread-local scope via `bind_to_scope()`,
//! so a test simply calls
//!
//! ```ignore
//! let _guard = secret_redactor::install_redactions();
//! insta::assert_snapshot!(name, formatted);
//! ```
//!
//! and every secret-shaped substring is replaced with a known-safe
//! placeholder before insta computes the diff.
//!
//! The matching `snapshot_redaction_lint.rs` integration test re-
//! greps every committed `*.snap` file with the same patterns and
//! fails CI when an unmasked secret slips through (e.g. because a
//! test author forgot the `install_redactions()` call).

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

/// JWT-shaped strings: three base64url segments separated by `.`.
/// Anchored to the leading `eyJ` literal so we don't swallow ordinary
/// dotted identifiers like `a.b.c`.
const JWT_PATTERN: &str = r"eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+";

/// `Bearer …` header / standalone substring. Case-insensitive on the
/// keyword so `bearer`/`BEARER` in error messages also match.
const BEARER_PATTERN: &str = r"(?i)Bearer\s+[A-Za-z0-9._\-+/=]+";

/// Conn-string credential params (`?token=…`, `&cert=…`, …). Captures
/// the leading separator + name so the redacted output keeps the
/// structure `?token=<REDACTED>` rather than a bare placeholder.
const PARAM_PATTERN: &str = r"(?i)(?P<sep>[?&])(?P<name>token|cert|key|ca)=[^&#\s]+";

/// Generic API-key shapes: `sk_/rs_/reddb_` followed by 16+ base62
/// or `_` chars. Catches OpenAI-style livemode keys plus RedDB-
/// issued tokens without false-positives on short identifiers. The
/// `_` inside the body covers the multi-segment provider shape.
const API_KEY_PATTERN: &str = r"(?:sk|rs|reddb)_[A-Za-z0-9_]{16,}";

/// Build an `insta::Settings` pre-loaded with the four redaction
/// filters. Most callers want [`install_redactions`] which binds
/// the settings to thread-local scope; this builder is exposed for
/// callers that want to layer additional filters on top.
///
/// Filter order matters: JWT runs first so a JWT carried inside a
/// Bearer header collapses to `<JWT-REDACTED>`; the bearer rule then
/// picks up any non-JWT bearer payload; param + api-key rules run
/// last and are mutually disjoint.
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
/// guard. Intended usage:
///
/// ```ignore
/// let _guard = secret_redactor::install_redactions();
/// insta::assert_snapshot!(name, formatted);
/// ```
///
/// Dropping the guard restores the previous thread-local insta
/// settings, so unrelated tests in the same binary are not
/// affected.
#[must_use = "drop the guard at the end of the test scope"]
pub fn install_redactions() -> SettingsBindDropGuard {
    settings_with_redactions().bind_to_scope()
}

/// Apply the same regex chain insta installs, but to an arbitrary
/// string. Used by the `snapshot_redaction_lint` integration test to
/// double-check committed `*.snap` files, and by the unit tests in
/// this module. Keeping the application logic next to the patterns
/// guards against drift between the live filter chain and the lint.
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

/// Return every secret-shaped substring `s` still contains after the
/// redaction filter chain has run. An empty vec means `s` is clean.
/// Used by the snapshot lint to produce a precise failure message
/// (file + offending pattern + offending substring).
///
/// Matches whose payload is a known-safe placeholder (`<REDACTED>`,
/// `<JWT-REDACTED>`, `<TOKEN-REDACTED>`) are skipped — otherwise the
/// param-pattern would re-flag every `?token=<REDACTED>` it just
/// helped produce.
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

/// Allowlist check: a match whose payload is a documented
/// placeholder is the redactor's own output, not an unmasked
/// secret. The matcher is conservative — it only excuses substrings
/// that *contain* a placeholder marker, so a sufficiently weird
/// regex coincidence still surfaces.
fn is_known_placeholder_match(matched: &str) -> bool {
    matched.contains("<REDACTED>")
        || matched.contains("<JWT-REDACTED>")
        || matched.contains("<TOKEN-REDACTED>")
}

/// One unmasked secret-shaped substring located by
/// [`find_unmasked_secrets`].
#[derive(Debug, Clone)]
pub struct UnmaskedHit {
    /// Short label naming which redactor pattern fired (e.g. `jwt`).
    pub pattern: &'static str,
    /// The exact substring the regex matched.
    pub matched: String,
    /// Byte offset within the input.
    pub offset: usize,
}

#[cfg(test)]
mod tests {
    //! Unit tests pinning each redaction pattern against a
    //! representative secret-shaped string. The `redact` helper
    //! above is the same code path the snapshot lint uses, so
    //! these tests double as a regression net for the lint.

    use super::*;
    use crate::support::parser_hardening::secret_fixture_gen as gen;

    #[test]
    fn bearer_in_url_is_masked() {
        let bearer = gen::bearer_header(0x1001);
        let input = format!("GET /v1/keys with Authorization: {}", bearer);
        let redacted = redact(&input);
        let body = bearer.split_whitespace().nth(1).expect("bearer body");
        assert!(
            redacted.contains(BEARER_PLACEHOLDER),
            "expected bearer placeholder in: {}",
            redacted
        );
        assert!(
            !redacted.contains(body),
            "raw token leaked through: {}",
            redacted
        );
        assert!(find_unmasked_secrets(&redacted).is_empty());
    }

    #[test]
    fn jwt_in_error_message_is_masked() {
        let jwt = gen::jwt(0x1002);
        let input = format!("parse error: token={}", jwt);
        let redacted = redact(&input);
        let header = jwt.split('.').next().expect("jwt header");
        assert!(
            redacted.contains(JWT_PLACEHOLDER),
            "expected jwt placeholder in: {}",
            redacted
        );
        assert!(
            !redacted.contains(header),
            "jwt header leaked: {}",
            redacted
        );
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
                "{} param not masked: {}",
                name,
                redacted
            );
        }
        assert!(
            !redacted.contains("hunter2"),
            "raw token value leaked: {}",
            redacted
        );
        assert!(find_unmasked_secrets(&redacted).is_empty());
    }

    #[test]
    fn sk_style_api_key_is_masked() {
        // Inputs assembled at runtime via `secret_fixture_gen`. No
        // literal in this file matches the redactor's regexes — the
        // policy this test enforces is the policy this test obeys.

        let sk_token = gen::api_key_token(&["sk", "live"], 24, 0x2001);
        let sk_body = sk_token.rsplit('_').next().expect("body");
        let input_sk = format!("Auth header carried {}", sk_token);
        let red_sk = redact(&input_sk);
        assert!(
            red_sk.contains(TOKEN_PLACEHOLDER),
            "sk_ not masked: {}",
            red_sk
        );
        assert!(
            !red_sk.contains(sk_body),
            "sk_ body leaked: {}",
            red_sk
        );

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
