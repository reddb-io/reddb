//! `AnswerCacheKey` — pure key derivation and TTL policy for the ASK
//! answer cache.
//!
//! Issue #403 (PRD #391): an opt-in answer cache lets ASK skip the LLM
//! when the same question lands against the same data under the same
//! determinism knobs. The cache is keyed by
//! `hash(tenant, user_scope, question, provider, model, temperature,
//! seed, sources_fingerprint)` and gated by per-query `CACHE TTL '5m'`
//! / `NOCACHE` clauses on top of deployment defaults.
//!
//! Deep module: no I/O, no clock, no storage. The caller hands in the
//! identity scope, the determinism-resolved request shape (`Applied`
//! from #400 in real wiring, plain fields here so the module stays
//! decoupled), and the source fingerprint that retrieval (#398) already
//! computes. We return a stable lowercase-hex SHA-256 key and, given
//! `Mode` + `Settings`, an effective TTL.
//!
//! ## Why the module owns these decisions
//!
//! The cache key is a security boundary: cross-tenant key collisions
//! leak answers. Pinning the canonical form here — with tests around
//! the per-tenant scope, around `Some(0)` vs `None` seed, around
//! `temperature` float canonicalisation — keeps the key derivation in
//! one place a reviewer can audit. The wiring slice that follows can
//! treat the key as an opaque string.
//!
//! ## Key canonical form
//!
//! Fields are concatenated in fixed order with the ASCII Unit Separator
//! (0x1f) as delimiter:
//!
//! ```text
//! tenant | 0x1f | user | 0x1f | question | 0x1f | provider | 0x1f
//!     | model | 0x1f | temperature | 0x1f | seed | 0x1f | fingerprint
//! ```
//!
//! - `temperature` serializes as `"none"` when absent, otherwise as the
//!   shortest round-tripping IEEE-754 representation produced by Rust's
//!   `{}` formatter (`0`, `0.5`, etc.). `0` and `none` are distinct.
//! - `seed` serializes as `"none"` when absent, otherwise as the decimal
//!   `u64`. `0` and `none` are distinct (guards against the same kind
//!   of `unwrap_or(0)` regression `DeterminismDecider` already pins).
//! - `0x1f` cannot appear in any of the inputs (SQL parser rejects it
//!   in strings; the fingerprint, provider, model, decimals, and hex
//!   are all ASCII printable), so the concatenation is injective without
//!   escaping. Same trick as [`super::determinism_decider::derive_seed`].

use std::time::Duration;

use sha2::{Digest, Sha256};

/// Identity scope. `tenant` is mandatory; `user` is empty when the
/// cache should be tenant-wide. Anonymous / embedded callers with no
/// auth context pass empty strings for both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scope<'a> {
    pub tenant: &'a str,
    pub user: &'a str,
}

/// All inputs that determine which answer a given call would receive.
/// Re-evaluating against a changed `temperature`, `seed`, `model`, or
/// `sources_fingerprint` must miss the cache, so each appears verbatim
/// in the key.
#[derive(Debug, Clone, Copy)]
pub struct Inputs<'a> {
    pub question: &'a str,
    pub provider: &'a str,
    pub model: &'a str,
    /// The temperature actually sent to the provider — i.e. what
    /// `DeterminismDecider::decide` returned, not what the user asked
    /// for.
    pub temperature: Option<f32>,
    /// The seed actually sent — same caveat as `temperature`.
    pub seed: Option<u64>,
    /// Opaque stable fingerprint over the retrieved sources (URNs +
    /// content versions). The retrieval layer (#398) owns the format.
    pub sources_fingerprint: &'a str,
}

/// Per-query `CACHE TTL '...'` / `NOCACHE` clause, parsed from the SQL
/// surface. Default-constructed `Mode` is [`Mode::Default`], which
/// means "fall back to settings".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// No per-query opinion. The effective behaviour comes from
    /// [`Settings::enabled`] / [`Settings::default_ttl`].
    Default,
    /// `ASK '...' CACHE TTL '5m'` — populate and consult the cache
    /// with this TTL regardless of the global default.
    Cache(Duration),
    /// `ASK '...' NOCACHE` — bypass the cache entirely on this call.
    NoCache,
}

impl Default for Mode {
    fn default() -> Self {
        Mode::Default
    }
}

/// Deployment-level cache settings, surfaced via `ask.cache.*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// `ask.cache.enabled` (default `false`).
    pub enabled: bool,
    /// `ask.cache.default_ttl`. `None` means "no default TTL"; queries
    /// must opt in with `CACHE TTL '...'` to populate the cache.
    pub default_ttl: Option<Duration>,
    /// `ask.cache.max_entries`. Not consulted here — the eviction
    /// policy lives in the cache store. Exposed for completeness.
    pub max_entries: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            default_ttl: None,
            max_entries: 0,
        }
    }
}

/// What the cache wrapper should do for a single ASK call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Skip the cache entirely (do not read, do not write).
    Bypass,
    /// Consult the cache; on miss, populate with `ttl`.
    Use { ttl: Duration },
}

/// Combine the per-query [`Mode`] with deployment [`Settings`] to get
/// the effective behaviour for this call.
///
/// Rules:
/// - `NOCACHE` always wins (explicit user opt-out).
/// - `CACHE TTL t` always wins when present (explicit user opt-in;
///   the deployment toggle does NOT gate per-query opt-in, only the
///   silent default).
/// - `Default` + `enabled=true` + `default_ttl=Some(t)` → use, ttl=t.
/// - `Default` + anything else → bypass.
pub fn decide(mode: Mode, settings: Settings) -> Decision {
    match mode {
        Mode::NoCache => Decision::Bypass,
        Mode::Cache(ttl) => Decision::Use { ttl },
        Mode::Default => match (settings.enabled, settings.default_ttl) {
            (true, Some(ttl)) => Decision::Use { ttl },
            _ => Decision::Bypass,
        },
    }
}

/// Derive the lowercase-hex SHA-256 cache key for one ASK call.
///
/// The key is a function of identity scope + request-shape inputs. It
/// does NOT include the TTL — two calls with the same identity and
/// shape collide on the same entry regardless of how long that entry
/// will live, which is the correct hit/miss semantic.
pub fn derive_key(scope: Scope<'_>, inputs: Inputs<'_>) -> String {
    const SEP: u8 = 0x1f;
    let mut hasher = Sha256::new();
    hasher.update(scope.tenant.as_bytes());
    hasher.update([SEP]);
    hasher.update(scope.user.as_bytes());
    hasher.update([SEP]);
    hasher.update(inputs.question.as_bytes());
    hasher.update([SEP]);
    hasher.update(inputs.provider.as_bytes());
    hasher.update([SEP]);
    hasher.update(inputs.model.as_bytes());
    hasher.update([SEP]);
    hasher.update(format_temperature(inputs.temperature).as_bytes());
    hasher.update([SEP]);
    hasher.update(format_seed(inputs.seed).as_bytes());
    hasher.update([SEP]);
    hasher.update(inputs.sources_fingerprint.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn format_temperature(t: Option<f32>) -> String {
    match t {
        None => "none".to_string(),
        Some(v) => format!("{v}"),
    }
}

fn format_seed(s: Option<u64>) -> String {
    match s {
        None => "none".to_string(),
        Some(v) => v.to_string(),
    }
}

/// Parse a TTL literal from `CACHE TTL '<lit>'`.
///
/// Accepts `<integer><unit>` with units `s` (seconds), `m` (minutes),
/// `h` (hours), `d` (days). Whitespace is not allowed. The integer
/// must be > 0; a zero TTL would mean "expire immediately" which is a
/// foot-gun the parser refuses on the user's behalf.
pub fn parse_ttl(literal: &str) -> Result<Duration, TtlParseError> {
    if literal.is_empty() {
        return Err(TtlParseError::Empty);
    }
    let bytes = literal.as_bytes();
    let unit_idx = bytes
        .iter()
        .position(|b| !b.is_ascii_digit())
        .ok_or(TtlParseError::MissingUnit)?;
    if unit_idx == 0 {
        return Err(TtlParseError::MissingNumber);
    }
    let (num_part, unit_part) = literal.split_at(unit_idx);
    let n: u64 = num_part.parse().map_err(|_| TtlParseError::InvalidNumber)?;
    if n == 0 {
        return Err(TtlParseError::ZeroTtl);
    }
    let secs = match unit_part {
        "s" => n,
        "m" => n.checked_mul(60).ok_or(TtlParseError::Overflow)?,
        "h" => n.checked_mul(3600).ok_or(TtlParseError::Overflow)?,
        "d" => n.checked_mul(86_400).ok_or(TtlParseError::Overflow)?,
        _ => return Err(TtlParseError::UnknownUnit),
    };
    Ok(Duration::from_secs(secs))
}

/// Why [`parse_ttl`] rejected a literal. Named variants so the runtime
/// can map each to a deterministic error message without a stringly
/// typed switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtlParseError {
    Empty,
    MissingNumber,
    MissingUnit,
    InvalidNumber,
    UnknownUnit,
    ZeroTtl,
    Overflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope<'static> {
        Scope {
            tenant: "acme",
            user: "alice",
        }
    }

    fn inputs() -> Inputs<'static> {
        Inputs {
            question: "what is the capital of france?",
            provider: "openai",
            model: "gpt-4o-mini",
            temperature: Some(0.0),
            seed: Some(42),
            sources_fingerprint: "abc123",
        }
    }

    // ---- key: determinism & scope separation -------------------------

    #[test]
    fn key_is_deterministic_across_calls() {
        let k1 = derive_key(scope(), inputs());
        let k2 = derive_key(scope(), inputs());
        assert_eq!(k1, k2);
        // sha256 hex is 64 chars.
        assert_eq!(k1.len(), 64);
        assert!(k1
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    #[test]
    fn key_changes_with_tenant() {
        let a = derive_key(
            Scope {
                tenant: "acme",
                user: "alice",
            },
            inputs(),
        );
        let b = derive_key(
            Scope {
                tenant: "globex",
                user: "alice",
            },
            inputs(),
        );
        assert_ne!(a, b, "per-tenant scope must isolate cache keys");
    }

    #[test]
    fn key_changes_with_user() {
        let a = derive_key(
            Scope {
                tenant: "acme",
                user: "alice",
            },
            inputs(),
        );
        let b = derive_key(
            Scope {
                tenant: "acme",
                user: "bob",
            },
            inputs(),
        );
        assert_ne!(a, b);
    }

    #[test]
    fn empty_user_is_distinct_from_named_user() {
        let anon = derive_key(
            Scope {
                tenant: "acme",
                user: "",
            },
            inputs(),
        );
        let named = derive_key(scope(), inputs());
        assert_ne!(anon, named);
    }

    // ---- key: every input field actually feeds the digest ------------

    #[test]
    fn key_changes_with_question() {
        let mut i = inputs();
        let base = derive_key(scope(), i);
        i.question = "different question";
        let other = derive_key(scope(), i);
        assert_ne!(base, other);
    }

    #[test]
    fn key_changes_with_provider() {
        let mut i = inputs();
        let base = derive_key(scope(), i);
        i.provider = "anthropic";
        let other = derive_key(scope(), i);
        assert_ne!(base, other);
    }

    #[test]
    fn key_changes_with_model() {
        let mut i = inputs();
        let base = derive_key(scope(), i);
        i.model = "gpt-4o";
        let other = derive_key(scope(), i);
        assert_ne!(base, other);
    }

    #[test]
    fn key_changes_with_temperature() {
        let mut i = inputs();
        let base = derive_key(scope(), i);
        i.temperature = Some(0.7);
        let other = derive_key(scope(), i);
        assert_ne!(base, other);
    }

    #[test]
    fn key_changes_with_seed() {
        let mut i = inputs();
        let base = derive_key(scope(), i);
        i.seed = Some(43);
        let other = derive_key(scope(), i);
        assert_ne!(base, other);
    }

    #[test]
    fn key_changes_with_fingerprint() {
        let mut i = inputs();
        let base = derive_key(scope(), i);
        i.sources_fingerprint = "def456";
        let other = derive_key(scope(), i);
        assert_ne!(
            base, other,
            "different sources must miss cache even for identical question"
        );
    }

    // ---- key: None vs Some(0) for optional knobs ---------------------

    #[test]
    fn temperature_none_distinct_from_zero() {
        let mut i = inputs();
        i.temperature = None;
        let none = derive_key(scope(), i);
        i.temperature = Some(0.0);
        let zero = derive_key(scope(), i);
        assert_ne!(
            none, zero,
            "None and Some(0.0) must not collide — a provider that ignores temperature is not the same as one that received zero"
        );
    }

    #[test]
    fn seed_none_distinct_from_zero() {
        let mut i = inputs();
        i.seed = None;
        let none = derive_key(scope(), i);
        i.seed = Some(0);
        let zero = derive_key(scope(), i);
        assert_ne!(none, zero);
    }

    // ---- key: pin the canonical form against accidental change ------

    #[test]
    fn key_pinned_against_known_value() {
        // If the canonical form ever changes (delimiter, field order,
        // float/seed serialization), this test will fail loudly. Update
        // the literal only on a deliberate schema bump and bump
        // ask.cache.max_entries-style call sites accordingly.
        let scope = Scope {
            tenant: "t",
            user: "u",
        };
        let i = Inputs {
            question: "q",
            provider: "p",
            model: "m",
            temperature: Some(0.0),
            seed: Some(1),
            sources_fingerprint: "f",
        };
        let key = derive_key(scope, i);
        // Computed by `printf 't\x1fu\x1fq\x1fp\x1fm\x1f0\x1f1\x1ff' | sha256sum`.
        assert_eq!(
            key,
            "ca47974209a1e07b9890aa73b5bdbcc2fda1bae0ba1d77f186c9dc168b54f903"
        );
    }

    // ---- decide(): TTL policy ---------------------------------------

    #[test]
    fn decide_nocache_always_bypasses() {
        let s = Settings {
            enabled: true,
            default_ttl: Some(Duration::from_secs(60)),
            max_entries: 100,
        };
        assert_eq!(decide(Mode::NoCache, s), Decision::Bypass);
    }

    #[test]
    fn decide_per_query_cache_wins_over_disabled_setting() {
        let s = Settings::default();
        assert_eq!(
            decide(Mode::Cache(Duration::from_secs(300)), s),
            Decision::Use {
                ttl: Duration::from_secs(300)
            }
        );
    }

    #[test]
    fn decide_default_bypass_when_disabled() {
        let s = Settings {
            enabled: false,
            default_ttl: Some(Duration::from_secs(60)),
            max_entries: 100,
        };
        assert_eq!(decide(Mode::Default, s), Decision::Bypass);
    }

    #[test]
    fn decide_default_bypass_when_no_default_ttl() {
        let s = Settings {
            enabled: true,
            default_ttl: None,
            max_entries: 100,
        };
        assert_eq!(decide(Mode::Default, s), Decision::Bypass);
    }

    #[test]
    fn decide_default_uses_setting_ttl_when_enabled_and_ttl_set() {
        let s = Settings {
            enabled: true,
            default_ttl: Some(Duration::from_secs(120)),
            max_entries: 100,
        };
        assert_eq!(
            decide(Mode::Default, s),
            Decision::Use {
                ttl: Duration::from_secs(120)
            }
        );
    }

    #[test]
    fn decide_per_query_cache_overrides_setting_default() {
        let s = Settings {
            enabled: true,
            default_ttl: Some(Duration::from_secs(60)),
            max_entries: 100,
        };
        assert_eq!(
            decide(Mode::Cache(Duration::from_secs(900)), s),
            Decision::Use {
                ttl: Duration::from_secs(900)
            }
        );
    }

    // ---- parse_ttl() ------------------------------------------------

    #[test]
    fn parse_ttl_seconds() {
        assert_eq!(parse_ttl("30s").unwrap(), Duration::from_secs(30));
    }

    #[test]
    fn parse_ttl_minutes() {
        assert_eq!(parse_ttl("5m").unwrap(), Duration::from_secs(300));
    }

    #[test]
    fn parse_ttl_hours() {
        assert_eq!(parse_ttl("2h").unwrap(), Duration::from_secs(7200));
    }

    #[test]
    fn parse_ttl_days() {
        assert_eq!(parse_ttl("1d").unwrap(), Duration::from_secs(86_400));
    }

    #[test]
    fn parse_ttl_empty_rejected() {
        assert_eq!(parse_ttl(""), Err(TtlParseError::Empty));
    }

    #[test]
    fn parse_ttl_zero_rejected() {
        // 0s is a foot-gun: an entry that expires the instant it's
        // written. Refuse it so misconfiguration shows up at parse time.
        assert_eq!(parse_ttl("0s"), Err(TtlParseError::ZeroTtl));
    }

    #[test]
    fn parse_ttl_missing_unit_rejected() {
        assert_eq!(parse_ttl("30"), Err(TtlParseError::MissingUnit));
    }

    #[test]
    fn parse_ttl_missing_number_rejected() {
        assert_eq!(parse_ttl("m"), Err(TtlParseError::MissingNumber));
    }

    #[test]
    fn parse_ttl_unknown_unit_rejected() {
        assert_eq!(parse_ttl("5x"), Err(TtlParseError::UnknownUnit));
        assert_eq!(parse_ttl("5ms"), Err(TtlParseError::UnknownUnit));
    }

    #[test]
    fn parse_ttl_whitespace_rejected() {
        // The SQL surface strips quotes already; we should not be
        // lenient about embedded whitespace inside the literal.
        assert_eq!(parse_ttl("5 m"), Err(TtlParseError::UnknownUnit));
        assert_eq!(parse_ttl(" 5m"), Err(TtlParseError::MissingNumber));
    }

    #[test]
    fn parse_ttl_negative_rejected() {
        // Leading '-' is not a digit, so position(!is_ascii_digit) =
        // 0 → MissingNumber. Pinned for clarity.
        assert_eq!(parse_ttl("-5m"), Err(TtlParseError::MissingNumber));
    }

    #[test]
    fn parse_ttl_invalid_number_rejected() {
        // u64 overflow at the integer parse step.
        assert_eq!(
            parse_ttl("99999999999999999999s"),
            Err(TtlParseError::InvalidNumber)
        );
    }

    #[test]
    fn parse_ttl_overflow_on_unit_multiplication() {
        // Large number that fits in u64 but overflows once multiplied
        // by 86_400.
        let max_d = u64::MAX / 86_400 + 1;
        let lit = format!("{}d", max_d);
        assert_eq!(parse_ttl(&lit), Err(TtlParseError::Overflow));
    }

    // ---- mode default ----------------------------------------------

    #[test]
    fn mode_default_is_inherit() {
        assert_eq!(Mode::default(), Mode::Default);
    }

    // ---- determinism across modes ----------------------------------

    #[test]
    fn decide_is_deterministic_across_calls() {
        let s = Settings {
            enabled: true,
            default_ttl: Some(Duration::from_secs(60)),
            max_entries: 10,
        };
        for mode in [
            Mode::Default,
            Mode::NoCache,
            Mode::Cache(Duration::from_secs(120)),
        ] {
            let d1 = decide(mode, s);
            let d2 = decide(mode, s);
            assert_eq!(d1, d2);
        }
    }
}
