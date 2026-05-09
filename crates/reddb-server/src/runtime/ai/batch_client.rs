//! Batch embedding client — issue #275.
//!
//! Accepts a `Vec<String>`, splits into sub-batches up to `max_batch_size`,
//! sends each via `AiTransport` with retry, and reassembles results in the
//! original order. Empty texts are skipped; their positions get an empty
//! `Vec<f32>` in the output.
//!
//! Issue #277 adds optional dedup cache and text chunking.

use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

use crate::ai::AiProvider;
use crate::json::{Map, Value as JsonValue};
use crate::runtime::ai::dedup_cache::{
    EmbeddingDedupCache, DEFAULT_DEDUP_LRU_SIZE, DEFAULT_DEDUP_TTL_MS,
};
use crate::runtime::ai::text_chunker::{ChunkMode, DEFAULT_MAX_TOKENS};
use crate::runtime::ai::transport::{AiHttpRequest, AiTransport, AiTransportError};
use crate::runtime::audit_log::AuditLogger;

pub const CONFIG_MAX_BATCH_SIZE: &str = "runtime.ai.embedding_max_batch_size";
pub const DEFAULT_OPENAI_MAX_BATCH: usize = 2048;
pub const DEFAULT_OTHER_MAX_BATCH: usize = 256;

/// One sub-batch worth of work.
pub struct SubBatchRequest {
    pub provider: String,
    pub api_key: String,
    pub api_base: String,
    pub model: String,
    pub inputs: Vec<String>,
}

pub struct SubBatchResponse {
    pub embeddings: Vec<Vec<f32>>,
    pub model: String,
    pub prompt_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub attempt_count: u32,
    pub total_wait_ms: u64,
}

/// Backend abstraction. Production uses `AiTransportSender`; tests use mocks.
pub trait SubBatchSender: Send + Sync {
    fn send(
        &self,
        request: SubBatchRequest,
    ) -> impl Future<Output = Result<SubBatchResponse, AiTransportError>> + Send + '_;
}

/// Production backend: routes sub-batches through `AiTransport`.
pub struct AiTransportSender {
    pub transport: AiTransport,
}

impl SubBatchSender for AiTransportSender {
    fn send(
        &self,
        request: SubBatchRequest,
    ) -> impl Future<Output = Result<SubBatchResponse, AiTransportError>> + Send + '_ {
        async move {
            let payload = crate::ai::build_embedding_payload(&request.model, &request.inputs);
            let url = format!("{}/embeddings", request.api_base.trim_end_matches('/'));
            let http_req = AiHttpRequest::post_json(request.provider.as_str(), url, payload)
                .model(request.model.clone())
                .header("authorization", format!("Bearer {}", request.api_key));

            let response = self.transport.request(http_req).await?;

            let parsed = crate::ai::parse_embedding_response(&response.body).map_err(|msg| {
                AiTransportError {
                    provider: request.provider.clone(),
                    status_code: None,
                    attempt_count: 1,
                    total_wait_ms: 0,
                    message: msg,
                }
            })?;

            Ok(SubBatchResponse {
                embeddings: parsed.embeddings,
                model: parsed.model,
                prompt_tokens: parsed.prompt_tokens,
                total_tokens: parsed.total_tokens,
                attempt_count: response.attempt_count,
                total_wait_ms: response.total_wait_ms,
            })
        }
    }
}

/// Batch embedding client.
///
/// Generic over the backend so tests can inject mocks without HTTP.
/// The default type parameter is `AiTransportSender` (production).
pub struct AiBatchClient<S = AiTransportSender> {
    sender: S,
    max_batch_size_override: Option<usize>,
    /// Optional dedup cache. None = dedup disabled (default).
    dedup_cache: Option<Arc<EmbeddingDedupCache>>,
    /// Chunk mode applied before sending. Default = Single.
    chunk_mode: ChunkMode,
    /// Max tokens per chunk (approximate: 1 token ≈ 4 bytes).
    max_tokens: usize,
    audit_log: Option<Arc<AuditLogger>>,
}

impl AiBatchClient<AiTransportSender> {
    pub fn new(transport: AiTransport) -> Self {
        Self {
            sender: AiTransportSender { transport },
            max_batch_size_override: None,
            dedup_cache: None,
            chunk_mode: ChunkMode::Single,
            max_tokens: DEFAULT_MAX_TOKENS,
            audit_log: None,
        }
    }

    pub fn from_runtime(runtime: &crate::runtime::RedDBRuntime) -> Self {
        use crate::runtime::ai::dedup_cache::{
            CONFIG_DEDUP_ENABLED, CONFIG_DEDUP_LRU_SIZE, CONFIG_DEDUP_TTL_MS,
        };
        use crate::runtime::ai::text_chunker::{CONFIG_CHUNK_MODE, CONFIG_MAX_TOKENS};
        use std::time::Duration;

        let transport = AiTransport::from_runtime(runtime);
        let dedup_enabled = runtime.config_bool(CONFIG_DEDUP_ENABLED, false);
        let dedup_cache = if dedup_enabled {
            let lru_size =
                runtime.config_u64(CONFIG_DEDUP_LRU_SIZE, DEFAULT_DEDUP_LRU_SIZE as u64) as usize;
            let ttl_ms = runtime.config_u64(CONFIG_DEDUP_TTL_MS, DEFAULT_DEDUP_TTL_MS);
            Some(Arc::new(EmbeddingDedupCache::new(
                lru_size,
                Duration::from_millis(ttl_ms),
            )))
        } else {
            None
        };
        let chunk_mode = ChunkMode::from_str(&runtime.config_string(CONFIG_CHUNK_MODE, "single"));
        let max_tokens = runtime.config_u64(CONFIG_MAX_TOKENS, DEFAULT_MAX_TOKENS as u64) as usize;

        Self {
            sender: AiTransportSender { transport },
            max_batch_size_override: None,
            dedup_cache,
            chunk_mode,
            max_tokens,
            audit_log: Some(runtime.audit_log_arc()),
        }
    }
}

impl<S: SubBatchSender> AiBatchClient<S> {
    /// Create with a custom backend (useful in tests).
    pub fn with_sender(sender: S) -> Self {
        Self {
            sender,
            max_batch_size_override: None,
            dedup_cache: None,
            chunk_mode: ChunkMode::Single,
            max_tokens: DEFAULT_MAX_TOKENS,
            audit_log: None,
        }
    }

    /// Override the max sub-batch size (defaults per provider if not set).
    pub fn with_max_batch_size(mut self, size: usize) -> Self {
        self.max_batch_size_override = Some(size.max(1));
        self
    }

    /// Enable dedup cache.
    pub fn with_dedup_cache(mut self, cache: Arc<EmbeddingDedupCache>) -> Self {
        self.dedup_cache = Some(cache);
        self
    }

    /// Set chunk mode (Single or Multi).
    pub fn with_chunk_mode(mut self, mode: ChunkMode) -> Self {
        self.chunk_mode = mode;
        self
    }

    /// Set max tokens per chunk.
    pub fn with_max_tokens(mut self, max: usize) -> Self {
        self.max_tokens = max.max(1);
        self
    }

    pub fn with_audit_log(mut self, audit_log: Arc<AuditLogger>) -> Self {
        self.audit_log = Some(audit_log);
        self
    }

    /// Embed `texts` in batch. Returns one `Vec<f32>` per input in order.
    /// Empty/whitespace-only inputs yield an empty `Vec<f32>` at their position
    /// without consuming a provider request slot.
    ///
    /// When dedup is enabled, previously-seen texts are served from cache and
    /// only unseen texts are sent to the provider. Duplicate texts within a
    /// single call are also deduplicated — the provider receives each unique
    /// text only once.
    ///
    /// When chunking is enabled, texts exceeding `max_tokens` are chunked;
    /// in Single mode the first chunk is sent to the provider.
    pub async fn embed_batch(
        &self,
        provider: &AiProvider,
        model: &str,
        api_key: &str,
        texts: Vec<String>,
    ) -> Result<Vec<Vec<f32>>, AiTransportError> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        let max_batch = self
            .max_batch_size_override
            .unwrap_or_else(|| default_max_batch_size(provider));
        let api_base = provider.resolve_api_base();
        let started = Instant::now();
        let mut local_dedup_hits = 0u64;
        let mut any_chunked = false;
        let mut retries_total = 0u64;
        let mut total_wait_ms = 0u64;
        let mut prompt_tokens_total = 0u64;
        let mut total_tokens_total = 0u64;

        // Step 1: apply chunking — each text → representative text to embed.
        // In Single mode this is the first (or only) chunk.
        let mut chunked_texts: Vec<String> = Vec::with_capacity(texts.len());
        for t in &texts {
            let chunks = crate::runtime::ai::text_chunker::chunk(t, self.max_tokens);
            if chunks.len() > 1 {
                any_chunked = true;
            }
            let chosen = crate::runtime::ai::text_chunker::apply_mode(chunks, self.chunk_mode);
            chunked_texts.push(chosen.into_iter().next().unwrap_or_default());
        }

        // Step 2: check dedup cache and collect unique provider misses.
        // result[i] = Some(embedding) when resolved, None when pending.
        let mut result: Vec<Option<Vec<f32>>> = vec![None; texts.len()];

        // unique_texts_to_embed: insertion-ordered unique texts that need a
        // provider call. text → index in unique_texts_to_embed.
        let mut unique_text_index: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let mut unique_texts_to_embed: Vec<String> = Vec::new();

        // For each input, map position → unique_texts index (for fan-out later).
        let mut pos_to_unique: Vec<Option<usize>> = vec![None; texts.len()];

        for (i, text) in chunked_texts.iter().enumerate() {
            if text.trim().is_empty() {
                result[i] = Some(vec![]);
                continue;
            }
            // Cache lookup (covers both warm cache and intra-batch duplicates
            // that were already cached in a prior iteration of this loop after
            // the provider returned).
            if let Some(cache) = &self.dedup_cache {
                if let Some(cached) = cache.get(text) {
                    local_dedup_hits = local_dedup_hits.saturating_add(1);
                    result[i] = Some(cached);
                    continue;
                }
            }
            // Dedup within this batch: if text already queued, reuse its slot.
            let unique_idx = if let Some(&existing) = unique_text_index.get(text.as_str()) {
                existing
            } else {
                let idx = unique_texts_to_embed.len();
                unique_text_index.insert(text.clone(), idx);
                unique_texts_to_embed.push(text.clone());
                idx
            };
            pos_to_unique[i] = Some(unique_idx);
        }

        // Step 3: send unique_texts_to_embed in sub-batches.
        let mut unique_embeddings: Vec<Vec<f32>> = vec![vec![]; unique_texts_to_embed.len()];

        for chunk in unique_texts_to_embed.chunks(max_batch) {
            crate::runtime::ai::metrics::record_batch_size(provider.token(), chunk.len());
            // Determine the start index of this chunk within unique_texts_to_embed.
            // (We need this to write results back into unique_embeddings.)
            let chunk_start = {
                // chunk is a subslice of unique_texts_to_embed; compute offset.
                let base = unique_texts_to_embed.as_ptr();
                let ptr = chunk.as_ptr();
                (ptr as usize - base as usize) / std::mem::size_of::<String>()
            };

            let request = SubBatchRequest {
                provider: provider.token().to_string(),
                api_key: api_key.to_string(),
                api_base: api_base.clone(),
                model: model.to_string(),
                inputs: chunk.to_vec(),
            };

            let response = match self.sender.send(request).await {
                Ok(response) => response,
                Err(err) => {
                    self.record_error_audit(provider.token(), &err);
                    return Err(err);
                }
            };
            retries_total =
                retries_total.saturating_add(u64::from(response.attempt_count.saturating_sub(1)));
            total_wait_ms = total_wait_ms.saturating_add(response.total_wait_ms);
            if let Some(tokens) = response.prompt_tokens {
                prompt_tokens_total = prompt_tokens_total.saturating_add(tokens);
            }
            if let Some(tokens) = response.total_tokens {
                total_tokens_total = total_tokens_total.saturating_add(tokens);
            }
            let token_metric = response
                .prompt_tokens
                .unwrap_or(0)
                .saturating_add(response.total_tokens.unwrap_or(0));
            crate::runtime::ai::metrics::record_tokens(
                provider.token(),
                &response.model,
                token_metric,
            );
            let embeddings = response.embeddings;

            if embeddings.len() != chunk.len() {
                let err = AiTransportError {
                    provider: provider.token().to_string(),
                    status_code: None,
                    attempt_count: 0,
                    total_wait_ms: 0,
                    message: format!(
                        "provider returned {} embeddings for {} inputs",
                        embeddings.len(),
                        chunk.len()
                    ),
                };
                self.record_error_audit(provider.token(), &err);
                return Err(err);
            }

            for (j, embedding) in embeddings.into_iter().enumerate() {
                let unique_idx = chunk_start + j;
                // Insert into dedup cache
                if let Some(cache) = &self.dedup_cache {
                    cache.insert(&unique_texts_to_embed[unique_idx], embedding.clone());
                }
                unique_embeddings[unique_idx] = embedding;
            }
        }

        // Step 4: fan-out unique_embeddings back to result positions.
        for (i, unique_idx_opt) in pos_to_unique.into_iter().enumerate() {
            if let Some(unique_idx) = unique_idx_opt {
                result[i] = Some(unique_embeddings[unique_idx].clone());
            }
        }

        self.record_batch_audit(BatchAudit {
            provider: provider.token(),
            model,
            batch_size: texts.len(),
            total_tokens: total_tokens_total,
            duration_ms: millis_u64(started.elapsed()),
            retries: retries_total,
            dedup_hits: local_dedup_hits,
            chunked: any_chunked,
            total_wait_ms,
            prompt_tokens: prompt_tokens_total,
        });

        Ok(result.into_iter().map(|v| v.unwrap_or_default()).collect())
    }

    fn record_batch_audit(&self, audit: BatchAudit<'_>) {
        tracing::info!(
            target: "reddb::developer",
            provider = audit.provider,
            model = audit.model,
            batch_size = audit.batch_size,
            total_tokens = audit.total_tokens,
            duration_ms = audit.duration_ms,
            retries = audit.retries,
            dedup_hits = audit.dedup_hits,
            chunked = audit.chunked,
            "ai embedding batch completed"
        );

        let Some(audit_log) = &self.audit_log else {
            return;
        };
        let mut details = Map::new();
        details.insert(
            "provider".to_string(),
            JsonValue::String(audit.provider.to_string()),
        );
        details.insert(
            "model".to_string(),
            JsonValue::String(audit.model.to_string()),
        );
        details.insert(
            "batch_size".to_string(),
            JsonValue::Number(audit.batch_size as f64),
        );
        details.insert(
            "total_tokens".to_string(),
            JsonValue::Number(audit.total_tokens as f64),
        );
        details.insert(
            "duration_ms".to_string(),
            JsonValue::Number(audit.duration_ms as f64),
        );
        details.insert(
            "retries".to_string(),
            JsonValue::Number(audit.retries as f64),
        );
        details.insert(
            "dedup_hits".to_string(),
            JsonValue::Number(audit.dedup_hits as f64),
        );
        details.insert("chunked".to_string(), JsonValue::Bool(audit.chunked));
        details.insert(
            "total_wait_ms".to_string(),
            JsonValue::Number(audit.total_wait_ms as f64),
        );
        details.insert(
            "prompt_tokens".to_string(),
            JsonValue::Number(audit.prompt_tokens as f64),
        );
        audit_log.record(
            "ai/embedding_batch",
            "system",
            audit.provider,
            "ok",
            JsonValue::Object(details),
        );
    }

    fn record_error_audit(&self, provider: &str, err: &AiTransportError) {
        tracing::warn!(
            target: "reddb::developer",
            provider = provider,
            status_code = err.status_code.unwrap_or(0),
            attempt_count = err.attempt_count,
            total_wait_ms = err.total_wait_ms,
            "ai embedding provider error"
        );

        let Some(audit_log) = &self.audit_log else {
            return;
        };
        let mut details = Map::new();
        details.insert(
            "provider".to_string(),
            JsonValue::String(provider.to_string()),
        );
        details.insert(
            "status_code".to_string(),
            err.status_code
                .map(|status| JsonValue::Number(status as f64))
                .unwrap_or(JsonValue::Null),
        );
        details.insert(
            "attempt_count".to_string(),
            JsonValue::Number(err.attempt_count as f64),
        );
        details.insert(
            "total_wait_ms".to_string(),
            JsonValue::Number(err.total_wait_ms as f64),
        );
        audit_log.record(
            "ai/embedding_error",
            "system",
            provider,
            "error",
            JsonValue::Object(details),
        );
    }
}

struct BatchAudit<'a> {
    provider: &'a str,
    model: &'a str,
    batch_size: usize,
    total_tokens: u64,
    duration_ms: u64,
    retries: u64,
    dedup_hits: u64,
    chunked: bool,
    total_wait_ms: u64,
    prompt_tokens: u64,
}

fn millis_u64(duration: std::time::Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn default_max_batch_size(provider: &AiProvider) -> usize {
    match provider {
        AiProvider::OpenAi
        | AiProvider::OpenRouter
        | AiProvider::Together
        | AiProvider::Venice
        | AiProvider::Groq
        | AiProvider::DeepSeek
        | AiProvider::Custom(_) => DEFAULT_OPENAI_MAX_BATCH,
        _ => DEFAULT_OTHER_MAX_BATCH,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    struct MockSender {
        call_count: Arc<AtomicUsize>,
        dims: usize,
    }

    impl SubBatchSender for MockSender {
        fn send(
            &self,
            request: SubBatchRequest,
        ) -> impl Future<Output = Result<SubBatchResponse, AiTransportError>> + Send + '_ {
            let n = request.inputs.len();
            let dims = self.dims;
            self.call_count.fetch_add(1, Ordering::SeqCst);
            async move {
                Ok(SubBatchResponse {
                    embeddings: (0..n).map(|_| vec![0.1f32; dims]).collect(),
                    model: request.model,
                    prompt_tokens: Some(n as u64),
                    total_tokens: Some(n as u64),
                    attempt_count: 1,
                    total_wait_ms: 0,
                })
            }
        }
    }

    fn mock_client(dims: usize) -> (AiBatchClient<MockSender>, Arc<AtomicUsize>) {
        let counter = Arc::new(AtomicUsize::new(0));
        let client = AiBatchClient::with_sender(MockSender {
            call_count: Arc::clone(&counter),
            dims,
        });
        (client, counter)
    }

    #[tokio::test]
    async fn embed_three_texts_returns_three_vectors() {
        let (client, _) = mock_client(3);
        let result = client
            .embed_batch(
                &AiProvider::OpenAi,
                "model",
                "key",
                vec!["a".into(), "b".into(), "c".into()],
            )
            .await
            .unwrap();
        assert_eq!(result.len(), 3);
        assert!(result.iter().all(|v| v.len() == 3));
    }

    #[tokio::test]
    async fn embed_empty_input_zero_requests() {
        let (client, counter) = mock_client(3);
        let result = client
            .embed_batch(&AiProvider::OpenAi, "model", "key", vec![])
            .await
            .unwrap();
        assert!(result.is_empty());
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn embed_1000_inputs_single_request_openai() {
        let (client, counter) = mock_client(4);
        let texts: Vec<String> = (0..1000).map(|i| format!("text {i}")).collect();
        let result = client
            .embed_batch(&AiProvider::OpenAi, "model", "key", texts)
            .await
            .unwrap();
        assert_eq!(result.len(), 1000);
        // 1000 < DEFAULT_OPENAI_MAX_BATCH (2048) → exactly 1 request
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn embed_splits_when_over_max_batch() {
        let (client, counter) = mock_client(2);
        let client = client.with_max_batch_size(3);
        let texts: Vec<String> = (0..7).map(|i| format!("t{i}")).collect();
        let result = client
            .embed_batch(&AiProvider::OpenAi, "model", "key", texts)
            .await
            .unwrap();
        assert_eq!(result.len(), 7);
        // ceil(7/3) = 3 batches
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn embed_records_batch_size_and_token_metrics() {
        let (client, _) = mock_client(2);
        let provider = AiProvider::Custom("test_batch_metrics_provider".to_string());
        let _ = client
            .with_max_batch_size(2)
            .embed_batch(
                &provider,
                "test-batch-metrics-model",
                "key",
                vec!["a".into(), "b".into(), "c".into()],
            )
            .await
            .unwrap();

        let mut body = String::new();
        crate::runtime::ai::metrics::render_ai_metrics(&mut body);
        assert!(
            body.contains(
                "reddb_ai_embedding_batch_size_count{provider=\"test_batch_metrics_provider\"} 2"
            ),
            "{body}"
        );
        assert!(
            body.contains(
                "reddb_ai_text_tokens_total{provider=\"test_batch_metrics_provider\",model=\"test-batch-metrics-model\"} 6"
            ),
            "{body}"
        );
    }

    #[tokio::test]
    async fn embed_empty_strings_skipped_positions_preserved() {
        let (client, counter) = mock_client(2);
        let texts = vec![
            "".to_string(),
            "hello".to_string(),
            "  ".to_string(),
            "world".to_string(),
        ];
        let result = client
            .embed_batch(&AiProvider::OpenAi, "model", "key", texts)
            .await
            .unwrap();
        assert_eq!(result.len(), 4);
        assert!(result[0].is_empty(), "empty string → empty vec");
        assert_eq!(result[1].len(), 2, "hello → embedding");
        assert!(result[2].is_empty(), "whitespace-only → empty vec");
        assert_eq!(result[3].len(), 2, "world → embedding");
        // Only 2 non-empty texts → 1 request
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn embed_error_propagated() {
        struct ErrorSender;

        impl SubBatchSender for ErrorSender {
            fn send(
                &self,
                request: SubBatchRequest,
            ) -> impl Future<Output = Result<SubBatchResponse, AiTransportError>> + Send + '_
            {
                async move {
                    Err(AiTransportError {
                        provider: request.provider,
                        status_code: Some(500),
                        attempt_count: 3,
                        total_wait_ms: 2000,
                        message: "server error".to_string(),
                    })
                }
            }
        }

        let client = AiBatchClient::with_sender(ErrorSender);
        let err = client
            .embed_batch(
                &AiProvider::OpenAi,
                "model",
                "key",
                vec!["text".to_string()],
            )
            .await
            .unwrap_err();
        assert_eq!(err.status_code, Some(500));
        assert_eq!(err.attempt_count, 3);
    }

    #[tokio::test]
    async fn embed_writes_structured_audit_line_when_logger_attached() {
        let (client, _) = mock_client(2);
        let dir = tempfile::tempdir().unwrap();
        let audit_path = dir.path().join(".audit.log");
        let audit_log = Arc::new(AuditLogger::with_max_bytes(audit_path, 1024 * 1024));
        let provider = AiProvider::Custom("test_audit_provider".to_string());

        let _ = client
            .with_audit_log(Arc::clone(&audit_log))
            .embed_batch(
                &provider,
                "test-audit-model",
                "key",
                vec!["alpha".into(), "beta".into()],
            )
            .await
            .unwrap();

        assert!(audit_log.wait_idle(Duration::from_secs(2)));
        let body = std::fs::read_to_string(audit_log.path()).unwrap();
        assert!(body.contains("\"action\":\"ai/embedding_batch\""), "{body}");
        assert!(
            body.contains("\"provider\":\"test_audit_provider\""),
            "{body}"
        );
        assert!(body.contains("\"model\":\"test-audit-model\""), "{body}");
        assert!(body.contains("\"batch_size\":2"), "{body}");
        assert!(body.contains("\"total_tokens\":2"), "{body}");
        assert!(body.contains("\"duration_ms\""), "{body}");
        assert!(body.contains("\"retries\":0"), "{body}");
        assert!(body.contains("\"dedup_hits\":0"), "{body}");
        assert!(body.contains("\"chunked\":false"), "{body}");
    }

    #[tokio::test]
    async fn embed_order_preserved_across_batches() {
        struct BatchNumberSender {
            call_count: Arc<AtomicUsize>,
        }

        impl SubBatchSender for BatchNumberSender {
            fn send(
                &self,
                request: SubBatchRequest,
            ) -> impl Future<Output = Result<SubBatchResponse, AiTransportError>> + Send + '_
            {
                let call = self.call_count.fetch_add(1, Ordering::SeqCst);
                let n = request.inputs.len();
                async move {
                    // encode batch number as first float for order verification
                    Ok(SubBatchResponse {
                        embeddings: (0..n).map(|_| vec![call as f32]).collect(),
                        model: request.model,
                        prompt_tokens: Some(n as u64),
                        total_tokens: Some(n as u64),
                        attempt_count: 1,
                        total_wait_ms: 0,
                    })
                }
            }
        }

        let counter = Arc::new(AtomicUsize::new(0));
        let client = AiBatchClient::with_sender(BatchNumberSender {
            call_count: Arc::clone(&counter),
        })
        .with_max_batch_size(3);

        // 5 texts → 2 batches (3 + 2)
        let texts: Vec<String> = (0..5).map(|i| format!("t{i}")).collect();
        let result = client
            .embed_batch(&AiProvider::OpenAi, "model", "key", texts)
            .await
            .unwrap();

        assert_eq!(result.len(), 5);
        assert_eq!(counter.load(Ordering::SeqCst), 2);
        // First 3 from batch 0
        assert_eq!(result[0], vec![0.0]);
        assert_eq!(result[1], vec![0.0]);
        assert_eq!(result[2], vec![0.0]);
        // Last 2 from batch 1
        assert_eq!(result[3], vec![1.0]);
        assert_eq!(result[4], vec![1.0]);
    }

    #[tokio::test]
    async fn default_max_batch_size_openai_is_2048() {
        assert_eq!(default_max_batch_size(&AiProvider::OpenAi), 2048);
    }

    #[tokio::test]
    async fn default_max_batch_size_ollama_is_256() {
        assert_eq!(default_max_batch_size(&AiProvider::Ollama), 256);
    }

    // ── Issue #277: dedup cache tests ──────────────────────────────────────

    #[tokio::test]
    async fn dedup_on_1000_inputs_10_unique_sends_10_to_provider() {
        let (base_client, counter) = mock_client(4);
        let cache = Arc::new(EmbeddingDedupCache::new(1024, Duration::from_secs(60)));
        let client = base_client.with_dedup_cache(Arc::clone(&cache));

        let unique: Vec<String> = (0..10).map(|i| format!("unique text {i}")).collect();
        let texts: Vec<String> = (0..1000).map(|i| unique[i % 10].clone()).collect();

        let result = client
            .embed_batch(&AiProvider::OpenAi, "model", "key", texts.clone())
            .await
            .unwrap();

        assert_eq!(result.len(), 1000);
        // Intra-batch dedup: provider receives 10 unique texts in 1 sub-batch.
        assert_eq!(counter.load(Ordering::SeqCst), 1, "1 sub-batch request");
        // First call: all 1000 texts check the cache → 1000 misses (cache empty).
        // Intra-batch duplicates are deduplicated via HashMap, but still count
        // as cache misses since each text is checked individually.
        assert_eq!(cache.misses(), 1000);
        assert_eq!(cache.hits(), 0);

        // Second call with same texts: cache now has 10 entries → all 1000
        // input texts hit cache (10 unique + 990 duplicates each hit).
        let result2 = client
            .embed_batch(&AiProvider::OpenAi, "model", "key", texts)
            .await
            .unwrap();
        assert_eq!(result2.len(), 1000);
        // No new provider call — all served from cache
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "still 1 provider request total"
        );
        assert_eq!(cache.hits(), 1000, "all 1000 hit cache on second call");
    }

    #[tokio::test]
    async fn dedup_off_by_default_all_texts_sent() {
        let (client, counter) = mock_client(4);
        // no dedup cache attached
        let texts: Vec<String> = (0..10).map(|i| format!("text {i}")).collect();
        let result = client
            .embed_batch(&AiProvider::OpenAi, "model", "key", texts.clone())
            .await
            .unwrap();
        assert_eq!(result.len(), 10);
        // Second call with same texts — still sends all (no cache)
        let _ = client
            .embed_batch(&AiProvider::OpenAi, "model", "key", texts)
            .await
            .unwrap();
        // 2 calls, each 1 sub-batch
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn chunker_long_text_truncated_to_first_chunk_single_mode() {
        // 1 token ≈ 4 bytes; max_tokens=10 → max 40 bytes
        let (base_client, counter) = mock_client(2);
        let client = base_client.with_max_tokens(10); // 40 byte chunks

        let long_text = "a".repeat(200); // 200 bytes >> 40 byte limit
        let result = client
            .embed_batch(&AiProvider::OpenAi, "model", "key", vec![long_text])
            .await
            .unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        // provider received 1 item (first chunk only in Single mode)
    }
}
