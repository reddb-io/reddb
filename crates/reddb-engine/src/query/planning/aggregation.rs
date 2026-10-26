//! Canonical native aggregate reference planning over an admitted query AST.
//! This plans output references; it does not admit reads or execute expressions.
use crate::query::runtime::aggregation::{
    aggregate_argument_key, aggregate_output_name, base_function_name, is_aggregate_function,
};
use crate::query::runtime::projection::{field_ref_name, projection_name};
use reddb_rql::ast::{
    BinOp, Expr, FieldRef, Filter, OrderByClause, Projection, SelectItem, Span, TableQuery,
    UnaryOp, WindowFrameBound, WindowSpec,
};
use reddb_rql::sql_lowering::{
    effective_table_group_by_exprs, effective_table_having_filter, effective_table_projections,
};
use reddb_types::Value;

#[derive(Default)]
pub struct AggregateRuntimePlan {
    pub aggregate_projections: Vec<Projection>,
    pub hidden_aggregates: Vec<Projection>,
    pub having: Option<Filter>,
    pub order_by: Vec<OrderByClause>,
}

#[derive(Default)]
struct HiddenAggregateRegistry {
    depth_error: bool,
    next_output_suffix: usize,
    reserved_outputs: std::collections::HashSet<String>,
    projections: Vec<Projection>,
    outputs_by_signature: std::collections::HashMap<String, String>,
}

impl HiddenAggregateRegistry {
    fn ensure_output_name(&mut self, func_name: &str, args: &[Expr]) -> Option<String> {
        let signature = aggregate_expr_signature(func_name, args)?;
        if let Some(output_name) = self.outputs_by_signature.get(&signature) {
            return Some(output_name.clone());
        }

        let projection_args = args
            .iter()
            .map(reddb_rql::sql_lowering::expr_to_projection)
            .collect::<Option<Vec<_>>>()?;
        if validate_nodes(projection_args.iter().map(Node::Projection)).is_err() {
            self.depth_error = true;
            return None;
        }
        let col_name = aggregate_argument_key(&projection_args)?;
        let mut projection = Projection::Function(func_name.to_string(), projection_args);
        let mut output_name = aggregate_output_name(&projection, func_name, &col_name);
        if self.reserved_outputs.contains(&output_name) {
            loop {
                output_name = format!("__reddb_aggregate_{}", self.next_output_suffix);
                self.next_output_suffix += 1;
                if !self.reserved_outputs.contains(&output_name) {
                    break;
                }
            }
            if let Projection::Function(name, _) = &mut projection {
                *name = format!("{func_name}:{output_name}");
            }
        }
        self.reserved_outputs.insert(output_name.clone());

        self.outputs_by_signature
            .insert(signature, output_name.clone());
        self.projections.push(projection);
        Some(output_name)
    }
}

pub fn prepare_aggregate_runtime_plan(
    query: &TableQuery,
) -> Result<AggregateRuntimePlan, &'static str> {
    validate_aggregate_planning_depth(query)?;
    let effective_projections = effective_table_projections(query);
    let effective_having = effective_table_having_filter(query);
    validate_nodes(
        effective_projections
            .iter()
            .map(Node::Projection)
            .chain(effective_having.iter().map(Node::Filter)),
    )?;
    let visible_outputs = effective_projections
        .iter()
        .filter_map(visible_aggregate_output_name_unchecked)
        .collect::<std::collections::HashMap<_, _>>();
    let mut reserved_outputs = std::collections::HashSet::new();
    for projection in &effective_projections {
        reserved_outputs.insert(projection_name(projection));
        match projection {
            Projection::Column(column) | Projection::Alias(column, _) => {
                reserved_outputs.insert(column.clone());
            }
            Projection::Field(field, _) => {
                reserved_outputs.insert(field_ref_name(field));
            }
            _ => {}
        }
    }
    for expression in effective_table_group_by_exprs(query) {
        if let Some(label) = render_expr_signature_unchecked(&expression) {
            reserved_outputs.insert(label);
        }
    }
    let mut hidden = HiddenAggregateRegistry {
        reserved_outputs,
        ..Default::default()
    };

    let having = effective_having
        .as_ref()
        .map(|filter| rewrite_aggregate_filter_refs(filter, &visible_outputs, &mut hidden));
    let order_by = query
        .order_by
        .iter()
        .map(|clause| rewrite_aggregate_order_by_refs(clause, &visible_outputs, &mut hidden))
        .collect();

    if hidden.depth_error {
        return Err(DEPTH_ERROR);
    }
    let mut aggregate_projections = effective_projections.into_iter()
        .filter(|projection| matches!(projection, Projection::Function(name, _) if is_aggregate_function(base_function_name(name))))
        .collect::<Vec<_>>();
    aggregate_projections.extend(hidden.projections.iter().cloned());
    let mut seen = std::collections::HashSet::new();
    aggregate_projections.retain(|projection| {
        let Projection::Function(name, arguments) = projection else {
            return false;
        };
        let name = base_function_name(name).to_uppercase();
        is_aggregate_function(&name)
            && seen.insert(aggregate_projection_signature(&name, arguments))
    });
    Ok(AggregateRuntimePlan {
        aggregate_projections,
        hidden_aggregates: hidden.projections,
        having,
        order_by,
    })
}

fn visible_aggregate_output_name_unchecked(projection: &Projection) -> Option<(String, String)> {
    let Projection::Function(name, args) = projection else {
        return None;
    };
    let func_name = base_function_name(name).to_uppercase();
    if !is_aggregate_function(&func_name) {
        return None;
    }

    let signature = aggregate_projection_signature(&func_name, args);
    let col_name = aggregate_argument_key(args)?;
    Some((
        signature,
        aggregate_output_name(projection, &func_name, &col_name),
    ))
}

fn rewrite_aggregate_order_by_refs(
    clause: &OrderByClause,
    visible_outputs: &std::collections::HashMap<String, String>,
    hidden: &mut HiddenAggregateRegistry,
) -> OrderByClause {
    OrderByClause {
        field: clause.field.clone(),
        expr: clause
            .expr
            .as_ref()
            .map(|expr| rewrite_aggregate_expr_refs(expr, visible_outputs, hidden)),
        ascending: clause.ascending,
        nulls_first: clause.nulls_first,
    }
}

fn rewrite_aggregate_filter_refs(
    filter: &Filter,
    visible_outputs: &std::collections::HashMap<String, String>,
    hidden: &mut HiddenAggregateRegistry,
) -> Filter {
    match filter {
        Filter::CompareExpr { lhs, op, rhs } => Filter::CompareExpr {
            lhs: rewrite_aggregate_expr_refs(lhs, visible_outputs, hidden),
            op: *op,
            rhs: rewrite_aggregate_expr_refs(rhs, visible_outputs, hidden),
        },
        Filter::And(left, right) => Filter::And(
            Box::new(rewrite_aggregate_filter_refs(left, visible_outputs, hidden)),
            Box::new(rewrite_aggregate_filter_refs(
                right,
                visible_outputs,
                hidden,
            )),
        ),
        Filter::Or(left, right) => Filter::Or(
            Box::new(rewrite_aggregate_filter_refs(left, visible_outputs, hidden)),
            Box::new(rewrite_aggregate_filter_refs(
                right,
                visible_outputs,
                hidden,
            )),
        ),
        Filter::Not(inner) => Filter::Not(Box::new(rewrite_aggregate_filter_refs(
            inner,
            visible_outputs,
            hidden,
        ))),
        other => other.clone(),
    }
}

fn rewrite_aggregate_expr_refs(
    expr: &Expr,
    visible_outputs: &std::collections::HashMap<String, String>,
    hidden: &mut HiddenAggregateRegistry,
) -> Expr {
    match expr {
        Expr::FunctionCall { name, args, span } => {
            let func_name = name.to_uppercase();
            if is_aggregate_function(&func_name) {
                if let Some(signature) = aggregate_expr_signature(&func_name, args) {
                    if let Some(output_name) = visible_outputs.get(&signature) {
                        return aggregate_output_expr(output_name.clone(), *span);
                    }
                }
                if let Some(output_name) = hidden.ensure_output_name(&func_name, args) {
                    return aggregate_output_expr(output_name, *span);
                }
            }

            Expr::FunctionCall {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| rewrite_aggregate_expr_refs(arg, visible_outputs, hidden))
                    .collect(),
                span: *span,
            }
        }
        Expr::BinaryOp { op, lhs, rhs, span } => Expr::BinaryOp {
            op: *op,
            lhs: Box::new(rewrite_aggregate_expr_refs(lhs, visible_outputs, hidden)),
            rhs: Box::new(rewrite_aggregate_expr_refs(rhs, visible_outputs, hidden)),
            span: *span,
        },
        Expr::UnaryOp { op, operand, span } => Expr::UnaryOp {
            op: *op,
            operand: Box::new(rewrite_aggregate_expr_refs(
                operand,
                visible_outputs,
                hidden,
            )),
            span: *span,
        },
        Expr::Cast {
            inner,
            target,
            span,
        } => Expr::Cast {
            inner: Box::new(rewrite_aggregate_expr_refs(inner, visible_outputs, hidden)),
            target: *target,
            span: *span,
        },
        Expr::Case {
            branches,
            else_,
            span,
        } => Expr::Case {
            branches: branches
                .iter()
                .map(|(cond, value)| {
                    (
                        rewrite_aggregate_expr_refs(cond, visible_outputs, hidden),
                        rewrite_aggregate_expr_refs(value, visible_outputs, hidden),
                    )
                })
                .collect(),
            else_: else_
                .as_ref()
                .map(|expr| Box::new(rewrite_aggregate_expr_refs(expr, visible_outputs, hidden))),
            span: *span,
        },
        Expr::IsNull {
            operand,
            negated,
            span,
        } => Expr::IsNull {
            operand: Box::new(rewrite_aggregate_expr_refs(
                operand,
                visible_outputs,
                hidden,
            )),
            negated: *negated,
            span: *span,
        },
        Expr::InList {
            target,
            values,
            negated,
            span,
        } => Expr::InList {
            target: Box::new(rewrite_aggregate_expr_refs(target, visible_outputs, hidden)),
            values: values
                .iter()
                .map(|value| rewrite_aggregate_expr_refs(value, visible_outputs, hidden))
                .collect(),
            negated: *negated,
            span: *span,
        },
        Expr::Between {
            target,
            low,
            high,
            negated,
            span,
        } => Expr::Between {
            target: Box::new(rewrite_aggregate_expr_refs(target, visible_outputs, hidden)),
            low: Box::new(rewrite_aggregate_expr_refs(low, visible_outputs, hidden)),
            high: Box::new(rewrite_aggregate_expr_refs(high, visible_outputs, hidden)),
            negated: *negated,
            span: *span,
        },
        other => other.clone(),
    }
}

fn aggregate_output_expr(output_name: String, span: Span) -> Expr {
    Expr::Column {
        field: FieldRef::TableColumn {
            table: String::new(),
            column: output_name,
        },
        span,
    }
}

fn aggregate_projection_signature(func_name: &str, args: &[Projection]) -> String {
    let rendered = args
        .iter()
        .map(render_projection_signature_unchecked)
        .collect::<Vec<_>>()
        .join(",");
    format!("{func_name}({rendered})")
}

fn aggregate_expr_signature(func_name: &str, args: &[Expr]) -> Option<String> {
    let rendered = args
        .iter()
        .map(render_expr_signature_unchecked)
        .collect::<Option<Vec<_>>>()?
        .join(",");
    Some(format!("{func_name}({rendered})"))
}

fn render_projection_signature_unchecked(projection: &Projection) -> String {
    match projection {
        Projection::All => "*".to_string(),
        Projection::Column(column) => column
            .strip_prefix("LIT:")
            .map(str::to_string)
            .unwrap_or_else(|| column.clone()),
        Projection::Alias(_, alias) => alias.clone(),
        Projection::Field(field, alias) => alias.clone().unwrap_or_else(|| field_ref_name(field)),
        Projection::Function(name, args) => format!(
            "{}({})",
            base_function_name(name),
            args.iter()
                .map(render_projection_signature_unchecked)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Projection::Expression(filter, alias) => alias
            .clone()
            .unwrap_or_else(|| render_filter_signature(filter)),
        Projection::Window {
            name, args, alias, ..
        } => alias.clone().unwrap_or_else(|| {
            format!(
                "{}({}) OVER (...)",
                name,
                args.iter()
                    .map(render_projection_signature_unchecked)
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }),
    }
}

fn render_filter_signature(filter: &Filter) -> String {
    match filter {
        Filter::Compare { field, op, value } => format!(
            "({}{}{})",
            field_ref_name(field),
            op,
            render_value_signature(value)
        ),
        Filter::CompareFields { left, op, right } => {
            format!("({}{}{})", field_ref_name(left), op, field_ref_name(right))
        }
        Filter::CompareExpr { lhs, op, rhs } => format!(
            "({}{}{})",
            render_expr_signature_unchecked(lhs).unwrap_or_else(|| "expr".to_string()),
            op,
            render_expr_signature_unchecked(rhs).unwrap_or_else(|| "expr".to_string())
        ),
        Filter::And(left, right) => format!(
            "({}AND{})",
            render_filter_signature(left),
            render_filter_signature(right)
        ),
        Filter::Or(left, right) => format!(
            "({}OR{})",
            render_filter_signature(left),
            render_filter_signature(right)
        ),
        Filter::Not(inner) => format!("(NOT{})", render_filter_signature(inner)),
        Filter::IsNull(field) => format!("({}ISNULL)", field_ref_name(field)),
        Filter::IsNotNull(field) => format!("({}ISNOTNULL)", field_ref_name(field)),
        Filter::In { field, values } => format!(
            "{}IN({})",
            field_ref_name(field),
            values
                .iter()
                .map(render_value_signature)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Filter::Between { field, low, high } => format!(
            "{}BETWEEN({},{})",
            field_ref_name(field),
            render_value_signature(low),
            render_value_signature(high)
        ),
        Filter::Like { field, pattern } => format!("{}LIKE({pattern})", field_ref_name(field)),
        Filter::StartsWith { field, prefix } => {
            format!("{}STARTSWITH({prefix})", field_ref_name(field))
        }
        Filter::EndsWith { field, suffix } => {
            format!("{}ENDSWITH({suffix})", field_ref_name(field))
        }
        Filter::Contains { field, substring } => {
            format!("{}CONTAINS({substring})", field_ref_name(field))
        }
    }
}

fn render_expr_signature_unchecked(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Literal { value, .. } => Some(render_value_signature(value)),
        Expr::Column { field, .. } => Some(field_ref_name(field)),
        Expr::Parameter { index, .. } => Some(format!("${index}")),
        Expr::BinaryOp { op, lhs, rhs, .. } => match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod | BinOp::Concat => {
                Some(format!(
                    "{}({},{})",
                    render_binop_signature_name(*op),
                    render_expr_signature_unchecked(lhs)?,
                    render_expr_signature_unchecked(rhs)?
                ))
            }
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Some(format!(
                "({}{}{})",
                render_expr_signature_unchecked(lhs)?,
                render_binop_compare_symbol(*op),
                render_expr_signature_unchecked(rhs)?
            )),
            BinOp::And => Some(format!(
                "({}AND{})",
                render_expr_signature_unchecked(lhs)?,
                render_expr_signature_unchecked(rhs)?
            )),
            BinOp::Or => Some(format!(
                "({}OR{})",
                render_expr_signature_unchecked(lhs)?,
                render_expr_signature_unchecked(rhs)?
            )),
        },
        Expr::UnaryOp { op, operand, .. } => match op {
            UnaryOp::Neg => Some(format!(
                "NEG({})",
                render_expr_signature_unchecked(operand)?
            )),
            UnaryOp::Not => Some(format!(
                "NOT({})",
                render_expr_signature_unchecked(operand)?
            )),
        },
        Expr::Cast { inner, target, .. } => Some(format!(
            "CAST({},TYPE:{target})",
            render_expr_signature_unchecked(inner)?
        )),
        Expr::FunctionCall { name, args, .. } => Some(format!(
            "{}({})",
            name.to_uppercase(),
            args.iter()
                .map(render_expr_signature_unchecked)
                .collect::<Option<Vec<_>>>()?
                .join(",")
        )),
        Expr::Case {
            branches, else_, ..
        } => {
            let mut parts = Vec::with_capacity(branches.len() * 2 + usize::from(else_.is_some()));
            for (cond, value) in branches {
                parts.push(render_expr_signature_unchecked(cond)?);
                parts.push(render_expr_signature_unchecked(value)?);
            }
            if let Some(else_expr) = else_ {
                parts.push(render_expr_signature_unchecked(else_expr)?);
            }
            Some(format!("CASE({})", parts.join(",")))
        }
        Expr::IsNull {
            operand, negated, ..
        } => Some(format!(
            "{}({})",
            if *negated { "IS_NOT_NULL" } else { "IS_NULL" },
            render_expr_signature_unchecked(operand)?
        )),
        Expr::InList {
            target,
            values,
            negated,
            ..
        } => Some(format!(
            "{}({},{})",
            if *negated { "NOT_IN" } else { "IN" },
            render_expr_signature_unchecked(target)?,
            values
                .iter()
                .map(render_expr_signature_unchecked)
                .collect::<Option<Vec<_>>>()?
                .join(",")
        )),
        Expr::Between {
            target,
            low,
            high,
            negated,
            ..
        } => Some(format!(
            "{}({},{},{})",
            if *negated { "NOT_BETWEEN" } else { "BETWEEN" },
            render_expr_signature_unchecked(target)?,
            render_expr_signature_unchecked(low)?,
            render_expr_signature_unchecked(high)?
        )),
        Expr::Subquery { .. } => None,
        Expr::WindowFunctionCall { .. } => None,
    }
}

fn render_binop_signature_name(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "ADD",
        BinOp::Sub => "SUB",
        BinOp::Mul => "MUL",
        BinOp::Div => "DIV",
        BinOp::Mod => "MOD",
        BinOp::Concat => "CONCAT",
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => "CMP",
        BinOp::And => "AND",
        BinOp::Or => "OR",
    }
}

fn render_binop_compare_symbol(op: BinOp) -> &'static str {
    match op {
        BinOp::Eq => "=",
        BinOp::Ne => "<>",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        _ => "?",
    }
}

fn render_value_signature(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Boolean(value) => value.to_string(),
        Value::Integer(value) => value.to_string(),
        Value::UnsignedInteger(value) => value.to_string(),
        Value::Float(value) => value.to_string(),
        Value::BigInt(value) => value.to_string(),
        Value::Decimal(value) => value.to_string(),
        Value::Text(value) => value.to_string(),
        other => other.display_string(),
    }
}

/// Same legacy rendering used by native GROUP BY labels. Literal containers
/// and subquery bodies retain the host's parser/admitted-value depth bounds.
pub fn render_projection_signature(projection: &Projection) -> Option<String> {
    validate_nodes([Node::Projection(projection)]).ok()?;
    Some(render_projection_signature_unchecked(projection))
}
pub fn render_expr_signature(expression: &Expr) -> Option<String> {
    validate_nodes([Node::Expr(expression)]).ok()?;
    render_expr_signature_unchecked(expression)
}
pub fn visible_aggregate_output_name(projection: &Projection) -> Option<(String, String)> {
    validate_nodes([Node::Projection(projection)]).ok()?;
    visible_aggregate_output_name_unchecked(projection)
}

const DEPTH_ERROR: &str = "aggregate query exceeds maximum nesting depth";

/// Bound the AST paths this planner and native admission recursively lower,
/// render or clone. Opaque subquery bodies and nested literal containers retain
/// the existing host parser/value admission limits, as does source resolution.
pub fn validate_aggregate_planning_depth(query: &TableQuery) -> Result<(), &'static str> {
    let mut roots = Vec::new();
    if query.select_items.is_empty() {
        roots.extend(query.columns.iter().map(Node::Projection));
    } else {
        for item in &query.select_items {
            if let SelectItem::Expr { expr, .. } = item {
                roots.push(Node::Expr(expr));
            }
        }
    }
    if let Some(filter) = &query.filter {
        roots.push(Node::Filter(filter));
    } else if let Some(expression) = &query.where_expr {
        roots.push(Node::Expr(expression));
    }
    if let Some(filter) = &query.having {
        roots.push(Node::Filter(filter));
    } else if let Some(expression) = &query.having_expr {
        roots.push(Node::Expr(expression));
    }
    roots.extend(query.group_by_exprs.iter().map(Node::Expr));
    roots.extend(
        query
            .order_by
            .iter()
            .filter_map(|clause| clause.expr.as_ref())
            .map(Node::Expr),
    );
    validate_nodes(roots)
}

enum Node<'a> {
    Projection(&'a Projection),
    Filter(&'a Filter),
    Expr(&'a Expr),
    Window(&'a WindowSpec),
}

fn validate_nodes<'a>(roots: impl IntoIterator<Item = Node<'a>>) -> Result<(), &'static str> {
    let mut pending: Vec<_> = roots.into_iter().map(|root| (root, 0)).collect();
    while let Some((node, depth)) = pending.pop() {
        if depth > reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
            return Err(DEPTH_ERROR);
        }
        let mut push = |node| pending.push((node, depth + 1));
        match node {
            Node::Projection(projection) => match projection {
                Projection::Function(_, arguments) => {
                    for argument in arguments {
                        push(Node::Projection(argument));
                    }
                }
                Projection::Window { args, window, .. } => {
                    for argument in args {
                        push(Node::Projection(argument));
                    }
                    push(Node::Window(window));
                }
                Projection::Expression(filter, _) => push(Node::Filter(filter)),
                _ => {}
            },
            Node::Filter(filter) => match filter {
                Filter::CompareExpr { lhs, rhs, .. } => {
                    push(Node::Expr(lhs));
                    push(Node::Expr(rhs));
                }
                Filter::And(left, right) | Filter::Or(left, right) => {
                    push(Node::Filter(left));
                    push(Node::Filter(right));
                }
                Filter::Not(inner) => push(Node::Filter(inner)),
                _ => {}
            },
            Node::Expr(expression) => match expression {
                Expr::BinaryOp { lhs, rhs, .. } => {
                    push(Node::Expr(lhs));
                    push(Node::Expr(rhs));
                }
                Expr::UnaryOp { operand, .. } | Expr::IsNull { operand, .. } => {
                    push(Node::Expr(operand))
                }
                Expr::Cast { inner, .. } => push(Node::Expr(inner)),
                Expr::FunctionCall { args, .. } => {
                    for argument in args {
                        push(Node::Expr(argument));
                    }
                }
                Expr::WindowFunctionCall { args, window, .. } => {
                    for argument in args {
                        push(Node::Expr(argument));
                    }
                    push(Node::Window(window));
                }
                Expr::Case {
                    branches, else_, ..
                } => {
                    for (condition, value) in branches {
                        push(Node::Expr(condition));
                        push(Node::Expr(value));
                    }
                    if let Some(value) = else_ {
                        push(Node::Expr(value));
                    }
                }
                Expr::InList { target, values, .. } => {
                    push(Node::Expr(target));
                    for value in values {
                        push(Node::Expr(value));
                    }
                }
                Expr::Between {
                    target, low, high, ..
                } => {
                    push(Node::Expr(target));
                    push(Node::Expr(low));
                    push(Node::Expr(high));
                }
                _ => {}
            },
            Node::Window(window) => {
                for expression in &window.partition_by {
                    push(Node::Expr(expression));
                }
                for item in &window.order_by {
                    push(Node::Expr(&item.expr));
                }
                if let Some(frame) = &window.frame {
                    for bound in std::iter::once(&frame.start).chain(frame.end.as_ref()) {
                        if let WindowFrameBound::Preceding(expression)
                        | WindowFrameBound::Following(expression) = bound
                        {
                            push(Node::Expr(expression));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
