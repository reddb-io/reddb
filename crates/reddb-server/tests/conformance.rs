//! Conformance test runner for the RQL parser.
//!
//! Reads every `*.toml` file under `tests/conformance/`, parses each as a
//! `ConformanceCase`, and verifies the parser output matches `expected_kind`.
//!
//! To add a case: copy any existing `.toml` file, edit `input` / `expected_kind`
//! / `source`. No code changes required. See `tests/conformance/README.md`.

use std::path::PathBuf;

use reddb_server::storage::query::{ast::QueryExpr, parser};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ConformanceCase {
    input: String,
    expected_kind: String,
    source: String,
    kind: String,
}

fn variant_name(q: &QueryExpr) -> &'static str {
    match q {
        QueryExpr::Table(_) => "Table",
        QueryExpr::Graph(_) => "Graph",
        QueryExpr::Join(_) => "Join",
        QueryExpr::Path(_) => "Path",
        QueryExpr::Vector(_) => "Vector",
        QueryExpr::Hybrid(_) => "Hybrid",
        QueryExpr::Insert(_) => "Insert",
        QueryExpr::Update(_) => "Update",
        QueryExpr::Delete(_) => "Delete",
        QueryExpr::CreateTable(_) => "CreateTable",
        QueryExpr::DropTable(_) => "DropTable",
        QueryExpr::AlterTable(_) => "AlterTable",
        QueryExpr::GraphCommand(_) => "GraphCommand",
        QueryExpr::SearchCommand(_) => "SearchCommand",
        QueryExpr::Ask(_) => "Ask",
        QueryExpr::CreateIndex(_) => "CreateIndex",
        QueryExpr::DropIndex(_) => "DropIndex",
        QueryExpr::ProbabilisticCommand(_) => "ProbabilisticCommand",
        QueryExpr::CreateTimeSeries(_) => "CreateTimeSeries",
        QueryExpr::DropTimeSeries(_) => "DropTimeSeries",
        QueryExpr::CreateQueue(_) => "CreateQueue",
        QueryExpr::DropQueue(_) => "DropQueue",
        QueryExpr::QueueCommand(_) => "QueueCommand",
        QueryExpr::CreateTree(_) => "CreateTree",
        QueryExpr::DropTree(_) => "DropTree",
        QueryExpr::TreeCommand(_) => "TreeCommand",
        QueryExpr::SetConfig { .. } => "SetConfig",
        QueryExpr::ShowConfig { .. } => "ShowConfig",
        QueryExpr::SetSecret { .. } => "SetSecret",
        QueryExpr::DeleteSecret { .. } => "DeleteSecret",
        QueryExpr::ShowSecrets { .. } => "ShowSecrets",
        QueryExpr::SetTenant(_) => "SetTenant",
        QueryExpr::ShowTenant => "ShowTenant",
        QueryExpr::ExplainAlter(_) => "ExplainAlter",
        QueryExpr::CreateMigration(_) => "CreateMigration",
        QueryExpr::ApplyMigration(_) => "ApplyMigration",
        QueryExpr::RollbackMigration(_) => "RollbackMigration",
        QueryExpr::ExplainMigration(_) => "ExplainMigration",
        QueryExpr::TransactionControl(_) => "TransactionControl",
        QueryExpr::MaintenanceCommand(_) => "MaintenanceCommand",
        QueryExpr::CreateSchema(_) => "CreateSchema",
        QueryExpr::DropSchema(_) => "DropSchema",
        QueryExpr::CreateSequence(_) => "CreateSequence",
        QueryExpr::DropSequence(_) => "DropSequence",
        QueryExpr::CopyFrom(_) => "CopyFrom",
        QueryExpr::CreateView(_) => "CreateView",
        QueryExpr::DropView(_) => "DropView",
        QueryExpr::RefreshMaterializedView(_) => "RefreshMaterializedView",
        QueryExpr::CreatePolicy(_) => "CreatePolicy",
        QueryExpr::DropPolicy(_) => "DropPolicy",
        QueryExpr::CreateServer(_) => "CreateServer",
        QueryExpr::DropServer(_) => "DropServer",
        QueryExpr::CreateForeignTable(_) => "CreateForeignTable",
        QueryExpr::DropForeignTable(_) => "DropForeignTable",
        _ => "Unknown",
    }
}

fn conformance_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("conformance")
}

#[test]
fn conformance_corpus() {
    let dir = conformance_dir();
    let entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {}", dir.display(), e))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "toml").unwrap_or(false))
        .collect();

    assert!(
        !entries.is_empty(),
        "no *.toml cases found in {}",
        dir.display()
    );

    let mut failures = Vec::new();

    for entry in entries {
        let path = entry.path();
        let raw =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
        let case: ConformanceCase = toml::from_str(&raw)
            .unwrap_or_else(|e| panic!("parse toml {}: {}", path.display(), e));

        let file_name = path.file_name().unwrap().to_string_lossy().into_owned();

        match case.kind.as_str() {
            "positive" => {
                match parser::parse(&case.input) {
                    Ok(qwc) => {
                        let got = variant_name(&qwc.query);
                        if got != case.expected_kind {
                            failures.push(format!(
                                "[{}] (source: {})\n  input:    {}\n  expected: {}\n  got:      {}",
                                file_name, case.source, case.input, case.expected_kind, got
                            ));
                        }
                    }
                    Err(e) => {
                        failures.push(format!(
                            "[{}] (source: {})\n  input:    {}\n  expected: {} — parse error: {}",
                            file_name, case.source, case.input, case.expected_kind, e
                        ));
                    }
                }
            }
            "negative" => {
                if parser::parse(&case.input).is_ok() {
                    failures.push(format!(
                        "[{}] (source: {})\n  input:    {}\n  expected: parse failure, but it succeeded",
                        file_name, case.source, case.input
                    ));
                }
            }
            other => {
                failures.push(format!(
                    "[{}] unknown kind {:?} — must be 'positive' or 'negative'",
                    file_name, other
                ));
            }
        }
    }

    if !failures.is_empty() {
        panic!("conformance failures:\n\n{}", failures.join("\n\n"));
    }
}
