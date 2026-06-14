#![allow(dead_code)]

#[path = "grouped/ai_search/support.rs"]
mod support;

#[path = "grouped/ai_search/e2e_ask_search_conformance.rs"]
mod e2e_ask_search_conformance;

#[path = "grouped/ai_search/e2e_ask_tenant_scoped.rs"]
mod e2e_ask_tenant_scoped;

#[path = "grouped/ai_search/e2e_comment_clustering.rs"]
mod e2e_comment_clustering;

#[path = "grouped/ai_search/e2e_issue_557_ask_context_retrieval.rs"]
mod e2e_issue_557_ask_context_retrieval;

#[path = "grouped/ai_search/e2e_ml_classify.rs"]
mod e2e_ml_classify;

#[path = "grouped/ai_search/mock_ai_provider.rs"]
mod mock_ai_provider;
