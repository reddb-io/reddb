#![allow(dead_code)]

#[path = "grouped/ai_local_vector/support.rs"]
mod support;

#[path = "grouped/ai_local_vector/integration_ai_live_comment_clustering.rs"]
mod integration_ai_live_comment_clustering;

#[path = "grouped/ai_local_vector/integration_ai_local_models_cache.rs"]
mod integration_ai_local_models_cache;

#[path = "grouped/ai_local_vector/integration_auto_embed_local.rs"]
mod integration_auto_embed_local;

#[path = "grouped/ai_local_vector/integration_external_env.rs"]
mod integration_external_env;

#[path = "grouped/ai_local_vector/integration_vector_query_text.rs"]
mod integration_vector_query_text;

#[path = "grouped/ai_local_vector/integration_vector_query_text_local.rs"]
mod integration_vector_query_text_local;

#[path = "grouped/ai_local_vector/snowplow_adapter_example.rs"]
mod snowplow_adapter_example;
