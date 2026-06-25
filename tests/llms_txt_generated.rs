//! `docs/llms.txt` is generated, not hand-maintained (ADR 0061).
//!
//! The file is assembled from the engine's own authorities: the RQL section is
//! emitted by `reddb_rql::knowledge`, the same source the `reddb://knowledge/rql`
//! MCP resource serves. This test owns the assembly and pins the contract:
//!
//! - `llms_txt_is_in_sync` fails if `docs/llms.txt` drifts from the generator.
//! - run with `REGENERATE_LLMS=1` to (re)write `docs/llms.txt` from source.
//! - `rql_section_matches_mcp_resource` proves the file's RQL section is byte
//!   identical to what `red mcp` serves, so the two surfaces share one source.

use std::fs;
use std::path::PathBuf;

fn llms_txt_path() -> PathBuf {
    // CARGO_MANIFEST_DIR is the umbrella crate root == repo root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("llms.txt")
}

/// Assemble the full, generated `docs/llms.txt`. The stable header is the only
/// hand-authored prose; every fact-bearing section is generated from source.
fn generate_llms_txt() -> String {
    let mut out = String::new();
    out.push_str("# RedDB — LLM Reference (llms.txt)\n\n");
    out.push_str(
        "> RedDB is a multi-model database (documents, key-value, queues, graph, \
vault, config, and RQL-tabular collections) with its own SQL-family query \
language (RQL). RedWire (`red://` / `reds://`) is the principal transport.\n\n",
    );
    out.push_str(
        "This file is generated from the engine's own authorities — do not edit by \
hand. See `AGENTS.md` for the human/agent overview. Each section below is \
emitted from source so it cannot drift from the engine.\n\n",
    );
    out.push_str(&reddb_rql::knowledge::rql_llms_section());
    out.push('\n');
    out
}

/// Extract the RQL block between the generated markers.
fn extract_rql_section(contents: &str) -> &str {
    let begin = reddb_rql::knowledge::LLMS_BEGIN_MARKER;
    let end = reddb_rql::knowledge::LLMS_END_MARKER;
    let start = contents
        .find(begin)
        .expect("docs/llms.txt is missing the RQL begin marker");
    let stop = contents
        .find(end)
        .expect("docs/llms.txt is missing the RQL end marker");
    contents[start..stop + end.len()].trim()
}

#[test]
fn llms_txt_is_in_sync() {
    let path = llms_txt_path();
    let generated = generate_llms_txt();

    if std::env::var_os("REGENERATE_LLMS").is_some() {
        fs::write(&path, &generated).expect("write docs/llms.txt");
        return;
    }

    let on_disk = fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "could not read {}: {err}. Run `REGENERATE_LLMS=1 cargo test --test \
llms_txt_generated llms_txt_is_in_sync` to generate it.",
            path.display()
        )
    });

    assert_eq!(
        on_disk, generated,
        "docs/llms.txt is out of sync with the generator. Run \
`REGENERATE_LLMS=1 cargo test --test llms_txt_generated llms_txt_is_in_sync` to \
regenerate it."
    );
}

#[test]
fn rql_section_matches_mcp_resource() {
    let path = llms_txt_path();
    let on_disk = fs::read_to_string(&path).expect("read docs/llms.txt");
    let section = extract_rql_section(&on_disk);
    // The same generated source feeds docs/llms.txt and the MCP resource.
    assert_eq!(section, reddb_rql::knowledge::rql_llms_section());
    assert!(section.contains(&reddb_rql::knowledge::rql_reference_markdown()));
}
