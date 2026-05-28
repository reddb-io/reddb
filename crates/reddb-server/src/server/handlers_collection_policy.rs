//! Policy-aware gate for direct HTTP collection endpoints (issue #754).
//!
//! Bridges Red UI's policy-aware action vocabulary (`/auth/can`,
//! `/catalog/collections/{name}`) into the raw collection HTTP surface
//! (scan, get-entity, insert/update/delete, KV operations, metadata).
//! The gate keeps the legacy role-based middleware intact and only
//! adds an additional fine-grained policy check when IAM is active —
//! defined as: an `AuthStore` is configured *and*
//! `iam_authorization_enabled()` is true (i.e. at least one policy
//! has been installed). When IAM is not active, the gate is a no-op
//! and existing clients keep their current behavior, per the
//! decision recorded on issue #741's thread.
//!
//! Returning `Some(HttpResponse)` indicates a deny; the caller must
//! short-circuit with that response. `None` means "policy allows;
//! proceed".

use super::*;
use crate::auth::policies::{EvalContext, ResourceRef};
use crate::auth::{Role, UserId};

impl RedDBServer {
    /// Policy gate for direct HTTP collection endpoints.
    ///
    /// Behavior:
    /// * No `AuthStore` configured → returns `None` (legacy mode).
    /// * IAM not active (`iam_authorization_enabled() == false`) →
    ///   returns `None` (legacy role-based middleware already gated).
    /// * Caller cannot be resolved (no bearer / unknown token) →
    ///   returns a UI-safe 401 deny.
    /// * Policy evaluation denies → returns a UI-safe 403 deny.
    /// * Otherwise → returns `None` (proceed).
    ///
    /// `action` is one of the action-catalog verbs (`select`, `insert`,
    /// `update`, `delete`) and `collection` is the resource name. The
    /// resource kind is fixed to `"collection"` to match how the Red
    /// UI probes via `/auth/can` and how `check_ddl_collection_privilege`
    /// resolves resources in the runtime layer.
    pub(crate) fn check_collection_http_policy(
        &self,
        headers: &BTreeMap<String, String>,
        action: &str,
        collection: &str,
    ) -> Option<HttpResponse> {
        let auth_store = self.auth_store.as_ref()?;
        if !auth_store.iam_authorization_enabled() {
            return None;
        }

        let (principal, role) = match resolve_caller(self, auth_store, headers) {
            CallerOutcome::Resolved { id, role } => (id, role),
            CallerOutcome::Anonymous => {
                return Some(collection_policy_deny(
                    401,
                    action,
                    collection,
                    "authentication required for policy-gated collection access",
                ));
            }
            CallerOutcome::InvalidToken => {
                return Some(collection_policy_deny(
                    401,
                    action,
                    collection,
                    "invalid or unknown bearer token",
                ));
            }
        };

        let tenant = principal.tenant.clone();
        let mut resource = ResourceRef::new("collection", collection);
        if let Some(ref t) = tenant {
            resource = resource.with_tenant(t.clone());
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let ctx = EvalContext {
            principal_tenant: tenant.clone(),
            current_tenant: tenant,
            peer_ip: None,
            mfa_present: false,
            now_ms,
            principal_is_admin_role: role == Role::Admin,
            principal_is_system_owned: auth_store.principal_is_system_owned(&principal),
            principal_is_platform_scoped: principal.tenant.is_none(),
        };

        if auth_store.check_policy_authz_with_role(&principal, action, &resource, &ctx, role) {
            None
        } else {
            Some(collection_policy_deny(
                403,
                action,
                collection,
                "denied by IAM policy",
            ))
        }
    }
}

enum CallerOutcome {
    Resolved { id: UserId, role: Role },
    Anonymous,
    InvalidToken,
}

fn resolve_caller(
    server: &RedDBServer,
    auth_store: &crate::auth::store::AuthStore,
    headers: &BTreeMap<String, String>,
) -> CallerOutcome {
    let token = match headers
        .get("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        Some(t) if !t.is_empty() => t,
        _ => return CallerOutcome::Anonymous,
    };
    if super::routing::looks_like_jwt(token) {
        if let Some(validator) = server.runtime.oauth_validator() {
            return match crate::wire::redwire::auth::validate_oauth_jwt_full(&validator, token) {
                Ok((tenant, username, role)) => CallerOutcome::Resolved {
                    id: UserId::from_parts(tenant.as_deref(), &username),
                    role,
                },
                Err(_) => CallerOutcome::InvalidToken,
            };
        }
    }
    match auth_store.validate_token_full(token) {
        Some((id, role)) => CallerOutcome::Resolved { id, role },
        None => CallerOutcome::InvalidToken,
    }
}

/// Structured, UI-safe deny payload. The body shape mirrors the
/// `/auth/can` result envelope (action + resource + reason) so the
/// Red UI can render policy-deny banners uniformly across the
/// probe-then-act flow. The `reason` field is run through the
/// tainted-string JSON guard since it can be surfaced verbatim.
fn collection_policy_deny(
    status: u16,
    action: &str,
    collection: &str,
    reason: &str,
) -> HttpResponse {
    let mut obj = Map::new();
    obj.insert("ok".to_string(), JsonValue::Bool(false));
    obj.insert(
        "error".to_string(),
        JsonValue::String("forbidden".to_string()),
    );
    obj.insert("action".to_string(), JsonValue::String(action.to_string()));
    let mut res = Map::new();
    res.insert(
        "kind".to_string(),
        JsonValue::String("collection".to_string()),
    );
    res.insert(
        "name".to_string(),
        JsonValue::String(collection.to_string()),
    );
    obj.insert("resource".to_string(), JsonValue::Object(res));
    obj.insert(
        "reason".to_string(),
        crate::json_field::SerializedJsonField::tainted(reason),
    );
    json_response(status, JsonValue::Object(obj))
}
