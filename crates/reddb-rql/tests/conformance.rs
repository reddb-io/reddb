//! sqllogictest-format conformance harness (ADR 0053, S1 tracer bullet).
//!
//! Drives every `.slt` script under `tests/corpus/` end-to-end against the
//! current **in-server engine** (`reddb-server`'s `RedDBRuntime`). The corpus
//! is the standard-SQL slice sourced from the public SQLite sqllogictest
//! corpus; this run is the behavioral net later extraction slices must keep
//! green.
//!
//! The engine lives behind a dev-dependency so the published `reddb-io-rql`
//! crate graph keeps its single edge to `reddb-io-types` — only this test
//! target reaches `reddb-server`.

use std::path::{Path, PathBuf};

use reddb_rql::{render_cell, CellType};
use reddb_server::{RedDBError, RedDBRuntime};
use reddb_types::Value;
use sqllogictest::{DBOutput, DefaultColumnType, Runner, DB};

/// One sqllogictest connection to the RedDB engine — a fresh in-memory runtime.
struct EngineDb {
    runtime: RedDBRuntime,
}

impl EngineDb {
    fn connect() -> Result<Self, RedDBError> {
        Ok(Self {
            runtime: RedDBRuntime::in_memory()?,
        })
    }
}

/// The natural sqllogictest cell type for a logical value. Mirrors SQLite's
/// three coercion classes so the engine's intrinsic value kinds render the
/// same way the corpus's `query <types>` headers expect.
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

impl DB for EngineDb {
    type Error = RedDBError;
    type ColumnType = DefaultColumnType;

    fn run(&mut self, sql: &str) -> Result<DBOutput<DefaultColumnType>, RedDBError> {
        let result = self.runtime.execute_query(sql)?;

        if result.statement_type != "select" {
            return Ok(DBOutput::StatementComplete(result.affected_rows));
        }

        let columns = &result.result.columns;
        let types = columns
            .iter()
            .map(|_| DefaultColumnType::Any)
            .collect::<Vec<_>>();

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
            .collect::<Vec<_>>();

        Ok(DBOutput::Rows { types, rows })
    }

    fn engine_name(&self) -> &str {
        "reddb-server"
    }
}

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
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

/// Runs the whole corpus slice against the server engine. Each script gets a
/// fresh runtime so state never leaks between files.
#[test]
fn conformance_corpus_is_green_against_server_engine() {
    let dir = corpus_dir();
    let files = slt_files(&dir);
    assert!(
        !files.is_empty(),
        "no .slt corpus files found under {}",
        dir.display()
    );

    for file in files {
        let mut runner = Runner::new(|| async { EngineDb::connect() });
        if let Err(err) = runner.run_file(&file) {
            panic!(
                "conformance corpus {} failed:\n{}",
                file.display(),
                err.display(true)
            );
        }
    }
}
