//! `AiProviderGate` (#711, S3) — planner-level policy gate for the
//! `USING <provider>` clause on `ASK`, `INSERT ... WITH AUTO EMBED`,
//! and `SEARCH SIMILAR ... USING`.
//!
//! Runs **before** the AI credential resolver. Default behaviour when
//! no policy matches is **Allow** (back-compat); operators opt in to
//! restriction by attaching a `Deny ai:provider:<token>` policy to a
//! principal. Allow / DefaultDeny / AdminBypass all permit; only an
//! explicit `Deny` short-circuits the planner with a typed error.
//!
//! When the planner denies, the resolver-side audit event
//! `ai.credential.resolve` is **not** emitted (the query never reaches
//! the resolver). A separate event `ai.provider.gate` records the
//! planner-level decision so operators can reconstruct both layers.
//!
//! Action token format: `ai:provider:<token>` where `<token>` matches
//! [`crate::ai::AiProvider::token`]. The wildcards `ai:provider:*` and
//! `ai:*` are honoured by the policy evaluator's prefix-match rules.
//!
//! Resource: `ai-provider:<token>`. This lets operators write
//! provider-targeted Deny statements without needing to know about
//! collections.

use crate::ai::AiProvider;
use crate::auth::policies::{self as iam_policies, Decision, EvalContext, Policy, ResourceRef};
use crate::auth::{Role, UserId};
use crate::runtime::RedDBRuntime;
use crate::{RedDBError, RedDBResult};

/// Enforce the `ai:provider:<token>` gate for `provider` against the
/// effective principal of the current statement. Returns `Ok(())` to
/// proceed and `Err(RedDBError::Query)` when an explicit Deny matches.
///
/// Back-compat guarantees (acceptance criteria of #711):
///
/// * No auth store installed (embedded callers) → allow.
/// * No statement identity (system caller) → allow.
/// * IAM authorization disabled → allow.
/// * No policy matches (DefaultDeny) → **allow** (operators opt in to
///   restriction by writing Deny statements).
pub fn enforce(runtime: &RedDBRuntime, provider: &AiProvider) -> RedDBResult<()> {
    let Some(auth_store) = runtime.inner.auth_store.read().clone() else {
        return Ok(());
    };
    if !auth_store.iam_authorization_enabled() {
        return Ok(());
    }
    let Some((username, role)) = crate::runtime::impl_core::current_auth_identity() else {
        return Ok(());
    };
    let tenant = crate::runtime::impl_core::current_tenant();
    let principal = UserId::from_parts(tenant.as_deref(), &username);
    let token = provider.token().to_string();
    let action = format!("ai:provider:{token}");
    let resource = ResourceRef::new("ai-provider", token.clone());
    let ctx = EvalContext {
        principal_tenant: tenant.clone(),
        current_tenant: tenant,
        peer_ip: None,
        mfa_present: false,
        now_ms: crate::auth::now_ms(),
        principal_is_admin_role: role == Role::Admin,
        principal_is_system_owned: auth_store.principal_is_system_owned(&principal),
        principal_is_platform_scoped: principal.tenant.is_none(),
    };

    let policies = auth_store.effective_policies(&principal);
    let refs: Vec<&Policy> = policies.iter().map(|p| p.as_ref()).collect();
    let decision = iam_policies::evaluate(&refs, &action, &resource, &ctx);

    match decision {
        Decision::Deny {
            ref matched_policy_id,
            ref matched_sid,
        } => {
            record_gate_audit(
                runtime,
                &principal,
                &action,
                &resource,
                "deny",
                Some(matched_policy_id.as_str()),
                matched_sid.as_deref(),
            );
            Err(RedDBError::Query(format!(
                "policy: principal '{}' is not allowed to use AI provider '{}'",
                principal.username, token
            )))
        }
        Decision::Allow {
            ref matched_policy_id,
            ref matched_sid,
        } => {
            record_gate_audit(
                runtime,
                &principal,
                &action,
                &resource,
                "allow",
                Some(matched_policy_id.as_str()),
                matched_sid.as_deref(),
            );
            Ok(())
        }
        // DefaultDeny / AdminBypass: back-compat default-allow. No
        // audit event — the gate did not affect the outcome, and we
        // don't want to spam audit on every AI-touching query in
        // deployments that haven't adopted the gate.
        Decision::DefaultDeny | Decision::AdminBypass => Ok(()),
    }
}

fn record_gate_audit(
    runtime: &RedDBRuntime,
    principal: &UserId,
    action: &str,
    resource: &ResourceRef,
    outcome: &str,
    matched_policy_id: Option<&str>,
    matched_sid: Option<&str>,
) {
    let mut detail = crate::json::Map::new();
    detail.insert(
        "action".to_string(),
        crate::json::Value::String(action.to_string()),
    );
    detail.insert(
        "resource".to_string(),
        crate::json::Value::String(format!("{}:{}", resource.kind, resource.name)),
    );
    if let Some(id) = matched_policy_id {
        detail.insert(
            "matched_policy_id".to_string(),
            crate::json::Value::String(id.to_string()),
        );
    }
    if let Some(sid) = matched_sid {
        detail.insert(
            "matched_sid".to_string(),
            crate::json::Value::String(sid.to_string()),
        );
    }
    let principal_label = match principal.tenant.as_deref() {
        Some(t) => format!("{t}/{}", principal.username),
        None => principal.username.clone(),
    };
    runtime.audit_log().record(
        "ai.provider.gate",
        &principal_label,
        &format!("{}:{}", resource.kind, resource.name),
        outcome,
        crate::json::Value::Object(detail),
    );
}
