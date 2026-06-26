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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{InsertEntityType, QueueSide};

    /// Build a minimal `TableQuery` carrying only the fields the renderer reads
    /// (`table`, `columns`, `filter`); every other slot stays at its inert
    /// default so the helper survives struct-field churn elsewhere.
    fn table_query(table: &str, columns: Vec<Projection>, filter: Option<Filter>) -> TableQuery {
        TableQuery {
            table: table.to_string(),
            source: None,
            alias: None,
            select_items: Vec::new(),
            columns,
            where_expr: None,
            filter,
            group_by_exprs: Vec::new(),
            group_by: Vec::new(),
            having_expr: None,
            having: None,
            order_by: Vec::new(),
            limit: None,
            limit_param: None,
            offset: None,
            offset_param: None,
            expand: None,
            as_of: None,
            sessionize: None,
            distinct: false,
        }
    }

    #[test]
    fn render_table_handles_star_columns_and_filter() {
        // Empty column list renders the SQL star.
        let star = render(&QueryExpr::Table(table_query("users", Vec::new(), None)));
        assert_eq!(star, "SELECT * FROM users");

        // Explicit columns plus an AND/OR filter tree exercise the
        // projection list and the recursive filter renderer.
        let filter = Filter::Or(
            Box::new(Filter::And(
                Box::new(Filter::Compare {
                    field: FieldRef::column("u", "age"),
                    op: CompareOp::Gt,
                    value: Value::Integer(18),
                }),
                Box::new(Filter::Compare {
                    field: FieldRef::column("", "active"),
                    op: CompareOp::Eq,
                    value: Value::Boolean(true),
                }),
            )),
            // A variant the renderer cannot express falls back to `1=1`.
            Box::new(Filter::Not(Box::new(Filter::Compare {
                field: FieldRef::column("", "banned"),
                op: CompareOp::Eq,
                value: Value::Boolean(false),
            }))),
        );
        let rendered = render(&QueryExpr::Table(table_query(
            "users",
            vec![
                Projection::All,
                Projection::Column("name".to_string()),
                Projection::Alias("name".to_string(), "n".to_string()),
                Projection::Field(FieldRef::column("u", "age"), Some("a".to_string())),
                Projection::Field(FieldRef::column("", "active"), None),
                // Window/Function variants are outside the renderer subset.
                Projection::Function("count".to_string(), Vec::new()),
            ],
            Some(filter),
        )));
        assert_eq!(
            rendered,
            "SELECT *, name, name AS n, u.age AS a, active, * FROM users \
             WHERE ((u.age > 18) AND (active = true)) OR (1=1)"
        );
    }

    #[test]
    fn render_insert_joins_columns_and_value_rows() {
        let insert = InsertQuery {
            table: "metrics".to_string(),
            entity_type: InsertEntityType::Row,
            columns: vec!["id".to_string(), "ratio".to_string(), "whole".to_string()],
            value_exprs: Vec::new(),
            values: vec![vec![
                Value::UnsignedInteger(7),
                Value::Float(1.5),
                Value::Float(2.0),
            ]],
            returning: None,
            ttl_ms: None,
            expires_at_ms: None,
            with_metadata: Vec::new(),
            auto_embed: None,
            suppress_events: false,
        };
        // UnsignedInteger, fractional Float, and whole-number Float all
        // render through their dedicated `render_value_sql` arms.
        assert_eq!(
            render(&QueryExpr::Insert(insert)),
            "INSERT INTO metrics (id, ratio, whole) VALUES (7, 1.5, 2.0)"
        );
    }

    #[test]
    fn render_queue_command_only_renders_push() {
        let push = QueueCommand::Push {
            queue: "jobs".to_string(),
            value: Value::Text("o'brien".into()),
            side: QueueSide::Right,
            priority: None,
            available: None,
        };
        // Text literals escape embedded single quotes.
        assert_eq!(
            render(&QueryExpr::QueueCommand(push)),
            "QUEUE PUSH jobs 'o''brien'",
        );

        // Non-Push queue commands are outside the renderer subset.
        let len = QueueCommand::Len {
            queue: "jobs".to_string(),
        };
        assert_eq!(render(&QueryExpr::QueueCommand(len)), "");
    }

    #[test]
    fn render_field_ref_falls_back_for_graph_refs() {
        // Empty-table column renders bare; populated table dots them together;
        // non-TableColumn refs collapse to the `field` placeholder.
        assert_eq!(render_field_ref(&FieldRef::column("", "bare")), "bare");
        assert_eq!(render_field_ref(&FieldRef::column("t", "c")), "t.c");
        assert_eq!(
            render_field_ref(&FieldRef::NodeProperty {
                alias: "n".to_string(),
                property: "p".to_string(),
            }),
            "field"
        );
    }

    #[test]
    fn render_value_sql_covers_scalar_arms() {
        assert_eq!(render_value_sql(&Value::Null), "NULL");
        assert_eq!(render_value_sql(&Value::Integer(-3)), "-3");
        assert_eq!(render_value_sql(&Value::UnsignedInteger(9)), "9");
        assert_eq!(render_value_sql(&Value::Float(2.0)), "2.0");
        assert_eq!(render_value_sql(&Value::Float(0.25)), "0.25");
        assert_eq!(render_value_sql(&Value::Boolean(true)), "true");
        assert_eq!(render_value_sql(&Value::Boolean(false)), "false");
        assert_eq!(render_value_sql(&Value::Text("it's".into())), "'it''s'");
        assert_eq!(
            render_value_sql(&Value::Json(b"{\"k\":1}".to_vec())),
            "{\"k\":1}"
        );
        // Unsupported scalar kinds fall back to NULL.
        assert_eq!(render_value_sql(&Value::Blob(vec![1, 2, 3])), "NULL");
    }

    #[test]
    fn render_returns_empty_for_unsupported_query_expr() {
        assert_eq!(render(&QueryExpr::ShowTenant), "");
    }
}
