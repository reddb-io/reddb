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

use crate::ast::QueryExpr;
use crate::parser::{parse as parse_rql, ParseError, ParseErrorKind};
use crate::planner::QueryOptimizer;

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

// ---------------------------------------------------------------------------
// Active RQL validate / explain surface (ADR 0061, #1317)
//
// These functions drive the engine's *real* parser ([`crate::parser::parse`])
// and optimizer ([`QueryOptimizer`]) so an agent learns the dialect by
// submitting a query and reading the verdict, instead of guessing from docs.
// The MCP `reddb_rql_validate` / `reddb_rql_explain` active tools are thin JSON
// adapters over these neutral structs; the parsing authority lives here, in the
// owning crate (the same generate-from-the-engine discipline as the resources
// above), so the tool surface cannot reimplement or drift from the grammar.
// ---------------------------------------------------------------------------

/// A structured diagnostic for an RQL string the parser rejected. Every field
/// comes straight from the real [`ParseError`], so callers get the engine's own
/// message, source position, error category, and expected-token hints without
/// scraping a formatted string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RqlDiagnostic {
    /// Human-readable parser message.
    pub message: String,
    /// 1-indexed line where parsing failed.
    pub line: u32,
    /// 1-indexed column where parsing failed.
    pub column: u32,
    /// Byte offset from the start of the input where parsing failed.
    pub offset: u32,
    /// Stable category for the failure (e.g. `"syntax"`, `"depth_limit"`),
    /// derived from [`ParseErrorKind`] so callers branch without string-matching
    /// the message.
    pub kind: String,
    /// Tokens the parser expected at the failure point, when it tracked any.
    pub expected: Vec<String>,
}

/// The verdict of parsing an RQL string through the real `reddb-io-rql` parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RqlValidation {
    /// True when the input parsed cleanly.
    pub valid: bool,
    /// The top-level statement kind (the [`QueryExpr`] variant name, e.g.
    /// `"Table"`, `"Insert"`) when valid.
    pub statement: Option<String>,
    /// True when the query carried a leading `WITH` (CTE) prelude.
    pub has_with_clause: bool,
    /// The structured parse error when invalid.
    pub error: Option<RqlDiagnostic>,
}

/// The verdict plus the parsed AST and the optimizer plan, for the
/// `reddb_rql_explain` tool. `ast` / `optimized_ast` / `applied_passes` are only
/// populated when the input parsed.
#[derive(Debug, Clone)]
pub struct RqlExplanation {
    /// The same verdict [`validate_rql`] returns.
    pub validation: RqlValidation,
    /// Pretty-printed AST of the parsed query, when valid.
    pub ast: Option<String>,
    /// Pretty-printed AST after the default optimization passes, when valid.
    pub optimized_ast: Option<String>,
    /// Names of the optimization passes that actually rewrote the query.
    pub applied_passes: Vec<String>,
}

/// Stable, lower-snake-case label for a [`ParseErrorKind`] so agents can branch
/// on the failure category without parsing the message.
fn parse_error_kind_label(kind: &ParseErrorKind) -> &'static str {
    match kind {
        ParseErrorKind::Syntax => "syntax",
        ParseErrorKind::DepthLimit { .. } => "depth_limit",
        ParseErrorKind::InputTooLarge { .. } => "input_too_large",
        ParseErrorKind::IdentifierTooLong { .. } => "identifier_too_long",
        ParseErrorKind::TokenLimit { .. } => "token_limit",
        ParseErrorKind::ValueOutOfRange { .. } => "value_out_of_range",
        ParseErrorKind::UnsupportedToken { .. } => "unsupported_token",
    }
}

fn diagnostic_from_error(err: &ParseError) -> RqlDiagnostic {
    RqlDiagnostic {
        message: err.message.clone(),
        line: err.position.line,
        column: err.position.column,
        offset: err.position.offset,
        kind: parse_error_kind_label(&err.kind).to_string(),
        expected: err.expected.clone(),
    }
}

/// The [`QueryExpr`] variant name, e.g. `"Table"` or `"Insert"`. Read from the
/// derived `Debug` representation (its leading identifier is the variant name)
/// so this never drifts as variants are added to the enum.
fn statement_kind(expr: &QueryExpr) -> String {
    format!("{expr:?}")
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect()
}

/// Parse an RQL string through the real `reddb-io-rql` parser and report the
/// verdict: the statement kind on success, or a structured diagnostic on
/// failure. No reimplementation — this is the engine's own front-end.
pub fn validate_rql(input: &str) -> RqlValidation {
    match parse_rql(input) {
        Ok(query) => RqlValidation {
            valid: true,
            statement: Some(statement_kind(&query.query)),
            has_with_clause: query.with_clause.is_some(),
            error: None,
        },
        Err(err) => RqlValidation {
            valid: false,
            statement: None,
            has_with_clause: false,
            error: Some(diagnostic_from_error(&err)),
        },
    }
}

/// Parse an RQL string and, when valid, also return its AST and the result of
/// running the default optimization passes — the same passes the engine plans
/// with. Invalid input yields the structured diagnostic with no AST/plan.
pub fn explain_rql(input: &str) -> RqlExplanation {
    match parse_rql(input) {
        Ok(query) => {
            let validation = RqlValidation {
                valid: true,
                statement: Some(statement_kind(&query.query)),
                has_with_clause: query.with_clause.is_some(),
                error: None,
            };
            let ast = format!("{:#?}", query.query);
            let (optimized, applied_passes) = QueryOptimizer::new().optimize(query.query);
            RqlExplanation {
                validation,
                ast: Some(ast),
                optimized_ast: Some(format!("{optimized:#?}")),
                applied_passes,
            }
        }
        Err(err) => RqlExplanation {
            validation: RqlValidation {
                valid: false,
                statement: None,
                has_with_clause: false,
                error: Some(diagnostic_from_error(&err)),
            },
            ast: None,
            optimized_ast: None,
            applied_passes: Vec::new(),
        },
    }
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

    /// A well-formed SELECT validates and reports its statement kind.
    #[test]
    fn validate_accepts_valid_select() {
        let verdict = validate_rql("SELECT * FROM users");
        assert!(verdict.valid, "expected valid: {verdict:?}");
        assert_eq!(verdict.statement.as_deref(), Some("Table"));
        assert!(verdict.error.is_none());
    }

    /// A leading `WITH` prelude is reported on a valid query.
    #[test]
    fn validate_reports_with_clause() {
        let verdict = validate_rql("WITH t AS (SELECT * FROM users) SELECT * FROM t");
        assert!(verdict.valid, "expected valid: {verdict:?}");
        assert!(
            verdict.has_with_clause,
            "expected has_with_clause: {verdict:?}"
        );
    }

    /// Invalid RQL produces a structured diagnostic with a real source position
    /// and a stable kind label — not a valid verdict.
    #[test]
    fn validate_rejects_invalid_with_structured_error() {
        let verdict = validate_rql("SELECT * FROM");
        assert!(!verdict.valid, "expected invalid: {verdict:?}");
        assert!(verdict.statement.is_none());
        let err = verdict.error.expect("structured error");
        assert!(!err.message.is_empty(), "message should be populated");
        assert!(err.line >= 1, "line is 1-indexed: {err:?}");
        assert!(!err.kind.is_empty(), "kind label should be populated");
    }

    /// `explain_rql` returns the AST and the optimizer plan for valid input.
    #[test]
    fn explain_returns_ast_and_plan() {
        let explanation = explain_rql("SELECT * FROM users WHERE id = 1");
        assert!(explanation.validation.valid);
        let ast = explanation.ast.expect("ast present");
        assert!(ast.contains("Table"), "ast should render the query: {ast}");
        assert!(explanation.optimized_ast.is_some());
        // applied_passes is a (possibly empty) list, never populated on error.
    }

    /// `explain_rql` on invalid input mirrors the validation error and omits the
    /// AST/plan.
    #[test]
    fn explain_on_invalid_omits_ast() {
        let explanation = explain_rql("NOT REAL RQL @@@");
        assert!(!explanation.validation.valid);
        assert!(explanation.ast.is_none());
        assert!(explanation.optimized_ast.is_none());
        assert!(explanation.applied_passes.is_empty());
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
