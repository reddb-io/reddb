use super::*;
impl RuntimeQueryPort for RedDBRuntime {
    fn execute_query(&self, query: &str) -> RedDBResult<RuntimeQueryResult> {
        RedDBRuntime::execute_query(self, query)
    }

    fn explain_query(&self, query: &str) -> RedDBResult<RuntimeQueryExplain> {
        RedDBRuntime::explain_query(self, query)
    }

    fn scan_collection(
        &self,
        collection: &str,
        cursor: Option<ScanCursor>,
        limit: usize,
    ) -> RedDBResult<ScanPage> {
        RedDBRuntime::scan_collection(self, collection, cursor, limit)
    }

    fn search_similar(
        &self,
        collection: &str,
        vector: &[f32],
        k: usize,
        min_score: f32,
    ) -> RedDBResult<Vec<SimilarResult>> {
        super::ensure_collection_model_read(
            &self.db(),
            collection,
            crate::catalog::CollectionModel::Vector,
        )?;
        RedDBRuntime::search_similar(self, collection, vector, k, min_score)
    }

    fn search_ivf(
        &self,
        collection: &str,
        vector: &[f32],
        k: usize,
        n_lists: usize,
        n_probes: Option<usize>,
    ) -> RedDBResult<RuntimeIvfSearchResult> {
        super::ensure_collection_model_read(
            &self.db(),
            collection,
            crate::catalog::CollectionModel::Vector,
        )?;
        RedDBRuntime::search_ivf(self, collection, vector, k, n_lists, n_probes)
    }

    fn search_hybrid(
        &self,
        vector: Option<Vec<f32>>,
        query: Option<String>,
        k: Option<usize>,
        collections: Option<Vec<String>>,
        entity_types: Option<Vec<String>>,
        capabilities: Option<Vec<String>>,
        graph_pattern: Option<RuntimeGraphPattern>,
        filters: Vec<RuntimeFilter>,
        weights: Option<RuntimeQueryWeights>,
        min_score: Option<f32>,
        limit: Option<usize>,
    ) -> RedDBResult<DslQueryResult> {
        RedDBRuntime::search_hybrid(
            self,
            vector,
            query,
            k,
            collections,
            entity_types,
            capabilities,
            graph_pattern,
            filters,
            weights,
            min_score,
            limit,
        )
    }

    fn search_text(
        &self,
        query: String,
        collections: Option<Vec<String>>,
        entity_types: Option<Vec<String>>,
        capabilities: Option<Vec<String>>,
        fields: Option<Vec<String>>,
        limit: Option<usize>,
        fuzzy: bool,
    ) -> RedDBResult<DslQueryResult> {
        RedDBRuntime::search_text(
            self,
            query,
            collections,
            entity_types,
            capabilities,
            fields,
            limit,
            fuzzy,
        )
    }

    fn search_multimodal(
        &self,
        query: String,
        collections: Option<Vec<String>>,
        entity_types: Option<Vec<String>>,
        capabilities: Option<Vec<String>>,
        limit: Option<usize>,
    ) -> RedDBResult<DslQueryResult> {
        RedDBRuntime::search_multimodal(self, query, collections, entity_types, capabilities, limit)
    }

    fn search_index(
        &self,
        index: String,
        value: String,
        exact: bool,
        collections: Option<Vec<String>>,
        entity_types: Option<Vec<String>>,
        capabilities: Option<Vec<String>>,
        limit: Option<usize>,
    ) -> RedDBResult<DslQueryResult> {
        RedDBRuntime::search_index(
            self,
            index,
            value,
            exact,
            collections,
            entity_types,
            capabilities,
            limit,
        )
    }

    fn search_context(
        &self,
        input: crate::application::SearchContextInput,
    ) -> RedDBResult<crate::runtime::ContextSearchResult> {
        RedDBRuntime::search_context(self, input)
    }

    fn resolve_semantic_api_key(&self, provider: &crate::ai::AiProvider) -> RedDBResult<String> {
        crate::ai::resolve_api_key_from_runtime(provider, None, self)
    }
}
