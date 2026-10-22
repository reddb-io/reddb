//! Canonical output column labels shared by execution hosts.

use reddb_rql::ast::{FieldRef, Projection};

pub fn projection_name(projection: &Projection) -> String {
    match projection {
        Projection::All => "*".to_string(),
        Projection::Column(column) => column.clone(),
        Projection::Alias(_, alias) => alias.clone(),
        // Scalar function projections encode `FUNC(...) AS alias`
        // as `"FUNC:alias"` in the first tuple field. If an alias
        // is present, expose it as the output column name; otherwise
        // fall back to the function name.
        Projection::Function(name, args) => {
            if let Some((_, alias)) = name.split_once(':') {
                alias.to_string()
            } else {
                // #1370 — an unaliased function / operator projection is labeled
                // with its source-text form (`UPPER(name)`, `id * 2`,
                // `name || '!'`), reconstructed from the lowered projection.
                render_projection_label(name, args)
            }
        }
        Projection::Expression(_, alias) => alias.clone().unwrap_or_else(|| "expr".to_string()),
        Projection::Field(field, alias) => alias.clone().unwrap_or_else(|| field_ref_name(field)),
        Projection::Window { name, alias, .. } => alias.clone().unwrap_or_else(|| name.clone()),
    }
}

/// SQL infix symbol for an arithmetic / concat operator that
/// `sql_lowering::projection_binop_name` lowered to a function name.
fn projection_operator_symbol(name: &str) -> Option<&'static str> {
    match name {
        "ADD" => Some("+"),
        "SUB" => Some("-"),
        "MUL" => Some("*"),
        "DIV" => Some("/"),
        "MOD" => Some("%"),
        "CONCAT" => Some("||"),
        _ => None,
    }
}

/// Render one argument of an unaliased function/operator projection back to
/// its source-text label.
fn projection_arg_label(projection: &Projection) -> String {
    match projection {
        Projection::Column(column) => match column.strip_prefix("LIT:") {
            // Numbers / bool / null render bare; string literals are re-quoted
            // to match the source text the user wrote (#1370).
            Some(lit)
                if lit.is_empty()
                    || lit.parse::<f64>().is_ok()
                    || lit.eq_ignore_ascii_case("true")
                    || lit.eq_ignore_ascii_case("false")
                    || lit.eq_ignore_ascii_case("null") =>
            {
                lit.to_string()
            }
            Some(lit) => format!("'{lit}'"),
            None => column.clone(),
        },
        Projection::Function(name, args) => {
            let base = name.split_once(':').map(|(b, _)| b).unwrap_or(name);
            render_projection_label(base, args)
        }
        Projection::Field(field, _) => field_ref_name(field),
        Projection::Alias(_, alias) => alias.clone(),
        other => projection_name(other),
    }
}

/// Reconstruct the source-text label of an unaliased function / operator
/// projection: operators render infix (`id * 2`), other functions render as
/// calls (`UPPER(name)`, `COALESCE(name, 'fb')`).
fn render_projection_label(name: &str, args: &[Projection]) -> String {
    if let Some(symbol) = projection_operator_symbol(name) {
        args.iter()
            .map(projection_arg_label)
            .collect::<Vec<_>>()
            .join(&format!(" {symbol} "))
    } else {
        format!(
            "{}({})",
            name,
            args.iter()
                .map(projection_arg_label)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

pub fn field_ref_name(field: &FieldRef) -> String {
    match field {
        FieldRef::TableColumn { table, column } => {
            if table.is_empty() {
                column.clone()
            } else {
                format!("{table}.{column}")
            }
        }
        FieldRef::NodeProperty { alias, property } => format!("{alias}.{property}"),
        FieldRef::EdgeProperty { alias, property } => format!("{alias}.{property}"),
        FieldRef::NodeId { alias } => format!("{alias}.id"),
    }
}
