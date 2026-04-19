use std::path::Path;

use reddb::regress::{discover_cases, format_result, run_case};
use reddb::runtime::{RedDBRuntime, RuntimeQueryResult};
use reddb::storage::query::unified::UnifiedRecord;
use reddb::storage::schema::Value;

#[test]
fn regression_sql_snapshots_match() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/regress");
    let cases = discover_cases(&root.join("sql"), &root.join("expected")).expect("discover cases");
    assert!(
        !cases.is_empty(),
        "expected at least one regression fixture"
    );

    let mut failures = Vec::new();
    for case in &cases {
        let runtime = RedDBRuntime::in_memory().expect("in-memory runtime");
        let failure = run_case(case, |sql| execute_statement(&runtime, sql)).expect("run case");
        if let Some(failure) = failure {
            failures.push(format!("case `{}` diff:\n{}", failure.name, failure.diff));
        }
    }

    if !failures.is_empty() {
        panic!("{}\n", failures.join("\n"));
    }
}

fn execute_statement(runtime: &RedDBRuntime, sql: &str) -> String {
    match runtime.execute_query(sql) {
        Ok(result) => format_runtime_result(&result),
        Err(err) => format!("ERROR: {err}\n"),
    }
}

fn format_runtime_result(result: &RuntimeQueryResult) -> String {
    if result.statement_type != "select" {
        if let Some(message) = extract_message(&result.result.records) {
            return format!("{message}\n");
        }
        return match result.statement_type {
            "insert" => format!("INSERT 0 {}\n", result.affected_rows),
            "update" => format!("UPDATE {}\n", result.affected_rows),
            "delete" => format!("DELETE {}\n", result.affected_rows),
            other => format!("{}\n", other.to_ascii_uppercase()),
        };
    }

    let columns = if !result.result.columns.is_empty() {
        result.result.columns.clone()
    } else {
        let mut columns: Vec<String> = result
            .result
            .records
            .first()
            .map(|record| record.values.keys().map(|k| k.to_string()).collect())
            .unwrap_or_default();
        columns.sort();
        columns
    };
    let rows = result
        .result
        .records
        .iter()
        .map(|record| {
            columns
                .iter()
                .map(|column| {
                    format_regress_value(record.values.get(column.as_str()).unwrap_or(&Value::Null))
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    trim_trailing_spaces(&format_result(&columns, &rows))
}

fn extract_message(records: &[UnifiedRecord]) -> Option<String> {
    let record = records.first()?;
    if record.values.len() != 1 {
        return None;
    }
    record.values.get("message").map(format_regress_value)
}

fn format_regress_value(value: &Value) -> String {
    match value {
        Value::Text(text)
        | Value::Email(text)
        | Value::Url(text)
        | Value::NodeRef(text)
        | Value::EdgeRef(text)
        | Value::TableRef(text) => text.clone(),
        Value::Password(_) | Value::Secret(_) => "***".to_string(),
        other => other.display_string(),
    }
}

fn trim_trailing_spaces(rendered: &str) -> String {
    let mut out = rendered
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    out.push('\n');
    out
}
