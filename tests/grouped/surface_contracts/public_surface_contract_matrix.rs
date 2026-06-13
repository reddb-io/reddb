use std::collections::HashSet;

const MATRIX: &str = include_str!("../../../docs/conformance/public-surface-contract-matrix.md");

#[derive(Debug)]
struct PromiseRow<'a> {
    id: &'a str,
    source: &'a str,
    status: &'a str,
    layer: &'a str,
    evidence: &'a str,
}

#[test]
fn public_surface_contract_matrix_has_required_contract_shape() {
    assert!(
        MATRIX.contains("# Public Surface Contract Matrix"),
        "matrix must have a stable title"
    );
    assert!(
        MATRIX.contains("## Public Promise Matrix"),
        "matrix must include the public promise table"
    );
    assert!(
        MATRIX.contains("## Feedback Scenario Coverage"),
        "matrix must include explicit feedback coverage"
    );
    assert!(
        MATRIX.contains("## Non-Public Inputs"),
        "matrix must distinguish ADR/internal planning inputs"
    );

    let rows = parse_promise_rows();
    assert!(!rows.is_empty(), "promise matrix should not be empty");

    let mut ids = HashSet::new();
    let mut failures = Vec::new();

    for row in &rows {
        if !ids.insert(row.id) {
            failures.push(format!("duplicate promise id `{}`", row.id));
        }

        if !matches!(
            row.status,
            "passing" | "failing" | "missing test coverage" | "intentionally unsupported"
        ) {
            failures.push(format!("{} has invalid status `{}`", row.id, row.status));
        }

        if !matches!(
            row.layer,
            "runtime/parser" | "HTTP" | "persistence" | "transport smoke" | "SDK"
        ) {
            failures.push(format!("{} has invalid layer `{}`", row.id, row.layer));
        }

        if row.evidence.trim().is_empty() {
            failures.push(format!("{} has no evidence or next action", row.id));
        }
    }

    for required_source in required_public_sources() {
        if !rows.iter().any(|row| row.source.contains(required_source)) {
            failures.push(format!(
                "missing a public promise sourced from `{required_source}`"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "public surface matrix validation failed:\n{}",
        failures.join("\n")
    );
}

#[test]
fn public_surface_contract_matrix_represents_every_feedback_scenario() {
    let ids: HashSet<&str> = MATRIX
        .lines()
        .filter(|line| line.starts_with("| FB-"))
        .filter_map(|line| line.trim_matches('|').split('|').next())
        .map(str::trim)
        .collect();

    let mut missing = Vec::new();
    for required in required_feedback_ids() {
        if !ids.contains(required) {
            missing.push(*required);
        }
    }

    assert!(
        missing.is_empty(),
        "feedback scenarios missing from public surface matrix:\n{}",
        missing.join("\n")
    );
}

fn parse_promise_rows() -> Vec<PromiseRow<'static>> {
    MATRIX
        .lines()
        .filter(|line| line.starts_with("| PSC-"))
        .map(parse_promise_row)
        .collect()
}

fn parse_promise_row(line: &'static str) -> PromiseRow<'static> {
    let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
    assert_eq!(cells.len(), 7, "promise row should have 7 cells: {line}");

    PromiseRow {
        id: cells[0],
        source: cells[1],
        status: cells[4],
        layer: cells[5],
        evidence: cells[6],
    }
}

fn required_public_sources() -> &'static [&'static str] {
    &[
        "README.md",
        "docs/query/",
        "docs/reference/",
        "drivers/",
        "crates/reddb-client/README.md",
        "examples/",
        "../feedbacks.md",
        "../feedbacks-new.md",
    ]
}

fn required_feedback_ids() -> &'static [&'static str] {
    &[
        "FB-OLD-01",
        "FB-OLD-02",
        "FB-OLD-03",
        "FB-OLD-04",
        "FB-OLD-05",
        "FB-OLD-06",
        "FB-OLD-07",
        "FB-OLD-08",
        "FB-OLD-09",
        "FB-OLD-10",
        "FB-OLD-11",
        "FB-OLD-12",
        "FB-OLD-13",
        "FB-OLD-14",
        "FB-OLD-15",
        "FB-OLD-16",
        "FB-OLD-17",
        "FB-OLD-18",
        "FB-OLD-19",
        "FB-OLD-20",
        "FB-OLD-21",
        "FB-OLD-22",
        "FB-OLD-23",
        "FB-OLD-24",
        "FB-OLD-25",
        "FB-OLD-26",
        "FB-OLD-27",
        "FB-OLD-28",
        "FB-OLD-29",
        "FB-OLD-30",
        "FB-OLD-31",
        "FB-OLD-32",
        "FB-OLD-33",
        "FB-OLD-34",
        "FB-OLD-35",
        "FB-OLD-36",
        "FB-OLD-37",
        "FB-OLD-38",
        "FB-OLD-39",
        "FB-OLD-40",
        "FB-OLD-41",
        "FB-OLD-42",
        "FB-OLD-43",
        "FB-OLD-44",
        "FB-NEW-01",
        "FB-NEW-02",
        "FB-NEW-03",
        "FB-NEW-04",
        "FB-NEW-05",
        "FB-NEW-06",
        "FB-NEW-07",
        "FB-NEW-08",
        "FB-NEW-09",
        "FB-NEW-10",
        "FB-NEW-11",
        "FB-NEW-12",
        "FB-NEW-13",
        "FB-NEW-14",
        "FB-NEW-15",
        "FB-NEW-16",
        "FB-NEW-17",
        "FB-NEW-18",
        "FB-NEW-19",
        "FB-NEW-20",
        "FB-NEW-21",
        "FB-NEW-22",
        "FB-NEW-23",
        "FB-NEW-24",
        "FB-NEW-25",
        "FB-NEW-26",
        "FB-NEW-27",
        "FB-NEW-28",
        "FB-NEW-29",
        "FB-NEW-30",
        "FB-NEW-31",
        "FB-NEW-32",
        "FB-NEW-33",
    ]
}
