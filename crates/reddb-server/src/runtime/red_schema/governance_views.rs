//! Governance / IAM / control-plane `red.*` snapshot builders.
//!
//! Extracted from the `red_schema` dispatcher (issue #1638). Serves
//! `red.registry`, `red.registry_history`, `red.managed_policies`,
//! `red.control_events`, `red.users`, `red.api_keys`,
//! `red.control_capabilities`, `red.policy.actions`, and `red.policies`.

use super::*;

pub(super) fn governance_registry_snapshot(runtime: &RedDBRuntime) -> Vec<UnifiedRecord> {
    let schema = Arc::new(
        GOVERNANCE_REGISTRY_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    runtime
        .config_registry()
        .list_active()
        .into_iter()
        .map(|entry| governance_registry_record(Arc::clone(&schema), entry))
        .collect()
}

pub(super) fn governance_registry_history_snapshot(runtime: &RedDBRuntime) -> Vec<UnifiedRecord> {
    let schema = Arc::new(
        GOVERNANCE_REGISTRY_HISTORY_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let registry = runtime.config_registry();
    let mut rows = Vec::new();
    for active in registry.list_active() {
        for record in registry.history(&active.id) {
            rows.push(governance_registry_history_record(
                Arc::clone(&schema),
                record,
            ));
        }
    }
    rows
}

pub(super) fn managed_policies_snapshot(runtime: &RedDBRuntime) -> Vec<UnifiedRecord> {
    let schema = Arc::new(
        MANAGED_POLICY_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    runtime
        .config_registry()
        .list_active()
        .into_iter()
        .filter(|entry| entry.managed && entry.resource_type == "policy")
        .map(|entry| {
            let policy_id = entry
                .required_resource
                .strip_prefix("policy:")
                .unwrap_or(&entry.id)
                .to_string();
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(policy_id),
                    Value::text(entry.id),
                    Value::UnsignedInteger(entry.version),
                    Value::text(entry.schema),
                    Value::text(entry.required_action),
                    Value::text(entry.required_resource),
                    Value::text(registry_evidence_requirement(entry.evidence_requirement)),
                    Value::text(entry.updated_by),
                    timestamp_ms_value(entry.updated_at_ms),
                ],
            )
        })
        .collect()
}

pub(super) fn control_events_snapshot(runtime: &RedDBRuntime, tenant: Option<&str>) -> Vec<UnifiedRecord> {
    let schema = Arc::new(
        CONTROL_EVENT_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let Some(manager) = runtime
        .db()
        .store()
        .get_collection(super::control_events::CONTROL_EVENTS_COLLECTION)
    else {
        return Vec::new();
    };
    manager
        .query_all(|_| true)
        .into_iter()
        .filter_map(|entity| {
            let row = entity.data.as_row()?;
            if let Some(tenant) = tenant {
                match row.get_field("scope") {
                    Some(Value::Text(scope)) if scope.as_ref() == tenant => {}
                    _ => return None,
                }
            }
            Some(UnifiedRecord::with_schema(
                Arc::clone(&schema),
                CONTROL_EVENT_COLUMNS
                    .iter()
                    .map(|column| row.get_field(column).cloned().unwrap_or(Value::Null))
                    .collect(),
            ))
        })
        .collect()
}

pub(super) fn users_snapshot(runtime: &RedDBRuntime, tenant: Option<&str>) -> Vec<UnifiedRecord> {
    let schema = Arc::new(
        USER_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let auth_store = runtime.inner.auth_store.read().clone();
    let Some(auth_store) = auth_store else {
        return Vec::new();
    };
    let tenant_filter = tenant.map(Some);
    auth_store
        .list_users_scoped(tenant_filter)
        .into_iter()
        .map(|user| {
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(user.username),
                    user.tenant_id.map(Value::text).unwrap_or(Value::Null),
                    Value::text(user.role.as_str()),
                    Value::Boolean(user.enabled),
                    timestamp_ms_value(user.created_at),
                    timestamp_ms_value(user.updated_at),
                    Value::UnsignedInteger(user.api_keys.len() as u64),
                ],
            )
        })
        .collect()
}

pub(super) fn api_keys_snapshot(runtime: &RedDBRuntime, tenant: Option<&str>) -> Vec<UnifiedRecord> {
    let schema = Arc::new(
        API_KEY_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let auth_store = runtime.inner.auth_store.read().clone();
    let Some(auth_store) = auth_store else {
        return Vec::new();
    };
    let tenant_filter = tenant.map(Some);
    let mut rows = Vec::new();
    for user in auth_store.list_users_scoped(tenant_filter) {
        let owner = match user.tenant_id.as_deref() {
            Some(tenant) => format!("{tenant}/{}", user.username),
            None => user.username.clone(),
        };
        for key in user.api_keys {
            rows.push(UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(owner.clone()),
                    user.tenant_id
                        .clone()
                        .map(Value::text)
                        .unwrap_or(Value::Null),
                    Value::text(key.name),
                    Value::text(key.role.as_str()),
                    timestamp_ms_value(key.created_at),
                    Value::text(api_key_fingerprint(&key.key)),
                ],
            ));
        }
    }
    rows
}

pub(super) fn control_capabilities_snapshot() -> Vec<UnifiedRecord> {
    use crate::auth::action_catalog::{ActionCategory, ACTIONS};

    let schema = Arc::new(
        CONTROL_CAPABILITY_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    // The historical snapshot was a hand-curated subset of the
    // allowlist — pure DML/DDL verbs (`select`, `insert`, …) and the
    // bare `*` wildcard were not advertised. Reproduce that subset by
    // filtering the catalog: only emit policy/admin/config/vault/other
    // entries plus their namespaced wildcards, never the catch-all `*`.
    ACTIONS
        .iter()
        .filter(|entry| {
            if entry.name == "*" {
                return false;
            }
            matches!(
                entry.category,
                ActionCategory::Policy
                    | ActionCategory::Admin
                    | ActionCategory::Config
                    | ActionCategory::Vault
                    | ActionCategory::Other
                    | ActionCategory::Wildcard
            )
        })
        .map(|entry| {
            let action = entry.name;
            let resource_kind = control_capability_resource_kind(action);
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(action),
                    Value::text(resource_kind),
                    Value::text(control_capability_scope(action)),
                    Value::text(format!("{action} on {resource_kind} resources")),
                ],
            )
        })
        .collect()
}

pub(super) fn policy_actions_snapshot() -> Vec<UnifiedRecord> {
    use crate::auth::action_catalog::{LifecycleState, ACTIONS};

    let schema = Arc::new(
        POLICY_ACTION_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    ACTIONS
        .iter()
        .map(|entry| {
            let (state_name, replacement, since_version) = match &entry.lifecycle_state {
                LifecycleState::Active => ("active", Value::Null, Value::Null),
                LifecycleState::Deprecated {
                    replacement,
                    since_version,
                } => (
                    "deprecated",
                    replacement.map(Value::text).unwrap_or(Value::Null),
                    Value::text(*since_version),
                ),
                LifecycleState::Removed => ("removed", Value::Null, Value::Null),
            };
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(entry.name),
                    Value::text(entry.category.as_str()),
                    Value::text(state_name),
                    replacement,
                    since_version,
                    Value::text(entry.gates_description),
                ],
            )
        })
        .collect()
}

pub(super) fn governance_registry_record(
    schema: Arc<Vec<Arc<str>>>,
    entry: crate::auth::registry::ConfigRegistryEntry,
) -> UnifiedRecord {
    UnifiedRecord::with_schema(
        schema,
        vec![
            Value::text(entry.id),
            Value::UnsignedInteger(entry.version),
            Value::text(entry.resource_type),
            Value::text(entry.schema),
            Value::text(registry_mutability(entry.mutability)),
            Value::text(registry_sensitivity(entry.sensitivity)),
            Value::Boolean(entry.managed),
            Value::text(entry.required_action),
            Value::text(entry.required_resource),
            Value::text(registry_evidence_requirement(entry.evidence_requirement)),
            Value::text(entry.updated_by),
            timestamp_ms_value(entry.updated_at_ms),
        ],
    )
}

pub(super) fn governance_registry_history_record(
    schema: Arc<Vec<Arc<str>>>,
    record: crate::auth::registry::ConfigRegistryHistoryRecord,
) -> UnifiedRecord {
    let entry = record.entry;
    UnifiedRecord::with_schema(
        schema,
        vec![
            Value::text(entry.id),
            Value::UnsignedInteger(entry.version),
            Value::text(entry.resource_type),
            Value::text(entry.schema),
            Value::text(registry_mutability(entry.mutability)),
            Value::text(registry_sensitivity(entry.sensitivity)),
            Value::Boolean(entry.managed),
            Value::text(entry.required_action),
            Value::text(entry.required_resource),
            Value::text(registry_evidence_requirement(entry.evidence_requirement)),
            Value::text(entry.updated_by),
            timestamp_ms_value(entry.updated_at_ms),
            Value::text(record.superseded_by),
            timestamp_ms_value(record.superseded_at_ms),
            Value::text(record.change_reason),
        ],
    )
}

pub(super) fn registry_mutability(value: crate::auth::registry::Mutability) -> &'static str {
    match value {
        crate::auth::registry::Mutability::Immutable => "immutable",
        crate::auth::registry::Mutability::MutableViaGovernance => "mutable_via_governance",
    }
}

pub(super) fn registry_sensitivity(value: crate::auth::registry::Sensitivity) -> &'static str {
    match value {
        crate::auth::registry::Sensitivity::Public => "public",
        crate::auth::registry::Sensitivity::Internal => "internal",
        crate::auth::registry::Sensitivity::Confidential => "confidential",
        crate::auth::registry::Sensitivity::Secret => "secret",
    }
}

pub(super) fn registry_evidence_requirement(
    value: crate::auth::registry::EvidenceRequirement,
) -> &'static str {
    match value {
        crate::auth::registry::EvidenceRequirement::None => "none",
        crate::auth::registry::EvidenceRequirement::Metadata => "metadata",
        crate::auth::registry::EvidenceRequirement::Full => "full",
    }
}

pub(super) fn api_key_fingerprint(key: &str) -> String {
    format!("blake3:{}", blake3::hash(key.as_bytes()).to_hex())
}

pub(super) fn control_capability_resource_kind(action: &str) -> &str {
    if action.starts_with("red.registry:") {
        "registry"
    } else if let Some((prefix, _)) = action.split_once(':') {
        prefix
    } else {
        "system"
    }
}

pub(super) fn control_capability_scope(action: &str) -> &'static str {
    if action.starts_with("admin:") || action.starts_with("red.registry:") {
        "platform"
    } else {
        "tenant"
    }
}

pub(super) fn policies_snapshot(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    visible_collections: Option<&HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let schema = Arc::new(
        POLICY_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let mut records = Vec::new();

    let enabled = runtime.inner.rls_enabled_tables.read().clone();
    let rls_policies = runtime.inner.rls_policies.read();
    let mut rls_entries: Vec<_> = rls_policies.iter().collect();
    rls_entries.sort_by(
        |((left_collection, left_name), _), ((right_collection, right_name), _)| {
            left_collection
                .cmp(right_collection)
                .then_with(|| left_name.cmp(right_name))
        },
    );
    for ((collection, _), policy) in rls_entries {
        if !collection_is_visible(collection, visible_collections) {
            continue;
        }
        records.push(policy_record(
            &schema,
            policy.name.clone(),
            Some(collection.clone()),
            "rls",
            "allow",
            rls_actions(policy.action),
            rls_principals(policy.role.as_deref()),
            Value::text(render_filter_for_catalog(&policy.using)),
            Value::Boolean(enabled.contains(collection)),
        ));
    }
    drop(rls_policies);

    let auth_store = runtime.inner.auth_store.read().clone();
    if let Some(auth_store) = auth_store {
        for policy in auth_store.list_policies() {
            if !iam_policy_visible_to_tenant(&policy, tenant) {
                continue;
            }
            for (statement_index, statement) in policy.statements.iter().enumerate() {
                let collection_names = iam_statement_collections(statement);
                if collection_names.is_empty() {
                    records.push(iam_policy_record(
                        &schema,
                        &policy,
                        statement_index,
                        statement,
                        None,
                    ));
                    continue;
                }
                for collection in collection_names {
                    if !collection_is_visible(&collection, visible_collections) {
                        continue;
                    }
                    records.push(iam_policy_record(
                        &schema,
                        &policy,
                        statement_index,
                        statement,
                        Some(collection),
                    ));
                }
            }
        }
    }

    records
}

pub(super) fn iam_policy_visible_to_tenant(policy: &Policy, tenant: Option<&str>) -> bool {
    match (tenant, policy.tenant.as_deref()) {
        (None, _) => true,
        (Some(_), None) => true,
        (Some(active), Some(policy_tenant)) => active == policy_tenant,
    }
}

pub(super) fn policy_record(
    schema: &Arc<Vec<Arc<str>>>,
    name: String,
    collection: Option<String>,
    kind: &'static str,
    effect: &'static str,
    actions: Vec<String>,
    principals: Vec<String>,
    predicate: Value,
    enabled: Value,
) -> UnifiedRecord {
    UnifiedRecord::with_schema(
        Arc::clone(schema),
        vec![
            Value::text(name),
            collection.map(Value::text).unwrap_or(Value::Null),
            Value::text(kind),
            Value::text(effect),
            Value::Array(actions.into_iter().map(Value::text).collect()),
            Value::Array(principals.into_iter().map(Value::text).collect()),
            predicate,
            enabled,
        ],
    )
}

pub(super) fn iam_policy_record(
    schema: &Arc<Vec<Arc<str>>>,
    policy: &Policy,
    statement_index: usize,
    statement: &Statement,
    collection: Option<String>,
) -> UnifiedRecord {
    let name = statement
        .sid
        .as_ref()
        .map(|sid| format!("{}:{sid}", policy.id))
        .unwrap_or_else(|| {
            if policy.statements.len() > 1 {
                format!("{}#{}", policy.id, statement_index)
            } else {
                policy.id.clone()
            }
        });
    policy_record(
        schema,
        name,
        collection,
        "iam",
        iam_effect(statement.effect),
        iam_actions(&statement.actions),
        Vec::new(),
        Value::Null,
        Value::Boolean(true),
    )
}

pub(super) fn rls_actions(action: Option<PolicyAction>) -> Vec<String> {
    match action {
        Some(PolicyAction::Select) => vec!["select".to_string()],
        Some(PolicyAction::Insert) => vec!["insert".to_string()],
        Some(PolicyAction::Update) => vec!["update".to_string()],
        Some(PolicyAction::Delete) => vec!["delete".to_string()],
        None => vec!["*".to_string()],
    }
}

pub(super) fn rls_principals(role: Option<&str>) -> Vec<String> {
    role.map(|role| vec![role.to_string()])
        .unwrap_or_else(|| vec!["*".to_string()])
}

pub(super) fn iam_effect(effect: Effect) -> &'static str {
    match effect {
        Effect::Allow => "allow",
        Effect::Deny => "deny",
    }
}

pub(super) fn iam_actions(actions: &[ActionPattern]) -> Vec<String> {
    actions.iter().map(render_action_pattern).collect()
}

pub(super) fn render_action_pattern(action: &ActionPattern) -> String {
    match action {
        ActionPattern::Exact(value) => value.clone(),
        ActionPattern::Wildcard => "*".to_string(),
        ActionPattern::Prefix(prefix) => format!("{prefix}:*"),
    }
}

pub(super) fn iam_statement_collections(statement: &Statement) -> Vec<String> {
    let mut out = Vec::new();
    for resource in &statement.resources {
        match resource {
            ResourcePattern::Exact { kind, name }
                if kind.eq_ignore_ascii_case("table")
                    || kind.eq_ignore_ascii_case("collection") =>
            {
                out.push(name.clone());
            }
            _ => {}
        }
    }
    out.sort();
    out.dedup();
    out
}

pub(super) fn render_filter_for_catalog(filter: &Filter) -> String {
    match filter {
        Filter::Compare { field, op, value } => {
            format!(
                "{} {} {}",
                render_field_for_catalog(field),
                op,
                crate::storage::query::renderer::render_value_sql(value)
            )
        }
        Filter::CompareFields { left, op, right } => {
            format!(
                "{} {} {}",
                render_field_for_catalog(left),
                op,
                render_field_for_catalog(right)
            )
        }
        Filter::CompareExpr { lhs, op, rhs } => {
            format!(
                "{} {} {}",
                render_expr_for_catalog(lhs),
                op,
                render_expr_for_catalog(rhs)
            )
        }
        Filter::And(left, right) => format!(
            "({}) AND ({})",
            render_filter_for_catalog(left),
            render_filter_for_catalog(right)
        ),
        Filter::Or(left, right) => format!(
            "({}) OR ({})",
            render_filter_for_catalog(left),
            render_filter_for_catalog(right)
        ),
        Filter::Not(inner) => format!("NOT ({})", render_filter_for_catalog(inner)),
        Filter::IsNull(field) => format!("{} IS NULL", render_field_for_catalog(field)),
        Filter::IsNotNull(field) => format!("{} IS NOT NULL", render_field_for_catalog(field)),
        Filter::In { field, values } => format!(
            "{} IN ({})",
            render_field_for_catalog(field),
            values
                .iter()
                .map(crate::storage::query::renderer::render_value_sql)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Filter::Between { field, low, high } => format!(
            "{} BETWEEN {} AND {}",
            render_field_for_catalog(field),
            crate::storage::query::renderer::render_value_sql(low),
            crate::storage::query::renderer::render_value_sql(high)
        ),
        Filter::Like { field, pattern } => {
            format!("{} LIKE '{}'", render_field_for_catalog(field), pattern)
        }
        Filter::StartsWith { field, prefix } => {
            format!(
                "{} STARTS WITH '{}'",
                render_field_for_catalog(field),
                prefix
            )
        }
        Filter::EndsWith { field, suffix } => {
            format!("{} ENDS WITH '{}'", render_field_for_catalog(field), suffix)
        }
        Filter::Contains { field, substring } => {
            format!(
                "{} CONTAINS '{}'",
                render_field_for_catalog(field),
                substring
            )
        }
    }
}

pub(super) fn render_expr_for_catalog(expr: &Expr) -> String {
    match expr {
        Expr::Literal { value, .. } => crate::storage::query::renderer::render_value_sql(value),
        Expr::Column { field, .. } => render_field_for_catalog(field),
        Expr::Parameter { index, .. } => format!("${index}"),
        Expr::BinaryOp { op, lhs, rhs, .. } => format!(
            "{} {:?} {}",
            render_expr_for_catalog(lhs),
            op,
            render_expr_for_catalog(rhs)
        ),
        Expr::UnaryOp { op, operand, .. } => match op {
            UnaryOp::Not => format!("NOT {}", render_expr_for_catalog(operand)),
            UnaryOp::Neg => format!("-{}", render_expr_for_catalog(operand)),
        },
        Expr::Cast { inner, target, .. } => {
            format!("CAST({} AS {:?})", render_expr_for_catalog(inner), target)
        }
        Expr::FunctionCall { name, args, .. } => format!(
            "{}({})",
            name,
            args.iter()
                .map(render_expr_for_catalog)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::Case { .. } => format!("{expr:?}"),
        Expr::IsNull {
            operand, negated, ..
        } => format!(
            "{} IS {}NULL",
            render_expr_for_catalog(operand),
            if *negated { "NOT " } else { "" }
        ),
        Expr::InList {
            target,
            values,
            negated,
            ..
        } => format!(
            "{} {}IN ({})",
            render_expr_for_catalog(target),
            if *negated { "NOT " } else { "" },
            values
                .iter()
                .map(render_expr_for_catalog)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::Between {
            target,
            low,
            high,
            negated,
            ..
        } => format!(
            "{} {}BETWEEN {} AND {}",
            render_expr_for_catalog(target),
            if *negated { "NOT " } else { "" },
            render_expr_for_catalog(low),
            render_expr_for_catalog(high)
        ),
        Expr::Subquery { .. } => "(SELECT ...)".to_string(),
        Expr::WindowFunctionCall { name, args, .. } => {
            let args = args
                .iter()
                .map(render_expr_for_catalog)
                .collect::<Vec<_>>()
                .join(", ");
            format!("{name}({args}) OVER (...)")
        }
    }
}

pub(super) fn render_field_for_catalog(field: &FieldRef) -> String {
    match field {
        FieldRef::TableColumn { table, column } if table.is_empty() => column.clone(),
        FieldRef::TableColumn { table, column } => format!("{table}.{column}"),
        FieldRef::NodeProperty { alias, property } => format!("{alias}.{property}"),
        FieldRef::EdgeProperty { alias, property } => format!("{alias}.{property}"),
        FieldRef::NodeId { alias } => format!("{alias}.id"),
    }
}
