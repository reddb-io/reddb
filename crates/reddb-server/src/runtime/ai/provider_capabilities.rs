//! `ProviderCapabilityRegistry` — pure provider capability lookup.
//!
//! Issue #396 (PRD #391): which LLM providers can reliably honor the
//! strict citation contract (#395), the deterministic seed (#400),
//! `temperature=0`, and streaming responses (#405)?
//!
//! This is a deep module: no I/O, no transport, no LLM calls. Given a
//! provider token (e.g. `"openai"`, `"ollama"`) and a caller-requested
//! [`Mode`] (strict / lenient), it returns a [`ModeOutcome`] saying
//! either "go ahead" or "the caller asked for strict but this provider
//! can't honor it — fall back to lenient and surface a warning".
//!
//! The caller is responsible for:
//! - actually surfacing the [`ModeWarning`] in the response envelope,
//! - recording the *effective* mode (not the requested one) in the
//!   audit row.
//!
//! ## Defaults
//!
//! Built-in capabilities (see [`Capabilities::for_provider`]) follow
//! these rules of thumb:
//!
//! - **citations**: every provider that exposes a steerable chat
//!   completion API can emit `[^N]` markers when the system prompt
//!   asks for them. Raw-inference endpoints (HuggingFace Inference
//!   API, the embedded `local` embeddings backend) cannot, and
//!   small-model Ollama installs are not reliable either — those
//!   default to `false`.
//! - **seed**: any provider speaking the OpenAI-compatible
//!   `seed` field — OpenAI, Groq, Together, OpenRouter, Venice,
//!   DeepSeek, Ollama (≥0.1.30). Anthropic's API does not accept
//!   `seed`, so it's `false` there even though the model is otherwise
//!   capable.
//! - **temperature_zero**: every chat provider in the list. `false`
//!   only for the embedded `local` backend, which doesn't take a
//!   temperature.
//! - **streaming**: every chat provider that documents an SSE / event
//!   stream. HuggingFace Inference returns one shot; `local` is
//!   synchronous; `custom` is conservatively `false` since we cannot
//!   know what the operator pointed at.
//!
//! Unknown tokens get the conservative defaults from
//! [`Capabilities::conservative`] — citations off, seed off,
//! temperature_zero on, streaming off. This is the safe baseline
//! described in the issue's AC ("Unknown provider returns conservative
//! defaults").
//!
//! ## Per-deployment overrides
//!
//! [`Registry`] holds a `HashMap` keyed by lower-cased token. An entry
//! supplied via [`Registry::with_override`] completely replaces the
//! built-in row for that token — there is no partial-merge, since the
//! settings surface in #401 uses one TOML table per provider. Callers
//! that want partial overrides should construct the merged
//! [`Capabilities`] themselves.

use std::collections::HashMap;

use crate::runtime::ai::strict_validator::Mode;

/// Per-provider capability bag. Each flag is independently testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Provider can emit `[^N]` markers reliably enough to honor the
    /// strict citation contract (#395).
    pub supports_citations: bool,
    /// Provider honors a `seed` parameter, enabling reproducible
    /// completions when paired with `temperature=0` (#400).
    pub supports_seed: bool,
    /// Provider accepts `temperature=0`. Endpoints that don't take a
    /// temperature at all (e.g. embedded embeddings) report `false`.
    pub supports_temperature_zero: bool,
    /// Provider exposes a streaming response (SSE / chunked / ws). Set
    /// to `false` for synchronous inference endpoints.
    pub supports_streaming: bool,
}

impl Capabilities {
    /// Defaults for a provider the registry has no row for. Picked to
    /// match the AC: "Unknown provider returns conservative defaults
    /// (no citation support, no seed)".
    pub const fn conservative() -> Self {
        Self {
            supports_citations: false,
            supports_seed: false,
            supports_temperature_zero: true,
            supports_streaming: false,
        }
    }

    /// Built-in capability row for a canonical provider token (the
    /// `AiProvider::token()` form: `"openai"`, `"anthropic"`, …).
    /// Unknown tokens get [`Capabilities::conservative`].
    pub fn for_provider(token: &str) -> Self {
        match token {
            "openai" => Self {
                supports_citations: true,
                supports_seed: true,
                supports_temperature_zero: true,
                supports_streaming: true,
            },
            "anthropic" => Self {
                supports_citations: true,
                supports_seed: false,
                supports_temperature_zero: true,
                supports_streaming: true,
            },
            "groq" | "together" | "openrouter" | "venice" | "deepseek" => Self {
                supports_citations: true,
                supports_seed: true,
                supports_temperature_zero: true,
                supports_streaming: true,
            },
            "ollama" => Self {
                supports_citations: false,
                supports_seed: true,
                supports_temperature_zero: true,
                supports_streaming: true,
            },
            "huggingface" => Self {
                supports_citations: false,
                supports_seed: false,
                supports_temperature_zero: true,
                supports_streaming: false,
            },
            "local" => Self {
                supports_citations: false,
                supports_seed: false,
                supports_temperature_zero: false,
                supports_streaming: false,
            },
            "custom" => Self::conservative(),
            _ => Self::conservative(),
        }
    }
}

/// Why the effective mode differs from the requested mode. The caller
/// surfaces this as a structured warning entry on the ASK response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeWarning {
    /// Stable identifier — drivers can branch on this.
    pub kind: ModeWarningKind,
    /// Human-readable explanation including the provider token.
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeWarningKind {
    /// Strict was requested but the provider's `supports_citations`
    /// is `false`. Effective mode is [`Mode::Lenient`].
    ModeFallback,
}

/// Result of consulting the registry for a strict-mode request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModeOutcome {
    /// Caller's requested mode is honored verbatim.
    Allowed { effective: Mode },
    /// Strict was downgraded to lenient. The caller MUST record the
    /// `effective` mode (not the requested one) and surface
    /// `warning`.
    Fallback {
        effective: Mode,
        warning: ModeWarning,
    },
}

impl ModeOutcome {
    /// The mode the caller should actually run with.
    pub fn effective(&self) -> Mode {
        match self {
            Self::Allowed { effective } | Self::Fallback { effective, .. } => *effective,
        }
    }

    /// Convenience for the audit log / response builder.
    pub fn warning(&self) -> Option<&ModeWarning> {
        match self {
            Self::Allowed { .. } => None,
            Self::Fallback { warning, .. } => Some(warning),
        }
    }
}

/// Capability registry with optional per-deployment overrides.
///
/// Construct via [`Registry::new`] for built-ins only, then layer
/// overrides with [`Registry::with_override`]. Lookups go through
/// [`Registry::capabilities`] (raw row) and [`Registry::evaluate_mode`]
/// (strict-fallback policy).
#[derive(Debug, Clone, Default)]
pub struct Registry {
    overrides: HashMap<String, Capabilities>,
}

impl Registry {
    /// Empty registry. Built-in defaults are still applied to every
    /// lookup — this constructor just means "no per-deployment
    /// overrides yet".
    pub fn new() -> Self {
        Self {
            overrides: HashMap::new(),
        }
    }

    /// Replace the capability row for `token` (lower-cased before
    /// storage). Returns `self` for builder-style chaining in tests.
    pub fn with_override(mut self, token: &str, caps: Capabilities) -> Self {
        self.overrides.insert(token.to_ascii_lowercase(), caps);
        self
    }

    /// Look up the capability row for a provider token, applying any
    /// override on top of the built-in row.
    pub fn capabilities(&self, token: &str) -> Capabilities {
        let key = token.to_ascii_lowercase();
        if let Some(c) = self.overrides.get(&key) {
            return *c;
        }
        Capabilities::for_provider(&key)
    }

    /// Decide what mode the caller should actually run in, given the
    /// requested mode and this provider's capabilities.
    ///
    /// Strict against a non-citing provider transparently degrades to
    /// lenient with a `mode_fallback` warning. Lenient is always
    /// allowed.
    pub fn evaluate_mode(&self, token: &str, requested: Mode) -> ModeOutcome {
        if requested == Mode::Lenient {
            return ModeOutcome::Allowed {
                effective: Mode::Lenient,
            };
        }
        let caps = self.capabilities(token);
        if caps.supports_citations {
            return ModeOutcome::Allowed {
                effective: Mode::Strict,
            };
        }
        ModeOutcome::Fallback {
            effective: Mode::Lenient,
            warning: ModeWarning {
                kind: ModeWarningKind::ModeFallback,
                detail: format!(
                    "provider '{}' does not support reliable citation emission; \
                     strict mode downgraded to lenient",
                    token.to_ascii_lowercase()
                ),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conservative_defaults_match_ac() {
        let c = Capabilities::conservative();
        assert!(!c.supports_citations);
        assert!(!c.supports_seed);
        assert!(c.supports_temperature_zero);
        assert!(!c.supports_streaming);
    }

    #[test]
    fn openai_supports_everything() {
        let c = Capabilities::for_provider("openai");
        assert!(c.supports_citations);
        assert!(c.supports_seed);
        assert!(c.supports_temperature_zero);
        assert!(c.supports_streaming);
    }

    #[test]
    fn anthropic_no_seed() {
        let c = Capabilities::for_provider("anthropic");
        assert!(c.supports_citations);
        assert!(!c.supports_seed);
        assert!(c.supports_temperature_zero);
        assert!(c.supports_streaming);
    }

    #[test]
    fn openai_compatible_family_uniform() {
        for token in ["groq", "together", "openrouter", "venice", "deepseek"] {
            let c = Capabilities::for_provider(token);
            assert!(c.supports_citations, "{token} citations");
            assert!(c.supports_seed, "{token} seed");
            assert!(c.supports_temperature_zero, "{token} temp0");
            assert!(c.supports_streaming, "{token} streaming");
        }
    }

    #[test]
    fn ollama_no_citations_but_seed_and_streaming() {
        let c = Capabilities::for_provider("ollama");
        assert!(!c.supports_citations);
        assert!(c.supports_seed);
        assert!(c.supports_temperature_zero);
        assert!(c.supports_streaming);
    }

    #[test]
    fn huggingface_inference_no_seed_no_streaming() {
        let c = Capabilities::for_provider("huggingface");
        assert!(!c.supports_citations);
        assert!(!c.supports_seed);
        assert!(c.supports_temperature_zero);
        assert!(!c.supports_streaming);
    }

    #[test]
    fn local_backend_has_no_temperature() {
        let c = Capabilities::for_provider("local");
        assert!(!c.supports_citations);
        assert!(!c.supports_seed);
        assert!(!c.supports_temperature_zero);
        assert!(!c.supports_streaming);
    }

    #[test]
    fn custom_is_conservative() {
        assert_eq!(
            Capabilities::for_provider("custom"),
            Capabilities::conservative()
        );
    }

    #[test]
    fn unknown_token_is_conservative() {
        assert_eq!(
            Capabilities::for_provider("totally-made-up"),
            Capabilities::conservative()
        );
    }

    #[test]
    fn token_lookup_is_case_insensitive_via_registry() {
        let r = Registry::new();
        // Built-in path lower-cases the token before consulting the
        // match arm, so OPENAI / OpenAI / openai all resolve.
        assert_eq!(
            r.capabilities("OPENAI"),
            Capabilities::for_provider("openai")
        );
        assert_eq!(
            r.capabilities("OpenAi"),
            Capabilities::for_provider("openai")
        );
    }

    #[test]
    fn override_completely_replaces_builtin_row() {
        let overridden = Capabilities {
            supports_citations: false,
            supports_seed: false,
            supports_temperature_zero: false,
            supports_streaming: false,
        };
        let r = Registry::new().with_override("openai", overridden);
        assert_eq!(r.capabilities("openai"), overridden);
        // Unrelated providers are untouched.
        assert_eq!(r.capabilities("groq"), Capabilities::for_provider("groq"));
    }

    #[test]
    fn override_key_is_lowercased() {
        let custom_caps = Capabilities {
            supports_citations: true,
            supports_seed: true,
            supports_temperature_zero: true,
            supports_streaming: true,
        };
        let r = Registry::new().with_override("CUSTOM-INTERNAL", custom_caps);
        // Stored lower-cased so lookup with any case finds it.
        assert_eq!(r.capabilities("custom-internal"), custom_caps);
        assert_eq!(r.capabilities("Custom-Internal"), custom_caps);
    }

    #[test]
    fn lenient_always_allowed_regardless_of_provider() {
        let r = Registry::new();
        for token in ["openai", "huggingface", "local", "totally-made-up"] {
            let outcome = r.evaluate_mode(token, Mode::Lenient);
            assert_eq!(
                outcome,
                ModeOutcome::Allowed {
                    effective: Mode::Lenient
                },
                "lenient should pass through for {token}"
            );
            assert!(outcome.warning().is_none());
        }
    }

    #[test]
    fn strict_allowed_for_citing_provider() {
        let r = Registry::new();
        let outcome = r.evaluate_mode("openai", Mode::Strict);
        assert_eq!(
            outcome,
            ModeOutcome::Allowed {
                effective: Mode::Strict
            }
        );
        assert!(outcome.warning().is_none());
    }

    #[test]
    fn strict_downgraded_for_non_citing_provider() {
        let r = Registry::new();
        let outcome = r.evaluate_mode("huggingface", Mode::Strict);
        match outcome {
            ModeOutcome::Fallback {
                effective,
                ref warning,
            } => {
                assert_eq!(effective, Mode::Lenient);
                assert_eq!(warning.kind, ModeWarningKind::ModeFallback);
                assert!(warning.detail.contains("huggingface"));
                assert!(warning.detail.contains("strict"));
            }
            other => panic!("expected Fallback, got {other:?}"),
        }
        assert_eq!(outcome.effective(), Mode::Lenient);
        assert!(outcome.warning().is_some());
    }

    #[test]
    fn strict_downgraded_for_unknown_provider() {
        let r = Registry::new();
        let outcome = r.evaluate_mode("brand-new-provider", Mode::Strict);
        assert_eq!(outcome.effective(), Mode::Lenient);
        match outcome {
            ModeOutcome::Fallback { warning, .. } => {
                assert_eq!(warning.kind, ModeWarningKind::ModeFallback);
                assert!(warning.detail.contains("brand-new-provider"));
            }
            other => panic!("expected Fallback, got {other:?}"),
        }
    }

    #[test]
    fn override_can_upgrade_non_citing_provider_to_citing() {
        let r = Registry::new().with_override(
            "ollama",
            Capabilities {
                supports_citations: true,
                supports_seed: true,
                supports_temperature_zero: true,
                supports_streaming: true,
            },
        );
        let outcome = r.evaluate_mode("ollama", Mode::Strict);
        assert_eq!(
            outcome,
            ModeOutcome::Allowed {
                effective: Mode::Strict
            }
        );
    }

    #[test]
    fn override_can_downgrade_citing_provider_to_non_citing() {
        let r = Registry::new().with_override(
            "openai",
            Capabilities {
                supports_citations: false,
                supports_seed: false,
                supports_temperature_zero: true,
                supports_streaming: false,
            },
        );
        let outcome = r.evaluate_mode("openai", Mode::Strict);
        match outcome {
            ModeOutcome::Fallback {
                effective,
                ref warning,
            } => {
                assert_eq!(effective, Mode::Lenient);
                assert_eq!(warning.kind, ModeWarningKind::ModeFallback);
                assert!(warning.detail.contains("openai"));
            }
            other => panic!("expected Fallback, got {other:?}"),
        }
    }

    #[test]
    fn evaluate_mode_is_deterministic() {
        let r = Registry::new();
        for _ in 0..16 {
            assert_eq!(
                r.evaluate_mode("openai", Mode::Strict),
                ModeOutcome::Allowed {
                    effective: Mode::Strict
                }
            );
            assert_eq!(
                r.evaluate_mode("huggingface", Mode::Strict).effective(),
                Mode::Lenient
            );
        }
    }

    #[test]
    fn all_eleven_provider_tokens_have_explicit_rows() {
        // The registry should have a non-conservative row for every
        // built-in provider (10 explicit + custom returns conservative
        // by design). Pin so adding/removing a provider in
        // `AiProvider` is a deliberate decision.
        let citing = [
            "openai",
            "anthropic",
            "groq",
            "together",
            "openrouter",
            "venice",
            "deepseek",
        ];
        let non_citing = ["ollama", "huggingface", "local"];
        for t in citing {
            assert!(
                Capabilities::for_provider(t).supports_citations,
                "{t} should cite"
            );
        }
        for t in non_citing {
            assert!(
                !Capabilities::for_provider(t).supports_citations,
                "{t} should not cite"
            );
        }
        // Custom is the 11th, and explicitly conservative.
        assert_eq!(
            Capabilities::for_provider("custom"),
            Capabilities::conservative()
        );
    }
}
