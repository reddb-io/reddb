#[path = "../../../tests/support/mock_ai_provider.rs"]
mod mock_ai_provider;

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use mock_ai_provider::{MockAiProvider, MockAiProviderConfig};
use reddb_server::ai::{openai_embeddings, OpenAiEmbeddingRequest};
use std::time::Instant;

const ROWS_PER_INSERT: usize = 1_000;
const PROVIDER_LATENCY_MS: u64 = 2;
const DUP_TEXT_COUNT: usize = 100;

fn ai_batch_foundation(c: &mut Criterion) {
    let inputs = MockAiProvider::duplicate_bench_inputs(ROWS_PER_INSERT, DUP_TEXT_COUNT);

    let mut group = c.benchmark_group("ai-batch-foundation");
    group.throughput(Throughput::Elements(ROWS_PER_INSERT as u64));
    group.sample_size(10);

    group.bench_function("legacy-per-row-insert-1000-auto-embed-mock", |b| {
        b.iter_custom(|iters| {
            let provider = MockAiProvider::start(MockAiProviderConfig {
                latency_ms: PROVIDER_LATENCY_MS,
                dup_text_count: DUP_TEXT_COUNT,
                ..MockAiProviderConfig::default()
            })
            .expect("mock AI provider should start");

            let started = Instant::now();
            for _ in 0..iters {
                for input in &inputs {
                    let response = openai_embeddings(OpenAiEmbeddingRequest {
                        api_key: "bench-key".to_string(),
                        model: "mock-embed".to_string(),
                        inputs: vec![input.clone()],
                        dimensions: None,
                        api_base: provider.api_base(),
                    })
                    .expect("mock embedding request should succeed");
                    black_box(response.embeddings);
                }
            }
            let elapsed = started.elapsed();

            let counters = provider.counters();
            let latency = provider.latency();
            let inserts = iters.max(1);
            eprintln!(
                "\n[ai_batch_bench] requests_per_insert={:.2} total_duration_ms={} provider_latency_p50={:.3} provider_latency_p99={:.3} total_requests={} total_inputs={} unique_inputs={}",
                counters.total_requests as f64 / inserts as f64,
                elapsed.as_millis(),
                latency.p50_ms,
                latency.p99_ms,
                counters.total_requests,
                counters.total_inputs,
                counters.unique_inputs
            );

            elapsed
        });
    });

    group.finish();
}

criterion_group!(benches, ai_batch_foundation);
criterion_main!(benches);
