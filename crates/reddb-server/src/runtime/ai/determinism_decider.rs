//! `DeterminismDecider` — pure resolution of `temperature` + `seed`.
//!
//! Issue #400 (PRD #391): every ASK call must default to
//! `temperature = 0` and a `seed` derived from
//! `hash(question + sources_fingerprint)` so that the same question
//! against the same data produces the same answer (within provider
//! determinism guarantees). Per-query `ASK '...' TEMPERATURE x SEED n`
//! overrides win, and providers that don't honor a given knob silently
//! drop it.
//!
//! This is a deep module: no I/O, no transport, no clock. Inputs are
//! plain data; the output is the pair of parameters the caller should
//! actually send to the provider, plus the values to record in the
//! audit row. The caller is responsible for:
//!
//! - threading [`Applied::temperature`] / [`Applied::seed`] into the
//!   provider request,
//! - writing what was *actually* applied (not what was requested) into
//!   `red_ask_audit` per AC.
//!
//! ## Policy
//!
//! Temperature:
//! - Per-query override always wins, when the provider accepts a
//!   temperature at all.
//! - Otherwise the value comes from `Settings::default_temperature`
//!   (which itself defaults to `0.0` upstream).
//! - If the provider does NOT accept a temperature (raw inference
//!   endpoints like the embedded `local` backend; see
//!   [`Capabilities::supports_temperature_zero`] in #396), the
//!   temperature is dropped to `None` regardless of override, since
//!   sending it would break the call.
//!
//! Seed:
//! - Per-query override wins for providers that honor `seed`.
//! - Otherwise the seed is derived deterministically from
//!   `sha256(question || 0x1f || sources_fingerprint)`, taking the
//!   first 8 bytes as a little-endian `u64`. Same question + same
//!   data ⇒ same seed, every call, every process.
//! - Providers that don't honor seed (Anthropic, HuggingFace, `local`,
//!   `custom`) drop the seed to `None`.
//!
//! ## Why this lives in its own module
//!
//! Spreading temperature/seed defaulting across the request builders
//! produced subtle bugs the first time we tried it: the audit row
//! recorded a seed the provider had silently ignored, and a single
//! `Option::unwrap_or` slipped past the capability check. Centralising
//! the decision (and pinning every branch under unit test) makes the
//! audit row trustworthy and lets the determinism contract be reasoned
//! about in isolation.

use sha2::{Digest, Sha256};

use crate::runtime::ai::provider_capabilities::Capabilities;

/// Per-query overrides parsed from `ASK '...' TEMPERATURE x SEED n`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Overrides {
    pub temperature: Option<f32>,
    pub seed: Option<u64>,
}

/// Deployment-level defaults. `default_temperature` comes from the
/// `ask.default_temperature` setting (default `0.0`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    pub default_temperature: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            default_temperature: 0.0,
        }
    }
}

/// Pure inputs to the deciding function.
#[derive(Debug, Clone, Copy)]
pub struct Inputs<'a> {
    pub question: &'a str,
    /// Stable hash over the URNs and content versions of the retrieved
    /// sources. The decider does not recompute this — the retrieval
    /// layer owns the fingerprint format and passes the bytes/string
    /// down. Treating it as opaque keeps the determinism contract
    /// independent of the source schema.
    pub sources_fingerprint: &'a str,
}

/// What the caller should actually send to the provider.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Applied {
    pub temperature: Option<f32>,
    pub seed: Option<u64>,
}

/// Resolve effective `temperature` and `seed` for a single ASK call.
///
/// `caps` should be the capabilities row for the *target* provider —
/// usually [`super::provider_capabilities::Registry::capabilities_for`].
pub fn decide(
    inputs: Inputs<'_>,
    caps: Capabilities,
    overrides: Overrides,
    settings: Settings,
) -> Applied {
    let temperature =
        resolve_temperature(caps, overrides.temperature, settings.default_temperature);
    let seed = resolve_seed(caps, overrides.seed, inputs);
    Applied { temperature, seed }
}

fn resolve_temperature(caps: Capabilities, override_t: Option<f32>, default_t: f32) -> Option<f32> {
    if !caps.supports_temperature_zero {
        // Provider takes no temperature at all (e.g. embedded `local`).
        // Sending one would be a request error.
        return None;
    }
    Some(override_t.unwrap_or(default_t))
}

fn resolve_seed(caps: Capabilities, override_s: Option<u64>, inputs: Inputs<'_>) -> Option<u64> {
    if !caps.supports_seed {
        return None;
    }
    if let Some(s) = override_s {
        return Some(s);
    }
    Some(derive_seed(inputs.question, inputs.sources_fingerprint))
}

/// `sha256(question || 0x1f || fingerprint) -> u64 little-endian`.
///
/// 0x1f (ASCII US, "unit separator") is used as the field delimiter
/// because it cannot appear in a question (the SQL parser would have
/// rejected it) or in a hex fingerprint, so the concatenation is
/// injective without escaping.
pub fn derive_seed(question: &str, sources_fingerprint: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(question.as_bytes());
    hasher.update([0x1f]);
    hasher.update(sources_fingerprint.as_bytes());
    let digest = hasher.finalize();
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&digest[..8]);
    u64::from_le_bytes(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_caps() -> Capabilities {
        // OpenAI-like: everything on.
        Capabilities {
            supports_citations: true,
            supports_seed: true,
            supports_temperature_zero: true,
            supports_streaming: true,
        }
    }

    fn no_seed_caps() -> Capabilities {
        // Anthropic-like: temperature ok, no seed.
        Capabilities {
            supports_citations: true,
            supports_seed: false,
            supports_temperature_zero: true,
            supports_streaming: true,
        }
    }

    fn no_temp_caps() -> Capabilities {
        // `local`-like: no temperature, no seed.
        Capabilities {
            supports_citations: false,
            supports_seed: false,
            supports_temperature_zero: false,
            supports_streaming: false,
        }
    }

    fn inputs() -> Inputs<'static> {
        Inputs {
            question: "what is the meaning of life?",
            sources_fingerprint: "abc123",
        }
    }

    // ---- defaults ----------------------------------------------------

    #[test]
    fn default_temperature_is_zero() {
        let out = decide(
            inputs(),
            full_caps(),
            Overrides::default(),
            Settings::default(),
        );
        assert_eq!(out.temperature, Some(0.0));
    }

    #[test]
    fn default_seed_is_derived_from_question_and_fingerprint() {
        let out = decide(
            inputs(),
            full_caps(),
            Overrides::default(),
            Settings::default(),
        );
        let expected = derive_seed(inputs().question, inputs().sources_fingerprint);
        assert_eq!(out.seed, Some(expected));
    }

    // ---- overrides ---------------------------------------------------

    #[test]
    fn temperature_override_wins_over_default() {
        let out = decide(
            inputs(),
            full_caps(),
            Overrides {
                temperature: Some(0.7),
                seed: None,
            },
            Settings::default(),
        );
        assert_eq!(out.temperature, Some(0.7));
    }

    #[test]
    fn seed_override_wins_over_derivation() {
        let out = decide(
            inputs(),
            full_caps(),
            Overrides {
                temperature: None,
                seed: Some(42),
            },
            Settings::default(),
        );
        assert_eq!(out.seed, Some(42));
    }

    #[test]
    fn settings_default_temperature_honored_when_no_override() {
        let out = decide(
            inputs(),
            full_caps(),
            Overrides::default(),
            Settings {
                default_temperature: 0.3,
            },
        );
        assert_eq!(out.temperature, Some(0.3));
    }

    #[test]
    fn override_temperature_beats_settings_default() {
        let out = decide(
            inputs(),
            full_caps(),
            Overrides {
                temperature: Some(0.9),
                seed: None,
            },
            Settings {
                default_temperature: 0.3,
            },
        );
        assert_eq!(out.temperature, Some(0.9));
    }

    // ---- capability gating -------------------------------------------

    #[test]
    fn no_seed_capability_drops_derived_seed() {
        let out = decide(
            inputs(),
            no_seed_caps(),
            Overrides::default(),
            Settings::default(),
        );
        assert_eq!(out.seed, None);
        // Temperature still present.
        assert_eq!(out.temperature, Some(0.0));
    }

    #[test]
    fn no_seed_capability_drops_override_seed_too() {
        // Audit row must reflect what the provider got, not what the
        // caller asked for — passing `SEED 42` to Anthropic does
        // nothing, so the decider drops it.
        let out = decide(
            inputs(),
            no_seed_caps(),
            Overrides {
                temperature: None,
                seed: Some(42),
            },
            Settings::default(),
        );
        assert_eq!(out.seed, None);
    }

    #[test]
    fn no_temperature_capability_drops_temperature() {
        let out = decide(
            inputs(),
            no_temp_caps(),
            Overrides::default(),
            Settings::default(),
        );
        assert_eq!(out.temperature, None);
        assert_eq!(out.seed, None);
    }

    #[test]
    fn no_temperature_capability_drops_override_temperature() {
        // Even an explicit `TEMPERATURE 0.7` is dropped if the
        // provider's endpoint takes no temperature parameter.
        let out = decide(
            inputs(),
            no_temp_caps(),
            Overrides {
                temperature: Some(0.7),
                seed: None,
            },
            Settings::default(),
        );
        assert_eq!(out.temperature, None);
    }

    #[test]
    fn conservative_capabilities_drop_seed_keep_temperature() {
        // Per #396, `Capabilities::conservative()` is: citations off,
        // seed off, temperature_zero on, streaming off. The decider
        // should send temperature but not seed.
        let out = decide(
            inputs(),
            Capabilities::conservative(),
            Overrides::default(),
            Settings::default(),
        );
        assert_eq!(out.temperature, Some(0.0));
        assert_eq!(out.seed, None);
    }

    // ---- determinism contract ----------------------------------------

    #[test]
    fn derive_seed_is_deterministic_across_calls() {
        let a = derive_seed("question", "fp");
        let b = derive_seed("question", "fp");
        assert_eq!(a, b);
    }

    #[test]
    fn derive_seed_differs_on_question_change() {
        let a = derive_seed("question A", "fp");
        let b = derive_seed("question B", "fp");
        assert_ne!(a, b);
    }

    #[test]
    fn derive_seed_differs_on_fingerprint_change() {
        // Same question, different data → different seed. This is the
        // load-bearing guarantee of #400: a row insert changes the
        // fingerprint and the next ASK call computes a new seed even
        // without the cache layer.
        let a = derive_seed("question", "fp1");
        let b = derive_seed("question", "fp2");
        assert_ne!(a, b);
    }

    #[test]
    fn derive_seed_is_injective_across_field_boundary() {
        // Without the 0x1f separator, `("ab", "c")` and `("a", "bc")`
        // would collide. Pin that they don't.
        let a = derive_seed("ab", "c");
        let b = derive_seed("a", "bc");
        assert_ne!(a, b);
    }

    #[test]
    fn decide_is_deterministic_across_calls() {
        let a = decide(
            inputs(),
            full_caps(),
            Overrides::default(),
            Settings::default(),
        );
        let b = decide(
            inputs(),
            full_caps(),
            Overrides::default(),
            Settings::default(),
        );
        assert_eq!(a, b);
    }

    // ---- audit-shape sanity ------------------------------------------

    #[test]
    fn applied_carries_both_knobs_when_provider_supports_both() {
        // Audit row reads from `Applied` directly; both fields must
        // populate when capabilities permit.
        let out = decide(
            inputs(),
            full_caps(),
            Overrides::default(),
            Settings::default(),
        );
        assert!(out.temperature.is_some());
        assert!(out.seed.is_some());
    }

    #[test]
    fn override_zero_temperature_is_preserved_not_treated_as_missing() {
        // f32 0.0 is a valid override — must not be confused with
        // "no override". Guards against an `unwrap_or(0.0)` regression
        // where the override and the default would be indistinguishable.
        let out = decide(
            inputs(),
            full_caps(),
            Overrides {
                temperature: Some(0.0),
                seed: None,
            },
            Settings {
                default_temperature: 0.5,
            },
        );
        assert_eq!(out.temperature, Some(0.0));
    }

    #[test]
    fn override_zero_seed_is_preserved() {
        // u64 0 is a legal seed; treating `Some(0)` as "no override"
        // would be a subtle bug.
        let out = decide(
            inputs(),
            full_caps(),
            Overrides {
                temperature: None,
                seed: Some(0),
            },
            Settings::default(),
        );
        assert_eq!(out.seed, Some(0));
    }
}
