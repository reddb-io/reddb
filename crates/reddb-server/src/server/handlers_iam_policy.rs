//! IAM policy admin HTTP endpoints.

use super::*;
use std::collections::BTreeMap;

struct IamPolicyCaller {
    id: crate::auth::UserId,
    role: crate::auth::Role,
}

fn resolve_iam_policy_caller(
    server: &RedDBServer,
    headers: &BTreeMap<String, String>,
) -> Option<IamPolicyCaller> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))?;
    let auth_store = server.auth_store.as_ref()?;
    if super::routing::looks_like_jwt(token) {
        if let Some(validator) = server.runtime.oauth_validator() {
            if let Ok((tenant, username, role)) =
                crate::wire::redwire::auth::validate_oauth_jwt_full(&validator, token)
            {
                return Some(IamPolicyCaller {
                    id: crate::auth::UserId::from_parts(tenant.as_deref(), &username),
                    role,
                });
            }
        }
    }
    let (id, role) = auth_store.validate_token_full(token)?;
    Some(IamPolicyCaller { id, role })
}

impl RedDBServer {
    fn iam_audit(&self, action: &str, target: &str, outcome: &str) {
        self.runtime
            .audit_log()
            .record(action, "operator", target, outcome, JsonValue::Null);
    }

    fn authorize_policy_mutation(
        &self,
        headers: &BTreeMap<String, String>,
        action: &str,
        policy_id: &str,
    ) -> Option<HttpResponse> {
        let Some(store) = self.auth_store.as_ref() else {
            return Some(json_error(503, "auth store not configured"));
        };
        if !store.iam_authorization_enabled() {
            return None;
        }
        let caller = resolve_iam_policy_caller(self, headers);
        let Some(caller) = caller else {
            if store.is_enabled() && store.config().require_auth {
                return Some(json_error(401, "authentication required"));
            }
            return None;
        };
        let resource = crate::auth::policies::ResourceRef::new("policy", policy_id);
        let ctx = store.eval_context_for_principal(&caller.id, caller.role, None);
        if store.check_policy_authz_with_role(&caller.id, action, &resource, &ctx, caller.role) {
            None
        } else {
            Some(json_error(
                403,
                format!("policy denied {action} on policy:{policy_id}"),
            ))
        }
    }

    /// `PUT /admin/policies/:id` — install or replace an IAM policy.
    pub(crate) fn handle_iam_policy_put(
        &self,
        headers: &BTreeMap<String, String>,
        id: &str,
        body: Vec<u8>,
    ) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        if let Some(response) = self.authorize_policy_mutation(headers, "policy:put", id) {
            return response;
        }
        let Ok(text) = std::str::from_utf8(&body) else {
            return json_error(400, "body must be utf-8 JSON");
        };
        let mut policy = match crate::auth::policies::Policy::from_json_str(text) {
            Ok(p) => p,
            Err(e) => return json_error(400, format!("policy parse: {e}")),
        };
        if policy.id != id {
            policy.id = id.to_string();
        }
        if let Err(e) = store.put_policy(policy) {
            return json_error(400, e.to_string());
        }
        self.runtime.invalidate_result_cache();
        self.iam_audit("iam/policy.put", id, "ok");
        let mut obj = Map::new();
        obj.insert("ok".to_string(), JsonValue::Bool(true));
        obj.insert("id".to_string(), JsonValue::String(id.to_string()));
        json_response(200, JsonValue::Object(obj))
    }

    /// `GET /admin/policies/:id` — fetch a single policy as JSON.
    pub(crate) fn handle_iam_policy_get(&self, id: &str) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        let Some(p) = store.get_policy(id) else {
            return json_error(404, format!("policy `{id}` not found"));
        };
        let body = p.to_json_string();
        HttpResponse {
            status: 200,
            content_type: "application/json",
            body: body.into_bytes(),
            extra_headers: Vec::new(),
        }
    }

    /// `GET /admin/policies` — list policies (id-sorted summary).
    pub(crate) fn handle_iam_policy_list(&self) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        let pols = store.list_policies();
        let items: Vec<JsonValue> = pols
            .iter()
            .map(|p| {
                let mut obj = Map::new();
                obj.insert("id".to_string(), JsonValue::String(p.id.clone()));
                obj.insert("version".to_string(), JsonValue::Number(p.version as f64));
                obj.insert(
                    "statements".to_string(),
                    JsonValue::Number(p.statements.len() as f64),
                );
                obj.insert(
                    "tenant".to_string(),
                    p.tenant
                        .as_deref()
                        .map(|t| JsonValue::String(t.to_string()))
                        .unwrap_or(JsonValue::Null),
                );
                JsonValue::Object(obj)
            })
            .collect();
        let mut envelope = Map::new();
        envelope.insert("count".to_string(), JsonValue::Number(items.len() as f64));
        envelope.insert("items".to_string(), JsonValue::Array(items));
        json_response(200, JsonValue::Object(envelope))
    }

    /// `GET /admin/policies/actions` — list every recognised policy
    /// action verb. Issue #709: HTTP introspection mirror of the
    /// `red.policy.actions` SQL virtual table.
    pub(crate) fn handle_iam_policy_actions(&self) -> HttpResponse {
        use crate::auth::action_catalog::{LifecycleState, ACTIONS};

        let items: Vec<JsonValue> = ACTIONS
            .iter()
            .map(|entry| {
                let (state, replacement, since_version) = match &entry.lifecycle_state {
                    LifecycleState::Active => ("active", JsonValue::Null, JsonValue::Null),
                    LifecycleState::Deprecated {
                        replacement,
                        since_version,
                    } => (
                        "deprecated",
                        replacement
                            .map(|r| JsonValue::String(r.to_string()))
                            .unwrap_or(JsonValue::Null),
                        JsonValue::String(since_version.to_string()),
                    ),
                    LifecycleState::Removed => ("removed", JsonValue::Null, JsonValue::Null),
                };
                let mut obj = Map::new();
                obj.insert(
                    "name".to_string(),
                    JsonValue::String(entry.name.to_string()),
                );
                obj.insert(
                    "category".to_string(),
                    JsonValue::String(entry.category.as_str().to_string()),
                );
                obj.insert(
                    "lifecycle_state".to_string(),
                    JsonValue::String(state.to_string()),
                );
                obj.insert("replacement".to_string(), replacement);
                obj.insert("since_version".to_string(), since_version);
                obj.insert(
                    "gates_description".to_string(),
                    JsonValue::String(entry.gates_description.to_string()),
                );
                JsonValue::Object(obj)
            })
            .collect();
        let mut envelope = Map::new();
        envelope.insert("count".to_string(), JsonValue::Number(items.len() as f64));
        envelope.insert("items".to_string(), JsonValue::Array(items));
        json_response(200, JsonValue::Object(envelope))
    }

    /// `POST /admin/policies/lint` — lint a policy document supplied
    /// in the request body. Returns an envelope `{count, diagnostics:
    /// [...]}` mirroring the SQL `LINT POLICY` result set. Issue #710.
    ///
    /// The body is treated as the policy JSON itself (not an envelope
    /// containing a policy under some key) so the surface composes
    /// with `PUT /admin/policies/:id` — operators can lint exactly the
    /// document they're about to install.
    pub(crate) fn handle_iam_policy_lint(&self, body: Vec<u8>) -> HttpResponse {
        let Ok(text) = std::str::from_utf8(&body) else {
            return json_error(400, "body must be utf-8 JSON");
        };
        let diags = crate::auth::policy_linter::lint(text);
        let items: Vec<JsonValue> = diags.iter().map(|d| d.to_json_value()).collect();
        let mut envelope = Map::new();
        envelope.insert("count".to_string(), JsonValue::Number(items.len() as f64));
        envelope.insert("diagnostics".to_string(), JsonValue::Array(items));
        json_response(200, JsonValue::Object(envelope))
    }

    /// `POST /admin/policies/migrate-mode` — body
    /// `{ "target": "policy_only", "dry_run": true | false }`. Runs the
    /// same pre-flight delta simulator as the SQL `MIGRATE POLICY MODE`
    /// command and mirrors its outcomes exactly. Issue #714.
    pub(crate) fn handle_iam_policy_migrate_mode(&self, body: Vec<u8>) -> HttpResponse {
        use crate::auth::enforcement_mode::PolicyEnforcementMode;
        use crate::auth::migrate_policy_mode::{
            principal_label, simulate_migration_delta, MigratePolicyDelta,
        };
        use crate::auth::policies::ResourceRef;

        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        let parsed = match crate::serde_json::from_str::<crate::serde_json::Value>(
            std::str::from_utf8(&body).unwrap_or(""),
        ) {
            Ok(v) => v,
            Err(e) => return json_error(400, format!("invalid JSON body: {e}")),
        };
        let obj = match parsed.as_object() {
            Some(o) => o,
            None => return json_error(400, "body must be a JSON object"),
        };
        let target = match obj.get("target").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return json_error(400, "missing `target`"),
        };
        let dry_run = obj
            .get("dry_run")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let parsed_mode = match PolicyEnforcementMode::parse(&target) {
            Some(m) => m,
            None => {
                return json_error(
                    400,
                    format!("invalid target `{target}` (expected `policy_only`)"),
                );
            }
        };
        if parsed_mode != PolicyEnforcementMode::PolicyOnly {
            return json_error(
                400,
                format!("target `{target}` is not supported — only `policy_only` may be migrated to via this endpoint"),
            );
        }

        let snapshot = self.runtime.catalog();
        let resources: Vec<ResourceRef> = snapshot
            .collections
            .iter()
            .map(|c| ResourceRef::new("table", c.name.clone()))
            .collect();
        let now_ms = crate::utils::now_unix_millis() as u128;
        let deltas: Vec<MigratePolicyDelta> =
            simulate_migration_delta(store.as_ref(), &resources, now_ms);

        let outcome_str = if dry_run {
            "dry_run"
        } else if deltas.is_empty() {
            "applied"
        } else {
            "refused"
        };
        self.iam_audit("iam/policy.migrate_mode", &target, outcome_str);

        let items: Vec<JsonValue> = deltas
            .iter()
            .map(|d| {
                let mut row = Map::new();
                row.insert(
                    "principal".to_string(),
                    JsonValue::String(principal_label(&d.principal)),
                );
                row.insert(
                    "role".to_string(),
                    JsonValue::String(d.role.as_str().to_string()),
                );
                row.insert("action".to_string(), JsonValue::String(d.action.clone()));
                row.insert(
                    "resource_kind".to_string(),
                    JsonValue::String(d.resource_kind.clone()),
                );
                row.insert(
                    "resource_name".to_string(),
                    JsonValue::String(d.resource_name.clone()),
                );
                JsonValue::Object(row)
            })
            .collect();
        let mut envelope = Map::new();
        envelope.insert("target".to_string(), JsonValue::String(target.clone()));
        envelope.insert("dry_run".to_string(), JsonValue::Bool(dry_run));
        envelope.insert(
            "outcome".to_string(),
            JsonValue::String(outcome_str.to_string()),
        );
        envelope.insert("count".to_string(), JsonValue::Number(items.len() as f64));
        envelope.insert("delta".to_string(), JsonValue::Array(items));

        if !dry_run && !deltas.is_empty() {
            // Refuse: 409 Conflict carries the delta so the client
            // can decide whether to attach allow policies and retry.
            return json_response(409, JsonValue::Object(envelope));
        }
        if !dry_run {
            store.set_enforcement_mode(parsed_mode);
        }
        json_response(200, JsonValue::Object(envelope))
    }

    /// `DELETE /admin/policies/:id` — drop a policy.
    pub(crate) fn handle_iam_policy_delete(
        &self,
        headers: &BTreeMap<String, String>,
        id: &str,
    ) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        if let Some(response) = self.authorize_policy_mutation(headers, "policy:drop", id) {
            return response;
        }
        match store.delete_policy(id) {
            Ok(()) => {
                self.runtime.invalidate_result_cache();
                self.iam_audit("iam/policy.drop", id, "ok");
                HttpResponse {
                    status: 204,
                    content_type: "application/json",
                    body: Vec::new(),
                    extra_headers: Vec::new(),
                }
            }
            Err(e) => json_error(404, e.to_string()),
        }
    }

    /// `PUT /admin/users/:user/policies/:policy_id`. `:user` may
    /// optionally be tenant-qualified as `tenant.username`.
    pub(crate) fn handle_iam_attach_user(
        &self,
        headers: &BTreeMap<String, String>,
        user: &str,
        policy_id: &str,
    ) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        if let Some(response) = self.authorize_policy_mutation(headers, "policy:attach", policy_id)
        {
            return response;
        }
        let uid = decode_user_arg(user);
        match store.attach_policy(
            crate::auth::store::PrincipalRef::User(uid.clone()),
            policy_id,
        ) {
            Ok(()) => {
                self.runtime.invalidate_result_cache();
                self.iam_audit(
                    "iam/policy.attach",
                    &format!("user:{uid}::{policy_id}"),
                    "ok",
                );
                let mut obj = Map::new();
                obj.insert("ok".to_string(), JsonValue::Bool(true));
                json_response(200, JsonValue::Object(obj))
            }
            Err(e) => json_error(400, e.to_string()),
        }
    }

    /// `DELETE /admin/users/:user/policies/:policy_id`.
    pub(crate) fn handle_iam_detach_user(
        &self,
        headers: &BTreeMap<String, String>,
        user: &str,
        policy_id: &str,
    ) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        if let Some(response) = self.authorize_policy_mutation(headers, "policy:detach", policy_id)
        {
            return response;
        }
        let uid = decode_user_arg(user);
        match store.detach_policy(
            crate::auth::store::PrincipalRef::User(uid.clone()),
            policy_id,
        ) {
            Ok(()) => {
                self.runtime.invalidate_result_cache();
                self.iam_audit(
                    "iam/policy.detach",
                    &format!("user:{uid}::{policy_id}"),
                    "ok",
                );
                HttpResponse {
                    status: 204,
                    content_type: "application/json",
                    body: Vec::new(),
                    extra_headers: Vec::new(),
                }
            }
            Err(e) => json_error(400, e.to_string()),
        }
    }

    /// `PUT /admin/users/:user/groups/:group`.
    pub(crate) fn handle_iam_add_user_group(&self, user: &str, group: &str) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        let uid = decode_user_arg(user);
        match store.add_user_to_group(&uid, group) {
            Ok(()) => {
                self.runtime.invalidate_result_cache();
                self.iam_audit("iam/group.add", &format!("user:{uid}::group:{group}"), "ok");
                let mut obj = Map::new();
                obj.insert("ok".to_string(), JsonValue::Bool(true));
                json_response(200, JsonValue::Object(obj))
            }
            Err(e) => json_error(400, e.to_string()),
        }
    }

    /// `DELETE /admin/users/:user/groups/:group`.
    pub(crate) fn handle_iam_remove_user_group(&self, user: &str, group: &str) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        let uid = decode_user_arg(user);
        match store.remove_user_from_group(&uid, group) {
            Ok(()) => {
                self.runtime.invalidate_result_cache();
                self.iam_audit(
                    "iam/group.remove",
                    &format!("user:{uid}::group:{group}"),
                    "ok",
                );
                HttpResponse {
                    status: 204,
                    content_type: "application/json",
                    body: Vec::new(),
                    extra_headers: Vec::new(),
                }
            }
            Err(e) => json_error(400, e.to_string()),
        }
    }

    /// `PUT /admin/groups/:group/policies/:policy_id`.
    pub(crate) fn handle_iam_attach_group(
        &self,
        headers: &BTreeMap<String, String>,
        group: &str,
        policy_id: &str,
    ) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        if let Some(response) = self.authorize_policy_mutation(headers, "policy:attach", policy_id)
        {
            return response;
        }
        match store.attach_policy(
            crate::auth::store::PrincipalRef::Group(group.to_string()),
            policy_id,
        ) {
            Ok(()) => {
                self.runtime.invalidate_result_cache();
                self.iam_audit(
                    "iam/policy.attach",
                    &format!("group:{group}::{policy_id}"),
                    "ok",
                );
                let mut obj = Map::new();
                obj.insert("ok".to_string(), JsonValue::Bool(true));
                json_response(200, JsonValue::Object(obj))
            }
            Err(e) => json_error(400, e.to_string()),
        }
    }

    /// `DELETE /admin/groups/:group/policies/:policy_id`.
    pub(crate) fn handle_iam_detach_group(
        &self,
        headers: &BTreeMap<String, String>,
        group: &str,
        policy_id: &str,
    ) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        if let Some(response) = self.authorize_policy_mutation(headers, "policy:detach", policy_id)
        {
            return response;
        }
        match store.detach_policy(
            crate::auth::store::PrincipalRef::Group(group.to_string()),
            policy_id,
        ) {
            Ok(()) => {
                self.runtime.invalidate_result_cache();
                self.iam_audit(
                    "iam/policy.detach",
                    &format!("group:{group}::{policy_id}"),
                    "ok",
                );
                HttpResponse {
                    status: 204,
                    content_type: "application/json",
                    body: Vec::new(),
                    extra_headers: Vec::new(),
                }
            }
            Err(e) => json_error(400, e.to_string()),
        }
    }

    /// `GET /admin/users/:user/effective-permissions[?resource=kind:name]`.
    pub(crate) fn handle_iam_effective_permissions(
        &self,
        user: &str,
        query: &std::collections::BTreeMap<String, String>,
    ) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        let uid = decode_user_arg(user);
        let pols = store.effective_policies(&uid);

        // Build a JSON array of policy summaries scoped to the user.
        // The optional `resource` query string parameter is parsed but
        // currently only echoed back — fine-grained matching falls
        // through to `simulate`.
        let resource_echo = query.get("resource").cloned();
        let items: Vec<JsonValue> = pols
            .iter()
            .map(|p| {
                let mut obj = Map::new();
                obj.insert("id".to_string(), JsonValue::String(p.id.clone()));
                obj.insert(
                    "statements".to_string(),
                    JsonValue::Number(p.statements.len() as f64),
                );
                JsonValue::Object(obj)
            })
            .collect();
        let mut envelope = Map::new();
        envelope.insert("user".to_string(), JsonValue::String(uid.to_string()));
        if let Some(r) = resource_echo {
            envelope.insert("resource".to_string(), JsonValue::String(r));
        }
        envelope.insert("count".to_string(), JsonValue::Number(items.len() as f64));
        envelope.insert("policies".to_string(), JsonValue::Array(items));
        json_response(200, JsonValue::Object(envelope))
    }

    /// `POST /admin/policies/simulate` —
    /// body: `{principal, action, resource: {kind, name, tenant?}, ctx?}`.
    pub(crate) fn handle_iam_simulate(&self, body: Vec<u8>) -> HttpResponse {
        let Some(store) = self.auth_store.as_ref() else {
            return json_error(503, "auth store not configured");
        };
        let parsed = match crate::serde_json::from_str::<crate::serde_json::Value>(
            std::str::from_utf8(&body).unwrap_or(""),
        ) {
            Ok(v) => v,
            Err(e) => return json_error(400, format!("invalid JSON body: {e}")),
        };
        let obj = match parsed.as_object() {
            Some(o) => o,
            None => return json_error(400, "body must be a JSON object"),
        };
        let principal = match obj.get("principal").and_then(|v| v.as_str()) {
            Some(s) => decode_user_arg(s),
            None => return json_error(400, "missing `principal`"),
        };
        let action = match obj.get("action").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return json_error(400, "missing `action`"),
        };
        let resource = match obj.get("resource") {
            Some(JsonValue::Object(r)) => {
                let kind = r
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = r
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if kind.is_empty() || name.is_empty() {
                    return json_error(400, "resource needs kind+name");
                }
                let mut rr = crate::auth::policies::ResourceRef::new(kind, name);
                if let Some(t) = r.get("tenant").and_then(|v| v.as_str()) {
                    rr = rr.with_tenant(t.to_string());
                }
                rr
            }
            Some(JsonValue::String(s)) => match s.split_once(':') {
                Some((k, n)) => crate::auth::policies::ResourceRef::new(k, n),
                None => return json_error(400, "resource string must be `kind:name`"),
            },
            _ => return json_error(400, "missing `resource`"),
        };
        let mut sim_ctx = crate::auth::store::SimCtx::default();
        if let Some(c) = obj.get("ctx").and_then(|v| v.as_object()) {
            if let Some(t) = c.get("current_tenant").and_then(|v| v.as_str()) {
                sim_ctx.current_tenant = Some(t.to_string());
            }
            if let Some(true) = c.get("mfa").and_then(|v| v.as_bool()) {
                sim_ctx.mfa_present = true;
            }
            if let Some(ip) = c
                .get("source_ip")
                .or_else(|| c.get("peer_ip"))
                .and_then(|v| v.as_str())
            {
                if let Ok(addr) = ip.parse() {
                    sim_ctx.peer_ip = Some(addr);
                }
            }
            if let Some(ms) = c.get("now_ms").and_then(|v| v.as_u64()) {
                sim_ctx.now_ms = Some(ms as u128);
            }
        }
        let outcome = store.simulate(&principal, &action, &resource, sim_ctx);
        let (decision_str, matched_pid, matched_sid) =
            crate::runtime::impl_core::decision_to_strings(&outcome.decision);

        self.iam_audit("iam/policy.simulate", &principal.to_string(), &decision_str);

        let mut envelope = Map::new();
        envelope.insert("decision".to_string(), JsonValue::String(decision_str));
        envelope.insert(
            "matched_policy_id".to_string(),
            matched_pid
                .map(JsonValue::String)
                .unwrap_or(JsonValue::Null),
        );
        envelope.insert(
            "matched_sid".to_string(),
            matched_sid
                .map(JsonValue::String)
                .unwrap_or(JsonValue::Null),
        );
        envelope.insert("reason".to_string(), JsonValue::String(outcome.reason));
        let trail: Vec<JsonValue> = outcome
            .trail
            .into_iter()
            .map(|t| {
                let mut obj = Map::new();
                obj.insert("policy_id".to_string(), JsonValue::String(t.policy_id));
                obj.insert(
                    "sid".to_string(),
                    t.sid.map(JsonValue::String).unwrap_or(JsonValue::Null),
                );
                obj.insert("matched".to_string(), JsonValue::Bool(t.matched));
                obj.insert(
                    "effect".to_string(),
                    JsonValue::String(
                        match t.effect {
                            crate::auth::policies::Effect::Allow => "allow",
                            crate::auth::policies::Effect::Deny => "deny",
                        }
                        .to_string(),
                    ),
                );
                obj.insert(
                    "why_skipped".to_string(),
                    t.why_skipped
                        .map(|s| JsonValue::String(s.to_string()))
                        .unwrap_or(JsonValue::Null),
                );
                JsonValue::Object(obj)
            })
            .collect();
        envelope.insert("trail".to_string(), JsonValue::Array(trail));
        json_response(200, JsonValue::Object(envelope))
    }
}

fn decode_user_arg(raw: &str) -> crate::auth::UserId {
    // Accepts `username` (platform tenant), `tenant.username` or
    // `tenant/username` to align with the SQL path / display form.
    if let Some((tenant, name)) = raw.split_once('/') {
        return crate::auth::UserId::scoped(tenant.to_string(), name.to_string());
    }
    if let Some((tenant, name)) = raw.split_once('.') {
        return crate::auth::UserId::scoped(tenant.to_string(), name.to_string());
    }
    crate::auth::UserId::platform(raw.to_string())
}
