//! Node CPU/RAM occupancy sampler — issue #1244 (PRD #1237, Phase C).
//!
//! Records whole-node CPU and RAM utilisation as point-in-time gauges
//! (ADR 0060 §2 "node samples" data class) so `/cluster/status` can show
//! current resource pressure. Like the latency slice (#1241) this is the
//! in-process measurement + current-snapshot read model both the status
//! surface and the red-ui occupancy panels consume; the durable rollup
//! substrate is a later slice and counters reset on restart by design.
//!
//! ## Honesty rule (#738 / ADR 0060 §6)
//!
//! Each field is one of three states, never a fabricated zero:
//!
//! * [`Occupancy::Measured`] — a real ratio in `0.0..=1.0`.
//! * [`Occupancy::NotSampled`] — the platform supports measurement but no
//!   value exists yet. CPU utilisation is a *delta* between two readings,
//!   so the very first sample after start only establishes a baseline and
//!   reports `NotSampled`; the next sample carries a measured value.
//! * [`Occupancy::Unsupported`] — this platform cannot measure the field
//!   at all (anything without `/proc/stat` + `/proc/meminfo`, i.e. non-Linux).
//!
//! The presentation layer maps these to a measured envelope
//! (`{ available: true, usage_ratio, usage_percent }`) or the stable
//! `{ available: false, reason }` envelope.
//!
//! ## Sampling interval and overhead (documented)
//!
//! The sampler is driven on-demand from the `/cluster/status` handler: each
//! status read calls [`OccupancySampler::sample`], which reads `/proc/stat`
//! (one line) and `/proc/meminfo` (two lines) and computes the CPU busy
//! ratio over the wall-clock interval **since the previous status read**.
//! red-ui polls `/cluster/status` on its own cadence (a few seconds), so the
//! CPU figure naturally reflects that polling interval. There is no
//! background thread: cost is two small `procfs` reads plus integer
//! arithmetic per status call (well under a millisecond, no allocation on
//! the hot path beyond the two read buffers, no lock contention outside the
//! sampler's own short critical sections). Because the window is the
//! status-poll interval rather than a fixed tick, a value is only reported
//! once two reads exist — consistent with the honesty rule above.

use std::sync::Mutex;

/// Cumulative CPU jiffies from the aggregate `cpu` line of `/proc/stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CpuTimes {
    /// Sum of every field on the line (busy + idle).
    total: u64,
    /// `idle + iowait` — the time the CPU was not doing work.
    idle: u64,
}

/// One occupancy field's honest state. See module docs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Occupancy {
    /// Measured utilisation ratio, clamped to `0.0..=1.0`.
    Measured(f64),
    /// Platform supports measurement but no value is available yet.
    NotSampled,
    /// This platform cannot measure the field.
    Unsupported,
}

/// A point-in-time CPU + RAM occupancy reading.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OccupancySample {
    pub cpu: Occupancy,
    pub ram: Occupancy,
}

/// Process-local CPU/RAM occupancy sampler. Holds the previous CPU jiffies
/// reading (for delta computation) and the latest computed sample.
#[derive(Debug, Default)]
pub struct OccupancySampler {
    /// Previous `/proc/stat` reading. `None` before the first sample.
    prev_cpu: Mutex<Option<CpuTimes>>,
    /// Latest computed sample, so a consumer can read the current value
    /// without forcing a fresh measurement.
    latest: Mutex<Option<OccupancySample>>,
}

impl OccupancySampler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take one measurement, update internal state, and return the sample.
    pub fn sample(&self) -> OccupancySample {
        let sample = self.measure();
        *self
            .latest
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(sample);
        sample
    }

    /// The most recent measured sample, without forcing a new reading.
    /// Before the first [`sample`](Self::sample) call this reports the
    /// platform's baseline state (`NotSampled` on Linux, `Unsupported`
    /// elsewhere) so the honesty rule holds even on the cold path.
    pub fn latest(&self) -> OccupancySample {
        self.latest
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .unwrap_or_else(unsampled_baseline)
    }

    #[cfg(target_os = "linux")]
    fn measure(&self) -> OccupancySample {
        let ram = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|c| parse_meminfo_used_ratio(&c))
            .map(Occupancy::Measured)
            .unwrap_or(Occupancy::NotSampled);

        let current = std::fs::read_to_string("/proc/stat")
            .ok()
            .and_then(|c| parse_proc_stat_cpu(&c));

        let mut prev = self
            .prev_cpu
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let cpu = match (*prev, current) {
            (Some(p), Some(c)) => cpu_busy_ratio(p, c)
                .map(Occupancy::Measured)
                .unwrap_or(Occupancy::NotSampled),
            _ => Occupancy::NotSampled,
        };
        if let Some(c) = current {
            *prev = Some(c);
        }
        OccupancySample { cpu, ram }
    }

    #[cfg(not(target_os = "linux"))]
    fn measure(&self) -> OccupancySample {
        OccupancySample {
            cpu: Occupancy::Unsupported,
            ram: Occupancy::Unsupported,
        }
    }
}

/// Baseline state reported before any sample exists.
#[cfg(target_os = "linux")]
fn unsampled_baseline() -> OccupancySample {
    OccupancySample {
        cpu: Occupancy::NotSampled,
        ram: Occupancy::NotSampled,
    }
}

#[cfg(not(target_os = "linux"))]
fn unsampled_baseline() -> OccupancySample {
    OccupancySample {
        cpu: Occupancy::Unsupported,
        ram: Occupancy::Unsupported,
    }
}

/// Parse the aggregate `cpu` line of `/proc/stat` into cumulative jiffies.
/// Fields after the label are: `user nice system idle iowait irq softirq
/// steal guest guest_nice`. `idle` here is `idle + iowait`.
fn parse_proc_stat_cpu(contents: &str) -> Option<CpuTimes> {
    let line = contents.lines().next()?;
    let mut fields = line.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    let vals: Vec<u64> = fields.filter_map(|v| v.parse::<u64>().ok()).collect();
    // Need at least user/nice/system/idle to be meaningful.
    if vals.len() < 4 {
        return None;
    }
    let idle = vals[3] + vals.get(4).copied().unwrap_or(0);
    let total: u64 = vals.iter().sum();
    Some(CpuTimes { total, idle })
}

/// Busy ratio between two cumulative CPU readings. `None` when the totals
/// did not advance (no elapsed time) or went backwards (counter reset).
fn cpu_busy_ratio(prev: CpuTimes, current: CpuTimes) -> Option<f64> {
    let total_delta = current.total.checked_sub(prev.total)?;
    let idle_delta = current.idle.checked_sub(prev.idle)?;
    if total_delta == 0 {
        return None;
    }
    let busy = total_delta.saturating_sub(idle_delta);
    Some((busy as f64 / total_delta as f64).clamp(0.0, 1.0))
}

/// Parse `MemTotal` / `MemAvailable` from `/proc/meminfo` into a used
/// ratio (`(total - available) / total`). `None` if either line is missing
/// or `MemTotal` is zero.
fn parse_meminfo_used_ratio(contents: &str) -> Option<f64> {
    let mut total: Option<u64> = None;
    let mut available: Option<u64> = None;
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total = rest.split_whitespace().next().and_then(|v| v.parse().ok());
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available = rest.split_whitespace().next().and_then(|v| v.parse().ok());
        }
    }
    let total = total?;
    let available = available?;
    if total == 0 {
        return None;
    }
    let used = total.saturating_sub(available);
    Some((used as f64 / total as f64).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_proc_stat_sums_total_and_idle() {
        // user nice system idle iowait irq softirq steal ...
        let stat = "cpu  100 0 50 800 40 0 10 0 0 0\ncpu0 ...\n";
        let times = parse_proc_stat_cpu(stat).unwrap();
        // idle = idle(800) + iowait(40) = 840
        assert_eq!(times.idle, 840);
        // total = 100+0+50+800+40+0+10+0+0+0 = 1000
        assert_eq!(times.total, 1000);
    }

    #[test]
    fn parse_proc_stat_rejects_malformed() {
        assert!(parse_proc_stat_cpu("").is_none());
        assert!(parse_proc_stat_cpu("notcpu 1 2 3 4\n").is_none());
        assert!(parse_proc_stat_cpu("cpu 1 2\n").is_none());
    }

    #[test]
    fn cpu_ratio_is_busy_over_total() {
        let prev = CpuTimes {
            total: 1000,
            idle: 840,
        };
        // 1000 more jiffies elapse, 250 of them idle -> 75% busy.
        let current = CpuTimes {
            total: 2000,
            idle: 1090,
        };
        let ratio = cpu_busy_ratio(prev, current).unwrap();
        assert!((ratio - 0.75).abs() < 1e-9);
    }

    #[test]
    fn cpu_ratio_none_when_no_time_elapsed() {
        let t = CpuTimes {
            total: 1000,
            idle: 840,
        };
        assert_eq!(cpu_busy_ratio(t, t), None);
    }

    #[test]
    fn cpu_ratio_none_on_counter_reset() {
        let prev = CpuTimes {
            total: 2000,
            idle: 1000,
        };
        let current = CpuTimes {
            total: 1000,
            idle: 500,
        };
        assert_eq!(cpu_busy_ratio(prev, current), None);
    }

    #[test]
    fn cpu_ratio_clamps_to_unit_interval() {
        // Idle advances more than total (impossible in practice but the
        // arithmetic must never produce a negative ratio).
        let prev = CpuTimes {
            total: 1000,
            idle: 500,
        };
        let current = CpuTimes {
            total: 1100,
            idle: 800,
        };
        let ratio = cpu_busy_ratio(prev, current).unwrap();
        assert!((0.0..=1.0).contains(&ratio));
    }

    #[test]
    fn meminfo_used_ratio_is_one_minus_available() {
        let meminfo = "MemTotal:       16000 kB\nMemFree:  1000 kB\nMemAvailable:    4000 kB\n";
        let ratio = parse_meminfo_used_ratio(meminfo).unwrap();
        // used = 16000 - 4000 = 12000 -> 0.75
        assert!((ratio - 0.75).abs() < 1e-9);
    }

    #[test]
    fn meminfo_none_when_fields_missing() {
        assert!(parse_meminfo_used_ratio("MemTotal: 16000 kB\n").is_none());
        assert!(parse_meminfo_used_ratio("MemAvailable: 4000 kB\n").is_none());
        assert!(parse_meminfo_used_ratio("MemTotal: 0 kB\nMemAvailable: 0 kB\n").is_none());
    }

    #[test]
    fn first_sample_establishes_baseline_only() {
        // The very first sample cannot produce a CPU delta — it must report
        // NotSampled (Linux) or Unsupported (other) for CPU, never a number.
        let sampler = OccupancySampler::new();
        let first = sampler.sample();
        assert!(
            !matches!(first.cpu, Occupancy::Measured(_)),
            "first CPU sample must not be a measured value, got {:?}",
            first.cpu
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn second_sample_returns_measured_cpu_on_linux() {
        // Two consecutive reads establish a delta. On an idle CI runner the
        // busy ratio can legitimately be ~0; assert the value is *measurable*
        // (a real ratio in the unit interval), not strictly positive.
        let sampler = OccupancySampler::new();
        let _ = sampler.sample(); // baseline
                                  // Burn a little CPU so the second reading reflects real work, but
                                  // do not assert on the magnitude — the runner may still be idle.
        let mut acc: u64 = 0;
        for i in 0..2_000_000u64 {
            acc = acc.wrapping_add(i);
        }
        std::hint::black_box(acc);
        // Guarantee the aggregate jiffy counter advances between the two
        // reads so the CPU delta is `Measured`, not `NotSampled`. Without a
        // wall-clock gap, two back-to-back reads can land in the same jiffy
        // window on a fast host (`total_delta == 0`); the busy ratio may
        // still be ~0 on an idle runner, which the range assertion allows.
        std::thread::sleep(std::time::Duration::from_millis(50));

        let second = sampler.sample();
        match second.cpu {
            Occupancy::Measured(ratio) => {
                assert!(
                    (0.0..=1.0).contains(&ratio),
                    "cpu ratio {ratio} must be in 0..=1"
                );
            }
            other => panic!("expected a measured CPU ratio on the second sample, got {other:?}"),
        }
        // RAM is a single-shot gauge — measurable on the very first read.
        assert!(
            matches!(second.ram, Occupancy::Measured(r) if (0.0..=1.0).contains(&r)),
            "expected a measured RAM ratio on Linux, got {:?}",
            second.ram
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_baseline_is_not_sampled_before_first_sample() {
        let sampler = OccupancySampler::new();
        assert!(matches!(sampler.latest().cpu, Occupancy::NotSampled));
        assert!(matches!(sampler.latest().ram, Occupancy::NotSampled));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn non_linux_is_unsupported() {
        let sampler = OccupancySampler::new();
        let sample = sampler.sample();
        assert!(matches!(sample.cpu, Occupancy::Unsupported));
        assert!(matches!(sample.ram, Occupancy::Unsupported));
        assert!(matches!(sampler.latest().cpu, Occupancy::Unsupported));
    }
}
