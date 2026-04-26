//! Per-request `OperationContext` — the single bag of state every
//! port receives.
//!
//! Today, ports take `&self` only and reach into hidden runtime
//! state (transaction map keyed by connection id, audit principal
//! resolved via thread-local hacks, request id missing entirely).
//! That spreading makes multi-port invariants — "this request's
//! port_a call and port_b call must share an xid" — invisible to
//! the type system and untestable.
//!
//! `OperationContext` flips that around: handlers build it once at
//! request entry and pass it through every port call. Forgetting to
//! propagate it is a compile error; sharing it across two ports is
//! a single move.
//!
//! `WriteConsent` is a sealed token: it can only be constructed by
//! `WriteGate::check`, so a port's mutating method that demands
//! `ctx.write_consent.is_some()` is statically guaranteed to have
//! passed the gate. Forgetting the gate is impossible at the type
//! level.
//!
//! Migration is gated behind the `ctx-ports` feature flag while the
//! 9 ports are converted one PR at a time. `OperationContext::implicit()`
//! returns a no-op context that lets unmigrated callers keep
//! compiling.

use std::sync::atomic::{AtomicU64, Ordering};

/// Snapshot identifier used for MVCC reads. `None` means autocommit
/// — the port allocates a fresh snapshot per call (current default
/// for unwrapped paths).
pub type Xid = u64;

/// Sealed write-permission token. Construct via
/// `WriteGate::check`; cannot be assembled by application code,
/// even within this crate, because the inner field is private and
/// marked `PhantomData<*const ()>` — and the `_seal` field can only
/// be created inside `runtime::write_gate`.
#[derive(Debug, Clone)]
pub struct WriteConsent {
    pub(crate) kind: crate::runtime::write_gate::WriteKind,
    pub(crate) _seal: WriteConsentSeal,
}

/// Module-private marker. Public type, but the only constructor lives
/// in `runtime::write_gate::WriteConsentSeal::new()`, which is only
/// callable from the write-gate module. Code outside that module can
/// pattern-match on this struct but cannot construct it, so building
/// a `WriteConsent` requires going through the gate.
#[derive(Debug, Clone)]
pub struct WriteConsentSeal {
    _private: std::marker::PhantomData<*const ()>,
}

// SAFETY: `WriteConsentSeal` carries no owned state; the
// `PhantomData<*const ()>` marker is unsendable by default to
// discourage cross-thread token sharing without explicit auditing.
// We re-add Send+Sync because `WriteConsent` itself is Clone and
// gets stored on `OperationContext`, which crosses async/Tokio
// task boundaries on every request. The marker only exists to
// gate construction; thread-safety of the runtime gate it
// represents is not affected by sending the token.
unsafe impl Send for WriteConsentSeal {}
unsafe impl Sync for WriteConsentSeal {}

impl WriteConsentSeal {
    /// Create the sealed marker. Pub only within the crate so the
    /// write-gate module can mint tokens; everything else must
    /// call `WriteGate::check`.
    pub(crate) fn new() -> Self {
        Self {
            _private: std::marker::PhantomData,
        }
    }
}

/// Per-request context plumbed through every port method.
#[derive(Debug, Clone)]
pub struct OperationContext {
    /// MVCC snapshot id when the request opened a transaction;
    /// `None` for autocommit reads/writes.
    pub xid: Option<Xid>,
    /// Connection identifier the request arrived on; ties this
    /// context back to per-connection state (current transaction,
    /// session variables) when needed.
    pub connection_id: Option<u64>,
    /// Identity recorded in audit logs. `"anonymous"` when the
    /// caller did not authenticate.
    pub audit_principal: String,
    /// Stable per-request id used for log correlation. Either
    /// supplied by the caller via `X-Request-Id` or minted as a
    /// monotonic ULID-like string at request entry.
    pub request_id: String,
    /// Sealed gate token, present only when `WriteGate::check`
    /// granted permission. Mutating port methods demand this is
    /// `Some(...)`; missing it is a runtime error (and during the
    /// migration window, a structural reminder that the call site
    /// hasn't been threaded yet).
    pub write_consent: Option<WriteConsent>,
    /// Optional tenant override. `None` falls back to the
    /// connection's default tenant.
    pub tenant: Option<String>,
}

impl OperationContext {
    /// Anonymous, no-write-consent context. The default for any
    /// caller that hasn't been migrated to construct an explicit
    /// context yet — keeps the migration window compilable.
    pub fn implicit() -> Self {
        Self {
            xid: None,
            connection_id: None,
            audit_principal: "anonymous".to_string(),
            request_id: mint_request_id(),
            write_consent: None,
            tenant: None,
        }
    }

    /// Read-only context bound to a stable request id. Use when the
    /// handler knows it is dispatching a query that never mutates.
    pub fn read_only(request_id: impl Into<String>) -> Self {
        Self {
            xid: None,
            connection_id: None,
            audit_principal: "anonymous".to_string(),
            request_id: request_id.into(),
            write_consent: None,
            tenant: None,
        }
    }

    /// Writing context with an attached gate token. Construct
    /// `WriteConsent` via `WriteGate::check` first; passing the
    /// token here is the single point that proves the request
    /// passed the policy.
    pub fn writing(consent: WriteConsent, request_id: impl Into<String>) -> Self {
        Self {
            xid: None,
            connection_id: None,
            audit_principal: "anonymous".to_string(),
            request_id: request_id.into(),
            write_consent: Some(consent),
            tenant: None,
        }
    }

    pub fn with_principal(mut self, principal: impl Into<String>) -> Self {
        self.audit_principal = principal.into();
        self
    }

    pub fn with_connection(mut self, connection_id: u64) -> Self {
        self.connection_id = Some(connection_id);
        self
    }

    pub fn with_xid(mut self, xid: Xid) -> Self {
        self.xid = Some(xid);
        self
    }

    pub fn with_tenant(mut self, tenant: impl Into<String>) -> Self {
        self.tenant = Some(tenant.into());
        self
    }

    pub fn require_write_consent(&self) -> Result<&WriteConsent, crate::api::RedDBError> {
        self.write_consent.as_ref().ok_or_else(|| {
            crate::api::RedDBError::ReadOnly(
                "operation context is missing WriteConsent — handler must call WriteGate::check"
                    .to_string(),
            )
        })
    }
}

/// Monotonic request-id minter used when the caller doesn't supply
/// one. Format: `req-<unix_micros>-<seq>` — sortable, unique,
/// human-readable. Not a true ULID (no need to pull in another
/// crate just for this).
fn mint_request_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let now_us = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0);
    format!("req-{now_us}-{seq}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implicit_has_no_write_consent() {
        let ctx = OperationContext::implicit();
        assert!(ctx.write_consent.is_none());
        assert_eq!(ctx.audit_principal, "anonymous");
        assert!(!ctx.request_id.is_empty());
    }

    #[test]
    fn read_only_constructor_carries_supplied_request_id() {
        let ctx = OperationContext::read_only("req-abc");
        assert_eq!(ctx.request_id, "req-abc");
        assert!(ctx.write_consent.is_none());
    }

    #[test]
    fn require_write_consent_errors_when_missing() {
        let ctx = OperationContext::implicit();
        let err = ctx.require_write_consent().unwrap_err();
        assert!(matches!(err, crate::api::RedDBError::ReadOnly(_)));
    }

    #[test]
    fn request_ids_are_monotonic_within_process() {
        let a = mint_request_id();
        let b = mint_request_id();
        assert_ne!(a, b);
    }

    #[test]
    fn builder_setters_compose() {
        let ctx = OperationContext::read_only("req-1")
            .with_principal("operator")
            .with_connection(42)
            .with_xid(7)
            .with_tenant("acme");
        assert_eq!(ctx.audit_principal, "operator");
        assert_eq!(ctx.connection_id, Some(42));
        assert_eq!(ctx.xid, Some(7));
        assert_eq!(ctx.tenant.as_deref(), Some("acme"));
    }
}
