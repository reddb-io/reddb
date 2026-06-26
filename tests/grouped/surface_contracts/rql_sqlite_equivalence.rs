//! Differential RQL-vs-SQLite conformance harness (issue #1353, PRD #1350
//! pillar B).
//!
//! Where `rql_conformance.rs` checks the RedDB engine against the *pinned*
//! oracle values transcribed into each `.slt` block, this harness runs the same
//! standard-SQL corpus against a **live** SQLite oracle and asserts the two
//! engines agree row-for-row. It is the analog of Turso's "make the result
//! equal SQLite" discipline: every standard-SQL query is executed twice — once
//! against `reddb-server`'s `RedDBRuntime` and once against an in-memory SQLite
//! connection — and the rendered result grids are compared under the corpus's
//! declared sort mode. A mismatch names the diverging query, file, and both
//! result sets.
//!
//! ## Pinned divergences are honored, never silently failing
//!
//! The standard-SQL corpus already records every RedDB-vs-standard-SQL
//! divergence (PRD #1098 / #1100) inline with a `skipif reddb-server` directive
//! and a reason comment (see `crates/reddb-rql/tests/corpus/README.md`). This
//! harness reuses those exact markers as the divergence registry: a query
//! carrying `skipif reddb-server` is **excluded** from the equivalence
//! assertion and collected into a reported list, so a documented decision never
//! masquerades as a failure and a *new* divergence (an unmarked query whose
//! results differ) fails loudly with the offending SQL.
//!
//! ## Extending the surface
//!
//! The corpus is the single source of breadth: drop another `.slt` file under
//! `crates/reddb-rql/tests/corpus/` (standard CREATE / INSERT / SELECT, truth =
//! SQLite) and it is automatically replayed against both engines here. No code
//! change is needed to widen coverage.

use std::path::{Path, PathBuf};

use reddb_rql::{render_cell, CellType};
use reddb_types::Value;
use rusqlite::types::ValueRef;
use rusqlite::Connection as SqliteConnection;
use sqllogictest::{parse_file, Condition, DefaultColumnType, Record, SortMode};

use super::support::{persistent_test_runtime, PersistentRuntime};

/// The engine label the corpus keys its pinned divergences on (`skipif
/// reddb-server`). A query carrying this skip condition is a documented
/// divergence and is excluded from the equivalence assertion.
const REDDB_LABEL: &str = "reddb-server";

/// Whether a record's inline conditions exclude it from the named engine.
///
/// Mirrors sqllogictest's own `Condition::should_skip` (which is crate-private):
/// `skipif <engine>` skips when the label matches; `onlyif <engine>` skips when
/// it does not.
fn skipped_for(conditions: &[Condition], engine: &str) -> bool {
    conditions.iter().any(|cond| match cond {
        Condition::SkipIf { label } => label == engine,
        Condition::OnlyIf { label } => label != engine,
    })
}

/// The natural sqllogictest cell type for a RedDB logical value. Identical to
/// the single-engine harness so a value renders the same way on both pages.
fn natural_type(value: &Value) -> CellType {
    match value {
        Value::Integer(_)
        | Value::UnsignedInteger(_)
        | Value::Boolean(_)
        | Value::Timestamp(_)
        | Value::TimestampMs(_)
        | Value::Duration(_)
        | Value::Date(_)
        | Value::Time(_) => CellType::Integer,
        Value::Float(_) => CellType::Real,
        _ => CellType::Text,
    }
}

/// Render a SQLite cell with the same rules `render_cell` applies to a RedDB
/// value of the matching kind: NULL → `NULL`; integers in decimal; reals with
/// exactly three decimals; text passes printable ASCII through and scrubs the
/// rest to `@`, with the empty string marked `(empty)`.
fn render_sqlite_cell(value: ValueRef<'_>) -> String {
    match value {
        ValueRef::Null => "NULL".to_string(),
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Real(f) => format!("{f:.3}"),
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => render_text_bytes(bytes),
    }
}

/// Text scrubbing identical to `reddb_rql`'s `render_text`, applied to raw
/// bytes decoded as lossy UTF-8.
fn render_text_bytes(bytes: &[u8]) -> String {
    let raw = String::from_utf8_lossy(bytes);
    if raw.is_empty() {
        return "(empty)".to_string();
    }
    raw.chars()
        .map(|c| {
            if ('\u{20}'..='\u{7e}').contains(&c) {
                c
            } else {
                '@'
            }
        })
        .collect()
}

/// Apply the corpus's declared sort mode, mirroring the sqllogictest runner:
/// `nosort` keeps engine order, `rowsort` sorts whole rows, `valuesort`
/// flattens to one value per row then sorts.
fn apply_sort(mut rows: Vec<Vec<String>>, mode: Option<SortMode>) -> Vec<Vec<String>> {
    match mode.unwrap_or(SortMode::NoSort) {
        SortMode::NoSort => {}
        SortMode::RowSort => rows.sort_unstable(),
        SortMode::ValueSort => {
            rows = rows
                .iter()
                .flat_map(|row| row.iter())
                .map(|s| vec![s.clone()])
                .collect();
            rows.sort_unstable();
        }
    }
    rows
}

/// Run one SQL query against SQLite and render its result grid.
fn sqlite_rows(conn: &SqliteConnection, sql: &str) -> rusqlite::Result<Vec<Vec<String>>> {
    let mut stmt = conn.prepare(sql)?;
    let column_count = stmt.column_count();
    let mut out = Vec::new();
    let mut query_rows = stmt.query([])?;
    while let Some(row) = query_rows.next()? {
        let mut cells = Vec::with_capacity(column_count);
        for idx in 0..column_count {
            cells.push(render_sqlite_cell(row.get_ref(idx)?));
        }
        out.push(cells);
    }
    Ok(out)
}

/// Run one SQL query against the RedDB engine and render its result grid.
/// Returns `None` for a non-SELECT statement (no comparable result set).
fn reddb_rows(runtime: &PersistentRuntime, sql: &str) -> Result<Option<Vec<Vec<String>>>, String> {
    let result = runtime
        .execute_query(sql)
        .map_err(|err| format!("reddb execute_query failed: {err:?}"))?;
    if result.statement_type != "select" {
        return Ok(None);
    }
    let columns = &result.result.columns;
    let rows = result
        .result
        .records
        .iter()
        .map(|record| {
            columns
                .iter()
                .map(|col| match record.get(col) {
                    Some(value) => render_cell(value, natural_type(value)),
                    None => render_cell(&Value::Null, CellType::Text),
                })
                .collect::<Vec<String>>()
        })
        .collect();
    Ok(Some(rows))
}

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/reddb-rql/tests/corpus")
}

fn slt_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read corpus dir {}: {e}", dir.display()))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("slt"))
        .collect();
    files.sort();
    files
}

/// One pinned-divergence query that was excluded from the equivalence
/// assertion, for the run summary.
struct Excluded {
    file: String,
    sql: String,
}

/// Replay the whole standard-SQL corpus against both engines and assert
/// equivalence on every non-divergent query.
#[test]
fn standard_sql_corpus_matches_sqlite_oracle() {
    let dir = corpus_dir();
    let files = slt_files(&dir);
    assert!(
        !files.is_empty(),
        "no .slt corpus files found under {}",
        dir.display()
    );

    let mut compared = 0_usize;
    let mut excluded: Vec<Excluded> = Vec::new();

    for file in &files {
        let records: Vec<Record<DefaultColumnType>> = parse_file(file)
            .unwrap_or_else(|e| panic!("parse corpus {} failed: {e}", file.display()));

        // Fresh, independent state per file on both engines so nothing leaks.
        let runtime = persistent_test_runtime("surface-rql-sqlite-equivalence");
        let sqlite = SqliteConnection::open_in_memory()
            .expect("open in-memory SQLite oracle for differential conformance");

        let display = file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("<corpus>")
            .to_string();

        for record in records {
            match record {
                Record::Statement {
                    sql, conditions, ..
                } => {
                    // Setup runs on whichever engine the directive admits. The
                    // standard slice's setup is unconditional, but honor any
                    // engine gate exactly as the single-engine harness would.
                    if !skipped_for(&conditions, REDDB_LABEL) {
                        runtime.execute_query(&sql).unwrap_or_else(|err| {
                            panic!("{display}: reddb setup failed for `{sql}`: {err:?}")
                        });
                    }
                    if !skipped_for(&conditions, "SQLite") {
                        sqlite.execute_batch(&sql).unwrap_or_else(|err| {
                            panic!("{display}: sqlite setup failed for `{sql}`: {err}")
                        });
                    }
                }
                Record::Query {
                    sql,
                    conditions,
                    expected,
                    ..
                } => {
                    // A query the corpus pins as a RedDB divergence is excluded
                    // from the equivalence check (documented decision), never
                    // silently failing — it is collected for the run summary.
                    if skipped_for(&conditions, REDDB_LABEL) {
                        excluded.push(Excluded {
                            file: display.clone(),
                            sql: sql.clone(),
                        });
                        continue;
                    }

                    let sort_mode = match &expected {
                        sqllogictest::QueryExpect::Results { sort_mode, .. } => *sort_mode,
                        sqllogictest::QueryExpect::Error(_) => None,
                    };

                    let reddb = match reddb_rows(&runtime, &sql) {
                        Ok(Some(rows)) => apply_sort(rows, sort_mode),
                        Ok(None) => panic!("{display}: `{sql}` is not a SELECT on reddb"),
                        Err(err) => panic!("{display}: `{sql}` -> {err}"),
                    };

                    let oracle = sqlite_rows(&sqlite, &sql).unwrap_or_else(|err| {
                        panic!("{display}: sqlite query failed for `{sql}`: {err}")
                    });
                    let oracle = apply_sort(oracle, sort_mode);

                    assert!(
                        reddb == oracle,
                        "differential conformance divergence in {display}\n  query: {sql}\n  \
                         reddb : {reddb:?}\n  sqlite: {oracle:?}\n\
                         If this is an intended RQL-vs-standard-SQL decision, pin it in the \
                         corpus with `skipif reddb-server` and a reason (see PRD #1098 / #1100); \
                         otherwise it is a real regression."
                    );
                    compared += 1;
                }
                _ => {}
            }
        }
    }

    assert!(
        compared > 0,
        "differential harness compared no queries; the corpus produced no runnable SELECTs"
    );

    // Surface the excluded pinned-divergence queries so they are explicitly
    // annotated in the run output rather than silently absent.
    eprintln!(
        "RQL-vs-SQLite differential conformance: {compared} queries matched the SQLite oracle; \
         {} pinned divergence(s) excluded:",
        excluded.len()
    );
    for ex in &excluded {
        eprintln!("  - [{}] {}", ex.file, ex.sql);
    }
}
