//! Per-request execution identity for the HTTP edge.
//!
//! The runtime's privilege gate (`runtime::authz::privilege`), the
//! column-policy / RLS projection gate and the tenant resolver all read a
//! thread-local identity that a transport is expected to install before
//! dispatching a statement. RedWire and PG-wire do this through their own
//! RAII guards; the HTTP edge historically installed nothing, so every
//! statement executed over `POST /query` (and the typed HTTP handlers)
//! ran with **no identity**, which the gate treats as "embedded caller,
//! enforcement off". IAM policies, legacy GRANTs, column policies, RLS and
//! tenant scoping were therefore bypassed over HTTP.
//!
//! [`RequestIdentityGuard`] mirrors `RedWireExecutionContextGuard`: it
//! installs the resolved bearer principal (username, role, tenant) for the
//! duration of one buffered or streaming request and restores the previous
//! thread state on drop. Because `dispatch_route` runs inside a single
//! `spawn_blocking` closure, the thread-local holds for the whole handler.
//!
//! Anonymous callers (no bearer, permitted only when the auth store is
//! disabled or `require_auth` is off) clear the identity **and the tenant**,
//! so a `SET TENANT` issued by a previous request on the same pooled thread
//! can no longer leak into an unrelated request.

use crate::auth::{Role, UserId};
use crate::runtime::execution_context::{
    clear_current_auth_identity, clear_current_tenant, current_auth_identity, current_tenant,
    set_current_auth_identity, set_current_tenant,
};

/// RAII guard that scopes the thread-local execution identity to one HTTP
/// request. Construct with [`RequestIdentityGuard::install`]; the previous
/// identity and tenant are restored when the guard drops.
pub(crate) struct RequestIdentityGuard {
    previous_identity: Option<(String, Role)>,
    previous_tenant: Option<String>,
}

impl RequestIdentityGuard {
    /// Install `caller` as the current-thread identity. `None` clears both
    /// identity and tenant (anonymous request).
    pub(crate) fn install(caller: Option<(UserId, Role)>) -> Self {
        let previous_identity = current_auth_identity();
        let previous_tenant = current_tenant();
        match caller {
            Some((id, role)) => {
                match id.tenant {
                    Some(tenant) => set_current_tenant(tenant),
                    None => clear_current_tenant(),
                }
                set_current_auth_identity(id.username, role);
            }
            None => {
                clear_current_auth_identity();
                clear_current_tenant();
            }
        }
        Self {
            previous_identity,
            previous_tenant,
        }
    }
}

impl Drop for RequestIdentityGuard {
    fn drop(&mut self) {
        match self.previous_identity.take() {
            Some((username, role)) => set_current_auth_identity(username, role),
            None => clear_current_auth_identity(),
        }
        match self.previous_tenant.take() {
            Some(tenant) => set_current_tenant(tenant),
            None => clear_current_tenant(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_installs_identity_and_tenant_then_restores() {
        clear_current_auth_identity();
        clear_current_tenant();
        {
            let _guard = RequestIdentityGuard::install(Some((
                UserId::from_parts(Some("acme"), "alice"),
                Role::Write,
            )));
            assert_eq!(
                current_auth_identity(),
                Some(("alice".to_string(), Role::Write))
            );
            assert_eq!(current_tenant().as_deref(), Some("acme"));
        }
        assert_eq!(current_auth_identity(), None);
        assert_eq!(current_tenant(), None);
    }

    #[test]
    fn anonymous_request_clears_leaked_tenant_and_identity() {
        set_current_auth_identity("leaked".to_string(), Role::Admin);
        set_current_tenant("leaked-tenant".to_string());
        {
            let _guard = RequestIdentityGuard::install(None);
            assert_eq!(current_auth_identity(), None);
            assert_eq!(current_tenant(), None);
        }
        // Previous state is restored on drop (the caller owns cleanup).
        assert_eq!(
            current_auth_identity(),
            Some(("leaked".to_string(), Role::Admin))
        );
        clear_current_auth_identity();
        clear_current_tenant();
    }
}
