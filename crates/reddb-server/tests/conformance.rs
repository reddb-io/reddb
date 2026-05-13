//! Conformance test runner for the RQL parser.
//!
//! Reads every `*.toml` file under `tests/conformance/`, parses each as a
//! `ConformanceCase`, and verifies the parser output matches `expected_kind`.
//!
//! To add a case: copy any existing `.toml` file, edit `input` / `expected_kind`
//! / `source`. No code changes required. See `tests/conformance/README.md`.

mod support {
    #[path = "../support/parser_hardening/mod.rs"]
    pub mod parser_hardening;
}

use std::path::{Path, PathBuf};

use reddb_server::storage::query::{
    ast::{QueryExpr, QueryWithCte},
    parser::{self, ParseError, ParseErrorKind, Parser, ParserLimits},
};
use serde::Deserialize;
use support::parser_hardening::secret_redactor::{find_unmasked_secrets, UnmaskedHit};

#[derive(Debug, Deserialize)]
struct ConformanceCase {
    input: String,
    input_repeat_count: Option<usize>,
    input_prefix: Option<String>,
    input_suffix: Option<String>,
    expected_kind: Option<String>,
    expected_error_substring: Option<String>,
    expected_error_kind: Option<String>,
    max_depth: Option<usize>,
    max_input_bytes: Option<usize>,
    max_identifier_chars: Option<usize>,
    source: String,
    kind: String,
}

impl ConformanceCase {
    fn expanded_input(&self) -> String {
        let mut input = String::new();
        if let Some(prefix) = &self.input_prefix {
            input.push_str(prefix);
        }
        for _ in 0..self.input_repeat_count.unwrap_or(1) {
            input.push_str(&self.input);
        }
        if let Some(suffix) = &self.input_suffix {
            input.push_str(suffix);
        }
        input
    }

    fn parser_limits(&self) -> ParserLimits {
        let mut limits = ParserLimits::default();
        if let Some(max_depth) = self.max_depth {
            limits.max_depth = max_depth;
        }
        if let Some(max_input_bytes) = self.max_input_bytes {
            limits.max_input_bytes = max_input_bytes;
        }
        if let Some(max_identifier_chars) = self.max_identifier_chars {
            limits.max_identifier_chars = max_identifier_chars;
        }
        limits
    }

    fn parse(&self, input: &str) -> Result<QueryWithCte, ParseError> {
        if self.max_depth.is_some()
            || self.max_input_bytes.is_some()
            || self.max_identifier_chars.is_some()
        {
            let mut parser = Parser::with_limits(input, self.parser_limits())?;
            parser.parse_with_cte()
        } else {
            parser::parse(input)
        }
    }
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
        QueryExpr::DropGraph(_) => "DropGraph",
        QueryExpr::DropVector(_) => "DropVector",
        QueryExpr::DropDocument(_) => "DropDocument",
        QueryExpr::DropKv(_) => "DropKv",
        QueryExpr::DropCollection(_) => "DropCollection",
        QueryExpr::Truncate(_) => "Truncate",
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
        QueryExpr::AlterQueue(_) => "AlterQueue",
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
        QueryExpr::Grant(_) => "Grant",
        QueryExpr::Revoke(_) => "Revoke",
        QueryExpr::CreateIamPolicy { .. } => "CreateIamPolicy",
        QueryExpr::DropIamPolicy { .. } => "DropIamPolicy",
        QueryExpr::AttachPolicy { .. } => "AttachPolicy",
        QueryExpr::DetachPolicy { .. } => "DetachPolicy",
        QueryExpr::ShowPolicies { .. } => "ShowPolicies",
        QueryExpr::ShowEffectivePermissions { .. } => "ShowEffectivePermissions",
        QueryExpr::SimulatePolicy { .. } => "SimulatePolicy",
        _ => "Unknown",
    }
}

fn conformance_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("conformance")
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crate lives under <repo>/crates/reddb-server")
        .to_path_buf()
}

fn collect_toml_cases(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {}", dir.display(), e))
    {
        let entry = entry.unwrap_or_else(|e| panic!("read_dir entry under {}: {e}", dir.display()));
        let path = entry.path();
        if path.is_dir() {
            collect_toml_cases(&path, out);
        } else if path.extension().map(|x| x == "toml").unwrap_or(false) {
            out.push(path);
        }
    }
}

fn conformance_cases() -> Vec<(PathBuf, ConformanceCase)> {
    let dir = conformance_dir();
    let mut entries = Vec::new();
    collect_toml_cases(&dir, &mut entries);
    entries.sort();

    assert!(
        !entries.is_empty(),
        "no *.toml cases found in {}",
        dir.display()
    );

    entries
        .into_iter()
        .map(|path| {
            let raw = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
            let case: ConformanceCase = toml::from_str(&raw)
                .unwrap_or_else(|e| panic!("parse toml {}: {}", path.display(), e));
            (path, case)
        })
        .collect()
}

fn format_secret_violation(path: &Path, content: &str, hit: &UnmaskedHit) -> String {
    let prefix = &content[..hit.offset.min(content.len())];
    let line = prefix.bytes().filter(|&b| b == b'\n').count() + 1;
    let col = prefix.rsplit('\n').next().map(|s| s.len()).unwrap_or(0) + 1;
    format!(
        "  {}:{}:{} - pattern={} matched={:?}",
        path.display(),
        line,
        col,
        hit.pattern,
        hit.matched
    )
}

fn has_case<F>(cases: &[(PathBuf, ConformanceCase)], predicate: F) -> bool
where
    F: Fn(&str, &str) -> bool,
{
    cases.iter().any(|(_, case)| {
        if case.kind != "positive" {
            return false;
        }
        let input = case.expanded_input();
        let upper = input.to_ascii_uppercase();
        predicate(input.as_str(), upper.as_str())
    })
}

fn validate_source_reference(root: &std::path::Path, source: &str) -> Result<(), String> {
    if source.starts_with("proptest-regression:") {
        return Ok(());
    }

    let Some((file, line)) = source.rsplit_once(':') else {
        return Err("expected source in file:line form".to_string());
    };
    let line: usize = line
        .parse()
        .map_err(|_| format!("source line is not numeric: {line:?}"))?;
    if line == 0 {
        return Err("source line must be 1-based".to_string());
    }

    let path = root.join(file);
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let count = raw.lines().count();
    if line > count {
        return Err(format!(
            "source line {line} is past end of {} ({count} lines)",
            path.display()
        ));
    }

    Ok(())
}

fn parse_error_kind_name(kind: &ParseErrorKind) -> &'static str {
    match kind {
        ParseErrorKind::Syntax => "Syntax",
        ParseErrorKind::DepthLimit { .. } => "DepthLimit",
        ParseErrorKind::InputTooLarge { .. } => "InputTooLarge",
        ParseErrorKind::IdentifierTooLong { .. } => "IdentifierTooLong",
        ParseErrorKind::ValueOutOfRange { .. } => "ValueOutOfRange",
        ParseErrorKind::UnsupportedToken { .. } => "UnsupportedToken",
    }
}

#[test]
fn conformance_corpus() {
    let mut failures = Vec::new();

    for (path, case) in conformance_cases() {
        let file_name = path.file_name().unwrap().to_string_lossy().into_owned();
        let input = case.expanded_input();

        match case.kind.as_str() {
            "positive" => match case.parse(&input) {
                Ok(qwc) => {
                    let got = variant_name(&qwc.query);
                    let expected = case.expected_kind.as_deref().unwrap_or("<missing>");
                    if got != expected {
                        failures.push(format!(
                            "[{}] (source: {})\n  input:    {}\n  expected: {}\n  got:      {}",
                            file_name, case.source, input, expected, got
                        ));
                    }
                }
                Err(e) => {
                    let expected = case.expected_kind.as_deref().unwrap_or("<missing>");
                    failures.push(format!(
                        "[{}] (source: {})\n  input:    {}\n  expected: {} — parse error: {}",
                        file_name, case.source, input, expected, e
                    ));
                }
            },
            "negative" => match case.parse(&input) {
                Ok(_) => {
                    failures.push(format!(
                        "[{}] (source: {})\n  input:    {}\n  expected: parse failure, but it succeeded",
                        file_name, case.source, input
                    ));
                }
                Err(e) => {
                    let rendered = e.to_string();
                    let Some(expected_substring) = case.expected_error_substring.as_deref() else {
                        failures.push(format!(
                            "[{}] negative case is missing expected_error_substring",
                            file_name
                        ));
                        continue;
                    };
                    if !rendered.contains(expected_substring) {
                        failures.push(format!(
                            "[{}] (source: {})\n  input:    {}\n  expected error substring: {:?}\n  got:      {}",
                            file_name, case.source, input, expected_substring, rendered
                        ));
                    }
                    if let Some(expected_kind) = case.expected_error_kind.as_deref() {
                        let got_kind = parse_error_kind_name(&e.kind);
                        if got_kind != expected_kind {
                            failures.push(format!(
                                "[{}] (source: {})\n  input:    {}\n  expected error kind: {}\n  got:      {} ({:?})",
                                file_name, case.source, input, expected_kind, got_kind, e.kind
                            ));
                        }
                    }
                }
            },
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

#[test]
fn conformance_sources_exist() {
    let root = repo_root();
    let mut failures = Vec::new();

    for (path, case) in conformance_cases() {
        if let Err(e) = validate_source_reference(&root, &case.source) {
            let file_name = path.file_name().unwrap().to_string_lossy().into_owned();
            failures.push(format!("[{}] source {:?}: {}", file_name, case.source, e));
        }
    }

    if !failures.is_empty() {
        panic!(
            "conformance source reference failures:\n\n{}",
            failures.join("\n\n")
        );
    }
}

#[test]
fn positive_conformance_corpus_covers_documented_parser_surface() {
    let cases = conformance_cases();
    let positive_count = cases
        .iter()
        .filter(|(_, case)| case.kind == "positive")
        .count();

    assert!(
        positive_count >= 40,
        "expected at least 40 positive conformance cases for issue #231, found {positive_count}"
    );

    let required: &[(&str, fn(&str, &str) -> bool)] = &[
        ("SELECT", |_: &str, upper: &str| {
            upper.starts_with("SELECT ")
        }),
        ("INSERT", |_: &str, upper: &str| {
            upper.starts_with("INSERT ")
        }),
        ("CREATE TABLE", |_: &str, upper: &str| {
            upper.starts_with("CREATE TABLE ")
        }),
        ("CREATE INDEX", |_: &str, upper: &str| {
            upper.starts_with("CREATE INDEX ")
        }),
        ("CREATE QUEUE", |_: &str, upper: &str| {
            upper.starts_with("CREATE QUEUE ")
        }),
        ("CREATE TIMESERIES", |_: &str, upper: &str| {
            upper.starts_with("CREATE TIMESERIES ")
        }),
        ("CREATE VIEW", |_: &str, upper: &str| {
            upper.starts_with("CREATE VIEW ")
        }),
        ("UPDATE", |_: &str, upper: &str| {
            upper.starts_with("UPDATE ")
        }),
        ("DELETE", |_: &str, upper: &str| {
            upper.starts_with("DELETE ")
        }),
        ("QUEUE command", |_: &str, upper: &str| {
            upper.starts_with("QUEUE ")
        }),
        ("GRAPH", |_: &str, upper: &str| {
            upper.starts_with("MATCH ") || upper.starts_with("PATH ")
        }),
        ("VECTOR SEARCH", |_: &str, upper: &str| {
            upper.contains("VECTOR SEARCH") || upper.starts_with("SEARCH SIMILAR ")
        }),
        ("HYBRID", |_: &str, upper: &str| upper.contains("HYBRID")),
        ("FROM ANY", |_: &str, upper: &str| {
            upper.starts_with("FROM ANY ")
        }),
    ];

    let missing: Vec<&str> = required
        .iter()
        .filter_map(|(label, predicate)| (!has_case(&cases, *predicate)).then_some(*label))
        .collect();

    assert!(
        missing.is_empty(),
        "positive conformance corpus is missing required parser surfaces: {}",
        missing.join(", ")
    );
}

#[test]
fn conformance_corpus_contains_no_unmasked_secret_shapes() {
    let cases = conformance_cases();
    let mut violations = Vec::new();

    for (path, case) in cases {
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
        for hit in find_unmasked_secrets(&raw) {
            violations.push(format_secret_violation(&path, &raw, &hit));
        }

        let input = case.expanded_input();
        for hit in find_unmasked_secrets(&input) {
            violations.push(format_secret_violation(&path, &input, &hit));
        }
    }

    assert!(
        violations.is_empty(),
        "conformance corpus contains unmasked secret-shaped substrings for issue #97:\n{}",
        violations.join("\n")
    );
}
