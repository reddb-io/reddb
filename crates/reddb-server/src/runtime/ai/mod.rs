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

pub mod batch_client;
pub mod dedup_cache;
pub mod metrics;
pub mod ner;
pub mod prompt_template;
pub mod text_chunker;
pub mod transport;
