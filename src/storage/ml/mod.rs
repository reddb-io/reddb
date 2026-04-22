//! Machine Learning subsystem — registry, versioning, async job queue.
//!
//! This is the shared foundation every ML feature (classifier, symbolic
//! regression, semantic cache, feature store, etc.) is built on top of.
//! The module intentionally has zero external ML-crate dependencies in
//! this sprint — only the registry + job queue + persistence primitives
//! needed for future algorithms to plug in.
//!
//! ## Overview
//!
//! * [`ModelRegistry`] catalogues named models and their immutable
//!   versions. Each `CREATE MODEL` / `ALTER MODEL` call creates a new
//!   version; versions are never mutated in place.
//! * [`MlJobQueue`] accepts training / backfill / inference-audit jobs
//!   and runs them on a worker pool. Jobs persist their state so a
//!   crash can resume from the last checkpoint.
//! * Metadata lives in `red_config` under the `red.ml.*` namespace so
//!   it survives restarts, backups, and replica sync without adding
//!   new system collections.
//!
//! The API surface is deliberately small. Feature-specific logic
//! (which algorithm, how to serialize weights, how to compute metrics)
//! lives in the caller — the registry only stores opaque bytes.

pub mod classifier;
pub mod jobs;
pub mod persist;
pub mod queue;
pub mod registry;
pub mod runtime;
pub mod semantic_cache;

pub use classifier::{
    evaluate as evaluate_classifier, ClassifierMetrics, IncrementalClassifier, LogisticRegression,
    LogisticRegressionConfig, MultinomialNaiveBayes, NaiveBayesConfig, TrainingExample, Vocabulary,
};
pub use jobs::{MlJob, MlJobId, MlJobKind, MlJobStatus};
pub use persist::{InMemoryMlPersistence, MlPersistence, MlPersistenceError, MlPersistenceResult};
pub use queue::{MlJobQueue, MlWorkFn};
pub use registry::{ModelRegistry, ModelRegistryError, ModelSummary, ModelVersion};
pub use runtime::{MlRuntime, MlRuntimeConfig};
pub use semantic_cache::{
    SemanticCache, SemanticCacheConfig, SemanticCacheEntry, SemanticCacheStats,
};
