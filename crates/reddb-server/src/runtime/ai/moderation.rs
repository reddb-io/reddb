//! Local content-moderation backend (#1274, PRD #1267, ADR 0057).
//!
//! Moderation is the third AI modality on the write/CDC lane (after
//! embeddings #1272 and vision #1275). A collection that declares a
//! `MODERATE (...)` policy names text/image-reference fields that are
//! screened by a moderation provider. Unlike embed/vision, moderation has
//! a **synchronous pre-commit gate**: when `sync = true`, the write path
//! screens the declared fields *before the durable commit* and refuses the
//! write on a reject (ADR 0057). Provider-down behaviour and re-moderation
//! of quarantined rows ride the existing CDC enrichment lane.
//!
//! Mirroring [`super::vision`] and [`super::local_embedding`], the engine
//! is a swappable, process-global [`LocalModerationBackend`]. There is no
//! built-in default — a real engine or a mock is installed via
//! [`install_local_moderation_backend`]. A backend may also report itself
//! *down* (returning [`ModerationOutcome::ProviderDown`]) so tests can
//! exercise the fail-open-quarantine and fail-closed paths deterministically.

use std::sync::{Arc, OnceLock, RwLock};

use crate::{RedDBError, RedDBResult};

pub use reddb_engine::storage::visibility::{
    entity_moderation_hidden, MODERATION_STATUS_FIELD, MODERATION_STATUS_PENDING,
    MODERATION_STATUS_REJECTED,
};

/// Verdict for one moderation pass over a row's declared fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModerationOutcome {
    /// Content passed — the write/row is allowed to be visible.
    Allow,
    /// Content failed moderation — the write is rejected. `categories`
    /// carries the flagged reasons for audit/appeal.
    Reject { categories: Vec<String> },
    /// The provider could not be reached. The caller applies the policy's
    /// degraded-mode behaviour (fail-open + quarantine, or fail-closed).
    ProviderDown { reason: String },
}

impl ModerationOutcome {
    /// True when the content was rejected by moderation.
    pub fn is_reject(&self) -> bool {
        matches!(self, Self::Reject { .. })
    }

    /// True when the provider was unreachable.
    pub fn is_provider_down(&self) -> bool {
        matches!(self, Self::ProviderDown { .. })
    }
}

/// A materialised moderation request handed to a backend. The declared
/// text fields are concatenated by the caller into `text`; an empty
/// `text` means there was nothing to screen.
#[derive(Debug, Clone)]
pub struct ModerationRequest {
    /// Model name as written in the collection's MODERATE policy.
    pub model: String,
    /// The concatenated text of the row's declared moderated fields.
    pub text: String,
}

/// Backend abstraction so the gate/enrichment lanes do not depend on a
/// specific moderation engine. Tests install a mock; production installs a
/// real engine via [`install_local_moderation_backend`].
pub trait LocalModerationBackend: Send + Sync {
    fn moderate(&self, request: &ModerationRequest) -> RedDBResult<ModerationOutcome>;
}

const LOCAL_MODERATION_DISABLED_MESSAGE: &str =
    "local moderation requires a backend installed via \
     runtime::ai::moderation::install_local_moderation_backend. Alternatively, \
     declare a moderation-capable remote provider in the collection's MODERATE \
     policy.";

type BackendSlot = Arc<dyn LocalModerationBackend>;

fn backend_slot() -> &'static RwLock<Option<BackendSlot>> {
    static SLOT: OnceLock<RwLock<Option<BackendSlot>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// Install (or replace) the process-global local moderation backend.
///
/// Production servers call this once at boot with their real engine. Tests
/// use it to swap in a mock moderation provider. Safe to call from any
/// thread; the most recent install wins.
pub fn install_local_moderation_backend(backend: Arc<dyn LocalModerationBackend>) {
    let mut guard = backend_slot()
        .write()
        .expect("moderation backend slot poisoned");
    *guard = Some(backend);
}

/// Test-only: clear the installed backend so a subsequent call exercises
/// the feature-disabled path again.
#[doc(hidden)]
pub fn clear_local_moderation_backend_for_tests() {
    let mut guard = backend_slot()
        .write()
        .expect("moderation backend slot poisoned");
    *guard = None;
}

fn current_backend() -> Option<BackendSlot> {
    backend_slot()
        .read()
        .expect("moderation backend slot poisoned")
        .as_ref()
        .map(Arc::clone)
}

/// Resolve and run a local moderation request end-to-end. Errors with a
/// clear message when no backend is installed; a *down provider* is NOT an
/// error — the backend signals it via [`ModerationOutcome::ProviderDown`]
/// so the caller can apply the policy's degraded-mode behaviour.
pub fn moderate_local(model: &str, text: String) -> RedDBResult<ModerationOutcome> {
    let backend = current_backend().ok_or_else(|| {
        RedDBError::FeatureNotEnabled(LOCAL_MODERATION_DISABLED_MESSAGE.to_string())
    })?;
    backend.moderate(&ModerationRequest {
        model: model.to_string(),
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AllowAll;
    impl LocalModerationBackend for AllowAll {
        fn moderate(&self, _request: &ModerationRequest) -> RedDBResult<ModerationOutcome> {
            Ok(ModerationOutcome::Allow)
        }
    }

    #[test]
    fn missing_backend_is_feature_disabled_error() {
        clear_local_moderation_backend_for_tests();
        let err = moderate_local("m", "hello".to_string()).expect_err("no backend");
        assert!(matches!(err, RedDBError::FeatureNotEnabled(_)));
    }

    #[test]
    fn installed_backend_drives_outcome() {
        install_local_moderation_backend(Arc::new(AllowAll));
        let outcome = moderate_local("m", "hello".to_string()).expect("allowed");
        assert_eq!(outcome, ModerationOutcome::Allow);
        clear_local_moderation_backend_for_tests();
    }

    #[test]
    fn outcome_predicates() {
        assert!(ModerationOutcome::Reject {
            categories: vec!["hate".to_string()],
        }
        .is_reject());
        assert!(ModerationOutcome::ProviderDown {
            reason: "timeout".to_string(),
        }
        .is_provider_down());
        assert!(!ModerationOutcome::Allow.is_reject());
    }
}
