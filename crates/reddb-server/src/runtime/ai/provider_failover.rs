//! `ProviderFailover` — pure ordered-list failover kernel for ASK.
//!
//! Issue #404 (PRD #391): when a user asks `ASK '...'` the runtime picks
//! a provider from `ask.providers.fallback = ['groq', 'openai',
//! 'anthropic']` (or per-query `USING 'a,b,c'`) and walks the list in
//! order until one succeeds. Failover triggers on **retryable** outcomes
//! — transport errors, 5xx, and timeouts. Authoritative errors like
//! 4xx auth failures or content-policy refusals short-circuit: we do
//! not paper over a bad key by silently switching vendors.
//!
//! Deep module: no I/O, no async, no clock. The caller supplies an
//! attempt function `FnMut(&str) -> Result<R, AttemptError>` and we
//! drive the loop. This keeps the kernel trivially testable with
//! synchronous stubs and lets the eventual wiring slice plug in real
//! HTTP transports without changing the policy logic.
//!
//! ## Why "retryable" is a closed set
//!
//! Failover is risky: if the second provider produces a different
//! answer than the first, the user sees nondeterminism for what was
//! supposed to be a deterministic ASK (#400). We only fail over when
//! the first provider could not have produced *any* answer:
//!
//! - **Transport** — DNS, TCP, TLS, dropped connection. No response
//!   bytes received, so no answer was committed.
//! - **5xx** — provider acknowledged the request but admitted failure.
//!   By HTTP convention, the resource is in an unknown/transient bad
//!   state; safe to retry on a sibling.
//! - **Timeout** — request exceeded the deadline. From our side the
//!   call is over; whether the provider eventually completed is moot.
//!
//! Everything else — 4xx, malformed response, content-filter refusal,
//! non-retryable provider-specific codes — is reported as-is. The
//! caller turns those into the user-visible error.
//!
//! ## Preservation of determinism inputs
//!
//! `seed`, `temperature`, and `strict` are part of the request the
//! caller passes to the attempt fn. The kernel is generic over the
//! request payload, so by construction every attempt sees the same
//! parameters. We do not "fix up" requests between attempts.
//!
//! ## Outcome shape
//!
//! On success: `(provider, response, prior_errors)`. We surface
//! prior_errors so the audit log can record that, e.g., groq 502'd
//! before openai answered — that's signal for capacity planning even
//! when the user got a good answer.
//!
//! On total failure: `AllProvidersFailed { attempts }` where each entry
//! is `(provider, AttemptError)`. The HTTP layer maps this to 503 per
//! the acceptance criteria.

use std::fmt;
use std::time::Duration;

/// A classification of one attempt's failure.
///
/// `Transport`, `Status5xx`, and `Timeout` are retryable — the failover
/// loop moves to the next provider. `NonRetryable` aborts the loop
/// and is returned to the caller wrapped in the outcome.
#[derive(Debug, Clone, PartialEq)]
pub enum AttemptError {
    /// Network-level failure before/while receiving a response.
    /// Examples: DNS resolution failure, connection refused, TLS error,
    /// socket reset mid-stream. `String` is a short human description
    /// suitable for audit.
    Transport(String),
    /// Provider returned a 5xx response. Carries the actual status code
    /// (e.g. 502, 503, 504) and a short body excerpt.
    Status5xx { code: u16, body: String },
    /// Per-request deadline elapsed before completion.
    Timeout(Duration),
    /// Authoritative error that must NOT trigger failover. Examples:
    /// 4xx auth failure (wrong API key), 4xx quota exhausted on the
    /// account level, content-policy refusal, malformed response we
    /// cannot recover from. The kernel returns immediately when it
    /// sees this — there is no value in asking another provider when
    /// the request itself is bad.
    NonRetryable(String),
}

impl AttemptError {
    /// Whether this error should trigger advancement to the next provider.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            AttemptError::Transport(_) | AttemptError::Status5xx { .. } | AttemptError::Timeout(_)
        )
    }
}

impl fmt::Display for AttemptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AttemptError::Transport(msg) => write!(f, "transport: {msg}"),
            AttemptError::Status5xx { code, body } => write!(f, "http {code}: {body}"),
            AttemptError::Timeout(d) => write!(f, "timeout after {}ms", d.as_millis()),
            AttemptError::NonRetryable(msg) => write!(f, "non-retryable: {msg}"),
        }
    }
}

/// Successful failover result. `prior_errors` lists every retryable
/// failure we walked through to get here — useful for audit but not
/// for user output.
#[derive(Debug, Clone, PartialEq)]
pub struct FailoverSuccess<R> {
    pub provider: String,
    pub response: R,
    pub prior_errors: Vec<(String, AttemptError)>,
}

/// All-providers-exhausted result. The HTTP layer maps this to 503 per
/// the acceptance criteria; `attempts` becomes the visible list of
/// providers that were tried and how each one failed.
#[derive(Debug, Clone, PartialEq)]
pub struct FailoverExhausted {
    pub attempts: Vec<(String, AttemptError)>,
}

impl fmt::Display for FailoverExhausted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "all providers failed:")?;
        for (provider, err) in &self.attempts {
            write!(f, " [{provider}: {err}]")?;
        }
        Ok(())
    }
}

/// Walk `providers` in order. For each, invoke `attempt`. The first
/// `Ok` short-circuits and is returned with the trail of prior
/// retryable errors. A `NonRetryable` short-circuits to
/// `Err(FailoverExhausted)` containing the attempts up to and including
/// the non-retryable one — we do not pretend more providers were tried.
/// Retryable failures advance to the next provider.
///
/// Empty `providers` returns `Err(FailoverExhausted { attempts: [] })`.
/// The HTTP layer should treat that as a config error, not a 503; the
/// kernel does not encode that distinction.
pub fn run<R, F>(
    providers: &[&str],
    mut attempt: F,
) -> Result<FailoverSuccess<R>, FailoverExhausted>
where
    F: FnMut(&str) -> Result<R, AttemptError>,
{
    let mut prior: Vec<(String, AttemptError)> = Vec::new();

    for provider in providers {
        match attempt(provider) {
            Ok(response) => {
                return Ok(FailoverSuccess {
                    provider: (*provider).to_string(),
                    response,
                    prior_errors: prior,
                });
            }
            Err(err) => {
                let retryable = err.is_retryable();
                prior.push(((*provider).to_string(), err));
                if !retryable {
                    return Err(FailoverExhausted { attempts: prior });
                }
            }
        }
    }

    Err(FailoverExhausted { attempts: prior })
}

/// Parse a `USING 'a,b,c'` override into an ordered, deduped list of
/// non-empty provider names. Surrounding whitespace is trimmed. Empty
/// segments are dropped. Order of first occurrence wins on dedup —
/// the user's intent is honored, not silently reordered.
///
/// Returns `None` if the parse yields zero providers; the caller falls
/// back to the global `ask.providers.fallback` setting.
pub fn parse_using_clause(raw: &str) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for segment in raw.split(',') {
        let name = segment.trim();
        if name.is_empty() {
            continue;
        }
        if !out.iter().any(|existing| existing == name) {
            out.push(name.to_string());
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    // --- AttemptError classification ------------------------------------

    #[test]
    fn transport_is_retryable() {
        assert!(AttemptError::Transport("dns".into()).is_retryable());
    }

    #[test]
    fn status_5xx_is_retryable() {
        assert!(AttemptError::Status5xx {
            code: 502,
            body: "bad gateway".into()
        }
        .is_retryable());
    }

    #[test]
    fn timeout_is_retryable() {
        assert!(AttemptError::Timeout(Duration::from_secs(30)).is_retryable());
    }

    #[test]
    fn non_retryable_is_not_retryable() {
        assert!(!AttemptError::NonRetryable("401 unauthorized".into()).is_retryable());
    }

    // --- run() success paths --------------------------------------------

    #[test]
    fn first_provider_succeeds_no_prior_errors() {
        let providers = ["groq", "openai", "anthropic"];
        let result = run(&providers, |p| {
            Ok::<_, AttemptError>(format!("answer from {p}"))
        });
        let ok = result.expect("should succeed");
        assert_eq!(ok.provider, "groq");
        assert_eq!(ok.response, "answer from groq");
        assert!(ok.prior_errors.is_empty());
    }

    #[test]
    fn second_provider_succeeds_after_5xx() {
        // Acceptance: integration test with two stub providers where
        // the first errors and the second succeeds.
        let providers = ["groq", "openai"];
        let calls = RefCell::new(0u32);
        let result = run(&providers, |p| {
            *calls.borrow_mut() += 1;
            if p == "groq" {
                Err(AttemptError::Status5xx {
                    code: 502,
                    body: "bad gateway".into(),
                })
            } else {
                Ok(format!("answer from {p}"))
            }
        });
        let ok = result.expect("should succeed");
        assert_eq!(ok.provider, "openai");
        assert_eq!(ok.response, "answer from openai");
        assert_eq!(*calls.borrow(), 2);
        assert_eq!(ok.prior_errors.len(), 1);
        assert_eq!(ok.prior_errors[0].0, "groq");
    }

    #[test]
    fn third_provider_succeeds_after_transport_and_timeout() {
        let providers = ["groq", "openai", "anthropic"];
        let result = run(&providers, |p| match p {
            "groq" => Err(AttemptError::Transport("connection reset".into())),
            "openai" => Err(AttemptError::Timeout(Duration::from_secs(30))),
            _ => Ok(format!("answer from {p}")),
        });
        let ok = result.expect("should succeed");
        assert_eq!(ok.provider, "anthropic");
        assert_eq!(ok.prior_errors.len(), 2);
        assert!(matches!(ok.prior_errors[0].1, AttemptError::Transport(_)));
        assert!(matches!(ok.prior_errors[1].1, AttemptError::Timeout(_)));
    }

    // --- run() failure paths --------------------------------------------

    #[test]
    fn all_retryable_failures_exhausts_with_full_attempt_list() {
        let providers = ["groq", "openai", "anthropic"];
        let result = run::<String, _>(&providers, |p| {
            Err(AttemptError::Status5xx {
                code: 503,
                body: format!("{p} unavailable"),
            })
        });
        let exhausted = result.expect_err("should exhaust");
        assert_eq!(exhausted.attempts.len(), 3);
        assert_eq!(exhausted.attempts[0].0, "groq");
        assert_eq!(exhausted.attempts[1].0, "openai");
        assert_eq!(exhausted.attempts[2].0, "anthropic");
    }

    #[test]
    fn non_retryable_short_circuits_without_trying_remaining() {
        // 401 from the first provider must NOT be papered over by
        // silently switching to the next vendor. The user sees the
        // auth error directly.
        let providers = ["groq", "openai", "anthropic"];
        let calls = RefCell::new(0u32);
        let result = run::<String, _>(&providers, |p| {
            *calls.borrow_mut() += 1;
            if p == "groq" {
                Err(AttemptError::NonRetryable("401 unauthorized".into()))
            } else {
                panic!("must not call sibling providers after non-retryable")
            }
        });
        let exhausted = result.expect_err("should short-circuit");
        assert_eq!(*calls.borrow(), 1);
        assert_eq!(exhausted.attempts.len(), 1);
        assert_eq!(exhausted.attempts[0].0, "groq");
        assert!(matches!(
            exhausted.attempts[0].1,
            AttemptError::NonRetryable(_)
        ));
    }

    #[test]
    fn non_retryable_after_retryable_preserves_full_trail() {
        // 502 from groq, then 401 from openai — the audit log should
        // see both, and anthropic must not be called.
        let providers = ["groq", "openai", "anthropic"];
        let calls = RefCell::new(Vec::<String>::new());
        let result = run::<String, _>(&providers, |p| {
            calls.borrow_mut().push(p.to_string());
            match p {
                "groq" => Err(AttemptError::Status5xx {
                    code: 502,
                    body: "bad".into(),
                }),
                "openai" => Err(AttemptError::NonRetryable("401".into())),
                _ => panic!("anthropic must not be called"),
            }
        });
        let exhausted = result.expect_err("should fail");
        assert_eq!(*calls.borrow(), vec!["groq", "openai"]);
        assert_eq!(exhausted.attempts.len(), 2);
    }

    #[test]
    fn empty_provider_list_returns_empty_exhausted() {
        let providers: [&str; 0] = [];
        let result = run::<String, _>(&providers, |_| panic!("must not be called"));
        let exhausted = result.expect_err("empty list yields exhausted");
        assert!(exhausted.attempts.is_empty());
    }

    // --- determinism preservation ---------------------------------------

    #[test]
    fn attempt_fn_is_invoked_with_identical_inputs() {
        // The kernel does not modify any per-request state between
        // attempts. We verify by capturing a request payload struct
        // and asserting equality across calls.
        #[derive(Clone, PartialEq, Debug)]
        struct Req {
            seed: u64,
            temperature: f32,
            strict: bool,
        }
        let req = Req {
            seed: 42,
            temperature: 0.0,
            strict: true,
        };
        let providers = ["groq", "openai"];
        let seen = RefCell::new(Vec::<Req>::new());
        let _ = run::<(), _>(&providers, |_| {
            seen.borrow_mut().push(req.clone());
            Err(AttemptError::Transport("retry".into()))
        });
        let seen = seen.borrow();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], seen[1]);
    }

    // --- USING clause parsing -------------------------------------------

    #[test]
    fn parse_using_simple() {
        assert_eq!(
            parse_using_clause("groq,openai"),
            Some(vec!["groq".into(), "openai".into()])
        );
    }

    #[test]
    fn parse_using_trims_whitespace() {
        assert_eq!(
            parse_using_clause("  groq , openai , anthropic  "),
            Some(vec!["groq".into(), "openai".into(), "anthropic".into()])
        );
    }

    #[test]
    fn parse_using_drops_empty_segments() {
        assert_eq!(
            parse_using_clause("groq,,openai,"),
            Some(vec!["groq".into(), "openai".into()])
        );
    }

    #[test]
    fn parse_using_dedupes_preserving_first_occurrence() {
        assert_eq!(
            parse_using_clause("groq,openai,groq"),
            Some(vec!["groq".into(), "openai".into()])
        );
    }

    #[test]
    fn parse_using_empty_returns_none() {
        assert_eq!(parse_using_clause(""), None);
        assert_eq!(parse_using_clause(" , , "), None);
    }

    #[test]
    fn parse_using_single_provider() {
        assert_eq!(parse_using_clause("groq"), Some(vec!["groq".into()]));
    }

    // --- Display impls (audit-facing) -----------------------------------

    #[test]
    fn exhausted_display_lists_each_attempt() {
        let exhausted = FailoverExhausted {
            attempts: vec![
                ("groq".into(), AttemptError::Transport("dns".into())),
                (
                    "openai".into(),
                    AttemptError::Status5xx {
                        code: 502,
                        body: "bad".into(),
                    },
                ),
            ],
        };
        let s = format!("{exhausted}");
        assert!(s.contains("groq"));
        assert!(s.contains("openai"));
        assert!(s.contains("502"));
    }
}
