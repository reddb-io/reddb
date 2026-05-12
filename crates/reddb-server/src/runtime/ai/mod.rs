//! AI runtime modules.
//!
//! Houses the LLM-touching pieces of the AskPipeline:
//!
//! - [`prompt_template`] — typed-slot prompt assembly with secret
//!   redaction and injection defence (issue #122).
//! - [`ner`] — opt-in LLM backend for AskPipeline Stage 1 entity
//!   extraction with auth gate, response sanitization, and a
//!   configurable heuristic fallback (issue #123).
//!
//! Both modules are pure additions — call-site wiring lives in
//! [`super::ask_pipeline`] and is opt-in via `ai.ner.backend = "llm"`
//! at runtime config time.

pub mod answer_cache_key;
pub mod ask_response_envelope;
pub mod audit_record_builder;
pub mod batch_client;
pub mod citation_parser;
pub mod cost_guard;
pub mod dedup_cache;
pub mod determinism_decider;
pub mod explain_plan_builder;
pub mod mcp_ask_tool;
pub mod metrics;
pub mod ner;
pub mod prompt_assembler;
pub mod prompt_template;
pub mod provider_capabilities;
pub mod provider_failover;
pub mod rrf_fuser;
pub mod sources_fingerprint;
pub mod sse_frame_encoder;
pub mod strict_validator;
pub mod text_chunker;
pub mod transport;
pub mod urn_codec;

pub(crate) fn block_on_ai<F, T>(future: F) -> crate::RedDBResult<T>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if matches!(
            handle.runtime_flavor(),
            tokio::runtime::RuntimeFlavor::MultiThread
        ) {
            return Ok(tokio::task::block_in_place(|| handle.block_on(future)));
        }

        return std::thread::Builder::new()
            .name("reddb-ai-blocking".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|err| {
                        crate::RedDBError::Query(format!("failed to start AI runtime: {err}"))
                    })?;
                Ok(runtime.block_on(future))
            })
            .map_err(|err| {
                crate::RedDBError::Query(format!("failed to spawn AI runtime thread: {err}"))
            })?
            .join()
            .map_err(|_| crate::RedDBError::Query("AI runtime thread panicked".to_string()))?;
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| crate::RedDBError::Query(format!("failed to start AI runtime: {err}")))?;
    Ok(runtime.block_on(future))
}
