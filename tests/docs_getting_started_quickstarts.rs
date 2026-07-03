//! Executable getting-started quickstarts (issue #1656, PRD #1621 — DX slice 1).
//!
//! Each `docs/getting-started/quickstart-*.md` page walks a newcomer from an
//! `docker run` / embedded open to a first meaningful result for one data
//! model. Those pages are *executable documentation*: this test extracts the
//! ` ```sql ` fences from every quickstart and runs them against a fresh
//! in-memory RedDB, so a quickstart can never silently rot — a renamed keyword
//! or dropped column fails the docs CI lane instead of a reader's terminal.
//!
//! Contract, per quickstart:
//!   * every executed RQL statement must succeed (`Ok`), and
//!   * at least one statement must return a row — the "first meaningful result"
//!     the page promises.
//!
//! A ` ```sql ` fence whose first content line is `-- doctest:skip` documents a
//! statement that cannot run headless (e.g. `ASK ... USING openai`, which needs
//! a configured provider); it is shown to readers but not executed here.

use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::{RedDBOptions, RedDBRuntime};
use std::fs;
use std::path::PathBuf;

/// Every model gets exactly one runnable quickstart (issue #1656 acceptance:
/// "Ten runnable quickstarts, one per model").
const QUICKSTARTS: &[&str] = &[
    "quickstart-relational.md",
    "quickstart-document.md",
    "quickstart-key-value.md",
    "quickstart-graph.md",
    "quickstart-vector.md",
    "quickstart-timeseries.md",
    "quickstart-queue.md",
    "quickstart-spatial.md",
    "quickstart-vcs.md",
    "quickstart-ask-rag.md",
];

const SKIP_MARKER: &str = "-- doctest:skip";

fn getting_started_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR is the umbrella crate root == repo root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("getting-started")
}

/// Extract the bodies of every ` ```sql ` fenced block, in document order.
fn sql_fences(markdown: &str) -> Vec<String> {
    let mut fences = Vec::new();
    let mut in_fence = false;
    let mut current = String::new();
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if !in_fence {
            // Open only on a bare ```sql fence — leave ```bash / ```rust /
            // ```json (illustrative, non-executable) blocks alone.
            if trimmed == "```sql" {
                in_fence = true;
                current.clear();
            }
            continue;
        }
        if trimmed.starts_with("```") {
            fences.push(std::mem::take(&mut current));
            in_fence = false;
            continue;
        }
        current.push_str(line);
        current.push('\n');
    }
    fences
}

/// Split a fence into individual RQL statements: drop whole-line `--` comments,
/// split on `;`, and trim. Authored quickstart SQL never embeds a `;` inside a
/// string literal, so a plain split is safe here.
fn statements(fence: &str) -> Vec<String> {
    let without_comments: String = fence
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    without_comments
        .split(';')
        .map(|stmt| stmt.trim().to_string())
        .filter(|stmt| !stmt.is_empty())
        .collect()
}

fn is_skipped(fence: &str) -> bool {
    fence
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|line| line.starts_with(SKIP_MARKER))
}

/// Run one quickstart's SQL end-to-end against a fresh in-memory engine and
/// enforce the executable-docs contract.
fn run_quickstart(file: &str) {
    let path = getting_started_dir().join(file);
    let markdown = fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("missing quickstart {}: {err}", path.display()));

    let fences = sql_fences(&markdown);
    assert!(
        !fences.is_empty(),
        "{file}: no ```sql fences found — a quickstart must be executable"
    );

    // VCS quickstarts issue CHECKPOINT / AS OF, which resolve the committing
    // connection from the MVCC thread-local; bind one so those verbs work.
    let needs_connection = markdown.contains("CHECKPOINT") || markdown.contains("AS OF");
    if needs_connection {
        set_current_connection_id(1);
    }

    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory())
        .unwrap_or_else(|err| panic!("{file}: could not open in-memory RedDB: {err}"));

    let mut executed = 0usize;
    let mut produced_row = false;
    for fence in &fences {
        if is_skipped(fence) {
            continue;
        }
        for stmt in statements(fence) {
            let result = rt.execute_query(&stmt).unwrap_or_else(|err| {
                panic!("{file}: quickstart statement failed:\n  {stmt}\n-> {err:?}")
            });
            executed += 1;
            if !result.result.records.is_empty() {
                produced_row = true;
            }
        }
    }

    if needs_connection {
        clear_current_connection_id();
    }

    assert!(
        executed >= 3,
        "{file}: only {executed} runnable statements — a quickstart should create, \
         write, and read"
    );
    assert!(
        produced_row,
        "{file}: no statement returned a row — the quickstart never reaches its \
         'first meaningful result'"
    );
}

#[test]
fn every_model_has_a_runnable_quickstart() {
    for file in QUICKSTARTS {
        run_quickstart(file);
    }
}

/// The docs acceptance criterion requires a newcomer path from the docs root to
/// every quickstart in <=2 clicks. The sidebar renders on every page, so a
/// sidebar entry per quickstart is that path — guard it so a new quickstart is
/// never added without wiring its navigation.
#[test]
fn every_quickstart_is_reachable_from_the_sidebar() {
    let sidebar = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("_sidebar.md");
    let contents = fs::read_to_string(&sidebar).expect("read docs/_sidebar.md");
    for file in QUICKSTARTS {
        let link = format!("/getting-started/{file}");
        assert!(
            contents.contains(&link),
            "docs/_sidebar.md is missing a link to {link} — the quickstart is not \
             reachable in <=2 clicks"
        );
    }
}
