//! Multi-Mode Query Parser
//!
//! Supports multiple query languages with automatic mode detection:
//! - SQL: `SELECT ... FROM ... WHERE`
//! - Gremlin: `g.V().out().has(...)`
//! - Cypher: `MATCH (a)-[r]->(b) RETURN`
//! - SPARQL: `SELECT ?var WHERE { ... }`
//! - Path: `PATH FROM ... TO ... VIA`
//! - Natural: Natural language queries
//!
//! # Example
//! ```ignore
//! use redblue::storage::query::modes::{detect_mode, QueryMode, parse_multi};
//!
//! let query = "g.V().has('name', 'alice').out('knows')";
//! let mode = detect_mode(query);
//! assert_eq!(mode, QueryMode::Gremlin);
//!
//! let result = parse_multi(query)?;
//! ```

pub mod detect;
pub mod gremlin;
pub mod natural;
pub mod sparql;

pub use detect::{detect_mode, QueryMode};
pub use gremlin::{GremlinParser, GremlinStep, GremlinTraversal};
pub use natural::{NaturalParser, NaturalQuery, QueryIntent};
pub use sparql::{SparqlParser, SparqlQuery, TriplePattern};

use crate::storage::query::ast::QueryExpr;

/// Parse a query string in any supported mode
pub fn parse_multi(input: &str) -> Result<QueryExpr, MultiParseError> {
    let mode = detect_mode(input);

    match mode {
        QueryMode::Sql | QueryMode::Cypher | QueryMode::Path => {
            // Use existing RQL parser for SQL, Cypher, and Path modes
            crate::storage::query::parser::parse(input)
                .map_err(|e| MultiParseError::Parse(e.to_string()))
        }
        QueryMode::Gremlin => {
            let traversal = GremlinParser::parse(input)?;
            Ok(traversal.to_query_expr())
        }
        QueryMode::Sparql => {
            let sparql = SparqlParser::parse(input)?;
            Ok(sparql.to_query_expr())
        }
        QueryMode::Natural => {
            let natural = NaturalParser::parse(input)?;
            Ok(natural.to_query_expr())
        }
        QueryMode::Unknown => Err(MultiParseError::UnknownMode(input.to_string())),
    }
}

/// Error type for multi-mode parsing
#[derive(Debug, Clone)]
pub enum MultiParseError {
    Parse(String),
    Gremlin(String),
    Sparql(String),
    Natural(String),
    UnknownMode(String),
}

impl std::fmt::Display for MultiParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "Parse error: {}", e),
            Self::Gremlin(e) => write!(f, "Gremlin error: {}", e),
            Self::Sparql(e) => write!(f, "SPARQL error: {}", e),
            Self::Natural(e) => write!(f, "Natural language error: {}", e),
            Self::UnknownMode(q) => write!(f, "Unknown query mode for: {}", q),
        }
    }
}

impl std::error::Error for MultiParseError {}

impl From<gremlin::GremlinError> for MultiParseError {
    fn from(e: gremlin::GremlinError) -> Self {
        Self::Gremlin(e.to_string())
    }
}

impl From<sparql::SparqlError> for MultiParseError {
    fn from(e: sparql::SparqlError) -> Self {
        Self::Sparql(e.to_string())
    }
}

impl From<natural::NaturalError> for MultiParseError {
    fn from(e: natural::NaturalError) -> Self {
        Self::Natural(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_sql() {
        assert_eq!(detect_mode("SELECT * FROM users"), QueryMode::Sql);
        assert_eq!(detect_mode("select name from hosts"), QueryMode::Sql);
    }

    #[test]
    fn test_detect_gremlin() {
        assert_eq!(detect_mode("g.V()"), QueryMode::Gremlin);
        assert_eq!(
            detect_mode("g.V().has('name', 'alice')"),
            QueryMode::Gremlin
        );
        assert_eq!(detect_mode("__.out('knows')"), QueryMode::Gremlin);
    }

    #[test]
    fn test_detect_cypher() {
        assert_eq!(
            detect_mode("MATCH (a)-[r]->(b) RETURN a"),
            QueryMode::Cypher
        );
        assert_eq!(detect_mode("match (n:Host) return n"), QueryMode::Cypher);
    }

    #[test]
    fn test_detect_sparql() {
        assert_eq!(
            detect_mode("SELECT ?name WHERE { ?s :name ?name }"),
            QueryMode::Sparql
        );
        assert_eq!(
            detect_mode("PREFIX ex: <http://example.org/> SELECT ?x"),
            QueryMode::Sparql
        );
    }

    #[test]
    fn test_detect_path() {
        assert_eq!(
            detect_mode("PATH FROM host('10.0.0.1') TO host('10.0.0.2')"),
            QueryMode::Path
        );
        assert_eq!(
            detect_mode("PATHS ALL FROM user('admin') TO credential('root')"),
            QueryMode::Path
        );
    }

    #[test]
    fn test_detect_natural() {
        assert_eq!(detect_mode("find all hosts with ssh"), QueryMode::Natural);
        assert_eq!(
            detect_mode("show me credentials for user admin"),
            QueryMode::Natural
        );
        assert_eq!(
            detect_mode("\"what vulnerabilities affect host 10.0.0.1?\""),
            QueryMode::Natural
        );
    }
}
