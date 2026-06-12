//! Authority fence for the RQL language front-end (ADR 0053, PRD #1098).
//!
//! `reddb-io-rql` is the front-end crate that owns the RQL language pipeline —
//! the lexer (`Lexer`/`Token`/`Spanned`/`LexerError`), the parser
//! (`Parser`/`ParseErrorKind`/`SafeTokenDisplay`), the canonical AST
//! (`QueryExpr`/`ExprSubquery`), the SQL frontend command surface
//! (`SqlCommand`/`FrontendStatement`/`parse_frontend`), the DDL analyzer
//! (`analyze_create_table`/`resolve_sql_type_name`/`resolve_declared_data_type`),
//! the expression typer (`TypedExpr`/`TypedExprKind`/`type_expr`), the
//! Filter⇄Expr lowering (`filter_to_expr`/`expr_to_filter`), and the
//! storage-agnostic optimizer (`FilterRanker`/`RankedFilter`/`RankingConfig`/
//! `Decorrelator`/`SubqueryAnalysis`/`StatsCollector`). The server tree may
//! only *re-export* those items through its `storage::query::*` shims; it must
//! never *declare* them again.
//!
//! This mirrors the type-authority prior art in `reddb-types`'s test suite
//! (`tests/type_authority.rs`) and the layout-authority fence in `reddb-file`:
//! a mechanical fence that fails the instant a forbidden redeclaration
//! reappears in the server source tree.

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/reddb-rql has workspace root two levels up")
        .to_path_buf()
}

fn read(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path.as_ref())
        .unwrap_or_else(|err| panic!("read {}: {err}", path.as_ref().display()))
}

/// Drop the `#[cfg(test)]` tail so a test module's local fixtures never trip
/// the fence. Matches the `reddb-types`/`reddb-file` prior art's helper.
fn non_test_source(text: &str) -> &str {
    text.split("#[cfg(test)]").next().unwrap_or(text)
}

fn rust_files_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let entries =
            fs::read_dir(&path).unwrap_or_else(|err| panic!("read_dir {}: {err}", path.display()));
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }
    out
}

/// True when `text` *declares* a type named `name` (as opposed to re-exporting
/// it via `pub use`). Trailing-delimiter forms give a cheap word boundary so a
/// longer identifier like `RankedFilterSet` does not match `RankedFilter`.
fn declares_type(text: &str, name: &str) -> bool {
    ["enum", "struct"].iter().any(|kind| {
        [" ", "{", "<", "("]
            .iter()
            .any(|suffix| text.contains(&format!("{kind} {name}{suffix}")))
    })
}

/// True when `text` declares a free function (or method) named `name`.
/// Re-exports use `pub use`, never `fn name(`, so this only fires on a real
/// redeclaration of a front-end entry point.
fn declares_fn(text: &str, name: &str) -> bool {
    text.contains(&format!("fn {name}("))
}

/// The rql crate is the sole declaration site for the language front-end.
/// Anchors the positive side of the boundary so the fence below has meaning.
#[test]
fn rql_crate_owns_the_query_front_end() {
    let root = repo_root();
    let src = root.join("crates/reddb-rql/src");

    let lexer_rs = read(src.join("lexer.rs"));
    for name in ["Token", "Spanned", "LexerError"] {
        assert!(
            declares_type(&lexer_rs, name),
            "reddb-rql/src/lexer.rs must declare the `{name}` lexer type"
        );
    }
    assert!(
        declares_type(&lexer_rs, "Lexer"),
        "reddb-rql/src/lexer.rs must declare the `Lexer` struct"
    );

    let error_rs = read(src.join("parser/error.rs"));
    for name in ["ParseErrorKind", "SafeTokenDisplay"] {
        assert!(
            declares_type(&error_rs, name),
            "reddb-rql/src/parser/error.rs must declare the `{name}` parser type"
        );
    }

    let core_rs = read(src.join("core.rs"));
    assert!(
        declares_type(&core_rs, "QueryExpr"),
        "reddb-rql/src/core.rs must declare the `QueryExpr` AST enum"
    );
    let ast_rs = read(src.join("ast.rs"));
    assert!(
        declares_type(&ast_rs, "ExprSubquery"),
        "reddb-rql/src/ast.rs must declare the `ExprSubquery` struct"
    );

    let sql_rs = read(src.join("sql.rs"));
    for name in ["SqlCommand", "FrontendStatement"] {
        assert!(
            declares_type(&sql_rs, name),
            "reddb-rql/src/sql.rs must declare the `{name}` enum"
        );
    }
    assert!(
        declares_fn(&sql_rs, "parse_frontend"),
        "reddb-rql/src/sql.rs must declare the `parse_frontend` entry point"
    );

    let analyzer_rs = read(src.join("analyzer.rs"));
    for name in [
        "analyze_create_table",
        "resolve_sql_type_name",
        "resolve_declared_data_type",
    ] {
        assert!(
            declares_fn(&analyzer_rs, name),
            "reddb-rql/src/analyzer.rs must declare the `{name}` analyzer entry point"
        );
    }

    let typing_rs = read(src.join("expr_typing.rs"));
    for name in ["TypedExpr", "TypedExprKind"] {
        assert!(
            declares_type(&typing_rs, name),
            "reddb-rql/src/expr_typing.rs must declare the `{name}` type"
        );
    }
    assert!(
        declares_fn(&typing_rs, "type_expr"),
        "reddb-rql/src/expr_typing.rs must declare the `type_expr` entry point"
    );

    let lowering_rs = read(src.join("sql_lowering.rs"));
    for name in ["filter_to_expr", "expr_to_filter"] {
        assert!(
            declares_fn(&lowering_rs, name),
            "reddb-rql/src/sql_lowering.rs must declare the `{name}` lowering entry point"
        );
    }

    let filter_rank_rs = read(src.join("optimizer/filter_rank.rs"));
    for name in ["FilterRanker", "RankedFilter", "RankingConfig"] {
        assert!(
            declares_type(&filter_rank_rs, name),
            "reddb-rql/src/optimizer/filter_rank.rs must declare the `{name}` struct"
        );
    }
    let decorrelate_rs = read(src.join("optimizer/decorrelate.rs"));
    for name in ["Decorrelator", "SubqueryAnalysis"] {
        assert!(
            declares_type(&decorrelate_rs, name),
            "reddb-rql/src/optimizer/decorrelate.rs must declare the `{name}` struct"
        );
    }
    let stats_rs = read(src.join("optimizer/stats.rs"));
    assert!(
        declares_type(&stats_rs, "StatsCollector"),
        "reddb-rql/src/optimizer/stats.rs must declare the `StatsCollector` struct"
    );
}

/// The fence: the server source tree must never redeclare a language
/// front-end item. Reintroduce any declaration below and this test fails.
#[test]
fn server_must_not_redeclare_the_query_front_end() {
    let root = repo_root();
    let server_src = root.join("crates/reddb-server/src");

    // Distinctive front-end type names — they have zero legitimate collisions
    // in the server tree, so the fence applies tree-wide. (`Parser`,
    // `ParseError`, `ColumnStats`, and `TableStats` are deliberately omitted:
    // they collide with the CLI/MCP parsers and the cost-based planner's own
    // stats structs, which legitimately stayed in `reddb-server`.)
    const TYPE_NAMES: &[&str] = &[
        // lexer
        "Lexer",
        "LexerError",
        "Spanned",
        // parser
        "ParseErrorKind",
        "SafeTokenDisplay",
        // canonical AST
        "QueryExpr",
        "ExprSubquery",
        // sql frontend command surface
        "SqlCommand",
        "FrontendStatement",
        // expression typer
        "TypedExpr",
        "TypedExprKind",
        // storage-agnostic optimizer
        "FilterRanker",
        "RankedFilter",
        "RankingConfig",
        "Decorrelator",
        "SubqueryAnalysis",
        "StatsCollector",
    ];
    // Front-end entry points re-homed into reddb-io-rql (ADR 0053).
    const FRONT_END_FNS: &[&str] = &[
        "parse_frontend",
        "analyze_create_table",
        "resolve_sql_type_name",
        "resolve_declared_data_type",
        "type_expr",
        "filter_to_expr",
        "expr_to_filter",
    ];

    for path in rust_files_under(&server_src) {
        let raw = read(&path);
        let text = non_test_source(&raw);
        let rel = path.strip_prefix(&root).unwrap_or(path.as_path());

        for name in TYPE_NAMES {
            assert!(
                !declares_type(text, name),
                "{} declares `{name}`; re-export `reddb_rql::{name}` instead of redeclaring it",
                rel.display()
            );
        }
        for name in FRONT_END_FNS {
            assert!(
                !declares_fn(text, name),
                "{} declares front-end entry point `{name}`; call `reddb_rql::{name}` instead",
                rel.display()
            );
        }
    }
}

/// The `storage::query` shims must stay pure re-exports of the front-end crate.
/// Guards the boundary from the positive side: if a shim is ever replaced by a
/// real declaration, its `pub use reddb_rql::` line disappears and this fails.
#[test]
fn front_end_shims_reexport_from_rql_crate() {
    let root = repo_root();
    let query = root.join("crates/reddb-server/src/storage/query");
    for shim in [
        "lexer.rs",
        "parser.rs",
        "ast.rs",
        "analyzer.rs",
        "expr_typing.rs",
        "filter_optimizer.rs",
        "optimizer.rs",
        "modes.rs",
        "renderer.rs",
        "sql.rs",
        "sql_lowering.rs",
        "planner/optimizer.rs",
        "planner/pathkeys.rs",
        "planner/projections.rs",
        "planner/rewriter.rs",
    ] {
        let text = read(query.join(shim));
        assert!(
            text.contains("pub use reddb_rql::"),
            "storage/query/{shim} must re-export from reddb_rql, not declare items locally"
        );
    }
}
