//! Runtime query-audit + control-event emission.
//!
//! Extracted verbatim from `impl_core.rs` (impl_core slice 7/10, issue #1628).
//! Houses the query-audit planning helpers and the control-event ledger
//! emission family:
//!
//! - **Free helpers** — `query_audit_plan`, `collect_query_audit_collections`,
//!   `push_query_audit_collection`, `query_control_event_specs`,
//!   `control_event_outcome_for_error`.
//! - **Methods** — `emit_control_event`, `policy_mutation_control_ctx`,
//!   `emit_query_audit`.
use super::execution_context::{current_auth_identity, current_connection_id, current_tenant};
use super::*;
use crate::auth::UserId;

#[derive(Clone)]
pub(crate) struct QueryControlEventSpec {
    pub(crate) kind: crate::runtime::control_events::EventKind,
    pub(crate) action: &'static str,
    pub(crate) resource: Option<String>,
    pub(crate) fields: Vec<(String, crate::runtime::control_events::Sensitivity)>,
}

#[derive(Clone)]
pub(crate) struct QueryAuditPlan {
    statement_kind: &'static str,
    collections: Vec<String>,
}

pub(crate) fn query_audit_plan(expr: &QueryExpr) -> Option<QueryAuditPlan> {
    let mut collections = Vec::new();
    let statement_kind = match expr {
        QueryExpr::Table(table) => {
            push_query_audit_collection(&mut collections, &table.table);
            "select"
        }
        QueryExpr::Join(join) => {
            collect_query_audit_collections(&join.left, &mut collections);
            collect_query_audit_collections(&join.right, &mut collections);
            "select"
        }
        QueryExpr::Insert(insert) => {
            push_query_audit_collection(&mut collections, &insert.table);
            "insert"
        }
        QueryExpr::Update(update) => {
            push_query_audit_collection(&mut collections, &update.table);
            "update"
        }
        QueryExpr::Delete(delete) => {
            push_query_audit_collection(&mut collections, &delete.table);
            "delete"
        }
        _ => return None,
    };
    if collections.is_empty() {
        None
    } else {
        Some(QueryAuditPlan {
            statement_kind,
            collections,
        })
    }
}

fn collect_query_audit_collections(expr: &QueryExpr, collections: &mut Vec<String>) {
    match expr {
        QueryExpr::Table(table) => push_query_audit_collection(collections, &table.table),
        QueryExpr::Join(join) => {
            collect_query_audit_collections(&join.left, collections);
            collect_query_audit_collections(&join.right, collections);
        }
        _ => {}
    }
}

fn push_query_audit_collection(collections: &mut Vec<String>, name: &str) {
    if name == "red" || name.starts_with("red.") || name.starts_with("__red_schema_") {
        return;
    }
    if !collections.iter().any(|existing| existing == name) {
        collections.push(name.to_string());
    }
}

pub(crate) fn query_control_event_specs(expr: &QueryExpr) -> Vec<QueryControlEventSpec> {
    use crate::runtime::control_events::{EventKind, Sensitivity};

    let mut specs = Vec::new();
    let mut schema = |action: &'static str, resource: Option<String>| {
        specs.push(QueryControlEventSpec {
            kind: EventKind::SchemaDdl,
            action,
            resource,
            fields: Vec::new(),
        });
    };
    match expr {
        QueryExpr::CreateTable(q) => {
            schema("create_table", Some(format!("table:{}", q.name)));
            if let Some(column) = &q.tenant_by {
                specs.push(QueryControlEventSpec {
                    kind: EventKind::TenantGovernance,
                    action: "create_table_tenant_by",
                    resource: Some(format!("table:{}", q.name)),
                    fields: vec![("tenant_column".to_string(), Sensitivity::raw(column))],
                });
            }
        }
        QueryExpr::CreateCollection(q) => {
            schema("create_collection", Some(format!("collection:{}", q.name)));
        }
        QueryExpr::CreateVector(q) => schema("create_vector", Some(format!("vector:{}", q.name))),
        QueryExpr::DropTable(q) => schema("drop_table", Some(format!("table:{}", q.name))),
        QueryExpr::DropGraph(q) => schema("drop_graph", Some(format!("graph:{}", q.name))),
        QueryExpr::DropVector(q) => schema("drop_vector", Some(format!("vector:{}", q.name))),
        QueryExpr::DropDocument(q) => {
            schema("drop_document", Some(format!("document:{}", q.name)));
        }
        QueryExpr::DropKv(q) => schema("drop_kv", Some(format!("kv:{}", q.name))),
        QueryExpr::DropCollection(q) => {
            schema("drop_collection", Some(format!("collection:{}", q.name)));
        }
        QueryExpr::Truncate(q) => schema("truncate", Some(format!("collection:{}", q.name))),
        QueryExpr::AlterTable(q) => {
            schema("alter_table", Some(format!("table:{}", q.name)));
            for op in &q.operations {
                match op {
                    crate::storage::query::ast::AlterOperation::EnableRowLevelSecurity => {
                        specs.push(QueryControlEventSpec {
                            kind: EventKind::RlsGovernance,
                            action: "enable_rls",
                            resource: Some(format!("table:{}", q.name)),
                            fields: Vec::new(),
                        });
                    }
                    crate::storage::query::ast::AlterOperation::DisableRowLevelSecurity => {
                        specs.push(QueryControlEventSpec {
                            kind: EventKind::RlsGovernance,
                            action: "disable_rls",
                            resource: Some(format!("table:{}", q.name)),
                            fields: Vec::new(),
                        });
                    }
                    crate::storage::query::ast::AlterOperation::EnableTenancy { column } => {
                        specs.push(QueryControlEventSpec {
                            kind: EventKind::TenantGovernance,
                            action: "enable_tenancy",
                            resource: Some(format!("table:{}", q.name)),
                            fields: vec![("tenant_column".to_string(), Sensitivity::raw(column))],
                        });
                    }
                    crate::storage::query::ast::AlterOperation::DisableTenancy => {
                        specs.push(QueryControlEventSpec {
                            kind: EventKind::TenantGovernance,
                            action: "disable_tenancy",
                            resource: Some(format!("table:{}", q.name)),
                            fields: Vec::new(),
                        });
                    }
                    _ => {}
                }
            }
        }
        QueryExpr::CreateVcsRef(q) => {
            let kind = match q.kind {
                crate::storage::query::ast::VcsRefKind::Branch => "branch",
                crate::storage::query::ast::VcsRefKind::Tag => "tag",
            };
            schema("create_vcs_ref", Some(format!("{kind}:{}", q.name)));
        }
        QueryExpr::DropVcsRef(q) => {
            let kind = match q.kind {
                crate::storage::query::ast::VcsRefKind::Branch => "branch",
                crate::storage::query::ast::VcsRefKind::Tag => "tag",
            };
            schema("drop_vcs_ref", Some(format!("{kind}:{}", q.name)));
        }
        QueryExpr::CreateIndex(q) => {
            schema(
                "create_index",
                Some(format!("index:{}:{}", q.table, q.name)),
            );
        }
        QueryExpr::DropIndex(q) => {
            schema("drop_index", Some(format!("index:{}:{}", q.table, q.name)));
        }
        QueryExpr::CreateTimeSeries(q) => {
            schema("create_timeseries", Some(format!("timeseries:{}", q.name)));
        }
        QueryExpr::CreateMetric(q) => {
            schema("create_metric", Some(format!("metric:{}", q.path)));
        }
        QueryExpr::AlterMetric(q) => {
            schema("alter_metric", Some(format!("metric:{}", q.path)));
        }
        QueryExpr::CreateSlo(q) => {
            schema("create_slo", Some(format!("slo:{}", q.path)));
        }
        QueryExpr::DropTimeSeries(q) => {
            schema("drop_timeseries", Some(format!("timeseries:{}", q.name)));
        }
        QueryExpr::CreateQueue(q) => schema("create_queue", Some(format!("queue:{}", q.name))),
        QueryExpr::AlterQueue(q) => schema("alter_queue", Some(format!("queue:{}", q.name))),
        QueryExpr::DropQueue(q) => schema("drop_queue", Some(format!("queue:{}", q.name))),
        QueryExpr::CreateTree(q) => {
            schema(
                "create_tree",
                Some(format!("tree:{}:{}", q.collection, q.name)),
            );
        }
        QueryExpr::DropTree(q) => {
            schema(
                "drop_tree",
                Some(format!("tree:{}:{}", q.collection, q.name)),
            );
        }
        QueryExpr::CreateSchema(q) => schema("create_schema", Some(format!("schema:{}", q.name))),
        QueryExpr::DropSchema(q) => schema("drop_schema", Some(format!("schema:{}", q.name))),
        QueryExpr::CreateSequence(q) => {
            schema("create_sequence", Some(format!("sequence:{}", q.name)));
        }
        QueryExpr::DropSequence(q) => schema("drop_sequence", Some(format!("sequence:{}", q.name))),
        QueryExpr::CreateView(q) => schema("create_view", Some(format!("view:{}", q.name))),
        QueryExpr::DropView(q) => schema("drop_view", Some(format!("view:{}", q.name))),
        QueryExpr::RefreshMaterializedView(q) => {
            schema(
                "refresh_materialized_view",
                Some(format!("view:{}", q.name)),
            );
        }
        QueryExpr::CreatePolicy(q) => {
            specs.push(QueryControlEventSpec {
                kind: EventKind::RlsGovernance,
                action: "create_policy",
                resource: Some(format!("table:{}:policy:{}", q.table, q.name)),
                fields: vec![(
                    "target_kind".to_string(),
                    Sensitivity::raw(q.target_kind.as_ident()),
                )],
            });
        }
        QueryExpr::DropPolicy(q) => {
            specs.push(QueryControlEventSpec {
                kind: EventKind::RlsGovernance,
                action: "drop_policy",
                resource: Some(format!("table:{}:policy:{}", q.table, q.name)),
                fields: Vec::new(),
            });
        }
        QueryExpr::SetTenant(value) => {
            let mut fields = Vec::new();
            if let Some(value) = value {
                fields.push(("tenant".to_string(), Sensitivity::raw(value)));
            }
            specs.push(QueryControlEventSpec {
                kind: EventKind::TenantGovernance,
                action: "set_tenant",
                resource: Some("tenant:session".to_string()),
                fields,
            });
        }
        QueryExpr::SetConfig { key, .. } => {
            specs.push(QueryControlEventSpec {
                kind: EventKind::ConfigWrite,
                action: "config:write",
                resource: Some(format!("config:{key}")),
                fields: vec![("key".to_string(), Sensitivity::raw(key))],
            });
        }
        QueryExpr::ConfigCommand(cmd) => match cmd {
            crate::storage::query::ast::ConfigCommand::Put {
                collection, key, ..
            }
            | crate::storage::query::ast::ConfigCommand::Rotate {
                collection, key, ..
            } => {
                let target = format!("{collection}/{key}");
                specs.push(QueryControlEventSpec {
                    kind: EventKind::ConfigWrite,
                    action: "config:write",
                    resource: Some(format!("config:{target}")),
                    fields: vec![
                        ("collection".to_string(), Sensitivity::raw(collection)),
                        ("key".to_string(), Sensitivity::raw(key)),
                    ],
                });
            }
            crate::storage::query::ast::ConfigCommand::Delete { collection, key } => {
                let target = format!("{collection}/{key}");
                specs.push(QueryControlEventSpec {
                    kind: EventKind::ConfigDelete,
                    action: "config:write",
                    resource: Some(format!("config:{target}")),
                    fields: vec![
                        ("collection".to_string(), Sensitivity::raw(collection)),
                        ("key".to_string(), Sensitivity::raw(key)),
                    ],
                });
            }
            _ => {}
        },
        QueryExpr::AlterUser(stmt) => {
            let disables = stmt.attributes.iter().any(|attr| {
                matches!(
                    attr,
                    crate::storage::query::ast::AlterUserAttribute::Disable
                )
            });
            specs.push(QueryControlEventSpec {
                kind: if disables {
                    EventKind::UserDisable
                } else {
                    EventKind::UserUpdate
                },
                action: "alter_user",
                resource: Some(format!("user:{}", stmt.username)),
                fields: Vec::new(),
            });
        }
        QueryExpr::CreateUser(stmt) => {
            specs.push(QueryControlEventSpec {
                kind: EventKind::UserCreate,
                action: "create_user",
                resource: Some(format!("user:{}", stmt.username)),
                fields: Vec::new(),
            });
        }
        _ => {}
    }
    specs
}

pub(crate) fn control_event_outcome_for_error(
    err: &RedDBError,
) -> crate::runtime::control_events::Outcome {
    match err {
        RedDBError::ReadOnly(_) => crate::runtime::control_events::Outcome::Denied,
        RedDBError::Query(msg)
            if msg.contains("permission denied")
                || msg.contains("cannot issue")
                || msg.contains("lacks") =>
        {
            crate::runtime::control_events::Outcome::Denied
        }
        _ => crate::runtime::control_events::Outcome::Error,
    }
}

impl RedDBRuntime {
    pub(crate) fn emit_control_event(
        &self,
        kind: crate::runtime::control_events::EventKind,
        outcome: crate::runtime::control_events::Outcome,
        action: &'static str,
        resource: Option<String>,
        reason: Option<String>,
        extra_fields: Vec<(String, crate::runtime::control_events::Sensitivity)>,
    ) -> RedDBResult<()> {
        use crate::runtime::control_events::{
            ActorRef, ControlEvent, ControlEventCtx, ControlEventLedger, Sensitivity,
        };

        let tenant = current_tenant();
        let principal = current_auth_identity();
        let actor_user = principal
            .as_ref()
            .map(|(principal, _)| UserId::from_parts(tenant.as_deref(), principal));
        let actor = actor_user
            .as_ref()
            .map(ActorRef::User)
            .unwrap_or(ActorRef::Anonymous);
        let ctx = ControlEventCtx {
            actor,
            scope: tenant
                .as_ref()
                .map(|scope| std::borrow::Cow::Borrowed(scope.as_str())),
            request_id: Some(std::borrow::Cow::Owned(format!(
                "conn-{}",
                current_connection_id()
            ))),
            trace_id: None,
        };
        let mut fields = std::collections::HashMap::new();
        fields.insert(
            "connection_id".to_string(),
            Sensitivity::raw(current_connection_id().to_string()),
        );
        if let Some((_, role)) = principal {
            fields.insert("actor_role".to_string(), Sensitivity::raw(role.as_str()));
        }
        for (key, value) in extra_fields {
            fields.insert(key, value);
        }
        let event = ControlEvent {
            kind,
            outcome,
            action: std::borrow::Cow::Borrowed(action),
            resource,
            reason,
            matched_policy_id: None,
            fields,
        };
        let ledger = self.inner.control_event_ledger.read();
        match ledger.emit(&ctx, event) {
            Ok(_) => Ok(()),
            Err(err) if self.inner.control_event_config.require_persistence() => {
                Err(RedDBError::Internal(err.to_string()))
            }
            Err(_) => Ok(()),
        }
    }

    pub(crate) fn policy_mutation_control_ctx<'a>(
        &self,
        actor: &'a crate::auth::UserId,
        tenant: Option<&'a str>,
    ) -> crate::runtime::control_events::ControlEventCtx<'a> {
        crate::runtime::control_events::ControlEventCtx {
            actor: crate::runtime::control_events::ActorRef::User(actor),
            scope: tenant.map(std::borrow::Cow::Borrowed),
            request_id: Some(std::borrow::Cow::Owned(format!(
                "conn-{}",
                current_connection_id()
            ))),
            trace_id: None,
        }
    }

    pub(crate) fn emit_query_audit(
        &self,
        query: &str,
        plan: &QueryAuditPlan,
        duration_ms: u64,
        result: &RuntimeQueryResult,
    ) {
        if !self.inner.query_audit.has_rules() {
            return;
        }
        let actor = current_auth_identity().map(|(principal, _)| principal);
        let tenant = current_tenant();
        let row_count = if result.statement_type == "select" {
            result.result.records.len() as u64
        } else {
            result.affected_rows
        };
        self.inner
            .query_audit
            .emit(crate::runtime::query_audit::QueryAuditEvent {
                actor,
                tenant,
                statement_kind: plan.statement_kind,
                touched_collections: plan.collections.clone(),
                duration_ms,
                row_count,
                request_id: Some(crate::crypto::uuid::Uuid::new_v7().to_string()),
                query_hash: Some(blake3::hash(query.as_bytes()).to_hex().to_string()),
            });
    }
}
