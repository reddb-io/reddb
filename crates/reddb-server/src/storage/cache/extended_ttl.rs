//! Extended TTL Policy & Effective Expiry
//!
//! Deep module that layers three additional expiry behaviours on top of the
//! base "hard expiry" already computed by [`crate::storage::cache::blob::BlobCachePolicy`]:
//!
//! 1. **Idle TTL (sliding expiry):** an entry is killed if it has not been
//!    accessed within `idle_ttl_ms`, even if its hard expiry is in the future.
//! 2. **Stale-While-Revalidate (SWR) window:** after the hard expiry passes,
//!    the entry can still be served as `Stale` for `stale_serve_ms`. Past that
//!    cumulative point, it is `Expired`.
//! 3. **Jitter at insert time:** a deterministic, seed-driven offset of
//!    `[0, jitter_pct]` percent of the base TTL. Used to avoid synchronized
//!    cache stampedes when many entries are written in lockstep.
//!
//! ## Design Rules
//!
//! - **Hard expiry always wins.** No idle/stale config can resurrect an entry
//!   past `hard_expires_at + stale_serve_ms`.
//! - **Pure functions only.** No clocks, no allocators, no global state. The
//!   caller supplies `now`, `last_access`, and pre-computed hard expiry.
//! - **Stdlib-only.** No `rand`, no `chrono`, no `serde`. Jitter uses a small
//!   LCG seeded by the caller.
//!
//! This module is **additive**: integration with `BlobCachePolicy::ttl_ms`
//! and `BlobCache::get` is a sequential follow-up. See module-level TODO
//! comments in `mod.rs` (deferred).

/// Extended TTL configuration that augments a base [`BlobCachePolicy`].
///
/// Construct via field literal or [`ExtendedTtlPolicy::off`]. All three knobs
/// are independently optional / zero-able; [`Self::is_active`] reports whether
/// any of them affects expiry decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtendedTtlPolicy {
    /// Sliding expiry: if the entry has not been accessed within this many
    /// milliseconds, it is considered expired even if hard TTL has not passed.
    /// `None` disables idle expiry.
    pub idle_ttl_ms: Option<u64>,

    /// Stale-while-revalidate window in milliseconds. After the hard expiry,
    /// the entry can still be served as [`ExpiryDecision::Stale`] for this
    /// many milliseconds before becoming [`ExpiryDecision::Expired`].
    /// `None` disables the SWR window (hard expiry → immediate Expired).
    pub stale_serve_ms: Option<u64>,

    /// Jitter percentage applied at insert time, clamped to `0..=100`.
    /// `0` disables jitter; values above `100` are treated as `100`.
    pub jitter_pct: u8,
}

impl ExtendedTtlPolicy {
    /// Returns the no-op extended policy: no idle, no SWR, no jitter.
    /// In this state, `EffectiveExpiry::compute` reduces to a pure
    /// hard-expiry check.
    pub fn off() -> Self {
        Self {
            idle_ttl_ms: None,
            stale_serve_ms: None,
            jitter_pct: 0,
        }
    }

    /// Reports whether this policy alters expiry behaviour relative to
    /// raw hard-expiry semantics.
    pub fn is_active(&self) -> bool {
        self.idle_ttl_ms.is_some() || self.stale_serve_ms.is_some() || self.jitter_pct > 0
    }
}

impl Default for ExtendedTtlPolicy {
    fn default() -> Self {
        Self::off()
    }
}

/// Outcome of an [`EffectiveExpiry::compute`] call.
///
/// Hard expiry always wins: once the cumulative `hard + stale_serve` window
/// is exhausted, no input can produce anything other than [`Self::Expired`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiryDecision {
    /// Entry is fully valid and should be served normally.
    Fresh,

    /// Hard expiry has passed but the entry is within its SWR window.
    /// Caller may serve the cached value while triggering an async refresh.
    Stale {
        /// Milliseconds remaining in the stale window. When this reaches
        /// zero on the next tick, the decision flips to `Expired`.
        window_remaining_ms: u64,
    },

    /// Entry must not be served. Either:
    /// - hard expiry + stale window is exhausted, or
    /// - idle TTL was exceeded since last access.
    Expired,
}

/// Stateless calculator for effective expiry decisions.
///
/// All methods are pure: they take inputs by value/reference, do no I/O,
/// and never read the system clock. The caller is responsible for supplying
/// a monotonically-correct `now_unix_ms`.
pub struct EffectiveExpiry;

impl EffectiveExpiry {
    /// Compute the effective expiry decision for a cache entry.
    ///
    /// # Arguments
    ///
    /// - `hard_expires_at_unix_ms` — pre-computed hard expiry from the base
    ///   `BlobCachePolicy`. `None` means "no hard expiry" (entry never
    ///   hard-expires; only idle TTL can kill it).
    /// - `now_unix_ms` — current time, supplied by caller.
    /// - `last_access_unix_ms` — wall-clock time of the most recent access.
    /// - `extended` — extended policy knobs.
    ///
    /// # Decision Order
    ///
    /// 1. **Idle TTL** — if configured and `now - last_access > idle_ttl`,
    ///    return `Expired` regardless of hard expiry. Idle is checked first
    ///    because an idle-killed entry is dead even within its hard window.
    /// 2. **No hard expiry** — if `hard_expires_at_unix_ms` is `None`, the
    ///    only remaining gate is idle (already passed) → `Fresh`.
    /// 3. **Within hard expiry** — `now <= hard` → `Fresh`.
    /// 4. **Within SWR window** — `now <= hard + stale_serve_ms` → `Stale`.
    /// 5. **Otherwise** → `Expired`.
    pub fn compute(
        hard_expires_at_unix_ms: Option<u64>,
        now_unix_ms: u64,
        last_access_unix_ms: u64,
        extended: &ExtendedTtlPolicy,
    ) -> ExpiryDecision {
        // 1. Idle TTL gate. Saturating_sub guards against clock skew where
        //    last_access could be slightly ahead of now (treat as zero idle).
        if let Some(idle_ttl_ms) = extended.idle_ttl_ms {
            let idle_ms = now_unix_ms.saturating_sub(last_access_unix_ms);
            if idle_ms > idle_ttl_ms {
                return ExpiryDecision::Expired;
            }
        }

        // 2. No hard expiry → Fresh (idle already cleared).
        let Some(hard) = hard_expires_at_unix_ms else {
            return ExpiryDecision::Fresh;
        };

        // 3. Within hard expiry.
        if now_unix_ms <= hard {
            return ExpiryDecision::Fresh;
        }

        // 4. Past hard expiry: check SWR window.
        let stale_window = extended.stale_serve_ms.unwrap_or(0);
        if stale_window == 0 {
            return ExpiryDecision::Expired;
        }

        // saturating_add prevents overflow when hard is near u64::MAX.
        let stale_deadline = hard.saturating_add(stale_window);
        if now_unix_ms <= stale_deadline {
            // Subtraction is safe: now > hard and now <= hard + stale_window
            // imply stale_deadline >= now.
            ExpiryDecision::Stale {
                window_remaining_ms: stale_deadline - now_unix_ms,
            }
        } else {
            ExpiryDecision::Expired
        }
    }

    /// Compute a jittered TTL given a base TTL, a jitter percentage, and a
    /// deterministic seed.
    ///
    /// Returns `base_ttl_ms + base_ttl_ms * offset / 100` where `offset`
    /// is in `[0, jitter_pct]` (clamped to `0..=100`). When `jitter_pct == 0`
    /// the result is exactly `base_ttl_ms`.
    ///
    /// Uses a small LCG (Numerical Recipes constants) so callers can supply
    /// any `u64` seed (entry hash, write timestamp, etc.) without needing
    /// `rand`. Same seed + same inputs → same result, always.
    ///
    /// Saturates on overflow rather than panicking.
    pub fn jittered_ttl_ms(base_ttl_ms: u64, jitter_pct: u8, seed: u64) -> u64 {
        let pct = jitter_pct.min(100) as u64;
        if pct == 0 || base_ttl_ms == 0 {
            return base_ttl_ms;
        }

        // Numerical Recipes LCG: x_{n+1} = 1664525 * x_n + 1013904223 mod 2^64
        // One step is sufficient for "spread", we don't need crypto quality.
        let mixed = seed
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);

        // offset ∈ [0, pct] inclusive → pct+1 buckets.
        let offset = mixed % (pct + 1);

        // base + base * offset / 100, with saturation at every step.
        let extra = base_ttl_ms.saturating_mul(offset) / 100;
        base_ttl_ms.saturating_add(extra)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // ExtendedTtlPolicy::off / is_active
    // ------------------------------------------------------------------

    #[test]
    fn off_is_inactive() {
        let p = ExtendedTtlPolicy::off();
        assert_eq!(p.idle_ttl_ms, None);
        assert_eq!(p.stale_serve_ms, None);
        assert_eq!(p.jitter_pct, 0);
        assert!(!p.is_active());
    }

    #[test]
    fn default_matches_off() {
        assert_eq!(ExtendedTtlPolicy::default(), ExtendedTtlPolicy::off());
    }

    #[test]
    fn any_field_set_makes_active() {
        assert!(ExtendedTtlPolicy {
            idle_ttl_ms: Some(1),
            stale_serve_ms: None,
            jitter_pct: 0,
        }
        .is_active());
        assert!(ExtendedTtlPolicy {
            idle_ttl_ms: None,
            stale_serve_ms: Some(1),
            jitter_pct: 0,
        }
        .is_active());
        assert!(ExtendedTtlPolicy {
            idle_ttl_ms: None,
            stale_serve_ms: None,
            jitter_pct: 1,
        }
        .is_active());
    }

    // ------------------------------------------------------------------
    // Hard expiry always wins
    // ------------------------------------------------------------------

    #[test]
    fn hard_expiry_always_wins_proptest_style() {
        // For any extended config, now > hard + stale ⇒ Expired.
        // We sweep a representative grid — full proptest crate not available
        // and the spec said "no external deps".
        let hards = [10u64, 100, 1_000, 10_000, u64::MAX / 2];
        let stales = [0u64, 1, 50, 1_000, 1_000_000];
        let idles = [None, Some(1u64), Some(10_000), Some(u64::MAX)];
        let jitters = [0u8, 25, 50, 100, 250];

        for &hard in &hards {
            for &stale in &stales {
                for &idle in &idles {
                    for &jitter in &jitters {
                        let ext = ExtendedTtlPolicy {
                            idle_ttl_ms: idle,
                            stale_serve_ms: Some(stale),
                            jitter_pct: jitter,
                        };
                        let now = hard.saturating_add(stale).saturating_add(1);
                        // last_access = now keeps idle from firing accidentally.
                        let decision = EffectiveExpiry::compute(Some(hard), now, now, &ext);
                        assert_eq!(
                            decision,
                            ExpiryDecision::Expired,
                            "hard={hard} stale={stale} idle={idle:?} jitter={jitter} now={now}",
                        );
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Idle TTL behaviour
    // ------------------------------------------------------------------

    #[test]
    fn idle_ttl_kills_entry() {
        let ext = ExtendedTtlPolicy {
            idle_ttl_ms: Some(100),
            stale_serve_ms: None,
            jitter_pct: 0,
        };
        // last_access=0, now=101 → idle = 101 > 100 → Expired
        let d = EffectiveExpiry::compute(Some(10_000), 101, 0, &ext);
        assert_eq!(d, ExpiryDecision::Expired);
    }

    #[test]
    fn idle_ttl_resets_on_access() {
        let ext = ExtendedTtlPolicy {
            idle_ttl_ms: Some(100),
            stale_serve_ms: None,
            jitter_pct: 0,
        };
        // last_access = now - 50, idle = 50 ≤ 100, hard far away → Fresh
        let d = EffectiveExpiry::compute(Some(10_000), 1_000, 950, &ext);
        assert_eq!(d, ExpiryDecision::Fresh);
    }

    #[test]
    fn idle_ttl_at_boundary_is_fresh() {
        // Spec: "now - last_access > idle_ttl_ms" — strictly greater.
        // Equal means still alive.
        let ext = ExtendedTtlPolicy {
            idle_ttl_ms: Some(100),
            stale_serve_ms: None,
            jitter_pct: 0,
        };
        let d = EffectiveExpiry::compute(Some(10_000), 100, 0, &ext);
        assert_eq!(d, ExpiryDecision::Fresh);
    }

    #[test]
    fn idle_ttl_handles_clock_skew() {
        // last_access slightly ahead of now — saturating_sub → idle = 0.
        let ext = ExtendedTtlPolicy {
            idle_ttl_ms: Some(100),
            stale_serve_ms: None,
            jitter_pct: 0,
        };
        let d = EffectiveExpiry::compute(Some(10_000), 500, 510, &ext);
        assert_eq!(d, ExpiryDecision::Fresh);
    }

    // ------------------------------------------------------------------
    // Stale window behaviour
    // ------------------------------------------------------------------

    #[test]
    fn stale_window_fires_after_hard() {
        // hard=100, stale=50, now=120 → Stale { window_remaining_ms: 30 }
        let ext = ExtendedTtlPolicy {
            idle_ttl_ms: None,
            stale_serve_ms: Some(50),
            jitter_pct: 0,
        };
        let d = EffectiveExpiry::compute(Some(100), 120, 120, &ext);
        assert_eq!(d, ExpiryDecision::Stale { window_remaining_ms: 30 });
    }

    #[test]
    fn stale_window_at_exact_boundary() {
        // now = hard + stale → still Stale with 0 ms remaining
        let ext = ExtendedTtlPolicy {
            idle_ttl_ms: None,
            stale_serve_ms: Some(50),
            jitter_pct: 0,
        };
        let d = EffectiveExpiry::compute(Some(100), 150, 150, &ext);
        assert_eq!(d, ExpiryDecision::Stale { window_remaining_ms: 0 });
    }

    #[test]
    fn stale_window_expires() {
        // hard=100, stale=50, now=151 → Expired
        let ext = ExtendedTtlPolicy {
            idle_ttl_ms: None,
            stale_serve_ms: Some(50),
            jitter_pct: 0,
        };
        let d = EffectiveExpiry::compute(Some(100), 151, 151, &ext);
        assert_eq!(d, ExpiryDecision::Expired);
    }

    #[test]
    fn no_stale_config_immediate_expired() {
        // hard=100, now=101, no stale window → Expired
        let ext = ExtendedTtlPolicy {
            idle_ttl_ms: None,
            stale_serve_ms: None,
            jitter_pct: 0,
        };
        let d = EffectiveExpiry::compute(Some(100), 101, 101, &ext);
        assert_eq!(d, ExpiryDecision::Expired);
    }

    #[test]
    fn stale_zero_acts_like_no_stale() {
        let ext = ExtendedTtlPolicy {
            idle_ttl_ms: None,
            stale_serve_ms: Some(0),
            jitter_pct: 0,
        };
        let d = EffectiveExpiry::compute(Some(100), 101, 101, &ext);
        assert_eq!(d, ExpiryDecision::Expired);
    }

    #[test]
    fn within_hard_is_fresh_even_with_stale_configured() {
        let ext = ExtendedTtlPolicy {
            idle_ttl_ms: None,
            stale_serve_ms: Some(1_000),
            jitter_pct: 0,
        };
        let d = EffectiveExpiry::compute(Some(100), 50, 50, &ext);
        assert_eq!(d, ExpiryDecision::Fresh);
    }

    #[test]
    fn hard_at_exact_boundary_is_fresh() {
        // Spec: "now <= hard" → Fresh. So now == hard is still Fresh.
        let ext = ExtendedTtlPolicy::off();
        let d = EffectiveExpiry::compute(Some(100), 100, 100, &ext);
        assert_eq!(d, ExpiryDecision::Fresh);
    }

    #[test]
    fn no_hard_expiry_is_fresh() {
        let ext = ExtendedTtlPolicy::off();
        let d = EffectiveExpiry::compute(None, u64::MAX, 0, &ext);
        assert_eq!(d, ExpiryDecision::Fresh);
    }

    #[test]
    fn no_hard_but_idle_still_kills() {
        let ext = ExtendedTtlPolicy {
            idle_ttl_ms: Some(50),
            stale_serve_ms: None,
            jitter_pct: 0,
        };
        let d = EffectiveExpiry::compute(None, 100, 0, &ext);
        assert_eq!(d, ExpiryDecision::Expired);
    }

    // ------------------------------------------------------------------
    // off() interaction with compute
    // ------------------------------------------------------------------

    #[test]
    fn off_compute_never_returns_stale() {
        let ext = ExtendedTtlPolicy::off();
        // Sweep a grid — every result must be Fresh or Expired, never Stale.
        for &hard in &[0u64, 1, 100, 10_000, u64::MAX] {
            for &now in &[0u64, 1, 99, 100, 101, 10_001, u64::MAX] {
                let d = EffectiveExpiry::compute(Some(hard), now, now, &ext);
                assert!(
                    matches!(d, ExpiryDecision::Fresh | ExpiryDecision::Expired),
                    "off() must not produce Stale: hard={hard} now={now} got {d:?}",
                );
            }
        }
    }

    // ------------------------------------------------------------------
    // Jitter
    // ------------------------------------------------------------------

    #[test]
    fn jitter_zero_is_identity() {
        for base in [0u64, 1, 100, 1_000, 10_000_000] {
            for seed in [0u64, 1, 42, u64::MAX] {
                assert_eq!(
                    EffectiveExpiry::jittered_ttl_ms(base, 0, seed),
                    base,
                    "base={base} seed={seed}",
                );
            }
        }
    }

    #[test]
    fn jitter_zero_base_is_zero() {
        // Edge case: 0 base TTL stays 0 regardless of jitter.
        for pct in [0u8, 25, 100] {
            assert_eq!(EffectiveExpiry::jittered_ttl_ms(0, pct, 12345), 0);
        }
    }

    #[test]
    fn jitter_bound_1000_calls() {
        // 1000 calls with base=1000, pct=20 → all in [1000, 1200].
        let base = 1_000u64;
        let pct = 20u8;
        for seed in 0u64..1_000 {
            let v = EffectiveExpiry::jittered_ttl_ms(base, pct, seed);
            assert!(
                (1_000..=1_200).contains(&v),
                "seed={seed} v={v} out of [1000, 1200]",
            );
        }
    }

    #[test]
    fn jitter_deterministic() {
        let base = 5_000u64;
        let pct = 50u8;
        for seed in [0u64, 1, 42, 999, u64::MAX, 0xDEAD_BEEF] {
            let a = EffectiveExpiry::jittered_ttl_ms(base, pct, seed);
            let b = EffectiveExpiry::jittered_ttl_ms(base, pct, seed);
            assert_eq!(a, b, "seed={seed}: {a} != {b}");
        }
    }

    #[test]
    fn jitter_pct_clamps_above_100() {
        // pct > 100 should be treated as 100 → max output base + base = 2*base.
        let base = 1_000u64;
        for seed in 0u64..200 {
            let v = EffectiveExpiry::jittered_ttl_ms(base, 250, seed);
            assert!(
                (1_000..=2_000).contains(&v),
                "seed={seed} v={v} out of [1000, 2000] for clamped pct",
            );
        }
    }

    #[test]
    fn jitter_distribution_covers_range() {
        // Sanity: with pct=100 and 10k seeds, we should hit both low and
        // high ends. Not a statistical test — just a smoke check that the
        // LCG isn't degenerate.
        let base = 1_000u64;
        let mut min_seen = u64::MAX;
        let mut max_seen = 0u64;
        for seed in 0u64..10_000 {
            let v = EffectiveExpiry::jittered_ttl_ms(base, 100, seed);
            min_seen = min_seen.min(v);
            max_seen = max_seen.max(v);
        }
        assert!(min_seen <= 1_100, "min_seen={min_seen} too high");
        assert!(max_seen >= 1_900, "max_seen={max_seen} too low");
    }

    #[test]
    fn jitter_saturates_on_overflow() {
        // base near u64::MAX with pct=100 — must not panic.
        let v = EffectiveExpiry::jittered_ttl_ms(u64::MAX, 100, 1);
        assert_eq!(v, u64::MAX);
    }

    // ------------------------------------------------------------------
    // Combinatorial "proptest-style" cross-check against reference impl
    // ------------------------------------------------------------------

    /// Reference implementation, written line-by-line from the spec.
    /// Used to verify the optimized `compute` over a wide grid.
    fn reference_decision(
        hard: Option<u64>,
        now: u64,
        last_access: u64,
        ext: &ExtendedTtlPolicy,
    ) -> ExpiryDecision {
        // Step 1: idle TTL.
        if let Some(idle) = ext.idle_ttl_ms {
            let elapsed = if now >= last_access { now - last_access } else { 0 };
            if elapsed > idle {
                return ExpiryDecision::Expired;
            }
        }
        // Step 2: no hard → Fresh.
        let h = match hard {
            None => return ExpiryDecision::Fresh,
            Some(v) => v,
        };
        // Step 3: within hard.
        if now <= h {
            return ExpiryDecision::Fresh;
        }
        // Step 4: stale window.
        let stale = ext.stale_serve_ms.unwrap_or(0);
        if stale == 0 {
            return ExpiryDecision::Expired;
        }
        let deadline = h.saturating_add(stale);
        if now <= deadline {
            ExpiryDecision::Stale {
                window_remaining_ms: deadline - now,
            }
        } else {
            ExpiryDecision::Expired
        }
    }

    #[test]
    fn combinatorial_matches_reference() {
        // Deterministic LCG-driven sweep — generates ~5000 tuples without
        // pulling proptest. Each tuple is checked against the reference.
        let mut state: u64 = 0x1234_5678_9ABC_DEF0;
        for _ in 0..5_000 {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let hard = if state & 1 == 0 {
                None
            } else {
                Some((state >> 1) % 10_000)
            };
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let now = state % 12_000;
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let last_access = state % 12_000;
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let idle = if state & 1 == 0 { None } else { Some((state >> 1) % 5_000) };
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let stale = if state & 1 == 0 { None } else { Some((state >> 1) % 5_000) };
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let jitter = (state % 256) as u8;

            let ext = ExtendedTtlPolicy {
                idle_ttl_ms: idle,
                stale_serve_ms: stale,
                jitter_pct: jitter,
            };

            let actual = EffectiveExpiry::compute(hard, now, last_access, &ext);
            let expected = reference_decision(hard, now, last_access, &ext);
            assert_eq!(
                actual, expected,
                "hard={hard:?} now={now} last={last_access} ext={ext:?}",
            );
        }
    }
}
