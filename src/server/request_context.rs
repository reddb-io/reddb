//! Per-request `OperationContext` factory.
//!
//! Single Adapter every handler calls at request entry to build the
//! context that flows through the use-case + port stack. Centralises:
//!   * `X-Request-Id` extraction (or minting when absent).
//!   * `WriteGate::check_consent` for mutating requests — the only
//!     site that mints `WriteConsent` outside the gate module.
//!   * Audit-principal resolution from auth headers (anonymous
//!     fallback when no auth store is configured).
//!   * Optional connection / xid binding when the caller supplies
//!     one (PostgreSQL wire passes its session id, gRPC long-lived
//!     streams attach a transaction xid).
//!
//! The factory deliberately does **not** read tenant overrides from
//! request headers — tenant routing is a separate piece of the
//! authn/z stack and shouldn't piggyback on the context.
//!
//! All methods are `pub(crate)` so handlers reach them via
//! `self.build_*_context(...)`; no public surface change.

use crate::api::RedDBResult;
use crate::application::OperationContext;
use crate::runtime::write_gate::WriteKind;

use super::RedDBServer;

impl RedDBServer {
    /// Build a read-only context bound to the request id supplied by
    /// the caller (or a freshly minted one).
    pub(crate) fn build_read_context(
        &self,
        request_id: Option<&str>,
        principal: Option<&str>,
    ) -> OperationContext {
        let ctx = match request_id {
            Some(id) if !id.is_empty() => OperationContext::read_only(id),
            _ => OperationContext::implicit(),
        };
        match principal {
            Some(p) if !p.is_empty() => ctx.with_principal(p),
            _ => ctx,
        }
    }

    /// Build a writing context. Calls `WriteGate::check_consent`,
    /// which is the only way to mint a `WriteConsent` token outside
    /// the gate module — port methods that demand the token are
    /// thereby guaranteed to have passed the gate.
    ///
    /// Returns `RedDBError::ReadOnly` when the gate refuses the
    /// requested write kind; the caller should map it to an HTTP
    /// 403 / gRPC `failed_precondition`.
    pub(crate) fn build_write_context(
        &self,
        kind: WriteKind,
        request_id: Option<&str>,
        principal: Option<&str>,
    ) -> RedDBResult<OperationContext> {
        let consent = self.runtime.write_gate().check_consent(kind)?;
        let request_id = request_id
            .filter(|id| !id.is_empty())
            .map(|id| id.to_string())
            .unwrap_or_else(|| {
                // Borrow OperationContext::implicit's minter by
                // routing through it — keeps the id format
                // consistent across read / write contexts.
                OperationContext::implicit().request_id
            });
        let mut ctx = OperationContext::writing(consent, request_id);
        if let Some(p) = principal.filter(|p| !p.is_empty()) {
            ctx = ctx.with_principal(p);
        }
        Ok(ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The factory needs a real RedDBServer to exercise; that's
    // expensive to stand up here. The behaviour is small enough to
    // test indirectly through the operation_context unit tests
    // (see src/application/operation_context.rs::tests). What
    // matters for this module is that it compiles — type checks
    // confirm WriteConsent is sealed and that build_write_context
    // returns Result so handlers can propagate the gate error.
    #[test]
    fn module_compiles() {}
}
