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

use crate::ai::AiProvider;
use crate::runtime::ai::dedup_cache::{
    EmbeddingDedupCache, DEFAULT_DEDUP_LRU_SIZE, DEFAULT_DEDUP_TTL_MS,
};
use crate::runtime::ai::text_chunker::{ChunkMode, DEFAULT_MAX_TOKENS};
use crate::runtime::ai::transport::{AiHttpRequest, AiTransport, AiTransportError};

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

/// Backend abstraction. Production uses `AiTransportSender`; tests use mocks.
pub trait SubBatchSender: Send + Sync {
    fn send(
        &self,
        request: SubBatchRequest,
    ) -> impl Future<Output = Result<Vec<Vec<f32>>, AiTransportError>> + Send + '_;
}

/// Production backend: routes sub-batches through `AiTransport`.
pub struct AiTransportSender {
    pub transport: AiTransport,
}

impl SubBatchSender for AiTransportSender {
    fn send(
        &self,
        request: SubBatchRequest,
    ) -> impl Future<Output = Result<Vec<Vec<f32>>, AiTransportError>> + Send + '_ {
        async move {
            let payload = crate::ai::build_embedding_payload(&request.model, &request.inputs);
            let url = format!("{}/embeddings", request.api_base.trim_end_matches('/'));
            let http_req =
                AiHttpRequest::post_json(request.provider.as_str(), url, payload)
                    .header("authorization", format!("Bearer {}", request.api_key));

            let response = self.transport.request(http_req).await?;

            crate::ai::parse_embedding_vectors(&response.body).map_err(|msg| {
                AiTransportError {
                    provider: request.provider,
                    status_code: None,
                    attempt_count: 1,
                    total_wait_ms: 0,
                    message: msg,
                }
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
}

impl AiBatchClient<AiTransportSender> {
    pub fn new(transport: AiTransport) -> Self {
        Self {
            sender: AiTransportSender { transport },
            max_batch_size_override: None,
            dedup_cache: None,
            chunk_mode: ChunkMode::Single,
            max_tokens: DEFAULT_MAX_TOKENS,
        }
    }

    pub fn from_runtime(runtime: &crate::runtime::RedDBRuntime) -> Self {
        use crate::runtime::ai::dedup_cache::{CONFIG_DEDUP_ENABLED, CONFIG_DEDUP_LRU_SIZE, CONFIG_DEDUP_TTL_MS};
        use crate::runtime::ai::text_chunker::{CONFIG_CHUNK_MODE, CONFIG_MAX_TOKENS};
        use std::time::Duration;

        let transport = AiTransport::from_runtime(runtime);
        let dedup_enabled = runtime.config_bool(CONFIG_DEDUP_ENABLED, false);
        let dedup_cache = if dedup_enabled {
            let lru_size = runtime.config_u64(CONFIG_DEDUP_LRU_SIZE, DEFAULT_DEDUP_LRU_SIZE as u64) as usize;
            let ttl_ms = runtime.config_u64(CONFIG_DEDUP_TTL_MS, DEFAULT_DEDUP_TTL_MS);
            Some(Arc::new(EmbeddingDedupCache::new(
                lru_size,
                Duration::from_millis(ttl_ms),
            )))
        } else {
            None
        };
        let chunk_mode = ChunkMode::from_str(
            &runtime.config_string(CONFIG_CHUNK_MODE, "single"),
        );
        let max_tokens =
            runtime.config_u64(CONFIG_MAX_TOKENS, DEFAULT_MAX_TOKENS as u64) as usize;

        Self {
            sender: AiTransportSender { transport },
            max_batch_size_override: None,
            dedup_cache,
            chunk_mode,
            max_tokens,
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

        // Step 1: apply chunking — each text → representative text to embed.
        // In Single mode this is the first (or only) chunk.
        let chunked_texts: Vec<String> = texts
            .iter()
            .map(|t| {
                let chunks = crate::runtime::ai::text_chunker::chunk(t, self.max_tokens);
                let chosen = crate::runtime::ai::text_chunker::apply_mode(chunks, self.chunk_mode);
                chosen.into_iter().next().unwrap_or_default()
            })
            .collect();

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

            let embeddings = self.sender.send(request).await?;

            if embeddings.len() != chunk.len() {
                return Err(AiTransportError {
                    provider: provider.token().to_string(),
                    status_code: None,
                    attempt_count: 0,
                    total_wait_ms: 0,
                    message: format!(
                        "provider returned {} embeddings for {} inputs",
                        embeddings.len(),
                        chunk.len()
                    ),
                });
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

        Ok(result.into_iter().map(|v| v.unwrap_or_default()).collect())
    }
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
        ) -> impl Future<Output = Result<Vec<Vec<f32>>, AiTransportError>> + Send + '_ {
            let n = request.inputs.len();
            let dims = self.dims;
            self.call_count.fetch_add(1, Ordering::SeqCst);
            async move { Ok((0..n).map(|_| vec![0.1f32; dims]).collect()) }
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
            ) -> impl Future<Output = Result<Vec<Vec<f32>>, AiTransportError>> + Send + '_ {
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
    async fn embed_order_preserved_across_batches() {
        struct BatchNumberSender {
            call_count: Arc<AtomicUsize>,
        }

        impl SubBatchSender for BatchNumberSender {
            fn send(
                &self,
                request: SubBatchRequest,
            ) -> impl Future<Output = Result<Vec<Vec<f32>>, AiTransportError>> + Send + '_ {
                let call = self.call_count.fetch_add(1, Ordering::SeqCst);
                let n = request.inputs.len();
                async move {
                    // encode batch number as first float for order verification
                    Ok((0..n).map(|_| vec![call as f32]).collect())
                }
            }
        }

        let counter = Arc::new(AtomicUsize::new(0));
        let client =
            AiBatchClient::with_sender(BatchNumberSender { call_count: Arc::clone(&counter) })
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
        let cache = Arc::new(EmbeddingDedupCache::new(
            1024,
            Duration::from_secs(60),
        ));
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
        assert_eq!(counter.load(Ordering::SeqCst), 1, "still 1 provider request total");
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
