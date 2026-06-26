//! CPU/RAM occupancy sampler — issue #1244 (PRD #1237, Phase C).
//!
//! Reads system-wide CPU and RAM occupancy on platforms where the engine
//! can obtain reliable measurements (currently Linux via `/proc`). On
//! unsupported platforms every method returns `None` so the
//! `/cluster/status` renderer keeps the honest `unavailable` envelope
//! (ADR 0060 §6 / honesty rule #738).
//!
//! ## Overhead
//!
//! `sample()` reads `/proc/stat` and `/proc/meminfo` — virtual files
//! served from kernel memory with no disk I/O. The call is intended to
//! be made at most once per `/cluster/status` scrape; it is NOT on the
//! query hot-path. Typical wall-clock cost is ≤ 100 µs per call.
//! CPU usage is computed as a delta between consecutive `sample()`
//! calls; the first call returns `None` for `cpu_usage` (no prior
//! reading exists yet) and real values on all subsequent calls.
//!
//! ## Interval
//!
//! The sampler is stateless between calls — the caller drives the
//! interval implicitly by how often it invokes `sample()`. No background
//! thread is spawned; there is no timer.

use std::sync::Mutex;

#[cfg(target_os = "linux")]
struct CpuSnapshot {
    busy: u64,
    total: u64,
}

struct SamplerState {
    #[cfg(target_os = "linux")]
    last_cpu: Option<CpuSnapshot>,
    cpu_usage: Option<f64>,
    ram_usage: Option<f64>,
}

impl std::fmt::Debug for SamplerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SamplerState")
            .field("cpu_usage", &self.cpu_usage)
            .field("ram_usage", &self.ram_usage)
            .finish_non_exhaustive()
    }
}

/// Process-local CPU/RAM occupancy sampler.
///
/// Call [`OccupancySampler::sample`] from the `/cluster/status` handler
/// to advance the measurement window and obtain the latest values. The
/// returned [`OccupancySample`] carries `None` in each field until a
/// real measurement is available (see module docs).
#[derive(Debug)]
pub struct OccupancySampler {
    state: Mutex<SamplerState>,
}

impl Default for OccupancySampler {
    fn default() -> Self {
        Self {
            state: Mutex::new(SamplerState {
                #[cfg(target_os = "linux")]
                last_cpu: None,
                cpu_usage: None,
                ram_usage: None,
            }),
        }
    }
}

/// Point-in-time occupancy values from a single [`OccupancySampler::sample`] call.
#[derive(Debug, Clone, Copy)]
pub struct OccupancySample {
    /// System-wide CPU usage as a fraction in `0.0..=1.0`.
    /// `None` on the first call (no delta yet) or on unsupported platforms.
    pub cpu_usage: Option<f64>,
    /// RAM occupancy (used / total) as a fraction in `0.0..=1.0`.
    /// `None` when total memory is unknown or on unsupported platforms.
    pub ram_usage: Option<f64>,
}

impl OccupancySampler {
    /// Advance the measurement window and return the latest occupancy values.
    ///
    /// The first call always returns `cpu_usage: None` because CPU usage
    /// requires two readings to compute a delta. All subsequent calls
    /// return real values as long as the platform supports measurement.
    pub fn sample(&self) -> OccupancySample {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Self::update(&mut state);
        OccupancySample {
            cpu_usage: state.cpu_usage,
            ram_usage: state.ram_usage,
        }
    }

    /// Return the most recently computed values without advancing the
    /// measurement window. Returns `None` in both fields before the
    /// first `sample()` call.
    pub fn current(&self) -> OccupancySample {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        OccupancySample {
            cpu_usage: state.cpu_usage,
            ram_usage: state.ram_usage,
        }
    }

    #[cfg(target_os = "linux")]
    fn update(state: &mut SamplerState) {
        if let Some(snap) = Self::read_cpu_stat() {
            if let Some(prev) = &state.last_cpu {
                let busy_delta = snap.busy.saturating_sub(prev.busy);
                let total_delta = snap.total.saturating_sub(prev.total);
                if total_delta > 0 {
                    state.cpu_usage =
                        Some((busy_delta as f64 / total_delta as f64).clamp(0.0, 1.0));
                }
            }
            state.last_cpu = Some(snap);
        }
        state.ram_usage = Self::read_ram_usage();
    }

    #[cfg(not(target_os = "linux"))]
    fn update(_state: &mut SamplerState) {
        // Platform does not expose CPU/RAM via /proc. Leave all fields
        // as None so the presentation layer emits honest unavailable
        // envelopes (ADR 0060 §6).
    }

    #[cfg(target_os = "linux")]
    fn read_cpu_stat() -> Option<CpuSnapshot> {
        // First line of /proc/stat:
        //   cpu  user nice system idle iowait irq softirq steal ...
        let raw = std::fs::read_to_string("/proc/stat").ok()?;
        let first = raw.lines().next()?;
        let mut parts = first.split_ascii_whitespace();
        if parts.next() != Some("cpu") {
            return None;
        }
        let mut fields = [0u64; 10];
        for f in fields.iter_mut() {
            *f = parts.next()?.parse().ok()?;
        }
        // busy = user + nice + system + irq + softirq + steal
        let (user, nice, system, idle, iowait, irq, softirq, steal) = (
            fields[0], fields[1], fields[2], fields[3], fields[4], fields[5], fields[6], fields[7],
        );
        let busy = user + nice + system + irq + softirq + steal;
        let total = busy + idle + iowait;
        Some(CpuSnapshot { busy, total })
    }

    #[cfg(target_os = "linux")]
    fn read_ram_usage() -> Option<f64> {
        let raw = std::fs::read_to_string("/proc/meminfo").ok()?;
        let mut total_kb = None::<u64>;
        let mut available_kb = None::<u64>;
        for line in raw.lines() {
            if line.starts_with("MemTotal:") {
                total_kb = line.split_whitespace().nth(1).and_then(|v| v.parse().ok());
            } else if line.starts_with("MemAvailable:") {
                available_kb = line.split_whitespace().nth(1).and_then(|v| v.parse().ok());
            }
            if total_kb.is_some() && available_kb.is_some() {
                break;
            }
        }
        let total = total_kb?;
        let available = available_kb?;
        if total == 0 {
            return None;
        }
        let used = total.saturating_sub(available) as f64;
        Some((used / total as f64).clamp(0.0, 1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sample_returns_none_for_cpu_no_delta_yet() {
        let sampler = OccupancySampler::default();
        let s = sampler.sample();
        assert!(
            s.cpu_usage.is_none(),
            "first sample must return None for cpu_usage — no delta exists yet"
        );
    }

    #[test]
    fn current_returns_none_before_first_sample() {
        let sampler = OccupancySampler::default();
        let s = sampler.current();
        assert!(s.cpu_usage.is_none());
        assert!(s.ram_usage.is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn second_sample_returns_measured_cpu_on_linux() {
        let sampler = OccupancySampler::default();
        sampler.sample(); // seed the first reading
                          // Do some work so the delta is measurable.
        let _ = (0u64..200_000).fold(0u64, |a, b| a.wrapping_add(b));
        let s = sampler.sample();
        let usage = s
            .cpu_usage
            .expect("second sample must return cpu_usage on Linux");
        assert!(
            (0.0..=1.0).contains(&usage),
            "cpu_usage must be in [0.0, 1.0], got {usage}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ram_usage_is_measured_on_linux() {
        let sampler = OccupancySampler::default();
        let s = sampler.sample();
        let usage = s.ram_usage.expect("ram_usage must be Some on Linux");
        assert!(
            (0.0..=1.0).contains(&usage),
            "ram_usage must be in [0.0, 1.0], got {usage}"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn unsupported_platform_always_returns_none() {
        let sampler = OccupancySampler::default();
        // Multiple samples — still None on unsupported platforms.
        let _ = sampler.sample();
        let s = sampler.sample();
        assert!(
            s.cpu_usage.is_none(),
            "cpu_usage must be None on non-Linux platforms"
        );
        assert!(
            s.ram_usage.is_none(),
            "ram_usage must be None on non-Linux platforms"
        );
    }
}
