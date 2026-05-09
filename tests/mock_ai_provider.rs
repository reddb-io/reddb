#[path = "support/mock_ai_provider.rs"]
mod mock_ai_provider;

use mock_ai_provider::{MockAiErrorKind, MockAiProvider, MockAiProviderConfig};

#[test]
fn mock_ai_provider_serves_openai_compatible_embeddings_and_counts_inputs() {
    let provider = MockAiProvider::start(MockAiProviderConfig {
        latency_ms: 1,
        dup_text_count: 1,
        ..MockAiProviderConfig::default()
    })
    .expect("mock provider should start");

    let response = post_json(
        &format!("{}/embeddings", provider.api_base()),
        r#"{"model":"mock-embed","input":["same","same","different"]}"#,
    );

    assert_eq!(response.status, 200);
    assert!(
        response.body.contains(r#""object":"list""#),
        "{}",
        response.body
    );
    assert!(
        response.body.contains(r#""embedding""#),
        "{}",
        response.body
    );

    let counters = provider.counters();
    assert_eq!(counters.total_requests, 1);
    assert_eq!(counters.total_inputs, 3);
    assert_eq!(counters.unique_inputs, 2);
    assert!(provider.latency().p50_ms >= 1.0);
}

#[test]
fn mock_ai_provider_can_return_rate_limit_and_server_errors() {
    for (kind, status) in [
        (MockAiErrorKind::RateLimit, 429),
        (MockAiErrorKind::Server, 500),
    ] {
        let provider = MockAiProvider::start(MockAiProviderConfig {
            error_rate: 1.0,
            error_kind: kind,
            ..MockAiProviderConfig::default()
        })
        .expect("mock provider should start");

        let response = post_json(
            &format!("{}/embeddings", provider.api_base()),
            r#"{"model":"mock-embed","input":"hello"}"#,
        );

        assert_eq!(response.status, status);
        assert!(response.body.contains("mock"));
        assert_eq!(provider.counters().total_requests, 1);
    }
}

struct HttpResponse {
    status: u16,
    body: String,
}

fn post_json(url: &str, body: &str) -> HttpResponse {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = agent
        .post(url)
        .header("content-type", "application/json")
        .send(body)
        .expect("mock request should complete");
    HttpResponse {
        status: response.status().as_u16(),
        body: response
            .body_mut()
            .read_to_string()
            .expect("response body should read"),
    }
}
