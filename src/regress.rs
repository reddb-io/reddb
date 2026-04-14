//! Regression test harness — Post-MVP credibility item.
//!
//! `.sql + .out` snapshot tests in the `src/test/regress`
//! style of PostgreSQL. Each test case is a pair:
//!
//! - `tests/regress/sql/<name>.sql` — input statements
//! - `tests/regress/expected/<name>.out` — golden output
//!
//! The harness:
//!
//! 1. Walks the `sql/` directory.
//! 2. For each `.sql` file, opens an in-memory reddb runtime.
//! 3. Splits the file by semicolon, executes each statement.
//! 4. Captures the output (rows + error if any) into a
//!    canonical text format.
//! 5. Diffs against the matching `.out` file.
//! 6. Reports pass/fail with a unified diff on mismatch.
//!
//! ## Why
//!
//! End-to-end tests are reddb's biggest credibility gap.
//! Cargo unit tests prove individual functions work; a
//! regression suite proves the *whole stack* still parses,
//! plans, and executes a given query the same way release
//! after release. PG's regression suite is the reason
//! upgrading from 13 to 16 doesn't silently change query
//! semantics.
//!
//! ## Format
//!
//! Output canonical form:
//!
//! ```text
//! -- <statement-1>
//! col1 | col2 | col3
//! -----+------+-----
//!  v1  |  v2  |  v3
//!  v4  |  v5  |  v6
//! (2 rows)
//!
//! -- <statement-2>
//! ERROR: <error message>
//!
//! -- <statement-3>
//! INSERT 0 5
//! ```
//!
//! Mirrors PG's psql tabular output as much as practical so
//! the .out files are human-readable and CI failure messages
//! are obvious.
//!
//! ## Wiring
//!
//! Phase post-MVP: a `tests/regress.rs` integration test in
//! Cargo's `tests/` directory walks the harness over every
//! `.sql` file. Initial fixture suite covers Fase 1 unblock
//! features (CAST, CASE, aritmética, BETWEEN, JOIN, ORDER BY,
//! ||, subquery FROM) — each becomes one .sql/.out pair so
//! a regression in any of them fails CI loudly.

use std::fs;
use std::path::{Path, PathBuf};

/// Runs a single regression test case against an executor
/// callback. Returns the diff between expected and actual
/// output, or `None` when they match.
pub struct RegressCase {
    pub name: String,
    pub sql_path: PathBuf,
    pub expected_path: PathBuf,
}

/// Aggregate result of a regression run.
#[derive(Debug, Default)]
pub struct RegressReport {
    pub passed: Vec<String>,
    pub failed: Vec<RegressFailure>,
}

#[derive(Debug)]
pub struct RegressFailure {
    pub name: String,
    pub diff: String,
    pub expected: String,
    pub actual: String,
}

impl RegressReport {
    pub fn pass_count(&self) -> usize {
        self.passed.len()
    }
    pub fn fail_count(&self) -> usize {
        self.failed.len()
    }
    pub fn is_green(&self) -> bool {
        self.failed.is_empty()
    }
}

/// Discover every `.sql` file under `sql_dir` and pair it
/// with the matching `.out` file under `expected_dir`. Files
/// without a matching `.out` are reported as a missing-golden
/// failure.
pub fn discover_cases(sql_dir: &Path, expected_dir: &Path) -> std::io::Result<Vec<RegressCase>> {
    let mut cases = Vec::new();
    for entry in fs::read_dir(sql_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sql") {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem.is_empty() {
            continue;
        }
        let expected_path = expected_dir.join(format!("{stem}.out"));
        cases.push(RegressCase {
            name: stem.to_string(),
            sql_path: path,
            expected_path,
        });
    }
    cases.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(cases)
}

/// Runs a single case by feeding the SQL through `executor`,
/// capturing stdout-equivalent output, and diffing against
/// the expected golden.
///
/// `executor` is a closure so the harness doesn't depend on
/// the runtime types directly — tests inject a closure that
/// takes a SQL string and returns the canonical output.
pub fn run_case<F>(case: &RegressCase, executor: F) -> Result<Option<RegressFailure>, std::io::Error>
where
    F: FnMut(&str) -> String,
{
    let sql = fs::read_to_string(&case.sql_path)?;
    let actual = run_sql_to_canonical(&sql, executor);
    let expected = fs::read_to_string(&case.expected_path).unwrap_or_default();
    if actual == expected {
        Ok(None)
    } else {
        Ok(Some(RegressFailure {
            name: case.name.clone(),
            diff: render_diff(&expected, &actual),
            expected,
            actual,
        }))
    }
}

/// Walk an entire suite of cases, returning the aggregate
/// report. Continues past failures so CI can show every
/// regression at once.
pub fn run_suite<F>(cases: &[RegressCase], mut executor: F) -> Result<RegressReport, std::io::Error>
where
    F: FnMut(&str) -> String,
{
    let mut report = RegressReport::default();
    for case in cases {
        match run_case(case, &mut executor)? {
            None => report.passed.push(case.name.clone()),
            Some(failure) => report.failed.push(failure),
        }
    }
    Ok(report)
}

/// Split a SQL file into individual statements by semicolon
/// boundaries (respecting string literals) and feed each to
/// the executor, accumulating the canonical output.
fn run_sql_to_canonical<F>(sql: &str, mut executor: F) -> String
where
    F: FnMut(&str) -> String,
{
    let mut out = String::new();
    for stmt in split_statements(sql) {
        let stmt_trimmed = stmt.trim();
        if stmt_trimmed.is_empty() {
            continue;
        }
        out.push_str("-- ");
        out.push_str(stmt_trimmed);
        out.push('\n');
        let result = executor(stmt_trimmed);
        out.push_str(&result);
        if !result.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

/// Split a SQL blob by semicolons, respecting `'..'` and
/// `"..."` string literals so semicolons inside strings don't
/// terminate statements early.
fn split_statements(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut prev = '\0';
    for ch in sql.chars() {
        match ch {
            '\'' if !in_double && prev != '\\' => {
                in_single = !in_single;
                current.push(ch);
            }
            '"' if !in_single && prev != '\\' => {
                in_double = !in_double;
                current.push(ch);
            }
            ';' if !in_single && !in_double => {
                if !current.trim().is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
        prev = ch;
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    out
}

/// Render a unified-style line-by-line diff of expected vs
/// actual. Produces output similar to `diff -u` so CI logs
/// are familiar to humans. Hand-rolled to avoid pulling a
/// diff crate into the dep graph.
fn render_diff(expected: &str, actual: &str) -> String {
    let exp_lines: Vec<&str> = expected.lines().collect();
    let act_lines: Vec<&str> = actual.lines().collect();
    let mut out = String::new();
    let max = exp_lines.len().max(act_lines.len());
    for i in 0..max {
        match (exp_lines.get(i), act_lines.get(i)) {
            (Some(e), Some(a)) if e == a => {
                out.push_str("  ");
                out.push_str(e);
                out.push('\n');
            }
            (Some(e), Some(a)) => {
                out.push_str("- ");
                out.push_str(e);
                out.push('\n');
                out.push_str("+ ");
                out.push_str(a);
                out.push('\n');
            }
            (Some(e), None) => {
                out.push_str("- ");
                out.push_str(e);
                out.push('\n');
            }
            (None, Some(a)) => {
                out.push_str("+ ");
                out.push_str(a);
                out.push('\n');
            }
            (None, None) => {}
        }
    }
    out
}

/// Format a tabular result set in psql-style for the
/// canonical output. Used by test executors that want to
/// produce the exact same shape PG's regression tests do.
pub fn format_result(columns: &[String], rows: &[Vec<String>]) -> String {
    if columns.is_empty() {
        return String::new();
    }
    // Compute column widths.
    let mut widths: Vec<usize> = columns.iter().map(|c| c.len()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if let Some(w) = widths.get_mut(i) {
                *w = (*w).max(cell.len());
            }
        }
    }
    let mut out = String::new();
    // Header.
    for (i, col) in columns.iter().enumerate() {
        if i > 0 {
            out.push_str(" | ");
        }
        out.push_str(&pad_right(col, widths[i]));
    }
    out.push('\n');
    // Separator.
    for (i, w) in widths.iter().enumerate() {
        if i > 0 {
            out.push_str("-+-");
        }
        out.push_str(&"-".repeat(*w));
    }
    out.push('\n');
    // Rows.
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                out.push_str(" | ");
            }
            let w = widths.get(i).copied().unwrap_or(cell.len());
            out.push_str(&pad_right(cell, w));
        }
        out.push('\n');
    }
    // Footer.
    out.push_str(&format!(
        "({} row{})\n",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" }
    ));
    out
}

fn pad_right(s: &str, width: usize) -> String {
    if s.len() >= width {
        s.to_string()
    } else {
        let mut out = String::with_capacity(width);
        out.push_str(s);
        for _ in s.len()..width {
            out.push(' ');
        }
        out
    }
}
