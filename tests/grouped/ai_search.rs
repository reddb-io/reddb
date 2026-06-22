//! Grouped integration-test harness for the related domain files.
//!
//! Cargo builds one linked binary per integration target. Keep the
//! original test files in `tests/` and include them here so test names
//! remain scoped by their source file while link count stays small.

#![allow(dead_code, unexpected_cfgs)]

#[path = "surface_contracts/compile_fail.rs"]
mod compile_fail;

#[path = "ai_search/e2e_ask_search_conformance.rs"]
mod e2e_ask_search_conformance;

#[path = "ai_search/e2e_comment_clustering.rs"]
mod e2e_comment_clustering;

#[path = "sql_window/e2e_explain.rs"]
mod e2e_explain;

#[path = "ai_search/e2e_issue_557_ask_context_retrieval.rs"]
mod e2e_issue_557_ask_context_retrieval;

#[path = "graph_analytics/e2e_issue_746_red_typed_vector_graph_relations.rs"]
mod e2e_issue_746_red_typed_vector_graph_relations;

#[path = "tenancy_policy/e2e_issue_756_vector_policy_aware.rs"]
mod e2e_issue_756_vector_policy_aware;

#[path = "graph_analytics/e2e_issue_796_louvain_tvf.rs"]
mod e2e_issue_796_louvain_tvf;

#[path = "ai_search/e2e_ml_classify.rs"]
mod e2e_ml_classify;

#[path = "ai_local_vector/integration_ai_live_comment_clustering.rs"]
mod integration_ai_live_comment_clustering;

#[path = "ai_local_vector/integration_ai_local_models_cache.rs"]
mod integration_ai_local_models_cache;

#[path = "ai_provider_contracts/integration_ai_local_models_registry.rs"]
mod integration_ai_local_models_registry;

#[path = "ai_provider_contracts/integration_ai_multi_provider.rs"]
mod integration_ai_multi_provider;

#[path = "ai_local_vector/integration_auto_embed_local.rs"]
mod integration_auto_embed_local;

#[path = "runtime_persistence/integration_local_embedding_conformance.rs"]
mod integration_local_embedding_conformance;

#[path = "ai_local_vector/integration_vector_query_text.rs"]
mod integration_vector_query_text;

#[path = "ai_local_vector/integration_vector_query_text_local.rs"]
mod integration_vector_query_text_local;

#[path = "ai_search/mock_ai_provider.rs"]
mod mock_ai_provider;

#[path = "surface_contracts/smoke_embedded.rs"]
mod smoke_embedded;
