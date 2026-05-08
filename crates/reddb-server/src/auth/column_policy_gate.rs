//! Column-level IAM policy gate.
//!
//! This module is the narrow enforcement interface for column resources
//! (`column:[schema.]table.column`). It deliberately sits on top of the
//! existing IAM policy kernel instead of adding a second policy language.
//!
//! Semantics:
//! - table access is checked first and is required;
//! - explicit column `deny` rejects that projected column;
//! - explicit column `allow` permits that projected column;
//! - column `default-deny` inherits an allowed table decision so existing
//!   table-only policies keep working until callers opt into precise
//!   projection checks.

use std::fmt;

use super::policies::{self as iam_policies, Decision, EvalContext, Policy, ResourceRef};

/// One resolved table column requested by a query path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnRef {
    pub schema: Option<String>,
    pub table: String,
    pub column: String,
}

impl ColumnRef {
    pub fn new(table: impl Into<String>, column: impl Into<String>) -> Self {
        Self {
            schema: None,
            table: table.into(),
            column: column.into(),
        }
    }

    pub fn with_schema(
        schema: impl Into<String>,
        table: impl Into<String>,
        column: impl Into<String>,
    ) -> Self {
        Self {
            schema: Some(schema.into()),
            table: table.into(),
            column: column.into(),
        }
    }

    /// Parse the documented column resource name shape:
    /// `[schema.]table.column`. JSON paths are intentionally not accepted.
    pub fn parse_resource_name(name: &str) -> Result<Self, ColumnPolicyError> {
        let parts: Vec<&str> = name.split('.').collect();
        match parts.as_slice() {
            [table, column] if valid_part(table) && valid_part(column) => {
                Ok(Self::new(*table, *column))
            }
            [schema, table, column]
                if valid_part(schema) && valid_part(table) && valid_part(column) =>
            {
                Ok(Self::with_schema(*schema, *table, *column))
            }
            _ => Err(ColumnPolicyError::InvalidColumnResource(name.to_string())),
        }
    }

    pub fn table_resource_name(&self) -> String {
        match &self.schema {
            Some(schema) => format!("{schema}.{}", self.table),
            None => self.table.clone(),
        }
    }

    pub fn column_resource_name(&self) -> String {
        format!("{}.{}", self.table_resource_name(), self.column)
    }
}

/// A set of resolved columns from one table-like source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnAccessRequest {
    pub action: String,
    pub schema: Option<String>,
    pub table: String,
    pub columns: Vec<String>,
}

impl ColumnAccessRequest {
    pub fn select(
        table: impl Into<String>,
        columns: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            action: "select".to_string(),
            schema: None,
            table: table.into(),
            columns: columns.into_iter().map(Into::into).collect(),
        }
    }

    pub fn with_schema(mut self, schema: impl Into<String>) -> Self {
        self.schema = Some(schema.into());
        self
    }

    fn table_resource_name(&self) -> String {
        match &self.schema {
            Some(schema) => format!("{schema}.{}", self.table),
            None => self.table.clone(),
        }
    }

    fn column_ref(&self, column: &str) -> ColumnRef {
        ColumnRef {
            schema: self.schema.clone(),
            table: self.table.clone(),
            column: column.to_string(),
        }
    }
}

/// Per-column decision after table inheritance is applied.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDecision {
    pub column: String,
    pub resource: ResourceRef,
    pub raw_decision: Decision,
    pub effective: ColumnDecisionEffect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnDecisionEffect {
    Allowed,
    Denied,
    InheritedTableAllow,
}

/// Full gate result for one projected table source.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnPolicyOutcome {
    pub table_resource: ResourceRef,
    pub table_decision: Decision,
    pub columns: Vec<ColumnDecision>,
}

impl ColumnPolicyOutcome {
    pub fn allowed(&self) -> bool {
        table_decision_allows(&self.table_decision)
            && self
                .columns
                .iter()
                .all(|c| c.effective != ColumnDecisionEffect::Denied)
    }

    pub fn first_denied_column(&self) -> Option<&ColumnDecision> {
        self.columns
            .iter()
            .find(|c| c.effective == ColumnDecisionEffect::Denied)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnPolicyError {
    InvalidColumnResource(String),
}

impl fmt::Display for ColumnPolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidColumnResource(name) => write!(
                f,
                "invalid column resource `{name}`; expected [schema.]table.column"
            ),
        }
    }
}

impl std::error::Error for ColumnPolicyError {}

/// Thin evaluator wrapper over effective IAM policies.
pub struct ColumnPolicyGate<'a> {
    policies: &'a [&'a Policy],
}

impl<'a> ColumnPolicyGate<'a> {
    pub fn new(policies: &'a [&'a Policy]) -> Self {
        Self { policies }
    }

    pub fn evaluate(
        &self,
        request: &ColumnAccessRequest,
        ctx: &EvalContext,
    ) -> ColumnPolicyOutcome {
        let mut table_resource = ResourceRef::new("table", request.table_resource_name());
        if let Some(tenant) = ctx.current_tenant.as_deref() {
            table_resource = table_resource.with_tenant(tenant.to_string());
        }

        let table_decision =
            iam_policies::evaluate(self.policies, &request.action, &table_resource, ctx);

        let columns = request
            .columns
            .iter()
            .map(|column| {
                let column_ref = request.column_ref(column);
                let mut resource = ResourceRef::new("column", column_ref.column_resource_name());
                if let Some(tenant) = ctx.current_tenant.as_deref() {
                    resource = resource.with_tenant(tenant.to_string());
                }
                let raw_decision =
                    iam_policies::evaluate(self.policies, &request.action, &resource, ctx);
                let effective = effective_column_decision(&table_decision, &raw_decision);
                ColumnDecision {
                    column: column.clone(),
                    resource,
                    raw_decision,
                    effective,
                }
            })
            .collect();

        ColumnPolicyOutcome {
            table_resource,
            table_decision,
            columns,
        }
    }
}

fn effective_column_decision(
    table_decision: &Decision,
    column_decision: &Decision,
) -> ColumnDecisionEffect {
    match column_decision {
        Decision::Deny { .. } => ColumnDecisionEffect::Denied,
        Decision::Allow { .. } | Decision::AdminBypass => ColumnDecisionEffect::Allowed,
        Decision::DefaultDeny if table_decision_allows(table_decision) => {
            ColumnDecisionEffect::InheritedTableAllow
        }
        Decision::DefaultDeny => ColumnDecisionEffect::Denied,
    }
}

fn table_decision_allows(decision: &Decision) -> bool {
    matches!(decision, Decision::Allow { .. } | Decision::AdminBypass)
}

fn valid_part(s: &str) -> bool {
    !s.is_empty() && !s.starts_with("tenant/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::policies::{compile_action, Effect, ResourcePattern, Statement};

    fn policy(id: &str, effect: Effect, actions: &[&str], resources: &[&str]) -> Policy {
        let statements = vec![Statement {
            sid: Some(id.to_string()),
            effect,
            actions: actions.iter().map(|a| compile_action(a)).collect(),
            resources: resources
                .iter()
                .map(|r| {
                    if *r == "*" {
                        ResourcePattern::Wildcard
                    } else if r.contains('*') {
                        ResourcePattern::Glob((*r).to_string())
                    } else {
                        let (kind, name) = r.split_once(':').unwrap();
                        ResourcePattern::Exact {
                            kind: kind.to_string(),
                            name: name.to_string(),
                        }
                    }
                })
                .collect(),
            condition: None,
        }];
        Policy {
            id: id.to_string(),
            version: 1,
            statements,
            tenant: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn default_column_decision_inherits_table_allow() {
        let allow_table = policy("allow_table", Effect::Allow, &["select"], &["table:users"]);
        let policies = [&allow_table];
        let gate = ColumnPolicyGate::new(&policies);
        let request = ColumnAccessRequest::select("users", ["id", "name"]);

        let outcome = gate.evaluate(&request, &EvalContext::default());

        assert!(outcome.allowed());
        assert_eq!(
            outcome.columns[0].effective,
            ColumnDecisionEffect::InheritedTableAllow
        );
    }

    #[test]
    fn explicit_column_deny_overrides_table_allow() {
        let allow_table = policy("allow_table", Effect::Allow, &["select"], &["table:users"]);
        let deny_email = policy(
            "deny_email",
            Effect::Deny,
            &["select"],
            &["column:users.email"],
        );
        let policies = [&allow_table, &deny_email];
        let gate = ColumnPolicyGate::new(&policies);
        let request = ColumnAccessRequest::select("users", ["id", "email"]);

        let outcome = gate.evaluate(&request, &EvalContext::default());

        assert!(!outcome.allowed());
        let denied = outcome.first_denied_column().unwrap();
        assert_eq!(denied.column, "email");
        assert_eq!(denied.effective, ColumnDecisionEffect::Denied);
    }

    #[test]
    fn column_allow_does_not_bypass_missing_table_allow() {
        let allow_column = policy(
            "allow_email",
            Effect::Allow,
            &["select"],
            &["column:users.email"],
        );
        let policies = [&allow_column];
        let gate = ColumnPolicyGate::new(&policies);
        let request = ColumnAccessRequest::select("users", ["email"]);

        let outcome = gate.evaluate(&request, &EvalContext::default());

        assert!(!outcome.allowed());
        assert!(matches!(outcome.table_decision, Decision::DefaultDeny));
        assert_eq!(outcome.columns[0].effective, ColumnDecisionEffect::Allowed);
    }

    #[test]
    fn tenant_context_uses_existing_policy_resource_matching() {
        let allow_table = policy(
            "allow_tenant_table",
            Effect::Allow,
            &["select"],
            &["table:orders"],
        );
        let deny_email = policy(
            "deny_tenant_email",
            Effect::Deny,
            &["select"],
            &["column:orders.email"],
        );
        let policies = [&allow_table, &deny_email];
        let gate = ColumnPolicyGate::new(&policies);
        let ctx = EvalContext {
            current_tenant: Some("acme".to_string()),
            ..EvalContext::default()
        };

        let outcome = gate.evaluate(&ColumnAccessRequest::select("orders", ["email"]), &ctx);

        assert!(!outcome.allowed());
        assert_eq!(outcome.table_resource.tenant.as_deref(), Some("acme"));
        assert_eq!(outcome.columns[0].resource.name, "orders.email".to_string());
    }

    #[test]
    fn parses_only_documented_column_resource_shape() {
        assert_eq!(
            ColumnRef::parse_resource_name("billing.invoices.total").unwrap(),
            ColumnRef::with_schema("billing", "invoices", "total")
        );
        assert!(ColumnRef::parse_resource_name("users.profile.address.city").is_err());
        assert!(ColumnRef::parse_resource_name("tenant/acme/users.email").is_err());
    }
}
