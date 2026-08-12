use crate::runtime::{
    ContextSearchResult, RedDBRuntime, RuntimeFilter, RuntimeGraphPattern, RuntimeIvfSearchResult,
    RuntimeQueryExplain, RuntimeQueryResult, RuntimeQueryWeights, ScanCursor, ScanPage,
};
use crate::storage::unified::devx::SimilarResult;
use crate::storage::unified::dsl::QueryResult as DslQueryResult;
use crate::RedDBResult;

#[derive(Debug, Clone)]
pub struct ExecuteQueryInput {
    pub query: String,
}

#[derive(Debug, Clone)]
pub struct ExplainQueryInput {
    pub query: String,
}

#[derive(Debug, Clone)]
pub struct ScanCollectionInput {
    pub collection: String,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct SearchSimilarInput {
    pub collection: String,
    pub vector: Vec<f32>,
    pub k: usize,
    pub min_score: f32,
    /// Optional text for semantic search (generates embedding on-the-fly)
    pub text: Option<String>,
    /// AI provider for semantic search (default: "openai")
    pub provider: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SearchIvfInput {
    pub collection: String,
    pub vector: Vec<f32>,
    pub k: usize,
    pub n_lists: usize,
    pub n_probes: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct SearchTextInput {
    pub query: String,
    pub collections: Option<Vec<String>>,
    pub entity_types: Option<Vec<String>>,
    pub capabilities: Option<Vec<String>>,
    pub fields: Option<Vec<String>>,
    pub limit: Option<usize>,
    pub fuzzy: bool,
}

#[derive(Debug, Clone)]
pub struct SearchMultimodalInput {
    pub query: String,
    pub collections: Option<Vec<String>>,
    pub entity_types: Option<Vec<String>>,
    pub capabilities: Option<Vec<String>>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct SearchIndexInput {
    pub index: String,
    pub value: String,
    pub exact: bool,
    pub collections: Option<Vec<String>>,
    pub entity_types: Option<Vec<String>>,
    pub capabilities: Option<Vec<String>>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct SearchHybridInput {
    pub vector: Option<Vec<f32>>,
    pub query: Option<String>,
    pub k: Option<usize>,
    pub collections: Option<Vec<String>>,
    pub entity_types: Option<Vec<String>>,
    pub capabilities: Option<Vec<String>>,
    pub graph_pattern: Option<RuntimeGraphPattern>,
    pub filters: Vec<RuntimeFilter>,
    pub weights: Option<RuntimeQueryWeights>,
    pub min_score: Option<f32>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct SearchContextInput {
    pub query: String,
    pub field: Option<String>,
    pub vector: Option<Vec<f32>>,
    pub collections: Option<Vec<String>>,
    pub graph_depth: Option<usize>,
    pub graph_max_edges: Option<usize>,
    pub max_cross_refs: Option<usize>,
    pub follow_cross_refs: Option<bool>,
    pub expand_graph: Option<bool>,
    pub global_scan: Option<bool>,
    pub reindex: Option<bool>,
    pub limit: Option<usize>,
    pub min_score: Option<f32>,
}

impl RedDBRuntime {
    pub fn execute(&self, input: ExecuteQueryInput) -> RedDBResult<RuntimeQueryResult> {
        self.execute_query(&input.query)
    }

    pub fn explain(&self, input: ExplainQueryInput) -> RedDBResult<RuntimeQueryExplain> {
        self.explain_query(&input.query)
    }

    pub fn scan(&self, input: ScanCollectionInput) -> RedDBResult<ScanPage> {
        self.scan_collection(
            &input.collection,
            Some(ScanCursor {
                offset: input.offset,
            }),
            input.limit,
        )
    }

    pub fn search_similar_input(
        &self,
        mut input: SearchSimilarInput,
    ) -> RedDBResult<Vec<SimilarResult>> {
        // Semantic search: if text provided, generate embedding on-the-fly
        if let Some(text) = input.text.take() {
            if input.vector.is_empty() {
                let provider = match input.provider.as_deref() {
                    Some(p) => crate::ai::parse_provider(p)?,
                    None => {
                        let name = std::env::var("REDDB_AI_PROVIDER")
                            .ok()
                            .unwrap_or_else(|| "openai".to_string());
                        crate::ai::parse_provider(&name)?
                    }
                };
                // S3 / #711: planner-level provider gate runs before the
                // compatibility check + key resolver so neither emits
                // side-effects for a policy-denied query.
                crate::runtime::ai::provider_gate::enforce(self, &provider)?;
                // Gate non-OpenAI-compatible providers before we spend
                // cycles resolving a key — Anthropic has no embeddings
                // endpoint, HuggingFace uses a different wire shape,
                // Local needs the `local-models` feature flag.
                if matches!(provider, crate::ai::AiProvider::Local) {
                    return Err(crate::ai::local_embeddings_unavailable_error());
                }
                if !provider.is_openai_compatible() {
                    return Err(crate::RedDBError::Query(format!(
                        "SEARCH SIMILAR: embeddings are not yet available for provider '{}'. \
                         Use an OpenAI-compatible provider (openai, groq, ollama, openrouter, \
                         together, venice, deepseek, or a custom base URL).",
                        provider.token()
                    )));
                }
                let api_key = crate::ai::resolve_api_key_from_runtime(&provider, None, self)?;
                let model = std::env::var(format!(
                    "REDDB_{}_EMBEDDING_MODEL",
                    provider.token().to_ascii_uppercase()
                ))
                .ok()
                .or_else(|| std::env::var("REDDB_OPENAI_EMBEDDING_MODEL").ok())
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| provider.default_embedding_model().to_string());
                let transport = crate::runtime::ai::transport::AiTransport::new(
                    crate::runtime::ai::transport::AiTransportConfig::default(),
                );
                let request = crate::ai::OpenAiEmbeddingRequest {
                    api_key,
                    model,
                    inputs: vec![text],
                    dimensions: None,
                    api_base: provider.resolve_api_base(),
                };
                let response = crate::runtime::ai::block_on_ai(async move {
                    crate::ai::openai_embeddings_async(&transport, request).await
                })
                .and_then(|result| result)?;
                input.vector = response.embeddings.into_iter().next().ok_or_else(|| {
                    crate::RedDBError::Query("embedding API returned no vectors".to_string())
                })?;
            }
        }
        self.search_similar(&input.collection, &input.vector, input.k, input.min_score)
    }

    pub fn search_ivf_input(&self, input: SearchIvfInput) -> RedDBResult<RuntimeIvfSearchResult> {
        RedDBRuntime::search_ivf(
            self,
            &input.collection,
            &input.vector,
            input.k,
            input.n_lists,
            input.n_probes,
        )
    }

    pub fn search_text_input(&self, input: SearchTextInput) -> RedDBResult<DslQueryResult> {
        RedDBRuntime::search_text(
            self,
            input.query,
            input.collections,
            input.entity_types,
            input.capabilities,
            input.fields,
            input.limit,
            input.fuzzy,
        )
    }

    pub fn search_multimodal_input(
        &self,
        input: SearchMultimodalInput,
    ) -> RedDBResult<DslQueryResult> {
        RedDBRuntime::search_multimodal(
            self,
            input.query,
            input.collections,
            input.entity_types,
            input.capabilities,
            input.limit,
        )
    }

    pub fn search_index_input(&self, input: SearchIndexInput) -> RedDBResult<DslQueryResult> {
        RedDBRuntime::search_index(
            self,
            input.index,
            input.value,
            input.exact,
            input.collections,
            input.entity_types,
            input.capabilities,
            input.limit,
        )
    }

    pub fn search_hybrid_input(&self, input: SearchHybridInput) -> RedDBResult<DslQueryResult> {
        RedDBRuntime::search_hybrid(
            self,
            input.vector,
            input.query,
            input.k,
            input.collections,
            input.entity_types,
            input.capabilities,
            input.graph_pattern,
            input.filters,
            input.weights,
            input.min_score,
            input.limit,
        )
    }

    pub fn search_context_input(
        &self,
        input: SearchContextInput,
    ) -> RedDBResult<ContextSearchResult> {
        RedDBRuntime::search_context(self, input)
    }
}
