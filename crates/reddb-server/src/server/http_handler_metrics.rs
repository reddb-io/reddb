//! Prometheus metrics for the HTTP handler-thread pool.
//!
//! Slice 4 of issue #573 / parent #569. Exposes four series per
//! ADR-0017 (Prometheus / Grafana adapters) so operators can observe
//! saturation arriving before the cap is hit:
//!
//! - `http_active_handler_threads{transport}` — gauge, sourced from
//!   `HttpConnectionLimiter::current()` at scrape time.
//! - `http_handler_cap{transport}` — static gauge, sourced from the
//!   limiter's cap. Same value for `http` and `https` since slice 3
//!   collapsed both transports onto one cap.
//! - `http_handler_rejected_total{transport, reason}` — counter,
//!   incremented per 503 emitted by the accept-loop reject path
//!   (`reason=cap_exhausted`) or per slice-2 deadline exit
//!   (`reason=handler_timeout`).
//! - `http_handler_duration_seconds{transport}` — histogram of total
//!   handler wall-clock time, sampled on every handler exit
//!   (happy-path and timeout). Buckets are the standard Prometheus
//!   client default set so existing dashboards render quantiles via
//!   `histogram_quantile` without configuration.
//!
//! Counters and histogram updates are plain `AtomicU64` operations on
//! the hot path. No registry, no mutex.

use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::http_connection_limiter::HttpConnectionLimiter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpTransport {
    Http,
    Https,
}

impl HttpTransport {
    fn label(self) -> &'static str {
        match self {
            HttpTransport::Http => "http",
            HttpTransport::Https => "https",
        }
    }

    fn index(self) -> usize {
        match self {
            HttpTransport::Http => 0,
            HttpTransport::Https => 1,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum HttpRejectReason {
    CapExhausted,
    HandlerTimeout,
}

impl HttpRejectReason {
    fn label(self) -> &'static str {
        match self {
            HttpRejectReason::CapExhausted => "cap_exhausted",
            HttpRejectReason::HandlerTimeout => "handler_timeout",
        }
    }

    fn index(self) -> usize {
        match self {
            HttpRejectReason::CapExhausted => 0,
            HttpRejectReason::HandlerTimeout => 1,
        }
    }
}

/// Prometheus client default histogram buckets, in seconds. Aligned
/// with `prometheus.DefBuckets` so operator dashboards render
/// `histogram_quantile` without per-deployment tuning.
const DURATION_BUCKETS_SECONDS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

#[derive(Debug)]
struct TransportHistogram {
    /// One counter per bucket in `DURATION_BUCKETS_SECONDS`, plus a
    /// trailing `+Inf` slot that holds the total sample count.
    buckets: [AtomicU64; 11],
    inf: AtomicU64,
    /// Sum of observed durations in microseconds (we keep the sum in
    /// `u64` and convert to seconds at render time).
    sum_micros: AtomicU64,
}

impl TransportHistogram {
    fn new() -> Self {
        Self {
            buckets: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
            inf: AtomicU64::new(0),
            sum_micros: AtomicU64::new(0),
        }
    }

    fn observe_seconds(&self, value: f64) {
        let micros = (value * 1_000_000.0).round().clamp(0.0, u64::MAX as f64) as u64;
        self.sum_micros.fetch_add(micros, Ordering::Relaxed);
        self.inf.fetch_add(1, Ordering::Relaxed);
        for (i, le) in DURATION_BUCKETS_SECONDS.iter().enumerate() {
            if value <= *le {
                self.buckets[i].fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[derive(Debug)]
struct Inner {
    rejected: [[AtomicU64; 2]; 2],
    duration: [TransportHistogram; 2],
}

#[derive(Debug, Clone)]
pub struct HttpHandlerMetrics {
    inner: Arc<Inner>,
}

impl HttpHandlerMetrics {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                rejected: [
                    [AtomicU64::new(0), AtomicU64::new(0)],
                    [AtomicU64::new(0), AtomicU64::new(0)],
                ],
                duration: [TransportHistogram::new(), TransportHistogram::new()],
            }),
        }
    }

    pub fn record_reject(&self, transport: HttpTransport, reason: HttpRejectReason) {
        self.inner.rejected[transport.index()][reason.index()].fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_duration(&self, transport: HttpTransport, seconds: f64) {
        if !seconds.is_finite() || seconds < 0.0 {
            return;
        }
        self.inner.duration[transport.index()].observe_seconds(seconds);
    }

    pub fn rejected_count(&self, transport: HttpTransport, reason: HttpRejectReason) -> u64 {
        self.inner.rejected[transport.index()][reason.index()].load(Ordering::Relaxed)
    }

    pub fn duration_sample_count(&self, transport: HttpTransport) -> u64 {
        self.inner.duration[transport.index()]
            .inf
            .load(Ordering::Relaxed)
    }

    /// Render all four series in Prometheus text exposition format,
    /// appending to `body`. Reads the live `current()` and `cap()` off
    /// the supplied limiter; the limiter is the source of truth for
    /// the two gauges so the metrics can't drift from the admission
    /// path.
    pub fn render(&self, body: &mut String, limiter: &HttpConnectionLimiter) {
        let cap = limiter.cap();
        let current = limiter.current();

        // `http_active_handler_threads{transport}` — gauge.
        // The clear-text and TLS accept loops share a single limiter
        // (slice 3), so the live count is duplicated on both labels
        // for dashboard ergonomics: an operator can graph the per-
        // transport pane without special-casing the shared cap.
        let _ = writeln!(
            body,
            "# HELP http_active_handler_threads Live HTTP/HTTPS handler threads holding a limiter permit."
        );
        let _ = writeln!(body, "# TYPE http_active_handler_threads gauge");
        let _ = writeln!(
            body,
            "http_active_handler_threads{{transport=\"http\"}} {}",
            current
        );
        let _ = writeln!(
            body,
            "http_active_handler_threads{{transport=\"https\"}} {}",
            current
        );

        // `http_handler_cap{transport}` — static gauge.
        let _ = writeln!(
            body,
            "# HELP http_handler_cap Configured maximum concurrent HTTP/HTTPS handler threads."
        );
        let _ = writeln!(body, "# TYPE http_handler_cap gauge");
        let _ = writeln!(body, "http_handler_cap{{transport=\"http\"}} {}", cap);
        let _ = writeln!(body, "http_handler_cap{{transport=\"https\"}} {}", cap);

        // `http_handler_rejected_total{transport, reason}` — counter.
        let _ = writeln!(
            body,
            "# HELP http_handler_rejected_total HTTP/HTTPS handler rejections by reason since process start."
        );
        let _ = writeln!(body, "# TYPE http_handler_rejected_total counter");
        for transport in [HttpTransport::Http, HttpTransport::Https] {
            for reason in [
                HttpRejectReason::CapExhausted,
                HttpRejectReason::HandlerTimeout,
            ] {
                let _ = writeln!(
                    body,
                    "http_handler_rejected_total{{transport=\"{}\",reason=\"{}\"}} {}",
                    transport.label(),
                    reason.label(),
                    self.rejected_count(transport, reason)
                );
            }
        }

        // `http_handler_duration_seconds{transport}` — histogram.
        let _ = writeln!(
            body,
            "# HELP http_handler_duration_seconds Wall-clock handler duration per transport."
        );
        let _ = writeln!(body, "# TYPE http_handler_duration_seconds histogram");
        for transport in [HttpTransport::Http, HttpTransport::Https] {
            let hist = &self.inner.duration[transport.index()];
            for (i, le) in DURATION_BUCKETS_SECONDS.iter().enumerate() {
                let _ = writeln!(
                    body,
                    "http_handler_duration_seconds_bucket{{transport=\"{}\",le=\"{}\"}} {}",
                    transport.label(),
                    format_bucket_le(*le),
                    hist.buckets[i].load(Ordering::Relaxed)
                );
            }
            let inf = hist.inf.load(Ordering::Relaxed);
            let _ = writeln!(
                body,
                "http_handler_duration_seconds_bucket{{transport=\"{}\",le=\"+Inf\"}} {}",
                transport.label(),
                inf
            );
            let sum_secs = (hist.sum_micros.load(Ordering::Relaxed) as f64) / 1_000_000.0;
            let _ = writeln!(
                body,
                "http_handler_duration_seconds_sum{{transport=\"{}\"}} {}",
                transport.label(),
                sum_secs
            );
            let _ = writeln!(
                body,
                "http_handler_duration_seconds_count{{transport=\"{}\"}} {}",
                transport.label(),
                inf
            );
        }
    }
}

impl Default for HttpHandlerMetrics {
    fn default() -> Self {
        Self::new()
    }
}

fn format_bucket_le(le: f64) -> String {
    // Match Prometheus's exposition formatting: trailing zeros are
    // significant for canonical bucket labels, so we use the same
    // shape that the upstream client library emits.
    if le == le.trunc() && le.abs() < 1e16 {
        format!("{le:.1}")
    } else {
        format!("{le}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejected_counters_isolated_by_label() {
        let m = HttpHandlerMetrics::new();
        m.record_reject(HttpTransport::Http, HttpRejectReason::CapExhausted);
        m.record_reject(HttpTransport::Http, HttpRejectReason::CapExhausted);
        m.record_reject(HttpTransport::Https, HttpRejectReason::HandlerTimeout);
        assert_eq!(
            m.rejected_count(HttpTransport::Http, HttpRejectReason::CapExhausted),
            2
        );
        assert_eq!(
            m.rejected_count(HttpTransport::Http, HttpRejectReason::HandlerTimeout),
            0
        );
        assert_eq!(
            m.rejected_count(HttpTransport::Https, HttpRejectReason::HandlerTimeout),
            1
        );
        assert_eq!(
            m.rejected_count(HttpTransport::Https, HttpRejectReason::CapExhausted),
            0
        );
    }

    #[test]
    fn duration_histogram_buckets_are_cumulative() {
        let m = HttpHandlerMetrics::new();
        m.record_duration(HttpTransport::Http, 0.003);
        m.record_duration(HttpTransport::Http, 0.04);
        m.record_duration(HttpTransport::Http, 3.0);
        assert_eq!(m.duration_sample_count(HttpTransport::Http), 3);

        let limiter = HttpConnectionLimiter::new(4);
        let mut body = String::new();
        m.render(&mut body, &limiter);

        // `le="0.005"` includes only the 3ms sample (cumulative).
        assert!(body
            .contains("http_handler_duration_seconds_bucket{transport=\"http\",le=\"0.005\"} 1"));
        // `le="0.05"` includes the 3ms + 40ms samples.
        assert!(
            body.contains("http_handler_duration_seconds_bucket{transport=\"http\",le=\"0.05\"} 2")
        );
        // `le="+Inf"` sees all 3 samples.
        assert!(
            body.contains("http_handler_duration_seconds_bucket{transport=\"http\",le=\"+Inf\"} 3")
        );
        // HTTPS labelset present but empty.
        assert!(body
            .contains("http_handler_duration_seconds_bucket{transport=\"https\",le=\"+Inf\"} 0"));
    }

    #[test]
    fn render_includes_cap_and_current_from_limiter() {
        let limiter = HttpConnectionLimiter::new(7);
        let _p = limiter.try_acquire().unwrap();
        let m = HttpHandlerMetrics::new();
        let mut body = String::new();
        m.render(&mut body, &limiter);
        assert!(body.contains("http_handler_cap{transport=\"http\"} 7"));
        assert!(body.contains("http_handler_cap{transport=\"https\"} 7"));
        assert!(body.contains("http_active_handler_threads{transport=\"http\"} 1"));
        assert!(body.contains("http_active_handler_threads{transport=\"https\"} 1"));
    }

    #[test]
    fn render_emits_all_four_rejection_labels() {
        let m = HttpHandlerMetrics::new();
        let limiter = HttpConnectionLimiter::new(1);
        let mut body = String::new();
        m.render(&mut body, &limiter);
        for transport in ["http", "https"] {
            for reason in ["cap_exhausted", "handler_timeout"] {
                let expected = format!(
                    "http_handler_rejected_total{{transport=\"{transport}\",reason=\"{reason}\"}} 0"
                );
                assert!(body.contains(&expected), "missing line: {expected}");
            }
        }
    }

    #[test]
    fn negative_or_nan_durations_are_ignored() {
        let m = HttpHandlerMetrics::new();
        m.record_duration(HttpTransport::Http, -1.0);
        m.record_duration(HttpTransport::Http, f64::NAN);
        m.record_duration(HttpTransport::Http, f64::INFINITY);
        assert_eq!(m.duration_sample_count(HttpTransport::Http), 0);
    }
}
