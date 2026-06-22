//! HTTP request/error volume counters for operational telemetry.
//!
//! Issue #1239 / PRD #1237 Phase A, against the Phase-0 substrate
//! contract (ADR 0060). Every handled HTTP request increments a single
//! counter keyed by three **bounded** dimensions:
//!
//! - `method` — the request method, folded to the closed HTTP verb set
//!   (`GET`/`POST`/…); anything outside it collapses to `OTHER`.
//! - `route`  — the matched route *template* (`/catalog/collections/:name`),
//!   never the raw path. Requests that match no catalog route collapse to
//!   the reserved `__unmatched__` bucket. The normalization is the caller's
//!   job (it needs the route catalog); this module only stores the label.
//! - `status` — the HTTP status *class* (`2xx`/`4xx`/`5xx`/…) per ADR 0060
//!   §4, which keeps the status dimension at ≤6 values instead of the full
//!   code space.
//!
//! Because all three label components are `&'static str` (verb labels,
//! catalog route templates, and status-class strings are all static), the
//! key set is bounded by construction: the product of the closed verb set,
//! the static route table, and the six status classes. No raw path, id,
//! query string, tenant value, or authorization material is ever admitted
//! as a label — the caller passes pre-normalized static strings.
//!
//! The counter is a plain `Mutex<HashMap>`; HTTP request dispatch is not a
//! lock-contention-sensitive hot path at this granularity, and the bounded
//! key set keeps the map small. The substrate read model and the `/metrics`
//! exporter both read [`HttpRequestMetrics::snapshot`].

use std::collections::HashMap;
use std::fmt::Write;
use std::sync::{Arc, Mutex};

use super::handlers_admin::sanitize_label;

/// One bounded label set for a recorded HTTP request. Every component is
/// `&'static str` so the cardinality of the key space is fixed at compile
/// time (closed verb set × static route table × status classes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HttpRequestLabels {
    pub method: &'static str,
    pub route: &'static str,
    pub status: &'static str,
}

/// Reserved route label for any request that matched no catalog route.
/// Folding to a single bucket is what keeps raw 404 paths from blowing up
/// the `route` dimension (ADR 0060 §4 overflow rule).
pub const UNMATCHED_ROUTE: &str = "__unmatched__";

/// Reserved method label for any verb outside the closed HTTP set.
pub const OTHER_METHOD: &str = "OTHER";

/// Fold a request method to its bounded static label. The closed HTTP verb
/// set maps to its canonical upper-case label; anything else collapses to
/// [`OTHER_METHOD`] so a hostile/unknown verb cannot open a new series.
pub fn method_label(method: &str) -> &'static str {
    match method {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        "OPTIONS" => "OPTIONS",
        "HEAD" => "HEAD",
        _ => OTHER_METHOD,
    }
}

/// Map an HTTP status code to its bounded status *class*. Per ADR 0060 §4
/// the class (`2xx`/`4xx`/`5xx`/…) is preferred over the exact code so the
/// `status` dimension stays at ≤6 values.
pub fn status_class(status: u16) -> &'static str {
    match status {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        500..=599 => "5xx",
        _ => "other",
    }
}

#[derive(Debug, Clone)]
pub struct HttpRequestMetrics {
    counters: Arc<Mutex<HashMap<HttpRequestLabels, u64>>>,
}

impl HttpRequestMetrics {
    pub fn new() -> Self {
        Self {
            counters: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Record one handled HTTP request. `method` and `route` must already
    /// be normalized to bounded static labels by the caller (the verb set
    /// and the route catalog are the authorities); `status` is the raw
    /// response code, folded to its class here.
    pub fn record(&self, method: &'static str, route: &'static str, status: u16) {
        let key = HttpRequestLabels {
            method,
            route,
            status: status_class(status),
        };
        let mut guard = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        *guard.entry(key).or_insert(0) += 1;
    }

    /// Current value of one series, or 0 if never incremented. Visible for
    /// tests and the read model.
    pub fn count(&self, labels: HttpRequestLabels) -> u64 {
        let guard = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        guard.get(&labels).copied().unwrap_or(0)
    }

    /// Deterministically-ordered snapshot of every recorded series. This is
    /// the substrate read surface: `/metrics` renders it and the
    /// operational read model (red-ui / `/cluster/status` summaries) derives
    /// request throughput from the same measured facts.
    pub fn snapshot(&self) -> Vec<(HttpRequestLabels, u64)> {
        let guard = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        let mut rows: Vec<(HttpRequestLabels, u64)> = guard.iter().map(|(k, v)| (*k, *v)).collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows
    }

    /// Render `reddb_http_requests_total{method,route,status}` in Prometheus
    /// text exposition format, appending to `body`. Per ADR 0017 this is a
    /// boundary adapter: it reads the substrate snapshot and shapes it, it
    /// owns no storage. An empty substrate emits only HELP/TYPE (Prometheus
    /// has no envelope concept; absent series read as "nothing happened").
    pub fn render(&self, body: &mut String) {
        let _ = writeln!(
            body,
            "# HELP reddb_http_requests_total HTTP requests handled since process start, by method, route template, and status class."
        );
        let _ = writeln!(body, "# TYPE reddb_http_requests_total counter");
        for (labels, count) in self.snapshot() {
            let _ = writeln!(
                body,
                "reddb_http_requests_total{{method=\"{}\",route=\"{}\",status=\"{}\"}} {}",
                sanitize_label(labels.method),
                sanitize_label(labels.route),
                sanitize_label(labels.status),
                count
            );
        }
    }
}

impl Default for HttpRequestMetrics {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(
        method: &'static str,
        route: &'static str,
        status: &'static str,
    ) -> HttpRequestLabels {
        HttpRequestLabels {
            method,
            route,
            status,
        }
    }

    #[test]
    fn status_codes_fold_to_classes() {
        assert_eq!(status_class(200), "2xx");
        assert_eq!(status_class(204), "2xx");
        assert_eq!(status_class(301), "3xx");
        assert_eq!(status_class(404), "4xx");
        assert_eq!(status_class(503), "5xx");
        assert_eq!(status_class(700), "other");
    }

    #[test]
    fn record_increments_per_labelset() {
        let m = HttpRequestMetrics::new();
        m.record("GET", "/catalog/collections/:name", 200);
        m.record("GET", "/catalog/collections/:name", 200);
        m.record("GET", "/catalog/collections/:name", 404);
        assert_eq!(
            m.count(labels("GET", "/catalog/collections/:name", "2xx")),
            2
        );
        assert_eq!(
            m.count(labels("GET", "/catalog/collections/:name", "4xx")),
            1
        );
        assert_eq!(
            m.count(labels("POST", "/catalog/collections/:name", "2xx")),
            0
        );
    }

    #[test]
    fn high_cardinality_status_codes_collapse_to_one_class_series() {
        let m = HttpRequestMetrics::new();
        // Distinct 2xx codes must not each open a new series — they fold to
        // the single `2xx` class bucket.
        for status in [200u16, 201, 202, 204, 206] {
            m.record("POST", "/query", status);
        }
        let snapshot = m.snapshot();
        let query_2xx: Vec<_> = snapshot
            .iter()
            .filter(|(l, _)| l.route == "/query" && l.status == "2xx")
            .collect();
        assert_eq!(query_2xx.len(), 1, "all 2xx codes must share one series");
        assert_eq!(query_2xx[0].1, 5);
    }

    #[test]
    fn render_emits_prometheus_counter() {
        let m = HttpRequestMetrics::new();
        m.record("GET", "/health", 200);
        let mut body = String::new();
        m.render(&mut body);
        assert!(body.contains("# TYPE reddb_http_requests_total counter"));
        assert!(body.contains(
            "reddb_http_requests_total{method=\"GET\",route=\"/health\",status=\"2xx\"} 1"
        ));
    }

    #[test]
    fn snapshot_is_deterministically_ordered() {
        let m = HttpRequestMetrics::new();
        m.record("POST", "/query", 500);
        m.record("GET", "/health", 200);
        m.record("GET", "/health", 200);
        let snap = m.snapshot();
        // Sorted by (method, route, status) — GET sorts before POST.
        assert_eq!(snap[0].0.method, "GET");
        assert_eq!(snap[0].0.route, "/health");
        assert_eq!(snap[0].1, 2);
        assert_eq!(snap[1].0.method, "POST");
    }
}
