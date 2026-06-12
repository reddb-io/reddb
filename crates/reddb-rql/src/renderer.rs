//! AST → SQL/RQL renderer (partial subset for property round-trip tests).
//!
//! Covers the three query categories exercised by the property test:
//! - `SELECT col, … FROM table [WHERE simple-filter]`
//! - `INSERT INTO table (cols) VALUES (vals)`
//! - `QUEUE PUSH queue value`
//!
//! Run the round-trip property test with:
//! ```text
//! cargo test -p reddb-server property_round_trip
//! ```

use crate::ast::{
    CompareOp, FieldRef, Filter, InsertQuery, Projection, QueryExpr, QueueCommand, TableQuery,
};
use reddb_types::types::Value;

/// Render a `QueryExpr` back into canonical SQL/RQL.
///
/// Returns an empty string for variants outside the supported subset.
pub fn render(expr: &QueryExpr) -> String {
    match expr {
        QueryExpr::Table(tq) => render_table(tq),
        QueryExpr::Insert(iq) => render_insert(iq),
        QueryExpr::QueueCommand(qc) => render_queue_command(qc),
        _ => String::new(),
    }
}

fn render_table(tq: &TableQuery) -> String {
    let cols = if tq.columns.is_empty() {
        "*".to_string()
    } else {
        tq.columns
            .iter()
            .map(render_projection)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut sql = format!("SELECT {} FROM {}", cols, tq.table);
    if let Some(filter) = &tq.filter {
        sql.push_str(" WHERE ");
        sql.push_str(&render_filter(filter));
    }
    sql
}

fn render_insert(iq: &InsertQuery) -> String {
    let cols = iq.columns.join(", ");
    let rows: Vec<String> = iq
        .values
        .iter()
        .map(|row| {
            let vals = row
                .iter()
                .map(render_value_sql)
                .collect::<Vec<_>>()
                .join(", ");
            format!("({})", vals)
        })
        .collect();
    format!(
        "INSERT INTO {} ({}) VALUES {}",
        iq.table,
        cols,
        rows.join(", ")
    )
}

fn render_queue_command(qc: &QueueCommand) -> String {
    match qc {
        QueueCommand::Push { queue, value, .. } => {
            format!("QUEUE PUSH {} {}", queue, render_value_sql(value))
        }
        _ => String::new(),
    }
}

fn render_projection(p: &Projection) -> String {
    match p {
        Projection::All => "*".to_string(),
        Projection::Column(col) => col.clone(),
        Projection::Alias(col, alias) => format!("{} AS {}", col, alias),
        Projection::Field(field, alias) => {
            let col = render_field_ref(field);
            match alias {
                Some(a) => format!("{} AS {}", col, a),
                None => col,
            }
        }
        _ => "*".to_string(),
    }
}

pub(crate) fn render_field_ref(f: &FieldRef) -> String {
    match f {
        FieldRef::TableColumn { table, column } if table.is_empty() => column.clone(),
        FieldRef::TableColumn { table, column } => format!("{}.{}", table, column),
        _ => "field".to_string(),
    }
}

fn render_filter(filter: &Filter) -> String {
    match filter {
        Filter::Compare { field, op, value } => {
            format!(
                "{} {} {}",
                render_field_ref(field),
                op,
                render_value_sql(value)
            )
        }
        Filter::And(a, b) => format!("({}) AND ({})", render_filter(a), render_filter(b)),
        Filter::Or(a, b) => format!("({}) OR ({})", render_filter(a), render_filter(b)),
        _ => "1=1".to_string(),
    }
}

/// Render a `Value` as a SQL literal suitable for embedding in a query string.
/// Only the subset used by property tests is handled; others fall back to NULL.
///
/// `pub` (was `pub(crate)`) so the server's `storage::query::renderer` shim can
/// re-export it for the runtime `render_value_sql` call-sites (#1103).
pub fn render_value_sql(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_string(),
        Value::Integer(i) => i.to_string(),
        Value::UnsignedInteger(u) => u.to_string(),
        Value::Float(f) => {
            // Ensure the rendered form parses back as Float, not Integer.
            if f.fract() == 0.0 {
                format!("{:.1}", f)
            } else {
                format!("{}", f)
            }
        }
        Value::Boolean(b) => {
            if *b {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        Value::Text(s) => format!("'{}'", s.replace('\'', "''")),
        // JSON bytes are stored as canonical compact JSON; emit them raw so
        // the lexer picks them up as a JsonLiteral token on re-parse.
        Value::Json(bytes) => String::from_utf8_lossy(bytes).to_string(),
        _ => "NULL".to_string(),
    }
}
