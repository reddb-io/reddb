//! Agent-facing RQL knowledge reference — generated from the engine's own
//! authorities (ADR 0061, "Agent-Facing Knowledge & MCP Surface").
//!
//! The volatile facts about the RQL surface are emitted straight from the
//! source of truth so the reference cannot drift from the engine:
//!
//! - the keyword vocabulary comes from the lexer ([`crate::lexer`]), the RQL
//!   keyword authority, and
//! - the built-in function catalog comes from
//!   [`reddb_types::function_catalog`].
//!
//! Nothing in this module hand-maintains *which* keywords or functions exist —
//! that is read from the catalogs above. The same generated content is served
//! two ways from this one source: as the `reddb://knowledge/rql` MCP resource
//! and as the RQL section of the generated `docs/llms.txt`. The anti-drift
//! tests at the bottom pin the "generated, exhaustive, in-sync" contract.

use reddb_types::function_catalog::{FunctionKind, FUNCTION_CATALOG};

/// Canonical URI for the RQL knowledge resource served over MCP.
pub const RESOURCE_URI: &str = "reddb://knowledge/rql";

/// Short human title for the RQL knowledge resource.
pub const RESOURCE_TITLE: &str = "RedDB RQL Reference";

/// One-line description of the RQL knowledge resource.
pub const RESOURCE_DESCRIPTION: &str =
    "Generated RQL grammar reference: every keyword and built-in function the engine knows.";

/// Markers delimiting the generated RQL block inside `docs/llms.txt`. The
/// `docs/llms.txt` sync test reads the text between these markers and asserts
/// it equals [`rql_reference_markdown`], so the file is generated, not
/// hand-maintained.
pub const LLMS_BEGIN_MARKER: &str = "<!-- BEGIN GENERATED: rql -->";
/// Closing marker for the generated RQL block in `docs/llms.txt`.
pub const LLMS_END_MARKER: &str = "<!-- END GENERATED: rql -->";

/// Canonical RQL keyword catalog: every reserved word the lexer recognises as a
/// dedicated token.
///
/// This list MUST mirror the keyword match in
/// `crate::lexer::Lexer::scan_identifier`; the [`tests::keywords_are_recognised`]
/// anti-drift test fails the build if any entry here is not actually a keyword.
///
/// The list-queue verbs (`LPUSH` / `RPUSH` / `LPOP` / `RPOP`) are intentionally
/// excluded: the lexer maps them to `Token::Ident`, so they are operations
/// resolved later, not reserved words.
pub const KEYWORDS: &[&str] = &[
    "ACK",
    "ADD",
    "ALGORITHM",
    "ALL",
    "ALTER",
    "ANALYZE",
    "AND",
    "AS",
    "ASC",
    "ATTACH",
    "AVG",
    "BEGIN",
    "BETWEEN",
    "BY",
    "CASCADE",
    "CENTRALITY",
    "CLUSTERING",
    "COLLECTION",
    "COLUMN",
    "COMMIT",
    "COMMUNITY",
    "COMPONENTS",
    "COMPRESS",
    "CONTAINS",
    "COPY",
    "COSINE",
    "COUNT",
    "CREATE",
    "CROSS",
    "CURRENT",
    "CYCLES",
    "DATA",
    "DEFAULT",
    "DELETE",
    "DELIMITER",
    "DEPTH",
    "DESC",
    "DETACH",
    "DIRECTION",
    "DISABLE",
    "DISTINCT",
    "DOCUMENT",
    "DROP",
    "EDGE",
    "ENABLE",
    "ENDS",
    "ENRICH",
    "EXISTS",
    "EXPLAIN",
    "FALSE",
    "FIRST",
    "FOLLOWING",
    "FOR",
    "FOREIGN",
    "FORMAT",
    "FROM",
    "FULL",
    "FUSION",
    "FUZZY",
    "GAP",
    "GRAPH",
    "GROUP",
    "HASH",
    "HEADER",
    "HYBRID",
    "IF",
    "IN",
    "INCLUDE",
    "INCREMENT",
    "INDEX",
    "INNER",
    "INNERPRODUCT",
    "INNER_PRODUCT",
    "INSERT",
    "INTERSECTION",
    "INTO",
    "IS",
    "JOIN",
    "JSON",
    "K",
    "KEY",
    "KV",
    "L2",
    "LAST",
    "LEFT",
    "LEVEL",
    "LIKE",
    "LIMIT",
    "LIST",
    "MATCH",
    "MATERIALIZED",
    "MAX",
    "MAXITERATIONS",
    "MAXLENGTH",
    "MAX_ITERATIONS",
    "MAX_LENGTH",
    "METADATA",
    "METRIC",
    "MIN",
    "MINSCORE",
    "MIN_SCORE",
    "MODE",
    "NACK",
    "NEIGHBORHOOD",
    "NODE",
    "NOT",
    "NULL",
    "NULLS",
    "OF",
    "OFFSET",
    "ON",
    "OPTIONS",
    "OR",
    "ORDER",
    "OUTER",
    "OVER",
    "PARTITION",
    "PATH",
    "PEEK",
    "POLICY",
    "POP",
    "PRECEDING",
    "PRIMARY",
    "PRIORITY",
    "PROPERTIES",
    "PURGE",
    "PUSH",
    "QUEUE",
    "RANGE",
    "RECURSIVE",
    "REFRESH",
    "RELEASE",
    "RENAME",
    "RERANK",
    "RETENTION",
    "RETURN",
    "RETURNING",
    "RIGHT",
    "ROLLBACK",
    "ROW",
    "ROWS",
    "RRF",
    "SAVEPOINT",
    "SCHEMA",
    "SEARCH",
    "SECURITY",
    "SELECT",
    "SEQUENCE",
    "SERVER",
    "SESSIONIZE",
    "SET",
    "SHORTESTPATH",
    "SHORTEST_PATH",
    "SIMILAR",
    "START",
    "STARTS",
    "STRATEGY",
    "SUM",
    "TABLE",
    "TEXT",
    "THRESHOLD",
    "TIMESERIES",
    "TO",
    "TOPOLOGICALSORT",
    "TOPOLOGICAL_SORT",
    "TRANSACTION",
    "TRAVERSE",
    "TREE",
    "TRUE",
    "TRUNCATE",
    "UNBOUNDED",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USING",
    "VACUUM",
    "VALUES",
    "VECTOR",
    "VECTORS",
    "VIA",
    "VIEW",
    "WEIGHT",
    "WHERE",
    "WITH",
    "WORK",
    "WRAPPER",
];

/// Distinct built-in function names, sorted, taken from the engine's static
/// [`FUNCTION_CATALOG`]. The catalog carries one row per overload, so a name
/// (e.g. `COUNT`) can appear several times — this collapses them.
pub fn function_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = FUNCTION_CATALOG.iter().map(|entry| entry.name).collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Distinct function names of a single [`FunctionKind`], sorted. A name is
/// classified by the kind of its first catalog row (overloads of one name share
/// a kind in the engine's catalog).
fn function_names_of_kind(kind: FunctionKind) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Vec::new();
    for entry in FUNCTION_CATALOG {
        if entry.kind == kind && !names.contains(&entry.name) {
            // Only count a name under its first-seen kind so a name is never
            // listed twice across kind sections.
            let first_kind = FUNCTION_CATALOG
                .iter()
                .find(|candidate| candidate.name == entry.name)
                .map(|first| first.kind);
            if first_kind == Some(kind) {
                names.push(entry.name);
            }
        }
    }
    names.sort_unstable();
    names.dedup();
    names
}

fn render_code_list(names: &[&str]) -> String {
    names
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Generate the canonical RQL reference as Markdown, sourced entirely from the
/// engine's keyword and function authorities. This single string is what the
/// MCP `reddb://knowledge/rql` resource serves and what `docs/llms.txt` embeds.
pub fn rql_reference_markdown() -> String {
    let keywords = KEYWORDS;
    let functions = function_names();

    let mut out = String::new();
    out.push_str("# RedDB RQL Reference\n\n");
    out.push_str(
        "RQL (RedDB Query Language) is RedDB's SQL-family query surface across its \
multi-model store (documents, key-value, queues, graph, vault, config, and \
RQL-tabular collections).\n\n",
    );
    out.push_str(
        "This reference is generated from the `reddb-io-rql` lexer (keyword authority) \
and the `reddb-io-types` function catalog. Do not edit by hand — regenerate from \
the engine.\n\n",
    );

    out.push_str(&format!("## Keywords ({})\n\n", keywords.len()));
    out.push_str(
        "RQL recognises the following reserved keywords (case-insensitive at the \
lexer):\n\n",
    );
    out.push_str(&render_code_list(keywords));
    out.push_str("\n\n");

    out.push_str(&format!("## Functions ({})\n\n", functions.len()));
    out.push_str("RQL provides these built-in functions, grouped by kind:\n\n");

    for (kind, title) in [
        (FunctionKind::Aggregate, "Aggregate functions"),
        (FunctionKind::Scalar, "Scalar functions"),
        (FunctionKind::Window, "Window functions"),
        (FunctionKind::Volatile, "Volatile functions"),
    ] {
        let names = function_names_of_kind(kind);
        if names.is_empty() {
            continue;
        }
        out.push_str(&format!("### {title}\n\n"));
        out.push_str(&render_code_list(&names));
        out.push_str("\n\n");
    }

    // Trim the trailing blank line so the body ends with exactly one newline.
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

/// The RQL block as embedded in `docs/llms.txt`: the generated reference fenced
/// by the begin/end markers. Emitting the markers here keeps `docs/llms.txt`
/// and the MCP resource fed by one source.
pub fn rql_llms_section() -> String {
    format!(
        "{begin}\n{body}\n{end}",
        begin = LLMS_BEGIN_MARKER,
        body = rql_reference_markdown(),
        end = LLMS_END_MARKER,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::{Lexer, Token};

    /// catalog ⊆ engine: every keyword we publish must really be recognised by
    /// the lexer as a dedicated token (not fall through to a bare identifier).
    #[test]
    fn keywords_are_recognised() {
        for &keyword in KEYWORDS {
            let mut lexer = Lexer::new(keyword);
            let spanned = lexer
                .next_token()
                .unwrap_or_else(|err| panic!("keyword {keyword:?} failed to lex: {err:?}"));
            assert!(
                !matches!(spanned.token, Token::Ident(_)),
                "{keyword:?} is in the knowledge catalog but the lexer treats it as a \
plain identifier — it is not a real RQL keyword"
            );
        }
    }

    /// The published keyword list is sorted and deduplicated, so the generated
    /// reference is stable and reviewable.
    #[test]
    fn keyword_list_is_sorted_and_unique() {
        let mut sorted = KEYWORDS.to_vec();
        sorted.sort_unstable();
        assert_eq!(KEYWORDS.to_vec(), sorted, "KEYWORDS must be sorted");
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            KEYWORDS.len(),
            "KEYWORDS must not contain duplicates"
        );
    }

    /// A sanity guard that the recognised-keyword check has teeth: a word that
    /// is not a keyword must lex to an identifier.
    #[test]
    fn non_keyword_lexes_as_identifier() {
        let mut lexer = Lexer::new("not_a_keyword_xyzzy");
        let spanned = lexer.next_token().expect("lex");
        assert!(matches!(spanned.token, Token::Ident(_)));
    }

    /// engine ⊆ catalog (functions): every function the engine knows, from the
    /// authoritative `FUNCTION_CATALOG`, appears in the generated reference.
    #[test]
    fn reference_lists_every_function() {
        let reference = rql_reference_markdown();
        for name in function_names() {
            assert!(
                reference.contains(&format!("`{name}`")),
                "function {name:?} from FUNCTION_CATALOG is missing from the generated \
RQL reference"
            );
        }
    }

    /// Every catalogued keyword appears in the generated reference too.
    #[test]
    fn reference_lists_every_keyword() {
        let reference = rql_reference_markdown();
        for &keyword in KEYWORDS {
            assert!(
                reference.contains(&format!("`{keyword}`")),
                "keyword {keyword:?} is missing from the generated RQL reference"
            );
        }
    }

    /// The reference is deterministic (pure function of the catalogs).
    #[test]
    fn reference_is_deterministic() {
        assert_eq!(rql_reference_markdown(), rql_reference_markdown());
    }

    /// The `docs/llms.txt` block wraps exactly the reference between markers.
    #[test]
    fn llms_section_wraps_reference() {
        let section = rql_llms_section();
        assert!(section.starts_with(LLMS_BEGIN_MARKER));
        assert!(section.ends_with(LLMS_END_MARKER));
        assert!(section.contains(&rql_reference_markdown()));
    }
}
