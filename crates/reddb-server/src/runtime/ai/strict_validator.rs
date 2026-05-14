//! `StrictValidator` — pure citation validation policy.
//!
//! Issue #395 (PRD #391): after the `CitationParser` (issue #393)
//! scans the LLM answer, this module decides what to do with the
//! result given the requested mode and the current retry attempt.
//!
//! Deep module: no I/O, no transport, no LLM calls. Just an enum and
//! one function. The caller is responsible for actually issuing the
//! retry, mapping `GiveUp` to HTTP 422, etc.
//!
//! ## Policy
//!
//! Strict mode (the default per ADR 0013):
//!
//! - First call → if no malformed and no out-of-range, [`Decision::Ok`].
//! - First call → otherwise, [`Decision::Retry`] with a corrected
//!   prompt that tells the LLM the valid index range and asks it to
//!   reissue the answer with citations in `1..=sources_count`.
//! - Retry call → if still failing, [`Decision::GiveUp`] carrying the
//!   structured errors that the HTTP layer should pack into the 422
//!   response body under `validation.errors`.
//!
//! Exactly one retry is permitted. The validator tracks the retry
//! budget via the [`Attempt`] argument — callers MUST pass
//! [`Attempt::First`] on the initial call and [`Attempt::Retry`] on
//! the single follow-up. There is no `Attempt::Retry2`; the type is
//! the budget.
//!
//! Lenient mode ([`Mode::Lenient`], opt-in via `ASK '...' STRICT OFF`):
//!
//! - Always returns [`Decision::Ok`]. Warnings remain on the result
//!   for the caller to surface, but the validator never asks for a
//!   retry and never produces errors.
//!
//! ## Why a retry-prompt builder lives in here
//!
//! The retry message is part of the validator's contract — what the
//! LLM is told on retry affects whether the second call is likely to
//! succeed. Keeping prompt construction next to the decision logic
//! lets the unit tests pin the exact phrasing, and keeps the
//! `execute_ask` glue code tiny.

use crate::runtime::ai::citation_parser::{
    CitationParseResult, CitationWarning, CitationWarningKind,
};

/// Whether the caller wants strict validation or lenient warn-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Default. Structural failures trigger a retry; retry failure
    /// becomes a hard 422.
    Strict,
    /// `ASK '...' STRICT OFF`. Warnings are surfaced but never block.
    Lenient,
}

/// Which call this is — the validator uses this to enforce the
/// one-retry budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attempt {
    First,
    Retry,
}

/// Structured error returned in `validation.errors` on retry exhaust.
///
/// Mirrors the `CitationWarning` shape but reframed as an error
/// (the warning was advisory on the first call; on retry exhaust it
/// becomes the reason we couldn't deliver an answer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub kind: ValidationErrorKind,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationErrorKind {
    /// `[^N]` body wasn't a positive decimal terminated by `]`.
    Malformed,
    /// `N` was outside `1..=sources_count`.
    OutOfRange,
}

/// What the validator decided. The caller acts on this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Citations parsed cleanly — emit the answer to the user.
    Ok,
    /// Strict + first attempt + structural failure. Caller should
    /// issue exactly one follow-up LLM call with this prompt
    /// prepended to (or substituted for) the synthesis prompt.
    Retry { prompt: String },
    /// Strict + retry attempt + still failing. Caller should respond
    /// HTTP 422 with these errors in `validation.errors`.
    GiveUp { errors: Vec<ValidationError> },
}

/// Pure validation step.
///
/// `sources_count` is the length of `sources_flat`; we don't re-derive
/// out-of-range here because [`CitationParser`] already emitted the
/// warning during parsing. We just decide what to *do* about it.
pub fn validate(parsed: &CitationParseResult, mode: Mode, attempt: Attempt) -> Decision {
    if mode == Mode::Lenient {
        return Decision::Ok;
    }

    let structural_warnings: Vec<&CitationWarning> = parsed
        .warnings
        .iter()
        .filter(|w| {
            matches!(
                w.kind,
                CitationWarningKind::Malformed | CitationWarningKind::OutOfRange
            )
        })
        .collect();

    if structural_warnings.is_empty() {
        return Decision::Ok;
    }

    match attempt {
        Attempt::First => Decision::Retry {
            prompt: build_retry_prompt(&structural_warnings),
        },
        Attempt::Retry => Decision::GiveUp {
            errors: structural_warnings
                .iter()
                .map(|w| ValidationError {
                    kind: match w.kind {
                        CitationWarningKind::Malformed => ValidationErrorKind::Malformed,
                        CitationWarningKind::OutOfRange => ValidationErrorKind::OutOfRange,
                    },
                    detail: w.detail.clone(),
                })
                .collect(),
        },
    }
}

/// Construct the prompt the caller should send on the single retry.
///
/// The phrasing is pinned by tests; it intentionally:
///
/// - states the valid range explicitly,
/// - quotes the offending markers/details so the LLM sees its own
///   mistake,
/// - forbids inventing sources,
/// - asks for the answer to be re-emitted in full (we don't try to
///   patch the prior answer in place).
fn build_retry_prompt(warnings: &[&CitationWarning]) -> String {
    let mut out = String::from(
        "Your previous answer contained citation markers that do not match \
         the available sources. Reissue the answer in full, with every \
         `[^N]` marker referring to a real source by its 1-indexed position \
         in the provided context. Do not invent or renumber sources; if a \
         claim is not supported by a real source, drop the marker rather \
         than fabricate one. Problems detected:\n",
    );
    for w in warnings {
        let kind = match w.kind {
            CitationWarningKind::Malformed => "malformed",
            CitationWarningKind::OutOfRange => "out_of_range",
        };
        out.push_str(&format!("- [{kind}] {}\n", w.detail));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::ai::citation_parser::{Citation, CitationParseResult, CitationWarning};

    fn ok_result() -> CitationParseResult {
        CitationParseResult {
            citations: vec![Citation {
                marker: 1,
                span: 0..4,
                source_index: 0,
            }],
            warnings: vec![],
        }
    }

    fn malformed_result() -> CitationParseResult {
        CitationParseResult {
            citations: vec![],
            warnings: vec![CitationWarning {
                kind: CitationWarningKind::Malformed,
                span: 0..4,
                detail: "empty marker body".to_string(),
            }],
        }
    }

    fn out_of_range_result() -> CitationParseResult {
        CitationParseResult {
            citations: vec![Citation {
                marker: 9,
                span: 0..4,
                source_index: 8,
            }],
            warnings: vec![CitationWarning {
                kind: CitationWarningKind::OutOfRange,
                span: 0..4,
                detail: "marker [^9] references source #9 but only 2 sources available".to_string(),
            }],
        }
    }

    fn mixed_result() -> CitationParseResult {
        CitationParseResult {
            citations: vec![],
            warnings: vec![
                CitationWarning {
                    kind: CitationWarningKind::Malformed,
                    span: 0..3,
                    detail: "empty".into(),
                },
                CitationWarning {
                    kind: CitationWarningKind::OutOfRange,
                    span: 4..8,
                    detail: "marker [^7] references source #7 but only 1 sources available"
                        .to_string(),
                },
            ],
        }
    }

    // ---- Strict mode --------------------------------------------------

    #[test]
    fn strict_clean_is_ok_on_first() {
        assert_eq!(
            validate(&ok_result(), Mode::Strict, Attempt::First),
            Decision::Ok
        );
    }

    #[test]
    fn strict_clean_is_ok_on_retry_too() {
        // The retry call also produced clean output — that's the
        // success path for "first call failed, retry succeeded".
        assert_eq!(
            validate(&ok_result(), Mode::Strict, Attempt::Retry),
            Decision::Ok
        );
    }

    #[test]
    fn strict_malformed_first_attempt_asks_for_retry() {
        let decision = validate(&malformed_result(), Mode::Strict, Attempt::First);
        match decision {
            Decision::Retry { prompt } => {
                assert!(prompt.contains("Reissue the answer"));
                assert!(prompt.contains("malformed"));
                assert!(prompt.contains("empty marker body"));
            }
            other => panic!("expected Retry, got {other:?}"),
        }
    }

    #[test]
    fn strict_out_of_range_first_attempt_asks_for_retry() {
        let decision = validate(&out_of_range_result(), Mode::Strict, Attempt::First);
        match decision {
            Decision::Retry { prompt } => {
                assert!(prompt.contains("out_of_range"));
                assert!(prompt.contains("source #9"));
                // No-fabrication clause is part of the contract.
                assert!(prompt.contains("Do not invent"));
            }
            other => panic!("expected Retry, got {other:?}"),
        }
    }

    #[test]
    fn strict_malformed_retry_attempt_gives_up() {
        let decision = validate(&malformed_result(), Mode::Strict, Attempt::Retry);
        match decision {
            Decision::GiveUp { errors } => {
                assert_eq!(errors.len(), 1);
                assert_eq!(errors[0].kind, ValidationErrorKind::Malformed);
                assert_eq!(errors[0].detail, "empty marker body");
            }
            other => panic!("expected GiveUp, got {other:?}"),
        }
    }

    #[test]
    fn strict_out_of_range_retry_attempt_gives_up() {
        let decision = validate(&out_of_range_result(), Mode::Strict, Attempt::Retry);
        match decision {
            Decision::GiveUp { errors } => {
                assert_eq!(errors.len(), 1);
                assert_eq!(errors[0].kind, ValidationErrorKind::OutOfRange);
                assert!(errors[0].detail.contains("source #9"));
            }
            other => panic!("expected GiveUp, got {other:?}"),
        }
    }

    #[test]
    fn strict_mixed_warnings_carry_through_to_giveup() {
        let decision = validate(&mixed_result(), Mode::Strict, Attempt::Retry);
        match decision {
            Decision::GiveUp { errors } => {
                assert_eq!(errors.len(), 2);
                assert_eq!(errors[0].kind, ValidationErrorKind::Malformed);
                assert_eq!(errors[1].kind, ValidationErrorKind::OutOfRange);
            }
            other => panic!("expected GiveUp, got {other:?}"),
        }
    }

    #[test]
    fn strict_mixed_warnings_first_attempt_still_retries() {
        let decision = validate(&mixed_result(), Mode::Strict, Attempt::First);
        assert!(matches!(decision, Decision::Retry { .. }));
    }

    // ---- Lenient mode -------------------------------------------------

    #[test]
    fn lenient_passes_clean() {
        assert_eq!(
            validate(&ok_result(), Mode::Lenient, Attempt::First),
            Decision::Ok
        );
    }

    #[test]
    fn lenient_passes_malformed() {
        // Warnings are still on `parsed.warnings`; the validator just
        // refuses to act on them in lenient mode.
        assert_eq!(
            validate(&malformed_result(), Mode::Lenient, Attempt::First),
            Decision::Ok
        );
    }

    #[test]
    fn lenient_passes_out_of_range() {
        assert_eq!(
            validate(&out_of_range_result(), Mode::Lenient, Attempt::First),
            Decision::Ok
        );
    }

    #[test]
    fn lenient_ignores_attempt() {
        // Retry-budget tracking is a strict-mode concern. In lenient
        // mode the validator behaves identically regardless of attempt.
        assert_eq!(
            validate(&malformed_result(), Mode::Lenient, Attempt::Retry),
            Decision::Ok
        );
    }

    // ---- Retry-prompt contract ---------------------------------------

    #[test]
    fn retry_prompt_includes_every_warning_detail() {
        let parsed = mixed_result();
        let decision = validate(&parsed, Mode::Strict, Attempt::First);
        let Decision::Retry { prompt } = decision else {
            panic!("expected Retry");
        };
        for w in &parsed.warnings {
            assert!(
                prompt.contains(&w.detail),
                "retry prompt missing detail `{}`, got:\n{prompt}",
                w.detail
            );
        }
    }

    #[test]
    fn retry_prompt_is_deterministic() {
        // Two validations of the same input must produce byte-equal
        // retry prompts — required for the ASK determinism contract
        // (#400). Strings of side-effects (e.g. timestamps, RNG) must
        // never leak into the prompt builder.
        let parsed = mixed_result();
        let a = validate(&parsed, Mode::Strict, Attempt::First);
        let b = validate(&parsed, Mode::Strict, Attempt::First);
        assert_eq!(a, b);
    }

    #[test]
    fn retry_prompt_forbids_fabrication() {
        let decision = validate(&out_of_range_result(), Mode::Strict, Attempt::First);
        let Decision::Retry { prompt } = decision else {
            panic!("expected Retry");
        };
        // Anti-hallucination guard — the LLM must not "fix" the
        // citation by inventing a new source.
        assert!(prompt.contains("Do not invent"));
    }

    // ---- Boundary cases ----------------------------------------------

    #[test]
    fn empty_parse_is_ok_in_either_mode() {
        let empty = CitationParseResult::default();
        assert_eq!(validate(&empty, Mode::Strict, Attempt::First), Decision::Ok);
        assert_eq!(validate(&empty, Mode::Strict, Attempt::Retry), Decision::Ok);
        assert_eq!(
            validate(&empty, Mode::Lenient, Attempt::First),
            Decision::Ok
        );
    }

    #[test]
    fn citations_without_warnings_are_ok() {
        // Many successful citations, no warnings — the success path.
        let parsed = CitationParseResult {
            citations: vec![
                Citation {
                    marker: 1,
                    span: 0..4,
                    source_index: 0,
                },
                Citation {
                    marker: 2,
                    span: 5..9,
                    source_index: 1,
                },
                Citation {
                    marker: 3,
                    span: 10..14,
                    source_index: 2,
                },
            ],
            warnings: vec![],
        };
        assert_eq!(
            validate(&parsed, Mode::Strict, Attempt::First),
            Decision::Ok
        );
    }
}
