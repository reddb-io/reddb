//! AI metrics — issue #280.
//!
//! Global metric stores for the AI embedding path, rendered via
//! `render_ai_metrics()` into the `/metrics` Prometheus endpoint.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::fmt::Write;
use std::sync::OnceLock;

const DURATION_BOUNDS_MS: &[u64] = &[50, 100, 250, 500, 1_000, 2_500, 5_000, 10_000];
const BATCH_SIZE_BOUNDS: &[u64] = &[1, 4, 16, 64, 256, 512, 1_024, 2_048];

struct Hist {
    // counts[i] = observations <= BOUNDS[i]; last slot = +Inf
    counts: Vec<u64>,
    sum: u64,
    count: u64,
}

impl Hist {
    fn new(n: usize) -> Self {
        Self {
            counts: vec![0; n + 1],
            sum: 0,
            count: 0,
        }
    }

    fn observe(&mut self, value: u64, bounds: &[u64]) {
        for (i, &bound) in bounds.iter().enumerate() {
            if value <= bound {
                self.counts[i] += 1;
            }
        }
        *self.counts.last_mut().unwrap() += 1;
        self.sum = self.sum.saturating_add(value);
        self.count += 1;
    }
}

fn provider_requests() -> &'static Mutex<HashMap<(String, String, String), u64>> {
    static S: OnceLock<Mutex<HashMap<(String, String, String), u64>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

fn provider_duration() -> &'static Mutex<HashMap<(String, String), Hist>> {
    static S: OnceLock<Mutex<HashMap<(String, String), Hist>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

fn provider_retries() -> &'static Mutex<HashMap<(String, String), u64>> {
    static S: OnceLock<Mutex<HashMap<(String, String), u64>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

fn batch_size_hist() -> &'static Mutex<HashMap<String, Hist>> {
    static S: OnceLock<Mutex<HashMap<String, Hist>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

fn tokens_total() -> &'static Mutex<HashMap<(String, String), u64>> {
    static S: OnceLock<Mutex<HashMap<(String, String), u64>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn record_provider_request(provider: &str, model: &str, status: &str, duration_ms: u64) {
    {
        let key = (provider.to_string(), model.to_string(), status.to_string());
        *provider_requests().lock().entry(key).or_insert(0) += 1;
    }
    {
        let key = (provider.to_string(), model.to_string());
        let n = DURATION_BOUNDS_MS.len();
        provider_duration()
            .lock()
            .entry(key)
            .or_insert_with(|| Hist::new(n))
            .observe(duration_ms, DURATION_BOUNDS_MS);
    }
}

pub fn record_provider_retry(provider: &str, reason: &str) {
    let key = (provider.to_string(), reason.to_string());
    *provider_retries().lock().entry(key).or_insert(0) += 1;
}

pub fn record_batch_size(provider: &str, size: usize) {
    let n = BATCH_SIZE_BOUNDS.len();
    batch_size_hist()
        .lock()
        .entry(provider.to_string())
        .or_insert_with(|| Hist::new(n))
        .observe(size as u64, BATCH_SIZE_BOUNDS);
}

pub fn record_tokens(provider: &str, model: &str, count: u64) {
    if count == 0 {
        return;
    }
    let key = (provider.to_string(), model.to_string());
    *tokens_total().lock().entry(key).or_insert(0) += count;
}

/// Render all AI metrics into a Prometheus text body.
///
/// Called from `handle_metrics()`. Includes the dedup/chunked counters that
/// previously lived inline in the handler.
pub fn render_ai_metrics(body: &mut String) {
    use crate::runtime::ai::dedup_cache::{DEDUP_HITS_TOTAL, DEDUP_MISSES_TOTAL};
    use crate::runtime::ai::text_chunker::chunked_total;
    use std::sync::atomic::Ordering;

    let dedup_hits = DEDUP_HITS_TOTAL.load(Ordering::Relaxed);
    let dedup_misses = DEDUP_MISSES_TOTAL.load(Ordering::Relaxed);
    let chunked = chunked_total();

    let _ = writeln!(
        body,
        "# HELP reddb_ai_embedding_dedup_hits_total Embedding dedup cache hits."
    );
    let _ = writeln!(body, "# TYPE reddb_ai_embedding_dedup_hits_total counter");
    let _ = writeln!(body, "reddb_ai_embedding_dedup_hits_total {dedup_hits}");
    let _ = writeln!(
        body,
        "# HELP reddb_ai_embedding_dedup_misses_total Embedding dedup cache misses."
    );
    let _ = writeln!(body, "# TYPE reddb_ai_embedding_dedup_misses_total counter");
    let _ = writeln!(body, "reddb_ai_embedding_dedup_misses_total {dedup_misses}");
    let _ = writeln!(
        body,
        "# HELP reddb_ai_embedding_chunked_total Texts chunked before embedding."
    );
    let _ = writeln!(body, "# TYPE reddb_ai_embedding_chunked_total counter");
    let _ = writeln!(body, "reddb_ai_embedding_chunked_total {chunked}");

    // Provider request counters
    {
        let m = provider_requests().lock();
        if !m.is_empty() {
            let _ = writeln!(body, "# HELP reddb_ai_provider_requests_total Total AI provider embedding requests by outcome.");
            let _ = writeln!(body, "# TYPE reddb_ai_provider_requests_total counter");
            let mut rows: Vec<_> = m.iter().collect();
            rows.sort_by(|(left, _), (right, _)| left.cmp(right));
            for ((provider, model, status), count) in rows {
                let _ = writeln!(
                    body,
                    "reddb_ai_provider_requests_total{{provider=\"{}\",model=\"{}\",status=\"{}\"}} {count}",
                    escape_label(provider),
                    escape_label(model),
                    escape_label(status)
                );
            }
        }
    }

    // Request duration histograms
    {
        let m = provider_duration().lock();
        if !m.is_empty() {
            let _ = writeln!(body, "# HELP reddb_ai_provider_request_duration_ms AI provider request latency histogram (ms).");
            let _ = writeln!(
                body,
                "# TYPE reddb_ai_provider_request_duration_ms histogram"
            );
            let mut keys: Vec<_> = m.keys().cloned().collect();
            keys.sort();
            for key in keys {
                let hist = &m[&key];
                let (provider, model) = (&key.0, &key.1);
                for (i, bound) in DURATION_BOUNDS_MS.iter().enumerate() {
                    let _ = writeln!(
                        body,
                        "reddb_ai_provider_request_duration_ms_bucket{{provider=\"{}\",model=\"{}\",le=\"{bound}\"}} {}",
                        escape_label(provider),
                        escape_label(model),
                        hist.counts[i]
                    );
                }
                let _ = writeln!(
                    body,
                    "reddb_ai_provider_request_duration_ms_bucket{{provider=\"{}\",model=\"{}\",le=\"+Inf\"}} {}",
                    escape_label(provider),
                    escape_label(model),
                    hist.counts[DURATION_BOUNDS_MS.len()]
                );
                let _ = writeln!(
                    body,
                    "reddb_ai_provider_request_duration_ms_sum{{provider=\"{}\",model=\"{}\"}} {}",
                    escape_label(provider),
                    escape_label(model),
                    hist.sum
                );
                let _ = writeln!(
                    body,
                    "reddb_ai_provider_request_duration_ms_count{{provider=\"{}\",model=\"{}\"}} {}",
                    escape_label(provider),
                    escape_label(model),
                    hist.count
                );
            }
        }
    }

    // Retry counters
    {
        let m = provider_retries().lock();
        if !m.is_empty() {
            let _ = writeln!(
                body,
                "# HELP reddb_ai_provider_retries_total Total AI provider request retries."
            );
            let _ = writeln!(body, "# TYPE reddb_ai_provider_retries_total counter");
            let mut rows: Vec<_> = m.iter().collect();
            rows.sort_by(|(left, _), (right, _)| left.cmp(right));
            for ((provider, reason), count) in rows {
                let _ = writeln!(
                    body,
                    "reddb_ai_provider_retries_total{{provider=\"{}\",reason=\"{}\"}} {count}",
                    escape_label(provider),
                    escape_label(reason)
                );
            }
        }
    }

    // Batch size histograms
    {
        let m = batch_size_hist().lock();
        if !m.is_empty() {
            let _ = writeln!(
                body,
                "# HELP reddb_ai_embedding_batch_size Distribution of embedding sub-batch sizes."
            );
            let _ = writeln!(body, "# TYPE reddb_ai_embedding_batch_size histogram");
            let mut keys: Vec<_> = m.keys().cloned().collect();
            keys.sort();
            for provider in keys {
                let hist = &m[&provider];
                for (i, bound) in BATCH_SIZE_BOUNDS.iter().enumerate() {
                    let _ = writeln!(
                        body,
                        "reddb_ai_embedding_batch_size_bucket{{provider=\"{}\",le=\"{bound}\"}} {}",
                        escape_label(&provider),
                        hist.counts[i]
                    );
                }
                let _ = writeln!(
                    body,
                    "reddb_ai_embedding_batch_size_bucket{{provider=\"{}\",le=\"+Inf\"}} {}",
                    escape_label(&provider),
                    hist.counts[BATCH_SIZE_BOUNDS.len()]
                );
                let _ = writeln!(
                    body,
                    "reddb_ai_embedding_batch_size_sum{{provider=\"{}\"}} {}",
                    escape_label(&provider),
                    hist.sum
                );
                let _ = writeln!(
                    body,
                    "reddb_ai_embedding_batch_size_count{{provider=\"{}\"}} {}",
                    escape_label(&provider),
                    hist.count
                );
            }
        }
    }

    // Token counters
    {
        let m = tokens_total().lock();
        if !m.is_empty() {
            let _ = writeln!(body, "# HELP reddb_ai_text_tokens_total Total AI provider tokens consumed (best-effort from usage field).");
            let _ = writeln!(body, "# TYPE reddb_ai_text_tokens_total counter");
            let mut rows: Vec<_> = m.iter().collect();
            rows.sort_by(|(left, _), (right, _)| left.cmp(right));
            for ((provider, model), count) in rows {
                let _ = writeln!(
                    body,
                    "reddb_ai_text_tokens_total{{provider=\"{}\",model=\"{}\"}} {count}",
                    escape_label(provider),
                    escape_label(model)
                );
            }
        }
    }
}

fn escape_label(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hist_observe_correct_buckets() {
        let bounds = &[100u64, 500, 1000];
        let mut h = Hist::new(bounds.len());
        h.observe(50, bounds); // le=100, le=500, le=1000, +Inf
        h.observe(200, bounds); // le=500, le=1000, +Inf
        h.observe(2000, bounds); // +Inf only
        assert_eq!(h.counts, vec![1, 2, 3, 3]); // cumulative
        assert_eq!(h.sum, 2250);
        assert_eq!(h.count, 3);
    }

    #[test]
    fn record_and_render_roundtrip() {
        // Use unique provider names to avoid cross-test pollution from global state
        record_provider_request("test_prov_rnd", "model-x", "ok", 120);
        record_provider_retry("test_prov_rnd", "rate_limited");
        record_batch_size("test_prov_rnd", 32);
        record_tokens("test_prov_rnd", "model-x", 500);

        let mut body = String::new();
        render_ai_metrics(&mut body);

        assert!(
            body.contains("reddb_ai_provider_requests_total{provider=\"test_prov_rnd\""),
            "requests counter"
        );
        assert!(
            body.contains("reddb_ai_provider_retries_total{provider=\"test_prov_rnd\""),
            "retries counter"
        );
        assert!(
            body.contains("reddb_ai_embedding_batch_size_count{provider=\"test_prov_rnd\"}"),
            "batch size hist"
        );
        assert!(
            body.contains("reddb_ai_text_tokens_total{provider=\"test_prov_rnd\""),
            "tokens counter"
        );
        assert!(
            body.contains("reddb_ai_provider_request_duration_ms_count{provider=\"test_prov_rnd\""),
            "duration hist"
        );
    }

    #[test]
    fn zero_tokens_not_recorded() {
        record_tokens("test_zero_tok", "m", 0);
        let mut body = String::new();
        render_ai_metrics(&mut body);
        // zero-count should not appear
        assert!(!body.contains("test_zero_tok"));
    }
}
