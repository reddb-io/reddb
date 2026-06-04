//! Grouped integration-test harness for the related domain files.
//!
//! Cargo builds one linked binary per integration target. Keep the
//! original test files in `tests/` and include them here so test names
//! remain scoped by their source file while link count stays small.

#![allow(dead_code, unexpected_cfgs)]

#[path = "../compile_fail.rs"]
mod compile_fail;

#[path = "../e2e_ask_search_conformance.rs"]
mod e2e_ask_search_conformance;

#[path = "../e2e_comment_clustering.rs"]
mod e2e_comment_clustering;

#[path = "../e2e_explain.rs"]
mod e2e_explain;

#[path = "../e2e_issue_557_ask_context_retrieval.rs"]
mod e2e_issue_557_ask_context_retrieval;

#[path = "../e2e_issue_746_red_typed_vector_graph_relations.rs"]
mod e2e_issue_746_red_typed_vector_graph_relations;

#[path = "../e2e_issue_756_vector_policy_aware.rs"]
mod e2e_issue_756_vector_policy_aware;

#[path = "../e2e_issue_796_louvain_tvf.rs"]
mod e2e_issue_796_louvain_tvf;

#[path = "../e2e_ml_classify.rs"]
mod e2e_ml_classify;

#[path = "../integration_ai_live_comment_clustering.rs"]
mod integration_ai_live_comment_clustering;

#[path = "../integration_ai_local_models_cache.rs"]
mod integration_ai_local_models_cache;

#[path = "../integration_ai_local_models_registry.rs"]
mod integration_ai_local_models_registry;

#[path = "../integration_ai_multi_provider.rs"]
mod integration_ai_multi_provider;

#[path = "../integration_auto_embed_local.rs"]
mod integration_auto_embed_local;

#[path = "../integration_local_embedding_conformance.rs"]
mod integration_local_embedding_conformance;

#[path = "../integration_vector_query_text.rs"]
mod integration_vector_query_text;

#[path = "../integration_vector_query_text_local.rs"]
mod integration_vector_query_text_local;

#[path = "../mock_ai_provider.rs"]
mod mock_ai_provider;

#[path = "../smoke_embedded.rs"]
mod smoke_embedded;
