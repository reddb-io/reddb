//! IAM / GRANT / user / policy statement-execution family extracted
//! from `impl_core` (issue #1623, PRD #1619). Behaviour-preserving
//! move: these methods consume the policy-column helpers that live in
//! [`super::policy_columns`]. The central dispatch in
//! [`super::super::impl_core`] calls the (unchanged) `pub(crate)` methods.
use super::super::execution_context::{current_auth_identity, current_tenant};
use super::super::*;
use super::policy_columns::*;

impl RedDBRuntime {
    pub(crate) fn execute_grant_statement(
        &self,
        query: &str,
        stmt: &crate::storage::query::ast::GrantStmt,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::privileges::{Action, GrantPrincipal, Resource};
        use crate::auth::UserId;
        use crate::storage::query::ast::{GrantObjectKind, GrantPrincipalRef};

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;

        // Granter identity + role.
        let (gname, grole) = current_auth_identity().ok_or_else(|| {
            RedDBError::Query("GRANT requires an authenticated principal".to_string())
        })?;
        let granter = UserId::from_parts(current_tenant().as_deref(), &gname);
        let granter_role = grole;

        // Build the action set.
        let mut actions: Vec<Action> = Vec::new();
        if stmt.all {
            actions.push(Action::All);
        } else {
            for kw in &stmt.actions {
                let a = Action::from_keyword(kw).ok_or_else(|| {
                    RedDBError::Query(format!("unknown privilege keyword `{}`", kw))
                })?;
                actions.push(a);
            }
        }

        // Audit emit (printed; structured emission is Agent #4's lane).
        let mut applied = 0usize;
        for obj in &stmt.objects {
            let resource = match stmt.object_kind {
                GrantObjectKind::Table => Resource::Table {
                    schema: obj.schema.clone(),
                    table: obj.name.clone(),
                },
                GrantObjectKind::Schema => Resource::Schema(obj.name.clone()),
                GrantObjectKind::Database => Resource::Database,
                GrantObjectKind::Function => Resource::Function {
                    schema: obj.schema.clone(),
                    name: obj.name.clone(),
                },
            };
            for principal in &stmt.principals {
                let p = match principal {
                    GrantPrincipalRef::Public => GrantPrincipal::Public,
                    GrantPrincipalRef::Group(g) => GrantPrincipal::Group(g.clone()),
                    GrantPrincipalRef::User { tenant, name } => {
                        GrantPrincipal::User(UserId::from_parts(tenant.as_deref(), name))
                    }
                };
                // Tenant of the grant follows the granter's tenant
                // (cross-tenant guard inside `AuthStore::grant`).
                let tenant = granter.tenant.clone();
                auth_store
                    .grant(
                        &granter,
                        granter_role,
                        p.clone(),
                        resource.clone(),
                        actions.clone(),
                        stmt.with_grant_option,
                        tenant.clone(),
                    )
                    .map_err(|e| RedDBError::Query(e.to_string()))?;

                // IAM policy translation: every GRANT also lands as a
                // synthetic `_grant_<id>` policy attached to the
                // principal so the new evaluator sees it.
                if let Some(policy) =
                    grant_to_iam_policy(&p, &resource, &actions, tenant.as_deref())
                {
                    let pid = policy.id.clone();
                    auth_store
                        .put_policy_internal(policy)
                        .map_err(|e| RedDBError::Query(e.to_string()))?;
                    let attachment = match &p {
                        GrantPrincipal::User(uid) => {
                            crate::auth::store::PrincipalRef::User(uid.clone())
                        }
                        GrantPrincipal::Group(group) => {
                            crate::auth::store::PrincipalRef::Group(group.clone())
                        }
                        GrantPrincipal::Public => crate::auth::store::PrincipalRef::Group(
                            crate::auth::store::PUBLIC_IAM_GROUP.to_string(),
                        ),
                    };
                    auth_store
                        .attach_policy(attachment, &pid)
                        .map_err(|e| RedDBError::Query(e.to_string()))?;
                }
                applied += 1;
                tracing::info!(
                    target: "audit",
                    principal = %granter,
                    action = "grant",
                    "GRANT applied"
                );
            }
        }

        self.invalidate_result_cache();
        Ok(RuntimeQueryResult::ok_message(
            query.to_string(),
            &format!("GRANT applied to {} target(s)", applied),
            "grant",
        ))
    }

    /// Translate the parsed [`RevokeStmt`] into AuthStore mutations.
    pub(crate) fn execute_revoke_statement(
        &self,
        query: &str,
        stmt: &crate::storage::query::ast::RevokeStmt,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::privileges::{Action, GrantPrincipal, Resource};
        use crate::auth::UserId;
        use crate::storage::query::ast::{GrantObjectKind, GrantPrincipalRef};

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;

        let (_gname, grole) = current_auth_identity().ok_or_else(|| {
            RedDBError::Query("REVOKE requires an authenticated principal".to_string())
        })?;
        let granter_role = grole;

        let actions: Vec<Action> = if stmt.all {
            vec![Action::All]
        } else {
            stmt.actions
                .iter()
                .map(|kw| Action::from_keyword(kw).unwrap_or(Action::Select))
                .collect()
        };

        let mut total_removed = 0usize;
        for obj in &stmt.objects {
            let resource = match stmt.object_kind {
                GrantObjectKind::Table => Resource::Table {
                    schema: obj.schema.clone(),
                    table: obj.name.clone(),
                },
                GrantObjectKind::Schema => Resource::Schema(obj.name.clone()),
                GrantObjectKind::Database => Resource::Database,
                GrantObjectKind::Function => Resource::Function {
                    schema: obj.schema.clone(),
                    name: obj.name.clone(),
                },
            };
            for principal in &stmt.principals {
                let p = match principal {
                    GrantPrincipalRef::Public => GrantPrincipal::Public,
                    GrantPrincipalRef::Group(g) => GrantPrincipal::Group(g.clone()),
                    GrantPrincipalRef::User { tenant, name } => {
                        GrantPrincipal::User(UserId::from_parts(tenant.as_deref(), name))
                    }
                };
                let removed = auth_store
                    .revoke(granter_role, &p, &resource, &actions)
                    .map_err(|e| RedDBError::Query(e.to_string()))?;
                let _removed_policies =
                    auth_store.delete_synthetic_grant_policies(&p, &resource, &actions);
                total_removed += removed;
            }
        }

        self.invalidate_result_cache();
        Ok(RuntimeQueryResult::ok_message(
            query.to_string(),
            &format!("REVOKE removed {} grant(s)", total_removed),
            "revoke",
        ))
    }

    /// Translate the parsed [`CreateUserStmt`] into an AuthStore user.
    pub(crate) fn execute_create_user_statement(
        &self,
        query: &str,
        stmt: &crate::storage::query::ast::CreateUserStmt,
    ) -> RedDBResult<RuntimeQueryResult> {
        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;

        let (_gname, grole) = current_auth_identity().ok_or_else(|| {
            RedDBError::Query("CREATE USER requires an authenticated principal".to_string())
        })?;
        if grole != crate::auth::Role::Admin {
            return Err(RedDBError::Query(
                "CREATE USER requires Admin role".to_string(),
            ));
        }

        let role = crate::auth::Role::from_str(&stmt.role)
            .ok_or_else(|| RedDBError::Query(format!("invalid role `{}`", stmt.role)))?;
        let user = auth_store
            .create_user_in_tenant(stmt.tenant.as_deref(), &stmt.username, &stmt.password, role)
            .map_err(|e| RedDBError::Query(e.to_string()))?;

        self.invalidate_result_cache();
        let target = crate::auth::UserId::from_parts(user.tenant_id.as_deref(), &user.username);
        tracing::info!(
            target: "audit",
            principal = %target,
            role = %role,
            action = "create_user",
            "CREATE USER applied"
        );

        Ok(RuntimeQueryResult::ok_message(
            query.to_string(),
            &format!("CREATE USER {} applied", target),
            "create_user",
        ))
    }

    /// Translate the parsed [`AlterUserStmt`] into AuthStore mutations.
    pub(crate) fn execute_alter_user_statement(
        &self,
        query: &str,
        stmt: &crate::storage::query::ast::AlterUserStmt,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::privileges::UserAttributes;
        use crate::auth::UserId;
        use crate::storage::query::ast::AlterUserAttribute;

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;

        let (gname, grole) = current_auth_identity().ok_or_else(|| {
            RedDBError::Query("ALTER USER requires an authenticated principal".to_string())
        })?;
        if grole != crate::auth::Role::Admin {
            return Err(RedDBError::Query(
                "ALTER USER requires Admin role".to_string(),
            ));
        }

        let target = UserId::from_parts(stmt.tenant.as_deref(), &stmt.username);
        let actor_tenant = current_tenant();
        let actor = UserId::from_parts(actor_tenant.as_deref(), &gname);
        for attr in &stmt.attributes {
            let action = match attr {
                AlterUserAttribute::Disable => "user:disable",
                AlterUserAttribute::Password(_) => "user:password:change",
                _ => "user:update",
            };
            if auth_store.has_explicit_user_lifecycle_deny(&actor, grole, action, &target) {
                return Err(RedDBError::Query(format!(
                    "ALTER USER denied by IAM policy: action `{action}` resource `user:{target}`"
                )));
            }
        }

        // Apply attributes incrementally — each one reads the current
        // record, mutates the relevant field, writes back.
        let mut attrs = auth_store.user_attributes(&target);
        let mut enable_change: Option<bool> = None;

        for a in &stmt.attributes {
            match a {
                AlterUserAttribute::ValidUntil(ts) => {
                    // Parse ISO-ish timestamp → ms since epoch. Fall
                    // back to integer-ms parsing for callers that pass
                    // `'1234567890123'`.
                    let ms = parse_timestamp_to_ms(ts).ok_or_else(|| {
                        RedDBError::Query(format!("invalid VALID UNTIL timestamp `{ts}`"))
                    })?;
                    attrs.valid_until = Some(ms);
                }
                AlterUserAttribute::ConnectionLimit(n) => {
                    if *n < 0 {
                        return Err(RedDBError::Query(
                            "CONNECTION LIMIT must be non-negative".to_string(),
                        ));
                    }
                    attrs.connection_limit = Some(*n as u32);
                }
                AlterUserAttribute::SetSearchPath(p) => {
                    attrs.search_path = Some(p.clone());
                }
                AlterUserAttribute::AddGroup(g) => {
                    if !attrs.groups.iter().any(|existing| existing == g) {
                        attrs.groups.push(g.clone());
                        attrs.groups.sort();
                    }
                }
                AlterUserAttribute::DropGroup(g) => {
                    attrs.groups.retain(|existing| existing != g);
                }
                AlterUserAttribute::Enable => enable_change = Some(true),
                AlterUserAttribute::Disable => enable_change = Some(false),
                AlterUserAttribute::Password(_) => {
                    // Out of scope — accept the AST but no-op so the
                    // parser stays compatible with future password
                    // rotation work.
                }
            }
        }

        auth_store
            .set_user_attributes(&target, attrs)
            .map_err(|e| RedDBError::Query(e.to_string()))?;
        if let Some(en) = enable_change {
            auth_store
                .set_user_enabled(&target, en)
                .map_err(|e| RedDBError::Query(e.to_string()))?;
        }
        self.invalidate_result_cache();
        tracing::info!(
            target: "audit",
            principal = %target,
            action = "alter_user",
            "ALTER USER applied"
        );

        Ok(RuntimeQueryResult::ok_message(
            query.to_string(),
            &format!("ALTER USER {} applied", target),
            "alter_user",
        ))
    }

    // -----------------------------------------------------------------
    // IAM policy executors
    // -----------------------------------------------------------------

    pub(crate) fn execute_create_iam_policy(
        &self,
        query: &str,
        id: &str,
        json: &str,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::policies::Policy;

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;

        // Parse + validate. The kernel rejects oversize / bad shape /
        // bad action keywords. If the supplied id differs from the JSON
        // id, override it with the SQL-provided id (the JSON id is
        // optional context — the SQL DDL form is authoritative).
        let mut policy = Policy::from_json_str(json)
            .map_err(|e| RedDBError::Query(format!("policy parse: {e}")))?;
        if policy.id != id {
            policy.id = id.to_string();
        }
        let pid = policy.id.clone();
        let tenant = current_tenant();
        let (actor_name, actor_role) = current_auth_identity()
            .unwrap_or_else(|| ("anonymous".to_string(), crate::auth::Role::Read));
        let actor = crate::auth::UserId::from_parts(tenant.as_deref(), &actor_name);
        let eval_ctx = runtime_iam_context(actor_role, tenant.as_deref());
        let event_ctx = self.policy_mutation_control_ctx(&actor, tenant.as_deref());
        let ledger = self.inner.control_event_ledger.read();
        let control = crate::auth::store::PolicyMutationControl {
            ctx: &event_ctx,
            ledger: ledger.as_ref(),
            config: self.inner.control_event_config,
            registry: Some(self.inner.config_registry.as_ref()),
            actor: &actor,
            eval_ctx: &eval_ctx,
        };
        auth_store
            .put_policy_with_control_events(policy, &control)
            .map_err(|e| RedDBError::Query(e.to_string()))?;

        let principal = actor_name;
        tracing::info!(
            target: "audit",
            principal = %principal,
            action = "iam:policy.put",
            matched_policy_id = %pid,
            "CREATE POLICY applied"
        );
        self.inner.audit_log.record(
            "iam/policy.put",
            &principal,
            &pid,
            "ok",
            crate::json::Value::Null,
        );

        self.invalidate_result_cache();
        Ok(RuntimeQueryResult::ok_message(
            query.to_string(),
            &format!("policy `{pid}` stored"),
            "create_iam_policy",
        ))
    }

    pub(crate) fn execute_drop_iam_policy(
        &self,
        query: &str,
        id: &str,
    ) -> RedDBResult<RuntimeQueryResult> {
        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;
        let tenant = current_tenant();
        let (actor_name, actor_role) = current_auth_identity()
            .unwrap_or_else(|| ("anonymous".to_string(), crate::auth::Role::Read));
        let actor = crate::auth::UserId::from_parts(tenant.as_deref(), &actor_name);
        let eval_ctx = runtime_iam_context(actor_role, tenant.as_deref());
        let event_ctx = self.policy_mutation_control_ctx(&actor, tenant.as_deref());
        let ledger = self.inner.control_event_ledger.read();
        let control = crate::auth::store::PolicyMutationControl {
            ctx: &event_ctx,
            ledger: ledger.as_ref(),
            config: self.inner.control_event_config,
            registry: Some(self.inner.config_registry.as_ref()),
            actor: &actor,
            eval_ctx: &eval_ctx,
        };
        auth_store
            .delete_policy_with_control_events(id, &control)
            .map_err(|e| RedDBError::Query(e.to_string()))?;

        let principal = actor_name;
        tracing::info!(
            target: "audit",
            principal = %principal,
            action = "iam:policy.drop",
            matched_policy_id = %id,
            "DROP POLICY applied"
        );
        self.inner.audit_log.record(
            "iam/policy.drop",
            &principal,
            id,
            "ok",
            crate::json::Value::Null,
        );

        self.invalidate_result_cache();
        Ok(RuntimeQueryResult::ok_message(
            query.to_string(),
            &format!("policy `{id}` dropped"),
            "drop_iam_policy",
        ))
    }

    pub(crate) fn execute_attach_policy(
        &self,
        query: &str,
        policy_id: &str,
        principal: &crate::storage::query::ast::PolicyPrincipalRef,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::store::PrincipalRef;
        use crate::auth::UserId;
        use crate::storage::query::ast::PolicyPrincipalRef;

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;
        let p = match principal {
            PolicyPrincipalRef::User(u) => {
                PrincipalRef::User(UserId::from_parts(u.tenant.as_deref(), &u.username))
            }
            PolicyPrincipalRef::Group(g) => PrincipalRef::Group(g.clone()),
        };
        let pretty_target = principal_label(principal);
        let tenant = current_tenant();
        let (actor_name, actor_role) = current_auth_identity()
            .unwrap_or_else(|| ("anonymous".to_string(), crate::auth::Role::Read));
        let actor = crate::auth::UserId::from_parts(tenant.as_deref(), &actor_name);
        let eval_ctx = runtime_iam_context(actor_role, tenant.as_deref());
        let event_ctx = self.policy_mutation_control_ctx(&actor, tenant.as_deref());
        let ledger = self.inner.control_event_ledger.read();
        let control = crate::auth::store::PolicyMutationControl {
            ctx: &event_ctx,
            ledger: ledger.as_ref(),
            config: self.inner.control_event_config,
            registry: Some(self.inner.config_registry.as_ref()),
            actor: &actor,
            eval_ctx: &eval_ctx,
        };
        auth_store
            .attach_policy_with_control_events(p, policy_id, &control)
            .map_err(|e| RedDBError::Query(e.to_string()))?;

        let principal_str = actor_name;
        tracing::info!(
            target: "audit",
            principal = %principal_str,
            action = "iam:policy.attach",
            matched_policy_id = %policy_id,
            target = %pretty_target,
            "ATTACH POLICY applied"
        );
        self.inner.audit_log.record(
            "iam/policy.attach",
            &principal_str,
            &pretty_target,
            "ok",
            crate::json::Value::Null,
        );

        self.invalidate_result_cache();
        Ok(RuntimeQueryResult::ok_message(
            query.to_string(),
            &format!("policy `{policy_id}` attached to {pretty_target}"),
            "attach_policy",
        ))
    }

    pub(crate) fn execute_detach_policy(
        &self,
        query: &str,
        policy_id: &str,
        principal: &crate::storage::query::ast::PolicyPrincipalRef,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::store::PrincipalRef;
        use crate::auth::UserId;
        use crate::storage::query::ast::PolicyPrincipalRef;

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;
        let p = match principal {
            PolicyPrincipalRef::User(u) => {
                PrincipalRef::User(UserId::from_parts(u.tenant.as_deref(), &u.username))
            }
            PolicyPrincipalRef::Group(g) => PrincipalRef::Group(g.clone()),
        };
        let pretty_target = principal_label(principal);
        let tenant = current_tenant();
        let (actor_name, actor_role) = current_auth_identity()
            .unwrap_or_else(|| ("anonymous".to_string(), crate::auth::Role::Read));
        let actor = crate::auth::UserId::from_parts(tenant.as_deref(), &actor_name);
        let eval_ctx = runtime_iam_context(actor_role, tenant.as_deref());
        let event_ctx = self.policy_mutation_control_ctx(&actor, tenant.as_deref());
        let ledger = self.inner.control_event_ledger.read();
        let control = crate::auth::store::PolicyMutationControl {
            ctx: &event_ctx,
            ledger: ledger.as_ref(),
            config: self.inner.control_event_config,
            registry: Some(self.inner.config_registry.as_ref()),
            actor: &actor,
            eval_ctx: &eval_ctx,
        };
        auth_store
            .detach_policy_with_control_events(p, policy_id, &control)
            .map_err(|e| RedDBError::Query(e.to_string()))?;

        let principal_str = actor_name;
        tracing::info!(
            target: "audit",
            principal = %principal_str,
            action = "iam:policy.detach",
            matched_policy_id = %policy_id,
            target = %pretty_target,
            "DETACH POLICY applied"
        );
        self.inner.audit_log.record(
            "iam/policy.detach",
            &principal_str,
            &pretty_target,
            "ok",
            crate::json::Value::Null,
        );

        self.invalidate_result_cache();
        Ok(RuntimeQueryResult::ok_message(
            query.to_string(),
            &format!("policy `{policy_id}` detached from {pretty_target}"),
            "detach_policy",
        ))
    }

    pub(crate) fn execute_show_policies(
        &self,
        query: &str,
        filter: Option<&crate::storage::query::ast::PolicyPrincipalRef>,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::UserId;
        use crate::storage::query::ast::PolicyPrincipalRef;
        use crate::storage::query::unified::UnifiedRecord;
        use crate::storage::schema::Value as SchemaValue;
        use std::sync::Arc;

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;

        let pols = match filter {
            None => auth_store.list_policies(),
            Some(PolicyPrincipalRef::User(u)) => {
                let id = UserId::from_parts(u.tenant.as_deref(), &u.username);
                auth_store.effective_policies(&id)
            }
            Some(PolicyPrincipalRef::Group(g)) => auth_store.group_policies(g),
        };

        let mut records = Vec::with_capacity(pols.len() + 1);

        // Header row (#712 / S5A): synthetic record at index 0 that
        // reports the active PolicyEnforcementMode and the hard-cutover
        // version, so an operator running SHOW POLICIES can see the
        // current posture without a separate command.
        let mode = auth_store.enforcement_mode();
        let mut header = UnifiedRecord::default();
        header.set_arc(
            Arc::from("id"),
            SchemaValue::text("<enforcement_mode>".to_string()),
        );
        header.set_arc(Arc::from("statements"), SchemaValue::Integer(0));
        header.set_arc(Arc::from("tenant"), SchemaValue::Null);
        let header_json = format!(
            r#"{{"enforcement_mode":"{}","policy_only_hard_version":"{}"}}"#,
            mode.as_str(),
            crate::auth::enforcement_mode::POLICY_ONLY_HARD_VERSION
        );
        header.set_arc(Arc::from("json"), SchemaValue::text(header_json));
        records.push(header);

        for p in pols.iter() {
            let mut rec = UnifiedRecord::default();
            rec.set_arc(Arc::from("id"), SchemaValue::text(p.id.clone()));
            rec.set_arc(
                Arc::from("statements"),
                SchemaValue::Integer(p.statements.len() as i64),
            );
            rec.set_arc(
                Arc::from("tenant"),
                p.tenant
                    .as_deref()
                    .map(|t| SchemaValue::text(t.to_string()))
                    .unwrap_or(SchemaValue::Null),
            );
            rec.set_arc(Arc::from("json"), SchemaValue::text(p.to_json_string()));
            records.push(rec);
        }
        let mut result = crate::storage::query::unified::UnifiedResult::empty();
        result.records = records;
        Ok(RuntimeQueryResult {
            query: query.to_string(),
            mode: crate::storage::query::modes::QueryMode::Sql,
            statement: "show_policies",
            engine: "iam-policies",
            result,
            affected_rows: 0,
            statement_type: "select",
            bookmark: None,
            notice: None,
        })
    }

    pub(crate) fn execute_show_effective_permissions(
        &self,
        query: &str,
        user: &crate::storage::query::ast::PolicyUserRef,
        resource: Option<&crate::storage::query::ast::PolicyResourceRef>,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::UserId;
        use crate::storage::query::unified::UnifiedRecord;
        use crate::storage::schema::Value as SchemaValue;
        use std::sync::Arc;

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;
        let id = UserId::from_parts(user.tenant.as_deref(), &user.username);
        let pols = auth_store.effective_policies(&id);

        // Show one row per (policy, statement) tuple, plus any
        // resource-level filter passed by the caller.
        let mut records = Vec::new();
        for p in pols.iter() {
            for (idx, st) in p.statements.iter().enumerate() {
                if let Some(_r) = resource {
                    // Naive filter: render statement targets to strings
                    // and skip if no match. Conservative default = include
                    // (the simulator handles fine-grained matching).
                }
                let mut rec = UnifiedRecord::default();
                rec.set_arc(Arc::from("policy_id"), SchemaValue::text(p.id.clone()));
                rec.set_arc(
                    Arc::from("statement_index"),
                    SchemaValue::Integer(idx as i64),
                );
                rec.set_arc(
                    Arc::from("sid"),
                    st.sid
                        .as_deref()
                        .map(|s| SchemaValue::text(s.to_string()))
                        .unwrap_or(SchemaValue::Null),
                );
                rec.set_arc(
                    Arc::from("effect"),
                    SchemaValue::text(match st.effect {
                        crate::auth::policies::Effect::Allow => "allow",
                        crate::auth::policies::Effect::Deny => "deny",
                    }),
                );
                rec.set_arc(
                    Arc::from("actions"),
                    SchemaValue::Integer(st.actions.len() as i64),
                );
                rec.set_arc(
                    Arc::from("resources"),
                    SchemaValue::Integer(st.resources.len() as i64),
                );
                records.push(rec);
            }
        }
        let mut result = crate::storage::query::unified::UnifiedResult::empty();
        result.records = records;
        Ok(RuntimeQueryResult {
            query: query.to_string(),
            mode: crate::storage::query::modes::QueryMode::Sql,
            statement: "show_effective_permissions",
            engine: "iam-policies",
            result,
            affected_rows: 0,
            statement_type: "select",
            bookmark: None,
            notice: None,
        })
    }

    pub(crate) fn execute_lint_policy(
        &self,
        query: &str,
        source: &crate::storage::query::ast::LintPolicySource,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::policy_linter::lint;
        use crate::storage::query::ast::LintPolicySource;
        use crate::storage::query::unified::UnifiedRecord;
        use crate::storage::schema::Value as SchemaValue;
        use std::sync::Arc;

        // Resolve the policy text. `JSON` source lints the literal
        // verbatim; `Id` source fetches the stored document so
        // operators can lint a policy by name without rebuilding the
        // JSON from `SHOW POLICY`.
        let policy_text = match source {
            LintPolicySource::Json(text) => text.clone(),
            LintPolicySource::Id(id) => {
                let auth_store =
                    self.inner.auth_store.read().clone().ok_or_else(|| {
                        RedDBError::Query("auth store not configured".to_string())
                    })?;
                let policy = auth_store
                    .get_policy(id)
                    .ok_or_else(|| RedDBError::Query(format!("policy `{id}` not found")))?;
                policy.to_json_string()
            }
        };
        let diagnostics = lint(&policy_text);

        let principal_str = current_auth_identity()
            .map(|(u, _)| u)
            .unwrap_or_else(|| "anonymous".into());
        tracing::info!(
            target: "audit",
            principal = %principal_str,
            action = "iam:policy.lint",
            diagnostic_count = diagnostics.len(),
            "LINT POLICY issued"
        );
        self.inner.audit_log.record(
            "iam/policy.lint",
            &principal_str,
            match source {
                LintPolicySource::Id(id) => id.as_str(),
                LintPolicySource::Json(_) => "<json>",
            },
            "ok",
            crate::json::Value::Null,
        );

        // One row per diagnostic. Column order matches the HTTP
        // surface's JSON keys so the two contracts line up.
        const COLUMNS: [&str; 5] = ["severity", "code", "message", "suggested_fix", "location"];
        let schema = Arc::new(
            COLUMNS
                .iter()
                .map(|name| Arc::<str>::from(*name))
                .collect::<Vec<_>>(),
        );
        let records: Vec<UnifiedRecord> = diagnostics
            .iter()
            .map(|d| {
                UnifiedRecord::with_schema(
                    Arc::clone(&schema),
                    vec![
                        SchemaValue::text(d.severity.as_str()),
                        SchemaValue::text(d.code.as_str()),
                        SchemaValue::text(d.message.clone()),
                        d.suggested_fix
                            .as_deref()
                            .map(SchemaValue::text)
                            .unwrap_or(SchemaValue::Null),
                        d.location
                            .as_deref()
                            .map(SchemaValue::text)
                            .unwrap_or(SchemaValue::Null),
                    ],
                )
            })
            .collect();
        let mut result = crate::storage::query::unified::UnifiedResult::with_columns(
            COLUMNS.iter().map(|c| c.to_string()).collect(),
        );
        result.records = records;
        Ok(RuntimeQueryResult {
            query: query.to_string(),
            mode: crate::storage::query::modes::QueryMode::Sql,
            statement: "lint_policy",
            engine: "iam-policies",
            result,
            affected_rows: 0,
            statement_type: "select",
            bookmark: None,
            notice: None,
        })
    }

    /// `MIGRATE POLICY MODE TO '<target>' [DRY RUN]` — flip the install
    /// from `legacy_rbac` to `policy_only` after the pre-flight delta
    /// simulator confirms no non-admin principal would lose access.
    /// Issue #714.
    pub(crate) fn execute_migrate_policy_mode(
        &self,
        query: &str,
        target: &str,
        dry_run: bool,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::enforcement_mode::PolicyEnforcementMode;
        use crate::auth::migrate_policy_mode::{
            principal_label, simulate_migration_delta, MigratePolicyDelta,
        };
        use crate::auth::policies::ResourceRef;
        use crate::storage::query::unified::UnifiedRecord;
        use crate::storage::schema::Value as SchemaValue;
        use std::sync::Arc;

        // Only `policy_only` is a meaningful destination for this
        // command — flipping back to `legacy_rbac` is supported via
        // direct config writes (it doesn't need a pre-flight). We
        // reject everything else with the same allowlist `parse` uses.
        let parsed = PolicyEnforcementMode::parse(target).ok_or_else(|| {
            RedDBError::Query(format!(
                "MIGRATE POLICY MODE: invalid target `{target}` (expected `policy_only`)"
            ))
        })?;
        if parsed != PolicyEnforcementMode::PolicyOnly {
            return Err(RedDBError::Query(format!(
                "MIGRATE POLICY MODE: target `{target}` is not supported — only `policy_only` may be migrated to via this command"
            )));
        }

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;

        // Resource enumeration: every existing collection probed as
        // `table:<name>`. This is the realistic resource surface for
        // the legacy_rbac fallback (the role floors gate per-table
        // actions). Wildcard / column-scoped resources are still
        // covered by the policy evaluator because evaluate() resolves
        // resource patterns relative to the concrete resources we
        // probe here.
        let snapshot = self.inner.db.catalog_model_snapshot();
        let resources: Vec<ResourceRef> = snapshot
            .collections
            .iter()
            .map(|c| ResourceRef::new("table", c.name.clone()))
            .collect();

        let now_ms = crate::utils::now_unix_millis() as u128;
        let deltas: Vec<MigratePolicyDelta> =
            simulate_migration_delta(auth_store.as_ref(), &resources, now_ms);

        let principal_str = current_auth_identity()
            .map(|(u, _)| u)
            .unwrap_or_else(|| "anonymous".into());

        // Audit every issuance. The outcome line differentiates
        // dry-run, refused, and applied — operators can grep for these
        // strings in the audit log.
        let outcome_str = if dry_run {
            "dry_run"
        } else if deltas.is_empty() {
            "applied"
        } else {
            "refused"
        };
        tracing::info!(
            target: "audit",
            principal = %principal_str,
            action = "iam:policy.migrate_mode",
            target = %target,
            dry_run,
            delta_count = deltas.len(),
            outcome = outcome_str,
            "MIGRATE POLICY MODE issued"
        );
        self.inner.audit_log.record(
            "iam/policy.migrate_mode",
            &principal_str,
            target,
            outcome_str,
            crate::json::Value::Null,
        );

        // Refuse the non-dry-run path when any principal would lose
        // access. The error string carries a compact summary plus the
        // delta count so operators can re-run with DRY RUN to inspect.
        if !dry_run && !deltas.is_empty() {
            let summary = deltas
                .iter()
                .take(5)
                .map(|d| {
                    format!(
                        "{}:{}/{}:{}",
                        principal_label(&d.principal),
                        d.action,
                        d.resource_kind,
                        d.resource_name
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let more = if deltas.len() > 5 {
                format!(" (and {} more)", deltas.len() - 5)
            } else {
                String::new()
            };
            return Err(RedDBError::Query(format!(
                "MIGRATE POLICY MODE refused: {n} principal/action/resource pair(s) would lose access under `policy_only`. Run `MIGRATE POLICY MODE TO '{target}' DRY RUN` to inspect. Sample: {summary}{more}",
                n = deltas.len(),
            )));
        }

        // Mutate the live enforcement mode only on the non-dry-run
        // path with an empty delta. `set_enforcement_mode` also
        // persists to vault_kv so the new mode survives restart.
        if !dry_run {
            auth_store.set_enforcement_mode(parsed);
        }

        const COLUMNS: [&str; 5] = [
            "principal",
            "role",
            "action",
            "resource_kind",
            "resource_name",
        ];
        let schema = Arc::new(
            COLUMNS
                .iter()
                .map(|name| Arc::<str>::from(*name))
                .collect::<Vec<_>>(),
        );
        let records: Vec<UnifiedRecord> = deltas
            .iter()
            .map(|d| {
                UnifiedRecord::with_schema(
                    Arc::clone(&schema),
                    vec![
                        SchemaValue::text(principal_label(&d.principal)),
                        SchemaValue::text(d.role.as_str()),
                        SchemaValue::text(d.action.clone()),
                        SchemaValue::text(d.resource_kind.clone()),
                        SchemaValue::text(d.resource_name.clone()),
                    ],
                )
            })
            .collect();
        let mut result = crate::storage::query::unified::UnifiedResult::with_columns(
            COLUMNS.iter().map(|c| c.to_string()).collect(),
        );
        result.records = records;
        Ok(RuntimeQueryResult {
            query: query.to_string(),
            mode: crate::storage::query::modes::QueryMode::Sql,
            statement: "migrate_policy_mode",
            engine: "iam-policies",
            result,
            affected_rows: 0,
            statement_type: "select",
            bookmark: None,
            notice: None,
        })
    }

    pub(crate) fn execute_simulate_policy(
        &self,
        query: &str,
        user: &crate::storage::query::ast::PolicyUserRef,
        action: &str,
        resource: &crate::storage::query::ast::PolicyResourceRef,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::auth::policies::ResourceRef;
        use crate::auth::store::SimCtx;
        use crate::auth::UserId;
        use crate::storage::query::unified::UnifiedRecord;
        use crate::storage::schema::Value as SchemaValue;
        use std::sync::Arc;

        let auth_store = self
            .inner
            .auth_store
            .read()
            .clone()
            .ok_or_else(|| RedDBError::Query("auth store not configured".to_string()))?;
        let id = UserId::from_parts(user.tenant.as_deref(), &user.username);
        let r = ResourceRef::new(resource.kind.clone(), resource.name.clone());
        let outcome = auth_store.simulate(&id, action, &r, SimCtx::default());

        let principal_str = current_auth_identity()
            .map(|(u, _)| u)
            .unwrap_or_else(|| "anonymous".into());
        let (decision_str, matched_pid, matched_sid) = decision_to_strings(&outcome.decision);
        tracing::info!(
            target: "audit",
            principal = %principal_str,
            action = "iam:policy.simulate",
            decision = %decision_str,
            matched_policy_id = ?matched_pid,
            matched_sid = ?matched_sid,
            "SIMULATE issued"
        );
        self.inner.audit_log.record(
            "iam/policy.simulate",
            &principal_str,
            &id.to_string(),
            "ok",
            crate::json::Value::Null,
        );

        let mut rec = UnifiedRecord::default();
        rec.set_arc(Arc::from("decision"), SchemaValue::text(decision_str));
        rec.set_arc(
            Arc::from("matched_policy_id"),
            matched_pid
                .map(SchemaValue::text)
                .unwrap_or(SchemaValue::Null),
        );
        rec.set_arc(
            Arc::from("matched_sid"),
            matched_sid
                .map(SchemaValue::text)
                .unwrap_or(SchemaValue::Null),
        );
        rec.set_arc(Arc::from("reason"), SchemaValue::text(outcome.reason));
        rec.set_arc(
            Arc::from("trail_len"),
            SchemaValue::Integer(outcome.trail.len() as i64),
        );
        let mut result = crate::storage::query::unified::UnifiedResult::empty();
        result.records = vec![rec];
        Ok(RuntimeQueryResult {
            query: query.to_string(),
            mode: crate::storage::query::modes::QueryMode::Sql,
            statement: "simulate_policy",
            engine: "iam-policies",
            result,
            affected_rows: 0,
            statement_type: "select",
            bookmark: None,
            notice: None,
        })
    }
}

fn parse_timestamp_to_ms(s: &str) -> Option<u128> {
    // Bare integer ms.
    if let Ok(n) = s.parse::<u128>() {
        return Some(n);
    }
    // Fallback: ISO-8601 like 2030-01-02 03:04:05 — accept the date
    // portion only (midnight UTC). Full RFC3339 parsing is a stretch
    // goal; the common case is `'2030-01-01'`.
    if let Some(date) = s.split_whitespace().next() {
        let parts: Vec<&str> = date.split('-').collect();
        if parts.len() == 3 {
            let (y, m, d) = (parts[0], parts[1], parts[2]);
            if let (Ok(y), Ok(m), Ok(d)) = (y.parse::<i64>(), m.parse::<u32>(), d.parse::<u32>()) {
                // Days since 1970-01-01 — simple Julian arithmetic
                // suitable for years 1970-2100. Good enough for test
                // fixtures; precise parsing lands when we wire chrono.
                let days_in = days_from_civil(y, m, d);
                return Some((days_in as u128) * 86_400_000u128);
            }
        }
    }
    None
}

/// Days from Unix epoch using H. Hinnant's civil-from-days algorithm.
/// Robust for the entire Gregorian range; used by `parse_timestamp_to_ms`.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) as u64 + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe as i64 - 719468
}
