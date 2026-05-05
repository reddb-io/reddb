//! Adversarial-input fixtures for the connection-string parser.
//!
//! Each entry is an `(name, input)` pair. The same corpus is
//! consumed by:
//!   - the panic-safety property tests in
//!     `tests/parser_hardening.rs`
//!   - the fuzz seed corpus loaded by
//!     `fuzz_targets/conn_string_parser.rs`
//!
//! Adding a regression case here automatically extends both
//! safety nets. Keep entries small — the corpus is iterated on
//! every property-test run.

/// Adversarial inputs that historically (or theoretically) trip
/// pre-auth code paths. None of these should panic; all should
/// either parse or return an `Err`.
pub fn adversarial_inputs() -> Vec<(&'static str, String)> {
    vec![
        ("empty", String::new()),
        ("only_whitespace", "    \n\t  ".to_string()),
        ("scheme_only_colon", "red:".to_string()),
        ("scheme_only_slashes", "red://".to_string()),
        ("missing_scheme", "primary.svc:5050".to_string()),
        ("garbage_bytes", "@#$%^&*()_+|}{:?><".to_string()),
        ("nul_byte", "red://host\0:5050".to_string()),
        ("bom_prefix", "\u{feff}red://host:5050".to_string()),
        // RFC-defined right-to-left override; should not crash the
        // parser even though `url::Url` will reject it.
        (
            "rtl_override_in_host",
            "red://h\u{202e}ost:5050".to_string(),
        ),
        // BIDI / RTL hosts: punycode-style + raw Arabic.
        ("punycode_host", "red://xn--bcher-kva.example".to_string()),
        ("rtl_arabic_host", "red://مثال.إختبار".to_string()),
        // IPv6 bracket forms.
        ("ipv6_loopback", "red://[::1]:5050".to_string()),
        ("ipv6_full", "red://[2001:db8::1]:5050".to_string()),
        ("ipv6_no_port", "red://[::1]".to_string()),
        ("ipv6_unterminated_bracket", "red://[::1:5050".to_string()),
        // Percent-encoded path bytes — the documented `中` example.
        (
            "percent_encoded_path",
            "file:///srv/%E4%B8%AD/data.rdb".to_string(),
        ),
        (
            "percent_encoded_query",
            "red://h:5050?x=%E4%B8%AD".to_string(),
        ),
        // Mixed-case schemes — must normalise.
        ("mixed_case_red", "Red://Host:5050".to_string()),
        ("upper_case_reds", "REDS://host:5050".to_string()),
        ("mixed_case_grpc", "GrPc://primary".to_string()),
        // Ports.
        ("port_non_numeric", "red://host:nope".to_string()),
        ("port_overflow", "red://host:99999".to_string()),
        ("port_negative", "red://host:-1".to_string()),
        ("port_empty", "red://host:".to_string()),
        // Cluster forms.
        (
            "cluster_empty_middle",
            "grpc://primary,,replica".to_string(),
        ),
        ("cluster_only_commas", "grpc://,,,".to_string()),
        ("cluster_trailing_comma", "grpc://a,".to_string()),
        (
            "cluster_with_ipv6",
            "grpc://[::1]:5050,[::2]:5050".to_string(),
        ),
        (
            "cluster_with_route_override",
            "grpc://a,b?route=primary".to_string(),
        ),
        // Long inputs — exercise the size guard.
        (
            "oversized_input",
            format!("red://{}:5050", "h".repeat(16 * 1024)),
        ),
        (
            "many_query_params",
            format!(
                "red://h:5050?{}",
                (0..200)
                    .map(|i| format!("k{i}=v{i}"))
                    .collect::<Vec<_>>()
                    .join("&"),
            ),
        ),
        (
            "many_cluster_hosts",
            format!(
                "grpc://{}",
                (0..256)
                    .map(|i| format!("h{i}:5055"))
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        ),
        // Misc adversarial.
        ("only_question_mark", "red://?".to_string()),
        ("just_query", "?route=primary".to_string()),
        ("file_no_path", "file://".to_string()),
        ("unknown_scheme", "mongodb://localhost".to_string()),
        ("scheme_with_plus", "red+tls://host".to_string()),
        (
            "scheme_uppercase_unknown",
            "MONGODB://localhost".to_string(),
        ),
        // Unicode normalisation hazards.
        ("zero_width_joiner_host", "red://a\u{200d}b".to_string()),
        ("combining_marks_host", "red://a\u{0301}".to_string()),
    ]
}
