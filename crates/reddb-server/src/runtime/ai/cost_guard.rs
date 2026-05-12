//! `CostGuardEvaluator` — pure ASK resource-cap policy.
//!
//! Issue #401 (PRD #391): every ASK call must respect hard limits on
//! prompt size, completion size, source payload size, wall-clock
//! timeout, and a per-tenant daily USD cap. This module is the pure
//! kernel that decides whether a given call (or step within a call)
//! is allowed to proceed.
//!
//! Deep module: no I/O, no clock reads, no transport. The caller
//! threads in the current usage snapshot, the tenant's running daily
//! spend, the deployment settings, and the current `now`. The
//! evaluator returns either [`Decision::Allow`] or [`Decision::Reject`]
//! carrying the offending limit name and the HTTP status the API
//! layer should surface (413 for over-budget, 504 for timeout).
//!
//! ## Why a single evaluator
//!
//! The ASK pipeline has three natural checkpoints where caps matter:
//!
//! 1. **Pre-call** — once the prompt has been assembled and sources
//!    fetched, before sending to the LLM. Catches `max_prompt_tokens`,
//!    `max_sources_bytes`, and the daily cost cap (using an estimated
//!    cost for the planned call).
//! 2. **In-flight** — when streaming tokens back, the running
//!    `completion_tokens` count must not exceed `max_completion_tokens`,
//!    and the elapsed time must not exceed `timeout_ms`.
//! 3. **Post-call** — once the call returns, the daily cost counter
//!    is incremented; the next call sees the updated state.
//!
//! All three boil down to the same shape: "given this usage and
//! these limits, is the call still allowed?". Hence one function.
//!
//! ## Daily cap reset
//!
//! The daily cap resets at UTC midnight. The evaluator does not read
//! the wall clock; the caller passes [`Now::epoch_secs`], and the
//! evaluator checks whether the supplied [`DailyState::day_epoch_secs`]
//! is still the same UTC day. If a fresh day has started, the running
//! spend is treated as zero — the caller is responsible for actually
//! resetting the state afterwards (the evaluator is read-only).
//!
//! ## Multi-tenant isolation
//!
//! There is no tenant id in this module. Callers must keep a separate
//! [`DailyState`] per tenant and pass the right one. The evaluator
//! never mixes state across tenants because it never holds state at
//! all.

/// Deployment-wide ASK caps. All durations are in milliseconds, all
/// sizes in bytes, all token counts in raw tokens (not characters).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// Hard ceiling on the assembled prompt size sent to the LLM.
    /// Default 8192. Exceeded → 413 `max_prompt_tokens`.
    pub max_prompt_tokens: u32,
    /// Hard ceiling on the streamed completion size.
    /// Default 1024. Exceeded → 413 `max_completion_tokens`.
    pub max_completion_tokens: u32,
    /// Hard ceiling on the total bytes of source payloads (the
    /// concatenated `sources_flat` content). Default 262_144.
    /// Exceeded → 413 `max_sources_bytes`.
    pub max_sources_bytes: u32,
    /// Hard wall-clock timeout for a single ASK call.
    /// Default 30_000. Exceeded → 504 `timeout_ms`.
    pub timeout_ms: u32,
    /// Optional per-tenant daily USD cap. `None` means unlimited.
    /// Exceeded → 413 `daily_cost_cap_usd`.
    pub daily_cost_cap_usd: Option<f64>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            max_prompt_tokens: 8192,
            max_completion_tokens: 1024,
            max_sources_bytes: 262_144,
            timeout_ms: 30_000,
            daily_cost_cap_usd: None,
        }
    }
}

/// Snapshot of what the current call has spent so far. For pre-call
/// checks, `completion_tokens` is 0; for in-flight checks the caller
/// supplies the running totals.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Usage {
    /// Tokens in the assembled prompt (system + sources + question).
    pub prompt_tokens: u32,
    /// Completion tokens emitted by the LLM so far.
    pub completion_tokens: u32,
    /// Total bytes across the assembled `sources_flat` payload.
    pub sources_bytes: u32,
    /// Estimated USD cost the current call will add to the daily
    /// counter once finished. Pre-call this is an estimate; post-call
    /// it should match the actual provider charge.
    pub estimated_cost_usd: f64,
    /// Wall-clock millis since the call started.
    pub elapsed_ms: u32,
}

/// Per-tenant running daily spend.
///
/// `day_epoch_secs` is the epoch-second at the *start* of the UTC day
/// the spend was accrued in. The evaluator compares it against
/// `now.epoch_secs` rounded down to the same UTC day; if they differ,
/// `spent_usd` is treated as 0 (the day rolled over).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DailyState {
    pub spent_usd: f64,
    pub day_epoch_secs: i64,
}

/// Injected clock — the evaluator must not read system time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Now {
    pub epoch_secs: i64,
}

/// Which cap tripped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    PromptTokens,
    CompletionTokens,
    SourcesBytes,
    Timeout,
    DailyCostCap,
}

impl LimitKind {
    /// Field name surfaced in the API error body so operators can grep
    /// for the offending knob in their config.
    pub fn field_name(self) -> &'static str {
        match self {
            LimitKind::PromptTokens => "max_prompt_tokens",
            LimitKind::CompletionTokens => "max_completion_tokens",
            LimitKind::SourcesBytes => "max_sources_bytes",
            LimitKind::Timeout => "timeout_ms",
            LimitKind::DailyCostCap => "daily_cost_cap_usd",
        }
    }

    /// HTTP status the API layer should return for this breach.
    /// Timeout is the only 504; everything else is 413.
    pub fn http_status(self) -> u16 {
        match self {
            LimitKind::Timeout => 504,
            _ => 413,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Allow,
    Reject {
        limit: LimitKind,
        http_status: u16,
        detail: String,
    },
}

/// Pure cap evaluation.
///
/// Check order is fixed and tested: prompt → sources → completion →
/// timeout → daily cap. The first breach wins — the evaluator does
/// not aggregate. This means if two limits are both tripped, the
/// caller sees the structurally-cheapest one first (cheaper to fix:
/// prompt assembly happens before the LLM call).
pub fn evaluate(usage: &Usage, daily: &DailyState, settings: &Settings, now: Now) -> Decision {
    if usage.prompt_tokens > settings.max_prompt_tokens {
        return reject(
            LimitKind::PromptTokens,
            format!(
                "prompt {} tokens exceeds max_prompt_tokens={}",
                usage.prompt_tokens, settings.max_prompt_tokens
            ),
        );
    }

    if usage.sources_bytes > settings.max_sources_bytes {
        return reject(
            LimitKind::SourcesBytes,
            format!(
                "sources payload {} bytes exceeds max_sources_bytes={}",
                usage.sources_bytes, settings.max_sources_bytes
            ),
        );
    }

    if usage.completion_tokens > settings.max_completion_tokens {
        return reject(
            LimitKind::CompletionTokens,
            format!(
                "completion {} tokens exceeds max_completion_tokens={}",
                usage.completion_tokens, settings.max_completion_tokens
            ),
        );
    }

    if usage.elapsed_ms > settings.timeout_ms {
        return reject(
            LimitKind::Timeout,
            format!(
                "elapsed {}ms exceeds timeout_ms={}",
                usage.elapsed_ms, settings.timeout_ms
            ),
        );
    }

    if let Some(cap) = settings.daily_cost_cap_usd {
        let effective_spent = if same_utc_day(daily.day_epoch_secs, now.epoch_secs) {
            daily.spent_usd
        } else {
            0.0
        };
        let projected = effective_spent + usage.estimated_cost_usd;
        if projected > cap {
            return reject(
                LimitKind::DailyCostCap,
                format!(
                    "projected spend ${projected:.6} exceeds daily_cost_cap_usd=${cap:.6}"
                ),
            );
        }
    }

    Decision::Allow
}

fn reject(limit: LimitKind, detail: String) -> Decision {
    Decision::Reject {
        limit,
        http_status: limit.http_status(),
        detail,
    }
}

const SECS_PER_DAY: i64 = 86_400;

/// Compare two epoch-seconds for "same UTC calendar day".
///
/// Floor division on `SECS_PER_DAY` gives the day index. Both inputs
/// can be negative (pre-1970); Rust's `i64::div_euclid` handles the
/// sign correctly. This is the test boundary for the daily reset — a
/// call where `now` lands one second past UTC midnight sees a fresh
/// `spent_usd = 0`.
fn same_utc_day(a: i64, b: i64) -> bool {
    a.div_euclid(SECS_PER_DAY) == b.div_euclid(SECS_PER_DAY)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings::default()
    }

    fn now_at(epoch_secs: i64) -> Now {
        Now { epoch_secs }
    }

    fn fresh_state() -> DailyState {
        DailyState::default()
    }

    fn ok_usage() -> Usage {
        Usage::default()
    }

    // ---- Limit boundaries -------------------------------------------------

    #[test]
    fn at_limit_is_allowed() {
        let s = settings();
        let u = Usage {
            prompt_tokens: s.max_prompt_tokens,
            completion_tokens: s.max_completion_tokens,
            sources_bytes: s.max_sources_bytes,
            elapsed_ms: s.timeout_ms,
            ..ok_usage()
        };
        assert_eq!(evaluate(&u, &fresh_state(), &s, now_at(0)), Decision::Allow);
    }

    #[test]
    fn one_over_prompt_tokens_rejects_413() {
        let s = settings();
        let u = Usage {
            prompt_tokens: s.max_prompt_tokens + 1,
            ..ok_usage()
        };
        let d = evaluate(&u, &fresh_state(), &s, now_at(0));
        match d {
            Decision::Reject {
                limit,
                http_status,
                detail,
            } => {
                assert_eq!(limit, LimitKind::PromptTokens);
                assert_eq!(http_status, 413);
                assert!(detail.contains("max_prompt_tokens"));
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn over_sources_bytes_rejects_413() {
        let s = settings();
        let u = Usage {
            sources_bytes: s.max_sources_bytes + 1,
            ..ok_usage()
        };
        let d = evaluate(&u, &fresh_state(), &s, now_at(0));
        match d {
            Decision::Reject {
                limit, http_status, ..
            } => {
                assert_eq!(limit, LimitKind::SourcesBytes);
                assert_eq!(http_status, 413);
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn over_completion_tokens_rejects_413() {
        let s = settings();
        let u = Usage {
            completion_tokens: s.max_completion_tokens + 1,
            ..ok_usage()
        };
        let d = evaluate(&u, &fresh_state(), &s, now_at(0));
        match d {
            Decision::Reject {
                limit, http_status, ..
            } => {
                assert_eq!(limit, LimitKind::CompletionTokens);
                assert_eq!(http_status, 413);
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn over_timeout_rejects_504() {
        let s = settings();
        let u = Usage {
            elapsed_ms: s.timeout_ms + 1,
            ..ok_usage()
        };
        let d = evaluate(&u, &fresh_state(), &s, now_at(0));
        match d {
            Decision::Reject {
                limit, http_status, ..
            } => {
                assert_eq!(limit, LimitKind::Timeout);
                assert_eq!(http_status, 504);
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    // ---- Daily cap --------------------------------------------------------

    #[test]
    fn daily_cap_none_means_unlimited() {
        let s = Settings {
            daily_cost_cap_usd: None,
            ..settings()
        };
        let u = Usage {
            estimated_cost_usd: 9_999.0,
            ..ok_usage()
        };
        let daily = DailyState {
            spent_usd: 1_000_000.0,
            day_epoch_secs: 0,
        };
        assert_eq!(evaluate(&u, &daily, &s, now_at(0)), Decision::Allow);
    }

    #[test]
    fn daily_cap_blocks_when_projected_exceeds() {
        let s = Settings {
            daily_cost_cap_usd: Some(10.0),
            ..settings()
        };
        let u = Usage {
            estimated_cost_usd: 2.5,
            ..ok_usage()
        };
        let daily = DailyState {
            spent_usd: 8.0,
            day_epoch_secs: 0,
        };
        let d = evaluate(&u, &daily, &s, now_at(0));
        match d {
            Decision::Reject {
                limit, http_status, ..
            } => {
                assert_eq!(limit, LimitKind::DailyCostCap);
                assert_eq!(http_status, 413);
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn daily_cap_allows_at_exact_cap() {
        // Boundary: projected == cap is NOT a breach (strict >).
        let s = Settings {
            daily_cost_cap_usd: Some(10.0),
            ..settings()
        };
        let u = Usage {
            estimated_cost_usd: 2.0,
            ..ok_usage()
        };
        let daily = DailyState {
            spent_usd: 8.0,
            day_epoch_secs: 0,
        };
        assert_eq!(evaluate(&u, &daily, &s, now_at(0)), Decision::Allow);
    }

    #[test]
    fn daily_cap_resets_at_utc_midnight() {
        // State was accrued on day 0; now is 1 second into day 1.
        // Previous spend must be ignored — fresh budget.
        let s = Settings {
            daily_cost_cap_usd: Some(10.0),
            ..settings()
        };
        let u = Usage {
            estimated_cost_usd: 9.0,
            ..ok_usage()
        };
        let day_zero_start = 0;
        let day_one_start_plus_1 = SECS_PER_DAY + 1;
        let daily = DailyState {
            spent_usd: 100.0,
            day_epoch_secs: day_zero_start,
        };
        assert_eq!(
            evaluate(&u, &daily, &s, now_at(day_one_start_plus_1)),
            Decision::Allow,
            "stale spend from yesterday must not count against today",
        );
    }

    #[test]
    fn daily_cap_same_day_other_seconds_does_not_reset() {
        // Both timestamps land in the same UTC day; old spend stays.
        let s = Settings {
            daily_cost_cap_usd: Some(10.0),
            ..settings()
        };
        let u = Usage {
            estimated_cost_usd: 5.0,
            ..ok_usage()
        };
        let daily = DailyState {
            spent_usd: 9.0,
            day_epoch_secs: 0,
        };
        // 12:34 UTC same day
        let now_same_day = 45_240;
        let d = evaluate(&u, &daily, &s, now_at(now_same_day));
        assert!(matches!(
            d,
            Decision::Reject {
                limit: LimitKind::DailyCostCap,
                ..
            }
        ));
    }

    // ---- Check order ------------------------------------------------------

    #[test]
    fn prompt_check_fires_before_completion_check() {
        // Both tripped — caller sees PromptTokens (cheaper to act on).
        let s = settings();
        let u = Usage {
            prompt_tokens: s.max_prompt_tokens + 1,
            completion_tokens: s.max_completion_tokens + 1,
            ..ok_usage()
        };
        let d = evaluate(&u, &fresh_state(), &s, now_at(0));
        match d {
            Decision::Reject { limit, .. } => assert_eq!(limit, LimitKind::PromptTokens),
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn timeout_check_fires_before_daily_cap() {
        let s = Settings {
            daily_cost_cap_usd: Some(0.0),
            ..settings()
        };
        let u = Usage {
            estimated_cost_usd: 1.0,
            elapsed_ms: s.timeout_ms + 1,
            ..ok_usage()
        };
        let d = evaluate(&u, &fresh_state(), &s, now_at(0));
        match d {
            Decision::Reject { limit, .. } => assert_eq!(limit, LimitKind::Timeout),
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    // ---- Multi-tenant isolation ------------------------------------------

    #[test]
    fn separate_daily_states_do_not_interact() {
        // Tenant A is over cap, tenant B is fresh. Same settings.
        let s = Settings {
            daily_cost_cap_usd: Some(5.0),
            ..settings()
        };
        let u = Usage {
            estimated_cost_usd: 1.0,
            ..ok_usage()
        };
        let tenant_a = DailyState {
            spent_usd: 4.5,
            day_epoch_secs: 0,
        };
        let tenant_b = DailyState {
            spent_usd: 0.0,
            day_epoch_secs: 0,
        };
        assert!(matches!(
            evaluate(&u, &tenant_a, &s, now_at(0)),
            Decision::Reject {
                limit: LimitKind::DailyCostCap,
                ..
            }
        ));
        assert_eq!(evaluate(&u, &tenant_b, &s, now_at(0)), Decision::Allow);
    }

    // ---- Field/status surface --------------------------------------------

    #[test]
    fn field_names_match_settings_keys() {
        // The detail message and field_name must reference the same
        // operator-visible config key — operators grep the error to
        // find which knob to bump.
        assert_eq!(LimitKind::PromptTokens.field_name(), "max_prompt_tokens");
        assert_eq!(
            LimitKind::CompletionTokens.field_name(),
            "max_completion_tokens"
        );
        assert_eq!(LimitKind::SourcesBytes.field_name(), "max_sources_bytes");
        assert_eq!(LimitKind::Timeout.field_name(), "timeout_ms");
        assert_eq!(LimitKind::DailyCostCap.field_name(), "daily_cost_cap_usd");
    }

    #[test]
    fn http_status_mapping() {
        assert_eq!(LimitKind::PromptTokens.http_status(), 413);
        assert_eq!(LimitKind::CompletionTokens.http_status(), 413);
        assert_eq!(LimitKind::SourcesBytes.http_status(), 413);
        assert_eq!(LimitKind::DailyCostCap.http_status(), 413);
        assert_eq!(LimitKind::Timeout.http_status(), 504);
    }

    // ---- Default settings pinned -----------------------------------------

    #[test]
    fn defaults_match_spec() {
        let s = Settings::default();
        assert_eq!(s.max_prompt_tokens, 8192);
        assert_eq!(s.max_completion_tokens, 1024);
        assert_eq!(s.max_sources_bytes, 262_144);
        assert_eq!(s.timeout_ms, 30_000);
        assert_eq!(s.daily_cost_cap_usd, None);
    }

    // ---- Determinism / purity --------------------------------------------

    #[test]
    fn evaluation_is_deterministic() {
        let s = Settings {
            daily_cost_cap_usd: Some(10.0),
            ..settings()
        };
        let u = Usage {
            prompt_tokens: 100,
            completion_tokens: 50,
            sources_bytes: 1000,
            estimated_cost_usd: 0.5,
            elapsed_ms: 1234,
        };
        let daily = DailyState {
            spent_usd: 1.0,
            day_epoch_secs: 0,
        };
        let a = evaluate(&u, &daily, &s, now_at(500));
        let b = evaluate(&u, &daily, &s, now_at(500));
        assert_eq!(a, b);
    }

    #[test]
    fn same_utc_day_negative_epoch() {
        // Pre-1970 timestamps must round correctly (div_euclid).
        // -1 second is still UTC day -1, not day 0.
        assert!(same_utc_day(-1, -1));
        assert!(!same_utc_day(-1, 0));
        assert!(same_utc_day(0, SECS_PER_DAY - 1));
        assert!(!same_utc_day(0, SECS_PER_DAY));
    }
}
