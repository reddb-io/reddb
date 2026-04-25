//! Time helpers used across the engine. Centralised so the same
//! `SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default()`
//! incantation doesn't get re-typed at 20+ call sites.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current wall-clock unix milliseconds. `0` if the system clock is
/// before the epoch (impossible on real hardware, but the API
/// returns a `Result` so we degrade gracefully instead of panicking).
#[inline]
pub fn now_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Current wall-clock unix nanoseconds.
#[inline]
pub fn now_unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Current wall-clock unix seconds.
#[inline]
pub fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_returns_positive_values() {
        let ms = now_unix_millis();
        let ns = now_unix_nanos();
        let s = now_unix_secs();
        assert!(ms > 1_700_000_000_000, "ms must be after 2023");
        assert!(ns > 1_700_000_000_000_000_000, "ns must be after 2023");
        assert!(s > 1_700_000_000, "s must be after 2023");
    }
}
