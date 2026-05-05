//! Parser hardening test suite for the connection-string parser
//! (issue #90).
//!
//! Property-based and panic-safety tests. Snapshot tests for
//! pinned `ParseError` messages live in `parser_snapshots.rs`.
//! Both files reuse the harness in `tests/support/parser_hardening`.

mod support {
    pub mod parser_hardening;
}

use proptest::prelude::*;
use reddb_wire::{parse, parse_with_limits, ConnStringLimits, ParseError, ParseErrorKind};
use support::parser_hardening::{
    self as harness, assert_no_panic_on, conn_grammar, corpus::adversarial_inputs, HardenedParser,
};

/// Concrete `HardenedParser` shim around the conn-string parser.
pub struct ConnStringParser;

impl HardenedParser for ConnStringParser {
    type Error = ParseError;

    fn parse(input: &str) -> Result<(), Self::Error> {
        parse(input).map(|_| ())
    }

    fn parse_with_limits(input: &str, limits: ConnStringLimits) -> Result<(), Self::Error> {
        parse_with_limits(input, limits).map(|_| ())
    }
}

// ---- panic-safety on adversarial corpus -------------------------

#[test]
fn parser_does_not_panic_on_adversarial_corpus() {
    for (name, input) in adversarial_inputs() {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_no_panic_on::<ConnStringParser>(&input);
        }));
        if result.is_err() {
            panic!("adversarial corpus entry {} panicked", name);
        }
    }
}

// ---- property tests ---------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        max_shrink_iters: 64,
        ..ProptestConfig::default()
    })]

    /// Generated red:// / reds:// shapes parse cleanly and equal
    /// the canonical target the generator declared.
    #[test]
    fn proptest_red_roundtrips((uri, target) in conn_grammar::red_uri()) {
        harness::roundtrip_property::<ConnStringParser>(&uri);
        let got = parse(&uri).expect("valid red:// uri");
        prop_assert_eq!(got, target, "red:// did not round-trip: {}", uri);
    }

    /// Generated grpc:// / grpcs:// single-host shapes round-trip.
    #[test]
    fn proptest_grpc_roundtrips((uri, target) in conn_grammar::grpc_uri()) {
        harness::roundtrip_property::<ConnStringParser>(&uri);
        let got = parse(&uri).expect("valid grpc:// uri");
        prop_assert_eq!(got, target, "grpc:// did not round-trip: {}", uri);
    }

    /// Generated http:// / https:// shapes round-trip (default
    /// ports 80/443 differ from gRPC family).
    #[test]
    fn proptest_http_roundtrips((uri, target) in conn_grammar::http_uri()) {
        harness::roundtrip_property::<ConnStringParser>(&uri);
        let got = parse(&uri).expect("valid http:// uri");
        prop_assert_eq!(got, target, "http:// did not round-trip: {}", uri);
    }

    /// Generated memory:// / memory: aliases round-trip.
    #[test]
    fn proptest_memory_roundtrips((uri, target) in conn_grammar::memory_uri()) {
        harness::roundtrip_property::<ConnStringParser>(&uri);
        let got = parse(&uri).expect("valid memory:// uri");
        prop_assert_eq!(got, target, "memory:// did not round-trip: {}", uri);
    }

    /// Generated file:///abs/path round-trips.
    #[test]
    fn proptest_file_roundtrips((uri, target) in conn_grammar::file_uri()) {
        harness::roundtrip_property::<ConnStringParser>(&uri);
        let got = parse(&uri).expect("valid file:// uri");
        prop_assert_eq!(got, target, "file:// did not round-trip: {}", uri);
    }

    /// Generated cluster URIs round-trip — including ?route=primary.
    #[test]
    fn proptest_grpc_cluster_roundtrips((uri, target) in conn_grammar::grpc_cluster_uri()) {
        harness::roundtrip_property::<ConnStringParser>(&uri);
        let got = parse(&uri).expect("valid cluster uri");
        prop_assert_eq!(got, target, "cluster did not round-trip: {}", uri);
    }

    /// Arbitrary printable bytes never panic — Err is fine.
    #[test]
    fn proptest_arbitrary_bytes_no_panic(s in ".{0,2048}") {
        harness::roundtrip_property::<ConnStringParser>(&s);
    }

    /// Arbitrary raw bytes (any UTF-8 codepoint) never panic.
    /// Wider than the printable strategy above.
    #[test]
    fn proptest_arbitrary_unicode_no_panic(s in "\\PC{0,512}") {
        harness::roundtrip_property::<ConnStringParser>(&s);
    }

    /// Tighter max_uri_bytes always refuses oversized inputs with
    /// a structured LimitExceeded error.
    #[test]
    fn proptest_uri_size_limit_enforced(len in 100usize..1000) {
        let limits = ConnStringLimits {
            max_uri_bytes: 64,
            ..ConnStringLimits::default()
        };
        let input = format!("red://{}", "h".repeat(len));
        let r = parse_with_limits(&input, limits);
        prop_assert!(matches!(
            r,
            Err(ParseError { kind: ParseErrorKind::LimitExceeded, .. }),
        ), "oversized input must hit LimitExceeded, got {:?}", r);
    }

    /// Tighter max_query_params always refuses inputs with too
    /// many `&`-separated parameters.
    #[test]
    fn proptest_query_param_limit_enforced(n in 5usize..32) {
        let limits = ConnStringLimits {
            max_query_params: 4,
            ..ConnStringLimits::default()
        };
        let qs: Vec<String> = (0..n).map(|i| format!("k{i}=v{i}")).collect();
        let input = format!("red://h?{}", qs.join("&"));
        let r = parse_with_limits(&input, limits);
        prop_assert!(matches!(
            r,
            Err(ParseError { kind: ParseErrorKind::LimitExceeded, .. }),
        ), "too many query params must hit LimitExceeded, got {:?}", r);
    }

    /// Tighter max_cluster_hosts always refuses oversized cluster
    /// URIs.
    #[test]
    fn proptest_cluster_host_limit_enforced(n in 6usize..50) {
        let limits = ConnStringLimits {
            max_cluster_hosts: 5,
            ..ConnStringLimits::default()
        };
        let hosts: Vec<String> = (0..n).map(|i| format!("h{i}:5055")).collect();
        let input = format!("grpc://{}", hosts.join(","));
        let r = parse_with_limits(&input, limits);
        prop_assert!(matches!(
            r,
            Err(ParseError { kind: ParseErrorKind::LimitExceeded, .. }),
        ), "too many cluster hosts must hit LimitExceeded, got {:?}", r);
    }
}

// ---- targeted i18n / scheme-normalisation regression tests ------

#[test]
fn mixed_case_red_scheme_normalises() {
    // Per the conn-string doc: scheme is case-insensitive; the
    // canonical form is lowercase. `Red://Host` and `RED://Host`
    // must produce the same target as `red://Host`.
    let target = parse("Red://primary.svc:5050").expect("Red:// must parse");
    let baseline = parse("red://primary.svc:5050").unwrap();
    assert_eq!(target, baseline);
}

#[test]
fn upper_case_reds_scheme_normalises() {
    let target = parse("REDS://host.example").expect("REDS:// must parse");
    let baseline = parse("reds://host.example").unwrap();
    assert_eq!(target, baseline);
}

#[test]
fn mixed_case_grpc_scheme_normalises() {
    let target = parse("GrPc://primary").expect("GrPc:// must parse");
    let baseline = parse("grpc://primary").unwrap();
    assert_eq!(target, baseline);
}

#[test]
fn punycode_host_parses() {
    // Punycode forms are valid ASCII-only DNS labels — the parser
    // must accept them verbatim without trying to normalise them
    // further.
    let target = parse("red://xn--bcher-kva.example:5050").expect("punycode host must parse");
    match target {
        reddb_wire::ConnectionTarget::RedWire { host, .. } => {
            assert_eq!(host, "xn--bcher-kva.example");
        }
        other => panic!("expected RedWire, got {other:?}"),
    }
}

#[test]
fn ipv6_loopback_parses() {
    let target = parse("red://[::1]:5050").expect("IPv6 loopback must parse");
    match target {
        reddb_wire::ConnectionTarget::RedWire { host, port, .. } => {
            assert_eq!(host, "[::1]");
            assert_eq!(port, 5050);
        }
        other => panic!("expected RedWire, got {other:?}"),
    }
}

#[test]
fn ipv6_full_address_parses() {
    let target = parse("red://[2001:db8::1]:5050").expect("IPv6 full must parse");
    match target {
        reddb_wire::ConnectionTarget::RedWire { host, port, .. } => {
            assert_eq!(host, "[2001:db8::1]");
            assert_eq!(port, 5050);
        }
        other => panic!("expected RedWire, got {other:?}"),
    }
}

#[test]
fn percent_encoded_path_parses() {
    // The `中` character percent-encoded as the URL spec demands.
    let target = parse("file:///srv/%E4%B8%AD/data.rdb").expect("percent-encoded path must parse");
    match target {
        reddb_wire::ConnectionTarget::File { path } => {
            assert_eq!(path.to_string_lossy(), "/srv/%E4%B8%AD/data.rdb");
        }
        other => panic!("expected File, got {other:?}"),
    }
}

#[test]
fn dos_uri_size_default_8kib() {
    // Default ConnStringLimits caps URIs at 8 KiB. 9 KiB must
    // hit LimitExceeded BEFORE any url::Url work happens.
    let big = format!("red://{}", "a".repeat(9 * 1024));
    let err = parse(&big).expect_err("9 KiB URI must be refused");
    assert_eq!(err.kind, ParseErrorKind::LimitExceeded);
    assert!(
        err.message.contains("max_uri_bytes"),
        "message should name the limit: {}",
        err.message,
    );
}

#[test]
fn dos_query_params_default_32() {
    // 33 params trips the default cap.
    let qs: Vec<String> = (0..33).map(|i| format!("k{i}=v{i}")).collect();
    let uri = format!("red://h?{}", qs.join("&"));
    let err = parse(&uri).expect_err("33 query params must be refused");
    assert_eq!(err.kind, ParseErrorKind::LimitExceeded);
    assert!(
        err.message.contains("max_query_params"),
        "message should name the limit: {}",
        err.message,
    );
}

#[test]
fn dos_cluster_hosts_default_64() {
    // 65 cluster hosts trips the default cap.
    let hosts: Vec<String> = (0..65).map(|i| format!("h{i}:5055")).collect();
    let uri = format!("grpc://{}", hosts.join(","));
    let err = parse(&uri).expect_err("65 cluster hosts must be refused");
    assert_eq!(err.kind, ParseErrorKind::LimitExceeded);
    assert!(
        err.message.contains("max_cluster_hosts"),
        "message should name the limit: {}",
        err.message,
    );
}

#[test]
fn bom_prefix_is_rejected_not_panicked() {
    // U+FEFF byte-order mark at the start. Should not panic; an
    // error is acceptable since it is not part of the documented
    // grammar.
    let uri = "\u{feff}red://host:5050";
    let _ = parse(uri); // Ok or Err — only panic is a regression.
}
