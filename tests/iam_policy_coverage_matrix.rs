use std::collections::HashSet;

const MATRIX: &str = include_str!("../docs/security/iam_policy_coverage_matrix.md");

#[derive(Debug)]
struct MatrixRow<'a> {
    path: &'a str,
    action: &'a str,
    model: &'a str,
    relevant: &'a str,
    status: &'a str,
    evidence: &'a str,
}

#[test]
fn iam_policy_coverage_matrix_has_no_relevant_not_covered_gaps() {
    let rows = parse_matrix_rows();
    assert!(!rows.is_empty(), "coverage matrix should have rows");

    let mut seen_keys = HashSet::new();
    let mut failures = Vec::new();

    for row in &rows {
        if row.relevant != "yes" && row.relevant != "no" {
            failures.push(format!(
                "{} / {} / {} has invalid relevant value `{}`",
                row.path, row.action, row.model, row.relevant
            ));
        }

        if !matches!(row.status, "covered" | "not_covered" | "not_relevant") {
            failures.push(format!(
                "{} / {} / {} has invalid status `{}`",
                row.path, row.action, row.model, row.status
            ));
        }

        let key = (row.path, row.action, row.model);
        if !seen_keys.insert(key) {
            failures.push(format!(
                "{} / {} / {} appears more than once",
                row.path, row.action, row.model
            ));
        }

        if row.relevant == "yes" && row.status == "not_covered" {
            failures.push(format!(
                "{} / {} / {} is relevant but marked not_covered",
                row.path, row.action, row.model
            ));
        }

        if row.status == "covered" && row.evidence.trim().is_empty() {
            failures.push(format!(
                "{} / {} / {} is covered but has no evidence",
                row.path, row.action, row.model
            ));
        }
    }

    for required in required_relevant_keys() {
        if !rows.iter().any(|row| {
            row.path == required.0
                && row.action == required.1
                && row.model == required.2
                && row.relevant == "yes"
        }) {
            failures.push(format!(
                "{} / {} / {} is missing from relevant coverage",
                required.0, required.1, required.2
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "IAM policy coverage matrix validation failed:\n{}",
        failures.join("\n")
    );
}

fn required_relevant_keys() -> &'static [(&'static str, &'static str, &'static str)] {
    &[
        (
            "sql_text.table_explicit_projection",
            "select",
            "relational_table",
        ),
        (
            "sql_text.table_wildcard_projection",
            "select",
            "relational_table",
        ),
        ("sql_text.join_projection", "select", "relational_join"),
        ("sql_text.update_set_columns", "update", "relational_table"),
        (
            "sql_text.update_multi_column_set",
            "update",
            "relational_table",
        ),
        ("sql_text.update_tenant_scope", "update", "tenant_table"),
        (
            "sql_text.insert_named_columns",
            "insert",
            "relational_table",
        ),
        (
            "sql_text.insert_omitted_columns",
            "insert",
            "relational_table",
        ),
        ("sql_text.insert_multi_row", "insert", "relational_table"),
        ("sql_text.insert_tenant_autofill", "insert", "tenant_table"),
    ]
}

fn parse_matrix_rows() -> Vec<MatrixRow<'static>> {
    MATRIX
        .lines()
        .filter(|line| line.starts_with('|'))
        .filter(|line| !line.contains("| ---"))
        .filter(|line| !line.starts_with("| path |"))
        .map(parse_row)
        .collect()
}

fn parse_row(line: &'static str) -> MatrixRow<'static> {
    let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();

    assert_eq!(cells.len(), 6, "matrix row should have 6 cells: {line}");

    MatrixRow {
        path: cells[0],
        action: cells[1],
        model: cells[2],
        relevant: cells[3],
        status: cells[4],
        evidence: cells[5],
    }
}
