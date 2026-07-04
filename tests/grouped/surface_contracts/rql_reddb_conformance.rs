//! sqllogictest-format conformance harness for the **RedDB-only query
//! surfaces** that have no external oracle (ADR 0053, S3).
//!
//! Where the standard-SQL slice (`tests/rql_conformance.rs` over
//! `crates/reddb-rql/tests/corpus/`) draws its truth from the public SQLite
//! corpus, the surfaces covered here — vector search, the native `GRAPH`
//! command family, the four graph DSL modes (Gremlin / Cypher / SPARQL /
//! Path), natural-language, and vector extensions — are RedDB inventions.
//! Their expected results are
//! **hand-authored**: each value in a `query` block is what the surface's
//! semantics dictate (cosine ranking, neighbourhood reachability, shortest-path
//! hop counts), never blindly copied from engine output. Engine output is the
//! thing under test, not the source of truth.
//!
//! ## Volatile columns are projected away, not pinned
//!
//! The engine decorates every vector/graph result row with engine-internal,
//! run-varying columns — auto-assigned entity ids (`entity_id`, `node_id`,
//! `red_entity_id`), wall-clock stamps (`created_at`, `updated_at`), sequence
//! numbers, and the `red_*` capability metadata. Pinning those would freeze
//! non-determinism, so this harness projects each result down to a fixed
//! allowlist of **semantic, deterministic** columns (see [`KEEP`]) in a canonical
//! order. A golden therefore asserts the *meaning* of a result (which node,
//! which content, how many hops) and stays silent about engine bookkeeping.
//!
//! ## Accepted-but-unprojected surfaces are characterization-only
//!
//! The in-server executor parses and accepts the four graph DSL modes,
//! natural-language, and the `SEARCH SIMILAR … COLLECTION` form, but does not
//! yet *project* semantic columns for them — they return an empty column set.
//! Such a query has no hand-authorable result yet, so this harness reports it
//! as a completed statement and the corpus pins it with `statement ok`. Those
//! goldens are a **regression layer only** (clearly marked in each file and in
//! `reddb_corpus/README.md`); they characterize "the surface is accepted by
//! today's engine", never "this is the correct SPARQL/Gremlin/Cypher result".

use std::path::{Path, PathBuf};

use reddb_rql::{render_cell, CellType};
use reddb_server::RedDBError;
use reddb_types::Value;
use sqllogictest::{DBOutput, DefaultColumnType, Runner, DB};

/// Semantic, deterministic result columns, in canonical render order. Any
/// column not listed here (auto ids, timestamps, sequence numbers, `red_*`
/// capability metadata, raw distance/score floats whose exact value is
/// engine-defined rather than oracle-defined) is projected away before
/// comparison. A surface that yields none of these columns is treated as
/// accepted-but-unprojected and pinned with `statement ok` (characterization).
const KEEP: &[&str] = &[
    "label",
    "name",
    "content",
    "depth",
    "path_found",
    "hop_count",
    "total_weight",
    "nodes_visited",
    "negative_cycle_detected",
    "json_name",
    "json_visits",
    "json_column_name",
    "json_column_visits",
    "json_computed_name",
];

use super::support::PersistentRuntime;

/// One sqllogictest connection to the RedDB engine — a fresh persistent runtime.
struct EngineDb {
    runtime: PersistentRuntime,
}

impl EngineDb {
    fn connect() -> Result<Self, RedDBError> {
        Ok(Self {
            runtime: super::support::persistent_test_runtime("surface-rql-reddb-conformance"),
        })
    }
}

/// The natural sqllogictest cell type for a logical value. Mirrors the
/// standard-SQL harness so a RedDB value renders identically on both pages.
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

        // Project to the deterministic semantic allowlist, in canonical order.
        let present: Vec<&str> = KEEP
            .iter()
            .copied()
            .filter(|k| result.result.columns.iter().any(|c| c == k))
            .collect();

        // A select that yields none of the semantic columns is an
        // accepted-but-unprojected RedDB surface (a graph DSL mode,
        // natural-language, or the `SEARCH SIMILAR … COLLECTION` form). Report
        // it as a completed statement so the corpus can pin the accepted shape
        // with `statement ok` — a regression-only characterization, never
        // conformance truth.
        if present.is_empty() {
            return Ok(DBOutput::StatementComplete(
                result.result.records.len() as u64
            ));
        }

        let types = present
            .iter()
            .map(|_| DefaultColumnType::Any)
            .collect::<Vec<_>>();

        let rows = result
            .result
            .records
            .iter()
            .map(|record| {
                present
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
    Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/reddb-rql/tests/reddb_corpus")
}

fn slt_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read reddb corpus dir {}: {e}", dir.display()))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("slt"))
        .collect();
    files.sort();
    files
}

/// Runs the whole RedDB-only golden slice against the server engine. Each
/// script gets a fresh runtime so state never leaks between files.
#[test]
fn reddb_only_conformance_is_green_against_server_engine() {
    let dir = corpus_dir();
    let files = slt_files(&dir);
    assert!(
        !files.is_empty(),
        "no .slt goldens found under {}",
        dir.display()
    );

    for file in files {
        let mut runner = Runner::new(|| async { EngineDb::connect() });
        if let Err(err) = runner.run_file(&file) {
            panic!(
                "reddb-only conformance {} failed:\n{}",
                file.display(),
                err.display(true)
            );
        }
    }
}
