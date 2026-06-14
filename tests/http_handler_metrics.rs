//! Integration test for issue #573 slice 4: Prometheus metrics for
//! the HTTP handler-thread pool.
//!
//! Drives a known traffic shape against a real `RedDBServer`:
//!   - one happy-path request (recorded into duration histogram)
//!   - one cap_exhausted rejection (limiter cap saturated)
//!   - one handler_timeout rejection (slice-2 deadline trip)
//! then scrapes `/metrics` and asserts the four series declared in
//! the issue brief are present with the expected label sets and
//! monotonic counter relationships.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use reddb::server::RedDBServer;
use reddb::{RedDBOptions, RedDBRuntime};

fn boot(cap: usize, handler_timeout: Duration) -> (String, RedDBServer) {
    let opts = RedDBOptions::in_memory();
    let runtime = RedDBRuntime::with_options(opts).expect("runtime");
    let server = RedDBServer::new(runtime)
        .with_http_limiter_cap(cap)
        .with_handler_timeout(handler_timeout);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap().to_string();
    let server_clone = server.clone();
    thread::spawn(move || {
        let _ = server_clone.serve_on(listener);
    });
    thread::sleep(Duration::from_millis(80));
    (addr, server)
}

fn send_request(addr: &str, path: &str) -> String {
    let mut tcp = TcpStream::connect(addr).expect("connect");
    tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    tcp.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    tcp.write_all(req.as_bytes()).unwrap();
    tcp.flush().unwrap();
    let mut buf = Vec::new();
    let _ = tcp.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

fn spawn_slow_request(addr: String) -> thread::JoinHandle<String> {
    thread::spawn(move || send_request(&addr, "/health/live"))
}

fn metric_value(body: &str, line_prefix: &str) -> u64 {
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix(line_prefix) {
            let value = rest.trim();
            return value.parse::<u64>().unwrap_or_else(|_| {
                panic!("could not parse metric value for {line_prefix:?}: {rest:?}")
            });
        }
    }
    panic!("metric line not found: {line_prefix:?}\nbody:\n{body}")
}

fn metric_value_f64(body: &str, line_prefix: &str) -> f64 {
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix(line_prefix) {
            let value = rest.trim();
            return value.parse::<f64>().unwrap_or_else(|_| {
                panic!("could not parse metric value for {line_prefix:?}: {rest:?}")
            });
        }
    }
    panic!("metric line not found: {line_prefix:?}\nbody:\n{body}")
}

#[test]
fn metrics_emit_all_four_series_with_expected_label_sets() {
    // Cap=2 so we can saturate with a single held connection that
    // never sends a request and still leave room for the scrape.
    let cap = 2;
    let handler_timeout = Duration::from_millis(3_000);
    let (addr, server) = boot(cap, handler_timeout);

    // (1) Happy-path request — records one sample in the http
    // duration histogram. Use a separate connection from later steps.
    let happy = send_request(&addr, "/health/live");
    assert!(
        happy.starts_with("HTTP/1.1 2"),
        "happy-path /health/live should succeed: {happy:?}"
    );

    // (2) Saturate the cap with real in-flight requests. The async edge
    // limits requests, not idle sockets, so the test slow-inject hook
    // keeps each request occupying a permit long enough to observe the
    // cap-exhausted path.
    server.set_test_slow_inject_ms(2_000);
    let mut held = Vec::new();
    for _ in 0..cap {
        held.push(spawn_slow_request(addr.clone()));
    }
    for _ in 0..50 {
        if server.http_limiter().current() == cap {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        server.http_limiter().current(),
        cap,
        "limiter should be saturated by held connections"
    );

    // Next connection is rejected by the limiter with the static
    // 503 + Retry-After path — this is the cap_exhausted reason.
    let rejected = send_request(&addr, "/health/live");
    assert!(
        rejected.starts_with("HTTP/1.1 503"),
        "expected limiter 503, got: {rejected:?}"
    );

    // Drain held requests so the cap recovers.
    server.set_test_slow_inject_ms(0);
    for handle in held {
        let body = handle.join().expect("held request thread");
        assert!(
            body.starts_with("HTTP/1.1 2"),
            "held request should complete normally: {body:?}"
        );
    }
    for _ in 0..100 {
        if server.http_limiter().current() == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }

    // Settle: the accept loop may still be returning the just-drained
    // sockets to the limiter pool.
    thread::sleep(Duration::from_millis(100));

    // (3) Trip the slice-2 deadline once — handler_timeout reason.
    server.set_test_slow_inject_ms(4_000);
    let timed_out = send_request(&addr, "/health/live");
    assert!(
        timed_out.starts_with("HTTP/1.1 503"),
        "expected deadline 503, got: {timed_out:?}"
    );
    server.set_test_slow_inject_ms(0);

    // Wait for the permit to drop after the deadline-503 handler
    // exits — otherwise `current()` reported via /metrics will be 1.
    for _ in 0..100 {
        if server.http_limiter().current() == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }

    // (4) Scrape /metrics and parse out the four target series.
    let metrics = send_request(&addr, "/metrics");
    assert!(
        metrics.starts_with("HTTP/1.1 200"),
        "scrape should succeed: {metrics:?}"
    );
    let body = metrics.split("\r\n\r\n").nth(1).unwrap_or("");

    // http_handler_cap — both labels match the configured cap.
    assert_eq!(
        metric_value(body, "http_handler_cap{transport=\"http\"} "),
        cap as u64
    );
    assert_eq!(
        metric_value(body, "http_handler_cap{transport=\"https\"} "),
        cap as u64
    );

    // http_active_handler_threads — the /metrics handler itself holds
    // a permit while it serves the scrape, so the value seen inside
    // the response body is exactly 1.
    assert_eq!(
        metric_value(body, "http_active_handler_threads{transport=\"http\"} "),
        1
    );
    assert_eq!(
        metric_value(body, "http_active_handler_threads{transport=\"https\"} "),
        1
    );

    // http_handler_rejected_total — cap_exhausted on http should be
    // at least 1 (the limiter-reject above), handler_timeout on http
    // should be at least 1 (the deadline trip), and the https labels
    // should be present at 0 (no TLS traffic in this test).
    let http_cap_rejected = metric_value(
        body,
        "http_handler_rejected_total{transport=\"http\",reason=\"cap_exhausted\"} ",
    );
    assert!(
        http_cap_rejected >= 1,
        "expected http cap_exhausted >= 1, got {http_cap_rejected}"
    );
    let http_timeout_rejected = metric_value(
        body,
        "http_handler_rejected_total{transport=\"http\",reason=\"handler_timeout\"} ",
    );
    assert!(
        http_timeout_rejected >= 1,
        "expected http handler_timeout >= 1, got {http_timeout_rejected}"
    );
    assert_eq!(
        metric_value(
            body,
            "http_handler_rejected_total{transport=\"https\",reason=\"cap_exhausted\"} "
        ),
        0
    );
    assert_eq!(
        metric_value(
            body,
            "http_handler_rejected_total{transport=\"https\",reason=\"handler_timeout\"} "
        ),
        0
    );

    // http_handler_duration_seconds — histogram has at least one
    // sample on http (the happy-path /health/live) and zero on https.
    let http_count = metric_value(
        body,
        "http_handler_duration_seconds_count{transport=\"http\"} ",
    );
    assert!(
        http_count >= 1,
        "expected http duration count >= 1, got {http_count}"
    );
    assert_eq!(
        metric_value(
            body,
            "http_handler_duration_seconds_count{transport=\"https\"} "
        ),
        0
    );
    // The `+Inf` bucket must equal the total count for any valid
    // Prometheus histogram.
    assert_eq!(
        metric_value(
            body,
            "http_handler_duration_seconds_bucket{transport=\"http\",le=\"+Inf\"} "
        ),
        http_count
    );
    let sum = metric_value_f64(
        body,
        "http_handler_duration_seconds_sum{transport=\"http\"} ",
    );
    assert!(sum >= 0.0, "duration sum should be non-negative: {sum}");

    // (5) Second scrape — counters are monotonic. Active gauge can
    // float (1 during the scrape, 0 between); counters cannot regress.
    let metrics2 = send_request(&addr, "/metrics");
    let body2 = metrics2.split("\r\n\r\n").nth(1).unwrap_or("");
    let http_cap_rejected_2 = metric_value(
        body2,
        "http_handler_rejected_total{transport=\"http\",reason=\"cap_exhausted\"} ",
    );
    let http_timeout_rejected_2 = metric_value(
        body2,
        "http_handler_rejected_total{transport=\"http\",reason=\"handler_timeout\"} ",
    );
    let http_count_2 = metric_value(
        body2,
        "http_handler_duration_seconds_count{transport=\"http\"} ",
    );
    assert!(http_cap_rejected_2 >= http_cap_rejected);
    assert!(http_timeout_rejected_2 >= http_timeout_rejected);
    assert!(http_count_2 >= http_count);
}
