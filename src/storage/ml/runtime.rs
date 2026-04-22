//! `MlRuntime` — convenience bundle of [`ModelRegistry`] + [`MlJobQueue`]
//! so feature code (classifier, symbolic, semantic cache, …) only
//! needs to hold a single handle.
//!
//! The runtime is detached from [`crate::runtime::RedDBRuntime`]: it
//! can be constructed standalone (in-memory) for tests or bound to a
//! shared [`MlPersistence`] backend for durable deployments. A
//! future sprint will add a `RedDBRuntime::ml()` accessor that
//! returns the bound instance — this module provides the pieces it
//! will wire up.

use std::sync::Arc;

use super::persist::{InMemoryMlPersistence, MlPersistence};
use super::queue::{MlJobQueue, MlWorkFn};
use super::registry::ModelRegistry;

/// Shared entrypoint used by every ML feature.
///
/// Cloning the runtime is cheap — registry and queue both wrap
/// `Arc`s internally.
#[derive(Clone)]
pub struct MlRuntime {
    registry: ModelRegistry,
    queue: MlJobQueue,
    backend: Arc<dyn MlPersistence>,
}

impl std::fmt::Debug for MlRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MlRuntime")
            .field("registry", &self.registry)
            .field("queue", &self.queue)
            .finish()
    }
}

/// Compile-time defaults for a standalone runtime. Production
/// callers pass an [`MlRuntimeConfig`] with their own worker count.
#[derive(Debug, Clone)]
pub struct MlRuntimeConfig {
    pub worker_count: usize,
}

impl Default for MlRuntimeConfig {
    fn default() -> Self {
        Self {
            worker_count: default_worker_count(),
        }
    }
}

fn default_worker_count() -> usize {
    let logical = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2);
    // Leave one core for OLTP; guarantee at least one worker.
    logical.saturating_sub(1).max(1)
}

impl MlRuntime {
    /// Build a fully in-memory runtime. Jobs and versions disappear
    /// on drop — good for unit tests, bad for production.
    pub fn in_memory(worker_fn: MlWorkFn) -> Self {
        Self::with_backend(
            Arc::new(InMemoryMlPersistence::new()),
            worker_fn,
            MlRuntimeConfig::default(),
        )
    }

    /// Build a runtime that persists registry + job state into
    /// `backend`. On construction the registry and queue rehydrate
    /// automatically so prior state is observable immediately.
    pub fn with_backend(
        backend: Arc<dyn MlPersistence>,
        worker_fn: MlWorkFn,
        config: MlRuntimeConfig,
    ) -> Self {
        let registry = ModelRegistry::with_backend(Arc::clone(&backend));
        let queue =
            MlJobQueue::start_with_backend(config.worker_count, worker_fn, Arc::clone(&backend));
        Self {
            registry,
            queue,
            backend,
        }
    }

    pub fn registry(&self) -> &ModelRegistry {
        &self.registry
    }

    pub fn queue(&self) -> &MlJobQueue {
        &self.queue
    }

    /// Access the raw persistence backend — used by features that
    /// need their own namespace (e.g. semantic cache stats).
    pub fn backend(&self) -> &Arc<dyn MlPersistence> {
        &self.backend
    }

    /// Stop worker threads. Idempotent — safe to call more than
    /// once; subsequent calls are no-ops.
    pub fn shutdown(&self) {
        self.queue.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::super::jobs::{MlJobKind, MlJobStatus};
    use super::*;
    use std::time::{Duration, Instant};

    fn wait_until<F: Fn() -> bool>(predicate: F, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if predicate() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        predicate()
    }

    #[test]
    fn in_memory_runtime_runs_a_training_job() {
        let rt = MlRuntime::in_memory(Arc::new(|_| Ok("{\"ok\":true}".to_string())));
        let id = rt.queue().submit(MlJobKind::Train, "spam", "{}");
        assert!(wait_until(
            || rt
                .queue()
                .get(id)
                .map(|j| j.status == MlJobStatus::Completed)
                .unwrap_or(false),
            Duration::from_secs(2),
        ));
        rt.shutdown();
    }

    #[test]
    fn runtime_exposes_registry() {
        let rt = MlRuntime::in_memory(Arc::new(|_| Ok("{}".to_string())));
        assert_eq!(rt.registry().summaries().unwrap().len(), 0);
        rt.shutdown();
    }
}
