//! Query Mode Detection
//!
//! Automatically detects the query language based on syntax patterns.

/// Supported query modes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryMode {
    /// SQL-style: SELECT ... FROM ... WHERE
    Sql,
    /// Gremlin traversal: g.V().out().has(...)
    Gremlin,
    /// Cypher pattern matching: MATCH (a)-[r]->(b) RETURN
    Cypher,
    /// SPARQL RDF queries: SELECT ?var WHERE { ... }
    Sparql,
    /// Path queries: PATH FROM ... TO ... VIA
    Path,
    /// Natural language queries
    Natural,
    /// Unknown mode
    Unknown,
}

/// Detect the query mode from input string
pub fn detect_mode(input: &str) -> QueryMode {
    let trimmed = input.trim();
    let lower = trimmed.to_lowercase();

    // Check for quoted natural language (starts with quote)
    if trimmed.starts_with('"') || trimmed.starts_with('\'') {
        return QueryMode::Natural;
    }

    // Gremlin: starts with g. or __.
    if lower.starts_with("g.") || lower.starts_with("__.") {
        return QueryMode::Gremlin;
    }

    // Path: PATH or PATHS keyword at start
    if lower.starts_with("path ") || lower.starts_with("paths ") {
        return QueryMode::Path;
    }

    // SPARQL: has ?variable pattern or PREFIX keyword
    if lower.starts_with("prefix ") || has_sparql_pattern(&lower) {
        return QueryMode::Sparql;
    }

    // Cypher: MATCH keyword at start
    if lower.starts_with("match ") || lower.starts_with("match(") {
        return QueryMode::Cypher;
    }

    // SQL: SELECT, FROM, INSERT, UPDATE, DELETE, CREATE, DROP, ALTER, GRAPH, SEARCH at start
    if lower.starts_with("select ")
        || lower.starts_with("from ")
        || lower.starts_with("insert ")
        || lower.starts_with("update ")
        || lower.starts_with("delete ")
        || lower.starts_with("create ")
        || lower.starts_with("drop ")
        || lower.starts_with("alter ")
        || lower.starts_with("vector ")
        || lower.starts_with("hybrid ")
        || lower.starts_with("graph ")
        || lower.starts_with("queue ")
        || lower.starts_with("tree ")
        || lower.starts_with("search ")
        || lower.starts_with("ask ")
        || lower.starts_with("set config ")
        || lower.starts_with("show config")
    {
        // But check if it's SPARQL-style SELECT with ?variable
        if lower.starts_with("select ") && lower.contains(" ?") {
            return QueryMode::Sparql;
        }
        return QueryMode::Sql;
    }

    // Natural language detection: common question words and patterns
    if is_natural_language(&lower) {
        return QueryMode::Natural;
    }

    QueryMode::Unknown
}

/// Check for SPARQL-specific patterns
fn has_sparql_pattern(lower: &str) -> bool {
    // SPARQL variables start with ? or $
    // SPARQL has WHERE { } with triple patterns

    // Check for ?variable pattern (not after comparison operators)
    let has_var = lower.contains(" ?") && !lower.contains("= ?") && !lower.contains("> ?");

    // Check for typical SPARQL structure
    let has_triple_pattern = lower.contains(" where {") || lower.contains(" where{");

    // Check for RDF predicates (prefixed URIs like :predicate or prefix:pred)
    let has_prefix_pattern = lower.contains(":")
        && (lower.contains(":<")
            || lower.contains("> :")
            || lower.contains(" :") && lower.contains("?"));

    has_var || has_triple_pattern || has_prefix_pattern
}

/// Detect natural language patterns
fn is_natural_language(lower: &str) -> bool {
    // Question words
    let question_starters = [
        "find ", "show ", "list ", "what ", "which ", "where ", "how ", "who ", "get ", "give ",
        "tell ", "display ", "search ", "look ",
    ];

    // Common natural language verbs/phrases
    let nl_patterns = [
        " with ",
        " for ",
        " that ",
        " have ",
        " has ",
        " can ",
        " are ",
        " is ",
        " all ",
        " me ",
        " the ",
        " from ",
        " to ",
        " on ",
        " in ",
        "vulnerable",
        "credential",
        "password",
        "user",
        "host",
        "service",
        "connected",
        "reachable",
        "exposed",
        "critical",
    ];

    // Check starters
    for starter in question_starters.iter() {
        if lower.starts_with(starter) {
            return true;
        }
    }

    // Check for multiple natural language patterns (at least 2)
    let pattern_count = nl_patterns.iter().filter(|p| lower.contains(*p)).count();

    pattern_count >= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sql_detection() {
        assert_eq!(
            detect_mode("SELECT * FROM users WHERE id = 1"),
            QueryMode::Sql
        );
        assert_eq!(detect_mode("select name, age from hosts"), QueryMode::Sql);
        assert_eq!(
            detect_mode("FROM hosts h WHERE h.os = 'Linux'"),
            QueryMode::Sql
        );
        assert_eq!(
            detect_mode("INSERT INTO users VALUES (1, 'alice')"),
            QueryMode::Sql
        );
        assert_eq!(
            detect_mode("UPDATE hosts SET status = 'active'"),
            QueryMode::Sql
        );
        assert_eq!(
            detect_mode("DELETE FROM logs WHERE age > 30"),
            QueryMode::Sql
        );
        assert_eq!(
            detect_mode("QUEUE GROUP CREATE tasks workers"),
            QueryMode::Sql
        );
        assert_eq!(detect_mode("TREE VALIDATE forest.org"), QueryMode::Sql);
        assert_eq!(
            detect_mode("VECTOR SEARCH embeddings SIMILAR TO [1.0, 0.0] LIMIT 5"),
            QueryMode::Sql
        );
        assert_eq!(
            detect_mode("HYBRID FROM hosts VECTOR SEARCH embeddings SIMILAR TO [1.0, 0.0] LIMIT 5"),
            QueryMode::Sql
        );
        assert_eq!(
            detect_mode("ASK 'what happened on host 10.0.0.1?' USING groq"),
            QueryMode::Sql
        );
    }

    #[test]
    fn test_gremlin_detection() {
        assert_eq!(detect_mode("g.V()"), QueryMode::Gremlin);
        assert_eq!(detect_mode("g.V().hasLabel('host')"), QueryMode::Gremlin);
        assert_eq!(
            detect_mode("g.V().out('connects').in('has_service')"),
            QueryMode::Gremlin
        );
        assert_eq!(
            detect_mode("g.E().hasLabel('auth_access')"),
            QueryMode::Gremlin
        );
        assert_eq!(
            detect_mode("__.out('knows').has('name', 'bob')"),
            QueryMode::Gremlin
        );
        assert_eq!(
            detect_mode("g.V('host:10.0.0.1').repeat(out()).times(3)"),
            QueryMode::Gremlin
        );
    }

    #[test]
    fn test_cypher_detection() {
        assert_eq!(
            detect_mode("MATCH (a)-[r]->(b) RETURN a, b"),
            QueryMode::Cypher
        );
        assert_eq!(
            detect_mode("MATCH (h:Host)-[:HAS_SERVICE]->(s:Service)"),
            QueryMode::Cypher
        );
        assert_eq!(
            detect_mode("match (n) where n.ip = '10.0.0.1' return n"),
            QueryMode::Cypher
        );
        assert_eq!(
            detect_mode("MATCH(a:User) RETURN a.name"),
            QueryMode::Cypher
        );
    }

    #[test]
    fn test_sparql_detection() {
        assert_eq!(
            detect_mode("SELECT ?name WHERE { ?s :name ?name }"),
            QueryMode::Sparql
        );
        assert_eq!(
            detect_mode("PREFIX ex: <http://example.org/> SELECT ?x WHERE { ?x ex:type ?t }"),
            QueryMode::Sparql
        );
        assert_eq!(
            detect_mode("SELECT ?host ?ip WHERE { ?host :hasIP ?ip }"),
            QueryMode::Sparql
        );
    }

    #[test]
    fn test_path_detection() {
        assert_eq!(
            detect_mode("PATH FROM host('10.0.0.1') TO host('10.0.0.2')"),
            QueryMode::Path
        );
        assert_eq!(
            detect_mode("PATHS ALL FROM credential('admin') TO host('db')"),
            QueryMode::Path
        );
        assert_eq!(
            detect_mode("path from user('root') to service('ssh') via auth_access"),
            QueryMode::Path
        );
    }

    #[test]
    fn test_natural_detection() {
        assert_eq!(
            detect_mode("find all hosts with ssh open"),
            QueryMode::Natural
        );
        assert_eq!(
            detect_mode("show me vulnerable services"),
            QueryMode::Natural
        );
        assert_eq!(
            detect_mode("what credentials can reach the database?"),
            QueryMode::Natural
        );
        assert_eq!(
            detect_mode("list users with weak passwords"),
            QueryMode::Natural
        );
        assert_eq!(
            detect_mode("\"find hosts connected to 10.0.0.1\""),
            QueryMode::Natural
        );
        assert_eq!(
            detect_mode("which hosts have critical vulnerabilities?"),
            QueryMode::Natural
        );
    }

    #[test]
    fn test_edge_cases() {
        // Empty input
        assert_eq!(detect_mode(""), QueryMode::Unknown);

        // Just whitespace
        assert_eq!(detect_mode("   "), QueryMode::Unknown);

        // Case insensitivity
        assert_eq!(detect_mode("SELECT"), QueryMode::Unknown); // No space after
        assert_eq!(detect_mode("G.V()"), QueryMode::Gremlin);
        assert_eq!(detect_mode("Match (a) RETURN a"), QueryMode::Cypher);
    }
}
