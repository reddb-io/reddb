//! Differential conformance harness — pillar B of ADR 0053.
//!
//! Runs every `.slt` script in the standard-SQL corpus against **both** the
//! in-server RedDB engine and an in-memory SQLite oracle, then asserts that
//! the result sets are identical.
//!
//! The design follows Turso's "make the result equal SQLite" discipline:
//! correctness is defined by matching the authoritative SQL implementation,
//! not by whatever the engine currently emits.
//!
//! ## Divergence handling
//!
//! Records annotated with `skipif reddb-server` document a known dialect
//! divergence (ADR 0053). The differential harness skips them entirely so
//! the comparison is never attempted on a case where the result is expected
//! to differ. Records with `onlyif <engine>` narrow execution to a single
//! engine and are therefore unsuitable for cross-engine comparison; they are
//! also skipped.
//!
//! ## Cell rendering
//!
//! Both engines' outputs are rendered with the same rules that the existing
//! single-engine conformance harness uses (`src/conformance.rs`):
//! - `NULL`  →  literal `"NULL"`
//! - integer →  decimal i64
//! - real    →  three-decimal-place ASCII (`"3.000"`)
//! - text    →  printable ASCII passes through; other chars become `@`;
//!              empty string becomes `"(empty)"`

use std::path::{Path, PathBuf};

use reddb_rql::{render_cell, CellType};
use reddb_types::Value;
use sqllogictest::{Condition, DefaultColumnType, QueryExpect, Record, SortMode, StatementExpect};

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

fn render_sqlite_cell(value: &rusqlite::types::Value) -> String {
    use rusqlite::types::Value as Sv;
    match value {
        Sv::Null => "NULL".to_string(),
        Sv::Integer(i) => i.to_string(),
        Sv::Real(f) => format!("{:.3}", f),
        Sv::Text(s) => {
            if s.is_empty() {
                return "(empty)".to_string();
            }
            s.chars()
                .map(|c| {
                    let code = c as u32;
                    if code >= 0x20 && code <= 0x7e {
                        c
                    } else {
                        '@'
                    }
                })
                .collect()
        }
        Sv::Blob(b) => {
            if b.is_empty() {
                return "(empty)".to_string();
            }
            b.iter()
                .map(|&byte| {
                    if byte >= 0x20 && byte <= 0x7e {
                        byte as char
                    } else {
                        '@'
                    }
                })
                .collect()
        }
    }
}

/// Returns true when this record should be excluded from the differential
/// comparison.
///
/// `skipif reddb-server`: engine has a documented divergence for this case.
/// `onlyif <any>`: record is engine-scoped; one side can't run it.
fn skip_in_differential(conditions: &[Condition]) -> bool {
    conditions.iter().any(|c| match c {
        Condition::SkipIf { label } => label == "reddb-server",
        Condition::OnlyIf { .. } => true,
    })
}

fn run_reddb_query(
    runtime: &super::support::PersistentRuntime,
    sql: &str,
    file: &Path,
) -> Vec<Vec<String>> {
    let result = runtime.execute_query(sql).unwrap_or_else(|e| {
        panic!(
            "reddb query failed in {}:\nSQL: {sql}\nError: {e:?}",
            file.display()
        )
    });

    let columns = &result.result.columns;
    result
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
        .collect()
}

fn run_sqlite_query(conn: &rusqlite::Connection, sql: &str, file: &Path) -> Vec<Vec<String>> {
    let mut stmt = conn.prepare(sql).unwrap_or_else(|e| {
        panic!(
            "sqlite prepare failed in {}:\nSQL: {sql}\nError: {e}",
            file.display()
        )
    });
    let col_count = stmt.column_count();

    let mut result = Vec::new();
    let mut rows = stmt.query([]).unwrap_or_else(|e| {
        panic!(
            "sqlite query failed in {}:\nSQL: {sql}\nError: {e}",
            file.display()
        )
    });
    loop {
        let row = rows.next().unwrap_or_else(|e| {
            panic!(
                "sqlite row error in {}:\nSQL: {sql}\nError: {e}",
                file.display()
            )
        });
        let Some(row) = row else { break };
        let cells = (0..col_count)
            .map(|i| {
                let v: rusqlite::types::Value = row.get(i).unwrap_or_else(|e| {
                    panic!(
                        "sqlite column {i} error in {}:\nSQL: {sql}\nError: {e}",
                        file.display()
                    )
                });
                render_sqlite_cell(&v)
            })
            .collect::<Vec<_>>();
        result.push(cells);
    }
    result
}

fn apply_sort(rows: &mut Vec<Vec<String>>, sort_mode: Option<SortMode>) {
    match sort_mode {
        None | Some(SortMode::NoSort) => {}
        Some(SortMode::RowSort) => {
            rows.sort_unstable();
        }
        Some(SortMode::ValueSort) => {
            let flat: Vec<Vec<String>> = rows
                .iter()
                .flat_map(|row| row.iter())
                .map(|s| vec![s.clone()])
                .collect();
            *rows = flat;
            rows.sort_unstable();
        }
    }
}

/// Run every standard-SQL corpus script against both RedDB and SQLite and
/// assert that the result sets are identical.
///
/// A failure names the diverging file and query so the cause is immediately
/// actionable.
#[test]
fn rql_corpus_matches_sqlite_oracle() {
    let dir = corpus_dir();
    let files = slt_files(&dir);
    assert!(
        !files.is_empty(),
        "no .slt corpus files found under {}",
        dir.display()
    );

    for file in files {
        let records = sqllogictest::parse_file::<DefaultColumnType>(&file)
            .unwrap_or_else(|e| panic!("failed to parse {}: {e}", file.display()));

        let runtime = super::support::persistent_test_runtime("surface-rql-differential");
        let sqlite =
            rusqlite::Connection::open_in_memory().expect("sqlite in-memory connection should open");

        for record in &records {
            match record {
                Record::Statement {
                    conditions,
                    sql,
                    expected,
                    ..
                } => {
                    if skip_in_differential(conditions) {
                        continue;
                    }

                    let sqlite_ok = sqlite.execute_batch(sql).is_ok();
                    let reddb_ok = runtime.execute_query(sql).is_ok();

                    if matches!(expected, StatementExpect::Ok | StatementExpect::Count(_)) {
                        if !sqlite_ok {
                            panic!(
                                "sqlite statement failed (expected ok) in {}:\nSQL: {sql}",
                                file.display()
                            );
                        }
                        if !reddb_ok {
                            panic!(
                                "reddb statement failed (expected ok) in {}:\nSQL: {sql}",
                                file.display()
                            );
                        }
                    }
                }

                Record::Query {
                    conditions,
                    sql,
                    expected,
                    ..
                } => {
                    if skip_in_differential(conditions) {
                        continue;
                    }
                    let QueryExpect::Results { sort_mode, .. } = expected else {
                        continue;
                    };

                    let mut reddb_rows = run_reddb_query(&runtime, sql, &file);
                    let mut sqlite_rows = run_sqlite_query(&sqlite, sql, &file);

                    apply_sort(&mut reddb_rows, *sort_mode);
                    apply_sort(&mut sqlite_rows, *sort_mode);

                    assert_eq!(
                        reddb_rows,
                        sqlite_rows,
                        "differential mismatch in {}:\nSQL: {sql}",
                        file.display()
                    );
                }

                _ => {}
            }
        }
    }
}
