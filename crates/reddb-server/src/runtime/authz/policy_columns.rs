//! Free IAM/policy-column helpers extracted from `impl_core` (issue
//! #1622, PRD #1619). These pure functions back the query privilege
//! gates ([`super::privilege`]) and relational SELECT/JOIN column
//! authorization ([`super::projection`]); they moved together with their
//! consumers because the gates depend on them. Behaviour-preserving move.
use super::super::*;

/// Translate a parsed GRANT into a synthetic IAM policy whose id
/// starts with `_grant_<unique>`. PUBLIC is represented as an
/// implicit IAM group; legacy GROUP grants are still rejected by the
/// grant store and are not translated here.
pub(crate) fn grant_to_iam_policy(
    principal: &crate::auth::privileges::GrantPrincipal,
    resource: &crate::auth::privileges::Resource,
    actions: &[crate::auth::privileges::Action],
    tenant: Option<&str>,
) -> Option<crate::auth::policies::Policy> {
    use crate::auth::policies::{
        compile_action, ActionPattern, Effect, Policy, ResourcePattern, Statement,
    };
    use crate::auth::privileges::{Action, GrantPrincipal, Resource};

    if matches!(principal, GrantPrincipal::Group(_)) {
        return None;
    }

    let now = crate::auth::now_ms();
    let id = format!("_grant_{:x}_{:x}", now, std::process::id());

    let resource_str = match resource {
        Resource::Database => "table:*".to_string(),
        Resource::Schema(s) => format!("table:{s}.*"),
        Resource::Table { schema, table } => match schema {
            Some(s) => format!("table:{s}.{table}"),
            None => format!("table:{table}"),
        },
        Resource::Function { schema, name } => match schema {
            Some(s) => format!("function:{s}.{name}"),
            None => format!("function:{name}"),
        },
    };

    // Compile actions — fall back to `*` only when the grant included
    // `Action::All`. Map every other action keyword to its lowercase
    // form so it lines up with the kernel's allowlist.
    let action_patterns: Vec<ActionPattern> = if actions.contains(&Action::All) {
        vec![ActionPattern::Wildcard]
    } else {
        actions
            .iter()
            .map(|a| compile_action(&a.as_str().to_ascii_lowercase()))
            .collect()
    };
    if action_patterns.is_empty() {
        return None;
    }

    // Inline resource compilation matching the kernel's `compile_resource`:
    //   * `*` → wildcard
    //   * contains `*` → glob
    //   * `kind:name` → exact
    let resource_patterns = if resource_str == "*" {
        vec![ResourcePattern::Wildcard]
    } else if resource_str.contains('*') {
        vec![ResourcePattern::Glob(resource_str.clone())]
    } else if let Some((kind, name)) = resource_str.split_once(':') {
        vec![ResourcePattern::Exact {
            kind: kind.to_string(),
            name: name.to_string(),
        }]
    } else {
        vec![ResourcePattern::Wildcard]
    };

    let policy = Policy {
        id,
        version: 1,
        tenant: tenant.map(|t| t.to_string()),
        created_at: now,
        updated_at: now,
        statements: vec![Statement {
            sid: None,
            effect: Effect::Allow,
            actions: action_patterns,
            resources: resource_patterns,
            condition: None,
        }],
    };
    if policy.validate().is_err() {
        return None;
    }
    Some(policy)
}

/// Coerce a `key => <number>` table-function named argument into a positive
/// iteration count for the centrality TVFs (issue #797). The parser lexes all
/// named values as `f64`, so an integral, finite, strictly-positive value is
/// required here; anything else (fractional, zero, negative, NaN/inf) is a
/// clear query error. `func` names the function for the message.
pub(crate) fn parse_positive_iterations(func: &str, value: &f64) -> RedDBResult<usize> {
    if !value.is_finite() || *value < 1.0 || value.fract() != 0.0 {
        return Err(RedDBError::Query(format!(
            "table function '{func}' max_iterations must be a positive integer, got {value}"
        )));
    }
    Ok(*value as usize)
}

pub(crate) fn legacy_action_to_iam(action: crate::auth::privileges::Action) -> &'static str {
    use crate::auth::privileges::Action;
    match action {
        Action::Select => "select",
        Action::Insert => "insert",
        Action::Update => "update",
        Action::Delete => "delete",
        Action::Truncate => "truncate",
        Action::References => "references",
        Action::Execute => "execute",
        Action::Usage => "usage",
        Action::All => "*",
    }
}

pub(crate) fn update_set_target_columns(
    query: &crate::storage::query::ast::UpdateQuery,
) -> Vec<String> {
    let mut columns = Vec::new();
    for (column, _) in &query.assignment_exprs {
        if !columns.iter().any(|seen| seen == column) {
            columns.push(column.clone());
        }
    }
    columns
}

pub(crate) fn column_access_request_for_table_update(
    table_name: &str,
    columns: Vec<String>,
) -> crate::auth::ColumnAccessRequest {
    match table_name.split_once('.') {
        Some((schema, table)) => {
            crate::auth::ColumnAccessRequest::update(table.to_string(), columns)
                .with_schema(schema.to_string())
        }
        None => crate::auth::ColumnAccessRequest::update(table_name.to_string(), columns),
    }
}

pub(crate) fn column_access_request_for_table_select(
    table_name: &str,
    columns: Vec<String>,
) -> crate::auth::ColumnAccessRequest {
    match table_name.split_once('.') {
        Some((schema, table)) => {
            crate::auth::ColumnAccessRequest::select(table.to_string(), columns)
                .with_schema(schema.to_string())
        }
        None => crate::auth::ColumnAccessRequest::select(table_name.to_string(), columns),
    }
}

pub(crate) fn update_returning_columns_for_policy(
    runtime: &RedDBRuntime,
    query: &crate::storage::query::ast::UpdateQuery,
) -> Option<Vec<String>> {
    let items = query.returning.as_ref()?;
    let mut columns = Vec::new();
    let project_all = items
        .iter()
        .any(|item| matches!(item, crate::storage::query::ast::ReturningItem::All));
    if project_all {
        collect_returning_star_columns(runtime, query, &mut columns);
    } else {
        for item in items {
            let crate::storage::query::ast::ReturningItem::Column(column) = item else {
                continue;
            };
            push_returning_policy_column(&mut columns, column);
        }
    }
    (!columns.is_empty()).then_some(columns)
}

pub(crate) fn collect_returning_star_columns(
    runtime: &RedDBRuntime,
    query: &crate::storage::query::ast::UpdateQuery,
    columns: &mut Vec<String>,
) {
    let store = runtime.db().store();
    let Some(manager) = store.get_collection(&query.table) else {
        return;
    };
    if let Some(schema) = manager.column_schema() {
        for column in schema.iter() {
            push_returning_policy_column(columns, column);
        }
    }
    for entity in manager.query_all(|_| true) {
        if !returning_entity_matches_update_target(&entity, query.target) {
            continue;
        }
        match &entity.data {
            crate::storage::EntityData::Row(row) => {
                for (column, _) in row.iter_fields() {
                    push_returning_policy_column(columns, column);
                }
            }
            crate::storage::EntityData::Node(node) => {
                push_returning_policy_column(columns, "label");
                push_returning_policy_column(columns, "node_type");
                for column in node.properties.keys() {
                    push_returning_policy_column(columns, column);
                }
            }
            crate::storage::EntityData::Edge(edge) => {
                push_returning_policy_column(columns, "label");
                push_returning_policy_column(columns, "from_rid");
                push_returning_policy_column(columns, "to_rid");
                push_returning_policy_column(columns, "weight");
                for column in edge.properties.keys() {
                    push_returning_policy_column(columns, column);
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn push_returning_policy_column(columns: &mut Vec<String>, column: &str) {
    if returning_public_envelope_column(column) {
        return;
    }
    if !columns.iter().any(|seen| seen == column) {
        columns.push(column.to_string());
    }
}

pub(crate) fn returning_public_envelope_column(column: &str) -> bool {
    matches!(
        column.to_ascii_lowercase().as_str(),
        "rid" | "collection" | "kind" | "tenant" | "created_at" | "updated_at"
    )
}

pub(crate) fn returning_entity_matches_update_target(
    entity: &crate::storage::UnifiedEntity,
    target: crate::storage::query::ast::UpdateTarget,
) -> bool {
    use crate::storage::query::ast::UpdateTarget;
    match target {
        UpdateTarget::Rows => {
            matches!(returning_row_item_kind(entity), Some(ReturningRowKind::Row))
        }
        UpdateTarget::Documents => {
            matches!(
                returning_row_item_kind(entity),
                Some(ReturningRowKind::Document)
            )
        }
        UpdateTarget::Kv => matches!(returning_row_item_kind(entity), Some(ReturningRowKind::Kv)),
        UpdateTarget::Nodes => matches!(
            (&entity.kind, &entity.data),
            (
                crate::storage::EntityKind::GraphNode(_),
                crate::storage::EntityData::Node(_)
            )
        ),
        UpdateTarget::Edges => matches!(
            (&entity.kind, &entity.data),
            (
                crate::storage::EntityKind::GraphEdge(_),
                crate::storage::EntityData::Edge(_)
            )
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReturningRowKind {
    Row,
    Document,
    Kv,
}

pub(crate) fn returning_row_item_kind(
    entity: &crate::storage::UnifiedEntity,
) -> Option<ReturningRowKind> {
    let row = entity.data.as_row()?;
    let is_kv = row.iter_fields().all(|(column, _)| {
        column.eq_ignore_ascii_case("key") || column.eq_ignore_ascii_case("value")
    });
    if is_kv {
        return Some(ReturningRowKind::Kv);
    }
    let is_document = row
        .iter_fields()
        .any(|(_, value)| matches!(value, crate::storage::schema::Value::Json(_)));
    if is_document {
        Some(ReturningRowKind::Document)
    } else {
        Some(ReturningRowKind::Row)
    }
}

pub(crate) fn requested_table_columns_for_policy(
    table: &crate::storage::query::ast::TableQuery,
) -> Vec<String> {
    use crate::storage::query::sql_lowering::{
        effective_table_filter, effective_table_group_by_exprs, effective_table_having_filter,
        effective_table_projections,
    };

    let table_name = table.table.as_str();
    let table_alias = table.alias.as_deref();
    let mut columns = std::collections::BTreeSet::new();

    for projection in effective_table_projections(table) {
        collect_projection_columns(&projection, table_name, table_alias, &mut columns);
    }
    if let Some(filter) = effective_table_filter(table) {
        collect_filter_columns(&filter, table_name, table_alias, &mut columns);
    }
    for expr in effective_table_group_by_exprs(table) {
        collect_expr_columns(&expr, table_name, table_alias, &mut columns);
    }
    if let Some(filter) = effective_table_having_filter(table) {
        collect_filter_columns(&filter, table_name, table_alias, &mut columns);
    }
    for order in &table.order_by {
        if let Some(expr) = order.expr.as_ref() {
            collect_expr_columns(expr, table_name, table_alias, &mut columns);
        } else {
            collect_field_ref_column(&order.field, table_name, table_alias, &mut columns);
        }
    }

    columns.into_iter().collect()
}

pub(crate) fn collect_projection_columns(
    projection: &crate::storage::query::ast::Projection,
    table_name: &str,
    table_alias: Option<&str>,
    columns: &mut std::collections::BTreeSet<String>,
) {
    use crate::storage::query::ast::Projection;
    match projection {
        Projection::All => {
            columns.insert("*".to_string());
        }
        Projection::Column(column) | Projection::Alias(column, _) => {
            if column != "*" {
                columns.insert(column.clone());
            }
        }
        Projection::Function(_, args) => {
            for arg in args {
                collect_projection_columns(arg, table_name, table_alias, columns);
            }
        }
        Projection::Expression(filter, _) => {
            collect_filter_columns(filter, table_name, table_alias, columns);
        }
        Projection::Field(field, _) => {
            collect_field_ref_column(field, table_name, table_alias, columns);
        }
        // Slice 7a (#589): no runtime support yet; recurse into args so
        // any column references are still tracked in case a future
        // executor needs the column set.
        Projection::Window { args, .. } => {
            for arg in args {
                collect_projection_columns(arg, table_name, table_alias, columns);
            }
        }
    }
}

pub(crate) fn collect_filter_columns(
    filter: &crate::storage::query::ast::Filter,
    table_name: &str,
    table_alias: Option<&str>,
    columns: &mut std::collections::BTreeSet<String>,
) {
    use crate::storage::query::ast::Filter;
    match filter {
        Filter::Compare { field, .. }
        | Filter::IsNull(field)
        | Filter::IsNotNull(field)
        | Filter::In { field, .. }
        | Filter::Between { field, .. }
        | Filter::Like { field, .. }
        | Filter::StartsWith { field, .. }
        | Filter::EndsWith { field, .. }
        | Filter::Contains { field, .. } => {
            collect_field_ref_column(field, table_name, table_alias, columns);
        }
        Filter::CompareFields { left, right, .. } => {
            collect_field_ref_column(left, table_name, table_alias, columns);
            collect_field_ref_column(right, table_name, table_alias, columns);
        }
        Filter::CompareExpr { lhs, rhs, .. } => {
            collect_expr_columns(lhs, table_name, table_alias, columns);
            collect_expr_columns(rhs, table_name, table_alias, columns);
        }
        Filter::And(left, right) | Filter::Or(left, right) => {
            collect_filter_columns(left, table_name, table_alias, columns);
            collect_filter_columns(right, table_name, table_alias, columns);
        }
        Filter::Not(inner) => collect_filter_columns(inner, table_name, table_alias, columns),
    }
}

pub(crate) fn collect_expr_columns(
    expr: &crate::storage::query::ast::Expr,
    table_name: &str,
    table_alias: Option<&str>,
    columns: &mut std::collections::BTreeSet<String>,
) {
    use crate::storage::query::ast::Expr;
    match expr {
        Expr::Column { field, .. } => {
            collect_field_ref_column(field, table_name, table_alias, columns);
        }
        Expr::Literal { .. } | Expr::Parameter { .. } => {}
        Expr::UnaryOp { operand, .. } | Expr::Cast { inner: operand, .. } => {
            collect_expr_columns(operand, table_name, table_alias, columns);
        }
        Expr::BinaryOp { lhs, rhs, .. } => {
            collect_expr_columns(lhs, table_name, table_alias, columns);
            collect_expr_columns(rhs, table_name, table_alias, columns);
        }
        Expr::FunctionCall { args, .. } => {
            for arg in args {
                collect_expr_columns(arg, table_name, table_alias, columns);
            }
        }
        Expr::Case {
            branches, else_, ..
        } => {
            for (condition, value) in branches {
                collect_expr_columns(condition, table_name, table_alias, columns);
                collect_expr_columns(value, table_name, table_alias, columns);
            }
            if let Some(value) = else_ {
                collect_expr_columns(value, table_name, table_alias, columns);
            }
        }
        Expr::IsNull { operand, .. } => {
            collect_expr_columns(operand, table_name, table_alias, columns);
        }
        Expr::InList { target, values, .. } => {
            collect_expr_columns(target, table_name, table_alias, columns);
            for value in values {
                collect_expr_columns(value, table_name, table_alias, columns);
            }
        }
        Expr::Between {
            target, low, high, ..
        } => {
            collect_expr_columns(target, table_name, table_alias, columns);
            collect_expr_columns(low, table_name, table_alias, columns);
            collect_expr_columns(high, table_name, table_alias, columns);
        }
        Expr::Subquery { .. } => {}
        Expr::WindowFunctionCall { args, window, .. } => {
            for arg in args {
                collect_expr_columns(arg, table_name, table_alias, columns);
            }
            for e in &window.partition_by {
                collect_expr_columns(e, table_name, table_alias, columns);
            }
            for o in &window.order_by {
                collect_expr_columns(&o.expr, table_name, table_alias, columns);
            }
        }
    }
}

pub(crate) fn collect_field_ref_column(
    field: &crate::storage::query::ast::FieldRef,
    table_name: &str,
    table_alias: Option<&str>,
    columns: &mut std::collections::BTreeSet<String>,
) {
    if let Some(column) = policy_column_name_from_field_ref(field, table_name, table_alias) {
        if column != "*" {
            columns.insert(column);
        }
    }
}

pub(crate) fn policy_column_name_from_field_ref(
    field: &crate::storage::query::ast::FieldRef,
    table_name: &str,
    table_alias: Option<&str>,
) -> Option<String> {
    match field {
        crate::storage::query::ast::FieldRef::TableColumn { table, column } => {
            if column == "*" {
                return Some("*".to_string());
            }
            if table.is_empty() || table == table_name || Some(table.as_str()) == table_alias {
                Some(column.clone())
            } else {
                Some(format!("{table}.{column}"))
            }
        }
        _ => None,
    }
}

pub(crate) fn legacy_resource_to_iam(
    resource: &crate::auth::privileges::Resource,
    tenant: Option<&str>,
) -> crate::auth::policies::ResourceRef {
    use crate::auth::privileges::Resource;

    let (kind, name) = match resource {
        Resource::Database => ("database".to_string(), "*".to_string()),
        Resource::Schema(s) => ("schema".to_string(), format!("{s}.*")),
        Resource::Table { schema, table } => (
            "table".to_string(),
            match schema {
                Some(s) => format!("{s}.{table}"),
                None => table.clone(),
            },
        ),
        Resource::Function { schema, name } => (
            "function".to_string(),
            match schema {
                Some(s) => format!("{s}.{name}"),
                None => name.clone(),
            },
        ),
    };

    let mut out = crate::auth::policies::ResourceRef::new(kind, name);
    if let Some(t) = tenant {
        out = out.with_tenant(t.to_string());
    }
    out
}

#[derive(Debug)]
pub(crate) struct JoinTableSide {
    pub(crate) table: String,
    pub(crate) alias: String,
}

pub(crate) fn table_side_context(expr: &QueryExpr) -> Option<JoinTableSide> {
    match expr {
        QueryExpr::Table(table) => Some(JoinTableSide {
            table: table.table.clone(),
            alias: table.alias.clone().unwrap_or_else(|| table.table.clone()),
        }),
        _ => None,
    }
}

pub(crate) fn collect_projection_columns_for_table(
    projection: &Projection,
    table: &str,
    alias: Option<&str>,
    out: &mut BTreeSet<String>,
) {
    match projection {
        Projection::Column(column) | Projection::Alias(column, _) => {
            match split_qualified_column(column) {
                Some((qualifier, column))
                    if qualifier == table || alias.is_some_and(|alias| qualifier == alias) =>
                {
                    push_policy_column(column, out);
                }
                Some(_) => {}
                None => push_policy_column(column, out),
            }
        }
        Projection::Field(
            FieldRef::TableColumn {
                table: qualifier,
                column,
            },
            _,
        ) => {
            if qualifier.is_empty()
                || qualifier == table
                || alias.is_some_and(|alias| qualifier == alias)
            {
                push_policy_column(column, out);
            }
        }
        Projection::Field(
            FieldRef::NodeProperty {
                alias: qualifier,
                property,
            },
            _,
        )
        | Projection::Field(
            FieldRef::EdgeProperty {
                alias: qualifier,
                property,
            },
            _,
        ) => {
            if qualifier == table || alias.is_some_and(|alias| qualifier == alias) {
                push_policy_column(property, out);
            }
        }
        Projection::Function(_, args) => {
            for arg in args {
                collect_projection_columns_for_table(arg, table, alias, out);
            }
        }
        Projection::Expression(_, _) | Projection::All | Projection::Field(_, _) => {}
        Projection::Window { args, .. } => {
            for arg in args {
                collect_projection_columns_for_table(arg, table, alias, out);
            }
        }
    }
}

pub(crate) fn collect_projection_columns_for_join_side(
    projection: &Projection,
    left: Option<&JoinTableSide>,
    right: Option<&JoinTableSide>,
    out: &mut HashMap<String, BTreeSet<String>>,
) -> RedDBResult<()> {
    match projection {
        Projection::Column(column) | Projection::Alias(column, _) => {
            if let Some((qualifier, column)) = split_qualified_column(column) {
                push_qualified_join_column(qualifier, column, left, right, out);
            } else {
                push_unqualified_join_column(column, left, right, out);
            }
        }
        Projection::Field(FieldRef::TableColumn { table, column }, _) => {
            if table.is_empty() {
                push_unqualified_join_column(column, left, right, out);
            } else if let Some(side) = [left, right]
                .into_iter()
                .flatten()
                .find(|side| table == side.table.as_str() || table == side.alias.as_str())
            {
                push_join_column(&side.table, column, out);
            }
        }
        Projection::Field(FieldRef::NodeProperty { alias, property }, _)
        | Projection::Field(FieldRef::EdgeProperty { alias, property }, _) => {
            push_qualified_join_column(alias, property, left, right, out);
        }
        Projection::Function(_, args) => {
            for arg in args {
                collect_projection_columns_for_join_side(arg, left, right, out)?;
            }
        }
        Projection::Expression(_, _) | Projection::All | Projection::Field(_, _) => {}
        Projection::Window { args, .. } => {
            for arg in args {
                collect_projection_columns_for_join_side(arg, left, right, out)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn split_qualified_column(column: &str) -> Option<(&str, &str)> {
    let (qualifier, column) = column.split_once('.')?;
    if qualifier.is_empty() || column.is_empty() || column.contains('.') {
        return None;
    }
    Some((qualifier, column))
}

pub(crate) fn push_qualified_join_column(
    qualifier: &str,
    column: &str,
    left: Option<&JoinTableSide>,
    right: Option<&JoinTableSide>,
    out: &mut HashMap<String, BTreeSet<String>>,
) {
    if let Some(side) = [left, right]
        .into_iter()
        .flatten()
        .find(|side| qualifier == side.table.as_str() || qualifier == side.alias.as_str())
    {
        push_join_column(&side.table, column, out);
    }
}

pub(crate) fn push_unqualified_join_column(
    column: &str,
    left: Option<&JoinTableSide>,
    right: Option<&JoinTableSide>,
    out: &mut HashMap<String, BTreeSet<String>>,
) {
    for side in [left, right].into_iter().flatten() {
        push_join_column(&side.table, column, out);
    }
}

pub(crate) fn push_join_column(
    table: &str,
    column: &str,
    out: &mut HashMap<String, BTreeSet<String>>,
) {
    if is_policy_column_name(column) {
        out.entry(table.to_string())
            .or_default()
            .insert(column.to_string());
    }
}

pub(crate) fn push_policy_column(column: &str, out: &mut BTreeSet<String>) {
    if is_policy_column_name(column) {
        out.insert(column.to_string());
    }
}

pub(crate) fn is_policy_column_name(column: &str) -> bool {
    !column.is_empty()
        && column != "*"
        && !column.starts_with("LIT:")
        && !column.starts_with("TYPE:")
}

pub(crate) fn runtime_iam_context(
    role: crate::auth::Role,
    tenant: Option<&str>,
) -> crate::auth::policies::EvalContext {
    crate::auth::policies::EvalContext {
        principal_tenant: tenant.map(|t| t.to_string()),
        current_tenant: tenant.map(|t| t.to_string()),
        peer_ip: None,
        mfa_present: false,
        now_ms: crate::auth::now_ms(),
        principal_is_admin_role: role == crate::auth::Role::Admin,
        principal_is_platform_scoped: tenant.is_none(),
    }
}

pub(crate) fn explicit_table_projection_columns(
    query: &crate::storage::query::ast::TableQuery,
) -> Vec<String> {
    use crate::storage::query::ast::{FieldRef, Projection};

    let mut columns = Vec::new();
    for projection in crate::storage::query::sql_lowering::effective_table_projections(query) {
        match projection {
            Projection::Column(column) | Projection::Alias(column, _) => {
                push_unique(&mut columns, column)
            }
            Projection::Field(FieldRef::TableColumn { column, .. }, _) => {
                push_unique(&mut columns, column)
            }
            // SELECT * and expression/function projections need the
            // executor-wide column-policy context mapped in
            // docs/security/select-relational-column-policy-audit-2026-05-08.md.
            _ => {}
        }
    }
    columns
}

pub(crate) fn explicit_graph_projection_properties(
    query: &crate::storage::query::ast::GraphQuery,
) -> Vec<String> {
    use crate::storage::query::ast::{FieldRef, Projection};

    let mut columns = Vec::new();
    for projection in &query.return_ {
        match projection {
            Projection::Field(FieldRef::NodeProperty { property, .. }, _)
            | Projection::Field(FieldRef::EdgeProperty { property, .. }, _) => {
                push_unique(&mut columns, property.clone())
            }
            _ => {}
        }
    }
    columns
}

pub(crate) fn push_unique(columns: &mut Vec<String>, column: String) {
    if !columns.iter().any(|existing| existing == &column) {
        columns.push(column);
    }
}

pub(crate) fn principal_label(p: &crate::storage::query::ast::PolicyPrincipalRef) -> String {
    use crate::storage::query::ast::PolicyPrincipalRef;
    match p {
        PolicyPrincipalRef::User(u) => match &u.tenant {
            Some(t) => format!("user:{t}/{}", u.username),
            None => format!("user:{}", u.username),
        },
        PolicyPrincipalRef::Group(g) => format!("group:{g}"),
    }
}

/// Render a `Decision` into the (decision, matched_policy_id, matched_sid)
/// shape used by every audit emit + the simulator response.
pub(crate) fn decision_to_strings(
    d: &crate::auth::policies::Decision,
) -> (String, Option<String>, Option<String>) {
    use crate::auth::policies::Decision;
    match d {
        Decision::Allow {
            matched_policy_id,
            matched_sid,
        } => (
            "allow".into(),
            Some(matched_policy_id.clone()),
            matched_sid.clone(),
        ),
        Decision::Deny {
            matched_policy_id,
            matched_sid,
        } => (
            "deny".into(),
            Some(matched_policy_id.clone()),
            matched_sid.clone(),
        ),
        Decision::DefaultDeny => ("default_deny".into(), None, None),
        Decision::AdminBypass => ("admin_bypass".into(), None, None),
    }
}
