//! Shared ownership for completed materialized request results. Executor
//! workspaces and serialization allocations require separate admission.
use super::{memory_admission::QueryBufferReservation, RedDBRuntime, RuntimeQueryResult};
use crate::{RedDBError, RedDBResult};
use std::{borrow::Cow, fmt, ops::Deref, sync::Arc};

pub(crate) const SHARED_RESULT_EXECUTOR: &str = "shared_query_result";

/// Immutable result returned by the request seam. Clones share all records,
/// payloads and metadata; no infallible Clone can bypass shared admission.
#[derive(Clone)]
pub struct OwnedQueryResult {
    inner: Arc<ResultOwner>,
}

struct ResultOwner {
    // Declaration order frees all data before returning the reservation.
    value: RuntimeQueryResult,
    _reservation: Option<QueryBufferReservation>,
}

impl Deref for OwnedQueryResult {
    type Target = RuntimeQueryResult;
    fn deref(&self) -> &Self::Target {
        &self.inner.value
    }
}

impl AsRef<RuntimeQueryResult> for OwnedQueryResult {
    fn as_ref(&self) -> &RuntimeQueryResult {
        self
    }
}

impl fmt::Debug for OwnedQueryResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.value.fmt(formatter)
    }
}

impl OwnedQueryResult {
    /// Admitted results stay borrowed so their allocations cannot escape their
    /// charge by moving into an unaccounted output. A unique, unleased command
    /// acknowledgement keeps its existing move fast path. New rendered output
    /// still requires separate admission.
    pub(crate) fn render<T>(self, render: impl FnOnce(Cow<'_, RuntimeQueryResult>) -> T) -> T {
        match Arc::try_unwrap(self.inner) {
            Ok(owner) if owner._reservation.is_none() => render(Cow::Owned(owner.value)),
            // Keep the owner intact so fields also drop in the required order
            // if rendering unwinds.
            Ok(owner) => render(Cow::Borrowed(&owner.value)),
            Err(owner) => render(Cow::Borrowed(&owner.value)),
        }
    }
}

impl RedDBRuntime {
    pub(crate) fn own_query_result(
        &self,
        value: RuntimeQueryResult,
    ) -> RedDBResult<OwnedQueryResult> {
        // These are the same pure materialized plan families as query_budget.
        // Command results (e.g. queue consumption / ASK / sequence functions)
        // must not receive a late refusal after an acknowledged side effect.
        let is_read = value.statement_type == "select"
            && matches!(
                value.statement,
                "table" | "graph" | "join" | "path" | "vector" | "hybrid" | "select"
            );
        let reservation = if is_read {
            let owner_bytes = std::alloc::Layout::new::<[usize; 2]>()
                .extend(std::alloc::Layout::new::<ResultOwner>())
                .expect("fixed result owner layout is representable")
                .0
                .pad_to_align()
                .size();
            let bytes = value
                .result
                .retained_memory_bytes()
                .saturating_add(value.query.capacity())
                .saturating_add(value.bookmark.as_ref().map_or(0, String::capacity))
                .saturating_add(value.notice.as_ref().map_or(0, String::capacity))
                .saturating_add(owner_bytes);
            Some(self.try_reserve_query_buffer(bytes).ok_or_else(|| {
                self.memory_accounting().record_admission_denied();
                RedDBError::MaterializationLimitExceeded {
                    executor: SHARED_RESULT_EXECUTOR,
                    limit: usize::try_from(self.memory_budget().resolved_bytes)
                        .unwrap_or(usize::MAX),
                    current: usize::try_from(self.memory_accounting().total_used_bytes())
                        .unwrap_or(usize::MAX)
                        .saturating_add(bytes),
                }
            })?)
        } else {
            None
        };
        Ok(OwnedQueryResult {
            inner: Arc::new(ResultOwner {
                value,
                _reservation: reservation,
            }),
        })
    }
}
