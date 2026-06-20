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

/// A service modality an AI provider+model can be asked to perform.
///
/// Orthogonal to the text-chat flags on [`Capabilities`] (citations /
/// seed / streaming): a provider can be excellent at strict text chat
/// yet have no embeddings endpoint at all (Anthropic), or be an
/// embeddings-only backend that cannot generate (the embedded `local`
/// model). The modality matrix records *which kinds of request* a
/// provider+model can serve, so a policy that wires the wrong provider
/// to a job is rejected at DDL time instead of failing at call time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modality {
    /// Produce embedding vectors for text (`/embeddings`).
    Embed,
    /// Generate free-form text from a prompt (`/chat/completions`).
    Generate,
    /// Accept image input alongside text (multimodal vision).
    Vision,
    /// Classify content against a safety taxonomy (`/moderations`).
    Moderate,
}

impl Modality {
    /// Canonical lower-case token, the form used in policy DDL.
    pub fn token(self) -> &'static str {
        match self {
            Self::Embed => "embed",
            Self::Generate => "generate",
            Self::Vision => "vision",
            Self::Moderate => "moderate",
        }
    }

    /// Parse a policy-DDL token (case-insensitive). A couple of common
    /// synonyms are accepted so `embedding` / `chat` don't surprise.
    pub fn parse(token: &str) -> Option<Self> {
        match token.trim().to_ascii_lowercase().as_str() {
            "embed" | "embedding" | "embeddings" => Some(Self::Embed),
            "generate" | "generation" | "chat" | "completion" => Some(Self::Generate),
            "vision" | "image" | "multimodal" => Some(Self::Vision),
            "moderate" | "moderation" => Some(Self::Moderate),
            _ => None,
        }
    }

    /// All four modalities, for exhaustive iteration in tests / catalogs.
    pub const ALL: [Self; 4] = [Self::Embed, Self::Generate, Self::Vision, Self::Moderate];
}

/// Which modalities a provider+model can serve. Each axis is an
/// independently testable flag, mirroring the [`Capabilities`] shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Modalities {
    pub embed: bool,
    pub generate: bool,
    pub vision: bool,
    pub moderate: bool,
}

impl Modalities {
    /// Conservative defaults for a token the registry has no row for.
    ///
    /// An unknown OpenAI-compatible endpoint can plausibly serve the two
    /// universal text modalities — embeddings and generation — but we
    /// must *not* assume it offers vision (multimodal input) or a
    /// moderation endpoint, since those are specialised products. So a
    /// policy that requests `vision`/`moderate` against an undeclared
    /// provider is rejected until the operator supplies an override.
    /// This matches the spirit of [`Capabilities::conservative`]: deny
    /// the capabilities you cannot verify.
    pub const fn conservative() -> Self {
        Self {
            embed: true,
            generate: true,
            vision: false,
            moderate: false,
        }
    }

    /// Whether this row can serve `modality`.
    pub fn supports(&self, modality: Modality) -> bool {
        match modality {
            Modality::Embed => self.embed,
            Modality::Generate => self.generate,
            Modality::Vision => self.vision,
            Modality::Moderate => self.moderate,
        }
    }

    /// Built-in modality row for a canonical provider token. Unknown
    /// tokens get [`Modalities::conservative`].
    ///
    /// Defaults are rules of thumb based on each provider's public
    /// product surface (overridable per deployment):
    ///
    /// - **openai**: the full matrix — embeddings, chat, gpt-4o vision,
    ///   and a moderation endpoint.
    /// - **anthropic**: chat + vision, but no embeddings product and no
    ///   moderation endpoint.
    /// - **minimax**: OpenAI-compatible chat, embeddings, and vision
    ///   (abab multimodal models); no moderation endpoint.
    /// - **together / ollama**: chat, embeddings, and vision-capable
    ///   open models; no moderation.
    /// - **groq / openrouter / venice**: chat + vision, no first-party
    ///   embeddings, no moderation.
    /// - **deepseek**: chat only.
    /// - **huggingface**: raw inference for embeddings + generation; no
    ///   uniform vision/moderation surface.
    /// - **local**: embeddings-only backend — generation is out of
    ///   scope (mirrors the embeddings-only HTTP reject).
    /// - **custom / unknown**: [`Modalities::conservative`].
    pub fn for_provider(token: &str) -> Self {
        match token {
            "openai" => Self {
                embed: true,
                generate: true,
                vision: true,
                moderate: true,
            },
            "anthropic" => Self {
                embed: false,
                generate: true,
                vision: true,
                moderate: false,
            },
            "minimax" | "together" | "ollama" => Self {
                embed: true,
                generate: true,
                vision: true,
                moderate: false,
            },
            "groq" | "openrouter" | "venice" => Self {
                embed: false,
                generate: true,
                vision: true,
                moderate: false,
            },
            "deepseek" => Self {
                embed: false,
                generate: true,
                vision: false,
                moderate: false,
            },
            "huggingface" => Self {
                embed: true,
                generate: true,
                vision: false,
                moderate: false,
            },
            "local" => Self {
                embed: true,
                generate: false,
                vision: false,
                moderate: false,
            },
            "custom" => Self::conservative(),
            _ => Self::conservative(),
        }
    }
}

/// Rejection returned by the DDL-time / call-time modality gate when a
/// policy wires a provider+model to a job it cannot serve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModalityValidationError {
    /// Provider token as written in the policy.
    pub provider: String,
    /// Model name as written in the policy (informational — the gate
    /// decides on the provider row; the model is surfaced for context
    /// and reserved for future per-model overrides).
    pub model: String,
    /// The modality the policy requested.
    pub modality: Modality,
    /// Operator-actionable explanation.
    pub message: String,
}

impl std::fmt::Display for ModalityValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ModalityValidationError {}

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
    modality_overrides: HashMap<String, Modalities>,
}

impl Registry {
    /// Empty registry. Built-in defaults are still applied to every
    /// lookup — this constructor just means "no per-deployment
    /// overrides yet".
    pub fn new() -> Self {
        Self {
            overrides: HashMap::new(),
            modality_overrides: HashMap::new(),
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

    /// Replace the modality row for `token` (lower-cased before
    /// storage). Like [`Registry::with_override`], this is a complete
    /// replacement, not a partial merge. Returns `self` for chaining.
    pub fn with_modality_override(mut self, token: &str, modalities: Modalities) -> Self {
        self.modality_overrides
            .insert(token.to_ascii_lowercase(), modalities);
        self
    }

    /// Look up the modality row for a provider token, applying any
    /// per-deployment override on top of the built-in row.
    pub fn modalities(&self, token: &str) -> Modalities {
        let key = token.to_ascii_lowercase();
        if let Some(m) = self.modality_overrides.get(&key) {
            return *m;
        }
        Modalities::for_provider(&key)
    }

    /// Deterministic answer to "can provider `token` (+ `model`) serve
    /// `modality`?". The model is accepted for signature completeness
    /// and future per-model overrides; today the decision is driven by
    /// the provider row.
    pub fn can_serve(&self, token: &str, _model: &str, modality: Modality) -> bool {
        self.modalities(token).supports(modality)
    }

    /// DDL-time (and call-time) gate: reject a policy that wires
    /// `provider`/`model` to a `modality` it cannot serve.
    ///
    /// `Ok(())` means the policy is admissible. The `Err` carries an
    /// operator-actionable message naming the provider, model, and the
    /// unsupported modality. Use this both when a `CREATE … POLICY`
    /// statement is parsed (fail fast, before the policy is stored) and
    /// at call time as a defence-in-depth check.
    pub fn validate_policy_modality(
        &self,
        provider: &str,
        model: &str,
        modality: Modality,
    ) -> Result<(), ModalityValidationError> {
        if self.can_serve(provider, model, modality) {
            return Ok(());
        }
        Err(ModalityValidationError {
            provider: provider.to_string(),
            model: model.to_string(),
            modality,
            message: format!(
                "AI policy is invalid: provider '{}' (model '{}') cannot serve the '{}' modality; \
                 declare a provider that supports it or register a modality override",
                provider.to_ascii_lowercase(),
                model,
                modality.token()
            ),
        })
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

    // ---- modality matrix -------------------------------------------------

    #[test]
    fn modality_token_roundtrips_through_parse() {
        for m in Modality::ALL {
            assert_eq!(Modality::parse(m.token()), Some(m), "{m:?}");
        }
        // case-insensitive + common synonyms
        assert_eq!(Modality::parse("EMBEDDING"), Some(Modality::Embed));
        assert_eq!(Modality::parse("chat"), Some(Modality::Generate));
        assert_eq!(Modality::parse("image"), Some(Modality::Vision));
        assert_eq!(Modality::parse("moderation"), Some(Modality::Moderate));
        assert_eq!(Modality::parse("nonsense"), None);
    }

    #[test]
    fn unknown_provider_gets_conservative_modalities() {
        // AC: unknown token → conservative defaults (the two universal
        // text modalities allowed, specialised ones denied).
        let c = Modalities::for_provider("totally-made-up");
        assert_eq!(c, Modalities::conservative());
        assert!(c.embed);
        assert!(c.generate);
        assert!(!c.vision);
        assert!(!c.moderate);
        // `custom` shares the conservative row.
        assert_eq!(
            Modalities::for_provider("custom"),
            Modalities::conservative()
        );
    }

    #[test]
    fn openai_serves_every_modality() {
        let c = Modalities::for_provider("openai");
        for m in Modality::ALL {
            assert!(c.supports(m), "openai should serve {m:?}");
        }
    }

    #[test]
    fn minimax_serves_embed_generate_vision_not_moderate() {
        let c = Modalities::for_provider("minimax");
        assert!(c.supports(Modality::Embed));
        assert!(c.supports(Modality::Generate));
        assert!(c.supports(Modality::Vision));
        assert!(!c.supports(Modality::Moderate));
    }

    #[test]
    fn anthropic_cannot_embed() {
        // Anthropic has no embeddings product (pinned elsewhere in the
        // multi-provider contract tests).
        assert!(!Modalities::for_provider("anthropic").supports(Modality::Embed));
        assert!(Modalities::for_provider("anthropic").supports(Modality::Generate));
    }

    #[test]
    fn local_is_embeddings_only() {
        let c = Modalities::for_provider("local");
        assert!(c.supports(Modality::Embed));
        assert!(!c.supports(Modality::Generate));
        assert!(!c.supports(Modality::Vision));
        assert!(!c.supports(Modality::Moderate));
    }

    #[test]
    fn deepseek_is_generate_only() {
        let c = Modalities::for_provider("deepseek");
        assert!(c.supports(Modality::Generate));
        assert!(!c.supports(Modality::Embed));
        assert!(!c.supports(Modality::Vision));
        assert!(!c.supports(Modality::Moderate));
    }

    #[test]
    fn can_serve_is_case_insensitive_and_deterministic() {
        let r = Registry::new();
        for _ in 0..8 {
            assert!(r.can_serve("OpenAI", "gpt-4o", Modality::Vision));
            assert!(!r.can_serve("LOCAL", "all-MiniLM", Modality::Generate));
        }
    }

    #[test]
    fn validate_rejects_incapable_provider_modality() {
        let r = Registry::new();
        let err = r
            .validate_policy_modality("local", "all-MiniLM-L6-v2", Modality::Generate)
            .expect_err("local cannot generate");
        assert_eq!(err.provider, "local");
        assert_eq!(err.modality, Modality::Generate);
        let msg = err.to_string();
        assert!(msg.contains("local"), "{msg}");
        assert!(msg.contains("generate"), "{msg}");
        assert!(msg.contains("all-MiniLM-L6-v2"), "{msg}");
    }

    #[test]
    fn validate_accepts_capable_provider_modality() {
        let r = Registry::new();
        assert!(r
            .validate_policy_modality("openai", "text-embedding-3-small", Modality::Embed)
            .is_ok());
        assert!(r
            .validate_policy_modality("minimax", "abab6.5s-chat", Modality::Vision)
            .is_ok());
    }

    #[test]
    fn modality_override_completely_replaces_builtin_row() {
        // Operator declares that their pinned DeepSeek deployment also
        // exposes an embeddings endpoint.
        let upgraded = Modalities {
            embed: true,
            generate: true,
            vision: false,
            moderate: false,
        };
        let r = Registry::new().with_modality_override("deepseek", upgraded);
        assert_eq!(r.modalities("deepseek"), upgraded);
        assert!(r
            .validate_policy_modality("deepseek", "deepseek-embed", Modality::Embed)
            .is_ok());
        // Unrelated providers keep their built-in rows.
        assert_eq!(r.modalities("openai"), Modalities::for_provider("openai"));
    }

    #[test]
    fn modality_override_can_revoke_a_builtin_capability() {
        // A locked-down OpenAI deployment with vision disabled.
        let restricted = Modalities {
            embed: true,
            generate: true,
            vision: false,
            moderate: false,
        };
        let r = Registry::new().with_modality_override("OpenAI", restricted);
        // Stored lower-cased, so any-case lookup finds it.
        assert_eq!(r.modalities("openai"), restricted);
        let err = r
            .validate_policy_modality("openai", "gpt-4o", Modality::Vision)
            .expect_err("vision revoked by override");
        assert_eq!(err.modality, Modality::Vision);
    }
}
