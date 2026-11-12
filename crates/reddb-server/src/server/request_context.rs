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
use crate::application::{OperationContext, OperationContextFactory, OperationContextInput};
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
        OperationContextFactory::build(OperationContextInput {
            request_id: request_id.map(str::to_string),
            principal: principal.map(str::to_string),
            ..OperationContextInput::default()
        })
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
        self.runtime.validated_request_commit_policy(None)?;
        Ok(OperationContextFactory::build(OperationContextInput {
            request_id: request_id.map(str::to_string),
            principal: principal.map(str::to_string),
            write_consent: Some(consent),
            ..OperationContextInput::default()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RedDBError, RedDBRuntime};

    #[test]
    fn commit_policy_context_child() {
        if std::env::var_os("REDDB_COMMIT_CONTEXT_CHILD").is_none() {
            return;
        }
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        let server = RedDBServer::new(runtime);
        let error = server
            .build_write_context(WriteKind::Dml, None, None)
            .err()
            .expect("unsupported policy must not mint write consent");
        assert!(matches!(error, RedDBError::InvalidConfig(_)), "{error}");
    }

    #[test]
    fn commit_policy_write_context_rejects_unsupported_configuration() {
        // Isolate the process-wide configuration from concurrent tests.
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "server::request_context::tests::commit_policy_context_child",
                "--nocapture",
            ])
            .env("REDDB_COMMIT_CONTEXT_CHILD", "1")
            .env("RED_PRIMARY_COMMIT_POLICY", "remote_wal")
            .output()
            .expect("run configured child");
        assert!(output.status.success(), "{output:?}");
    }
}
