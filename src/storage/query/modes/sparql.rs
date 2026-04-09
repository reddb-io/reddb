//! SPARQL Parser
//!
//! Parses W3C SPARQL-like queries for RDF-style graph patterns:
//! - `SELECT ?host ?ip WHERE { ?host :hasIP ?ip }`
//! - `PREFIX ex: <http://example.org/> SELECT ?x WHERE { ?x ex:type ?t }`
//!
//! # Supported Features
//!
//! - SELECT queries with variables (?var)
//! - WHERE clause with triple patterns
//! - PREFIX declarations
//! - FILTER expressions
//! - OPTIONAL patterns
//! - LIMIT and OFFSET
//!
//! # Mapping to Graph Model
//!
//! SPARQL triple patterns map to our graph model:
//! - Subject → Node
//! - Predicate → Edge type
//! - Object → Node or literal value

use crate::storage::engine::graph_store::GraphEdgeType;
use crate::storage::query::ast::{
    CompareOp, EdgeDirection, EdgePattern, FieldRef, Filter, GraphPattern, GraphQuery, NodePattern,
    Projection, QueryExpr,
};
use crate::storage::schema::Value;
use std::collections::HashMap;

/// SPARQL parse error
#[derive(Debug, Clone)]
pub struct SparqlError {
    pub message: String,
    pub position: usize,
}

impl std::fmt::Display for SparqlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SPARQL error at {}: {}", self.position, self.message)
    }
}

impl std::error::Error for SparqlError {}

/// A SPARQL query
#[derive(Debug, Clone)]
pub struct SparqlQuery {
    /// PREFIX declarations
    pub prefixes: HashMap<String, String>,
    /// Selected variables
    pub select: Vec<String>,
    /// SELECT DISTINCT
    pub distinct: bool,
    /// WHERE clause patterns
    pub where_patterns: Vec<TriplePattern>,
    /// FILTER expressions
    pub filters: Vec<SparqlFilter>,
    /// OPTIONAL patterns
    pub optionals: Vec<Vec<TriplePattern>>,
    /// ORDER BY
    pub order_by: Vec<(String, bool)>, // (var, ascending)
    /// LIMIT
    pub limit: Option<u64>,
    /// OFFSET
    pub offset: Option<u64>,
}

/// A triple pattern (subject, predicate, object)
#[derive(Debug, Clone)]
pub struct TriplePattern {
    pub subject: SparqlTerm,
    pub predicate: SparqlTerm,
    pub object: SparqlTerm,
}

/// A term in a triple pattern
#[derive(Debug, Clone)]
pub enum SparqlTerm {
    /// Variable: ?name
    Variable(String),
    /// Prefixed IRI: prefix:local
    PrefixedName(String, String),
    /// Full IRI: <http://...>
    Iri(String),
    /// Literal string
    Literal(String),
    /// Typed literal
    TypedLiteral(String, String),
    /// Numeric literal
    Number(f64),
    /// Boolean
    Boolean(bool),
    /// Shorthand predicate 'a' for rdf:type
    A,
}

/// SPARQL filter expression
#[derive(Debug, Clone)]
pub enum SparqlFilter {
    /// Comparison: ?x = value
    Compare(String, CompareOp, SparqlTerm),
    /// REGEX filter
    Regex(String, String, Option<String>),
    /// BOUND(?var)
    Bound(String),
    /// !BOUND(?var)
    NotBound(String),
    /// isIRI(?var)
    IsIri(String),
    /// isLiteral(?var)
    IsLiteral(String),
    /// CONTAINS(?var, 'text')
    Contains(String, String),
    /// STRSTARTS(?var, 'prefix')
    StrStarts(String, String),
    /// STRENDS(?var, 'suffix')
    StrEnds(String, String),
    /// AND
    And(Box<SparqlFilter>, Box<SparqlFilter>),
    /// OR
    Or(Box<SparqlFilter>, Box<SparqlFilter>),
    /// NOT
    Not(Box<SparqlFilter>),
}

/// SPARQL parser
pub struct SparqlParser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> SparqlParser<'a> {
    /// Create a new parser
    pub fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    /// Parse a SPARQL query string
    pub fn parse(input: &str) -> Result<SparqlQuery, SparqlError> {
        let mut parser = SparqlParser::new(input);
        parser.parse_query()
    }

    /// Parse a full query
    fn parse_query(&mut self) -> Result<SparqlQuery, SparqlError> {
        let mut query = SparqlQuery {
            prefixes: HashMap::new(),
            select: Vec::new(),
            distinct: false,
            where_patterns: Vec::new(),
            filters: Vec::new(),
            optionals: Vec::new(),
            order_by: Vec::new(),
            limit: None,
            offset: None,
        };

        // Parse PREFIX declarations
        while self.peek_keyword("PREFIX") {
            self.consume_keyword("PREFIX")?;
            let prefix = self.parse_prefix_name()?;
            self.expect(':')?;
            let iri = self.parse_iri()?;
            query.prefixes.insert(prefix, iri);
        }

        // Parse SELECT
        self.consume_keyword("SELECT")?;

        // Check for DISTINCT
        if self.peek_keyword("DISTINCT") {
            self.consume_keyword("DISTINCT")?;
            query.distinct = true;
        }

        // Parse selected variables or *
        if self.consume_if("*") {
            query.select.push("*".to_string());
        } else {
            loop {
                self.skip_whitespace();
                if self.peek() != Some('?') && self.peek() != Some('$') {
                    break;
                }
                let var = self.parse_variable()?;
                query.select.push(var);
            }
        }

        // Parse WHERE clause
        self.consume_keyword("WHERE")?;
        self.expect('{')?;

        // Parse patterns inside WHERE
        self.parse_where_body(&mut query)?;

        self.expect('}')?;

        // Parse optional modifiers
        while !self.is_at_end() {
            self.skip_whitespace();

            if self.peek_keyword("ORDER") {
                self.consume_keyword("ORDER")?;
                self.consume_keyword("BY")?;

                loop {
                    self.skip_whitespace();
                    let ascending = if self.peek_keyword("DESC") {
                        self.consume_keyword("DESC")?;
                        self.expect('(')?;
                        let var = self.parse_variable()?;
                        self.expect(')')?;
                        query.order_by.push((var, false));
                        false
                    } else if self.peek_keyword("ASC") {
                        self.consume_keyword("ASC")?;
                        self.expect('(')?;
                        let var = self.parse_variable()?;
                        self.expect(')')?;
                        query.order_by.push((var, true));
                        true
                    } else if self.peek() == Some('?') || self.peek() == Some('$') {
                        let var = self.parse_variable()?;
                        query.order_by.push((var, true));
                        true
                    } else {
                        break;
                    };
                    let _ = ascending;
                }
            } else if self.peek_keyword("FILTER") {
                // FILTER can also appear after WHERE clause
                self.consume_keyword("FILTER")?;
                let filter = self.parse_filter()?;
                query.filters.push(filter);
            } else if self.peek_keyword("LIMIT") {
                self.consume_keyword("LIMIT")?;
                query.limit = Some(self.parse_integer()? as u64);
            } else if self.peek_keyword("OFFSET") {
                self.consume_keyword("OFFSET")?;
                query.offset = Some(self.parse_integer()? as u64);
            } else {
                break;
            }
        }

        Ok(query)
    }

    /// Parse the body of a WHERE clause
    fn parse_where_body(&mut self, query: &mut SparqlQuery) -> Result<(), SparqlError> {
        loop {
            self.skip_whitespace();

            if self.peek() == Some('}') {
                break;
            }

            // Check for OPTIONAL
            if self.peek_keyword("OPTIONAL") {
                self.consume_keyword("OPTIONAL")?;
                self.expect('{')?;
                let mut optional_patterns = Vec::new();
                self.parse_patterns(&mut optional_patterns)?;
                self.expect('}')?;
                query.optionals.push(optional_patterns);
                continue;
            }

            // Check for FILTER
            if self.peek_keyword("FILTER") {
                self.consume_keyword("FILTER")?;
                let filter = self.parse_filter()?;
                query.filters.push(filter);
                continue;
            }

            // Parse triple pattern
            if let Ok(pattern) = self.parse_triple_pattern() {
                query.where_patterns.push(pattern);

                // Optional dot separator
                self.skip_whitespace();
                self.consume_if(".");
            } else {
                break;
            }
        }

        Ok(())
    }

    /// Parse patterns into a vector
    fn parse_patterns(&mut self, patterns: &mut Vec<TriplePattern>) -> Result<(), SparqlError> {
        loop {
            self.skip_whitespace();

            if self.peek() == Some('}') {
                break;
            }

            if let Ok(pattern) = self.parse_triple_pattern() {
                patterns.push(pattern);
                self.skip_whitespace();
                self.consume_if(".");
            } else {
                break;
            }
        }
        Ok(())
    }

    /// Parse a triple pattern
    fn parse_triple_pattern(&mut self) -> Result<TriplePattern, SparqlError> {
        self.skip_whitespace();
        let subject = self.parse_term()?;

        self.skip_whitespace();
        let predicate = self.parse_term()?;

        self.skip_whitespace();
        let object = self.parse_term()?;

        Ok(TriplePattern {
            subject,
            predicate,
            object,
        })
    }

    /// Parse a single term
    fn parse_term(&mut self) -> Result<SparqlTerm, SparqlError> {
        self.skip_whitespace();

        // Variable
        if self.peek() == Some('?') || self.peek() == Some('$') {
            return Ok(SparqlTerm::Variable(self.parse_variable()?));
        }

        // Full IRI
        if self.peek() == Some('<') {
            return Ok(SparqlTerm::Iri(self.parse_iri()?));
        }

        // String literal
        if self.peek() == Some('"') || self.peek() == Some('\'') {
            let lit = self.parse_string()?;

            // Check for type annotation
            self.skip_whitespace();
            if self.consume_if("^^") {
                let datatype = self.parse_term()?;
                if let SparqlTerm::Iri(dt) | SparqlTerm::PrefixedName(_, dt) = &datatype {
                    return Ok(SparqlTerm::TypedLiteral(lit, dt.clone()));
                }
            }

            return Ok(SparqlTerm::Literal(lit));
        }

        // Number
        if self
            .peek()
            .map(|c| c.is_ascii_digit() || c == '-' || c == '+')
            .unwrap_or(false)
        {
            return Ok(SparqlTerm::Number(self.parse_number()?));
        }

        // Boolean
        if self.peek_keyword("true") {
            self.consume_keyword("true")?;
            return Ok(SparqlTerm::Boolean(true));
        }
        if self.peek_keyword("false") {
            self.consume_keyword("false")?;
            return Ok(SparqlTerm::Boolean(false));
        }

        // 'a' shorthand for rdf:type
        if self.peek() == Some('a') {
            let next = self.input.get(self.pos + 1..self.pos + 2);
            if next
                .map(|s| {
                    s.chars()
                        .next()
                        .map(|c| !c.is_alphanumeric())
                        .unwrap_or(true)
                })
                .unwrap_or(true)
            {
                self.pos += 1;
                return Ok(SparqlTerm::A);
            }
        }

        // Prefixed name: prefix:local
        let prefix = self.parse_prefix_name()?;
        if self.consume_if(":") {
            let local = self.parse_local_name()?;
            return Ok(SparqlTerm::PrefixedName(prefix, local));
        }

        // Just a local name with empty prefix
        Ok(SparqlTerm::PrefixedName(String::new(), prefix))
    }

    /// Parse a FILTER expression
    fn parse_filter(&mut self) -> Result<SparqlFilter, SparqlError> {
        self.skip_whitespace();
        self.expect('(')?;
        let filter = self.parse_filter_expr()?;
        self.expect(')')?;
        Ok(filter)
    }

    /// Parse filter expression inside parentheses
    fn parse_filter_expr(&mut self) -> Result<SparqlFilter, SparqlError> {
        self.skip_whitespace();

        // NOT
        if self.peek() == Some('!') {
            self.pos += 1;
            let inner = self.parse_filter_expr()?;
            return Ok(SparqlFilter::Not(Box::new(inner)));
        }

        // Function-style filters
        if self.peek_keyword("BOUND") {
            self.consume_keyword("BOUND")?;
            self.expect('(')?;
            let var = self.parse_variable()?;
            self.expect(')')?;
            return Ok(SparqlFilter::Bound(var));
        }

        if self.peek_keyword("isIRI") || self.peek_keyword("isURI") {
            self.skip_identifier();
            self.expect('(')?;
            let var = self.parse_variable()?;
            self.expect(')')?;
            return Ok(SparqlFilter::IsIri(var));
        }

        if self.peek_keyword("isLiteral") {
            self.consume_keyword("isLiteral")?;
            self.expect('(')?;
            let var = self.parse_variable()?;
            self.expect(')')?;
            return Ok(SparqlFilter::IsLiteral(var));
        }

        if self.peek_keyword("CONTAINS") {
            self.consume_keyword("CONTAINS")?;
            self.expect('(')?;
            let var = self.parse_variable()?;
            self.expect(',')?;
            let pattern = self.parse_string()?;
            self.expect(')')?;
            return Ok(SparqlFilter::Contains(var, pattern));
        }

        if self.peek_keyword("STRSTARTS") {
            self.consume_keyword("STRSTARTS")?;
            self.expect('(')?;
            let var = self.parse_variable()?;
            self.expect(',')?;
            let pattern = self.parse_string()?;
            self.expect(')')?;
            return Ok(SparqlFilter::StrStarts(var, pattern));
        }

        if self.peek_keyword("STRENDS") {
            self.consume_keyword("STRENDS")?;
            self.expect('(')?;
            let var = self.parse_variable()?;
            self.expect(',')?;
            let pattern = self.parse_string()?;
            self.expect(')')?;
            return Ok(SparqlFilter::StrEnds(var, pattern));
        }

        if self.peek_keyword("REGEX") {
            self.consume_keyword("REGEX")?;
            self.expect('(')?;
            let var = self.parse_variable()?;
            self.expect(',')?;
            let pattern = self.parse_string()?;
            let flags = if self.consume_if(",") {
                Some(self.parse_string()?)
            } else {
                None
            };
            self.expect(')')?;
            return Ok(SparqlFilter::Regex(var, pattern, flags));
        }

        // Comparison expression: ?var op value
        if self.peek() == Some('?') || self.peek() == Some('$') {
            let var = self.parse_variable()?;
            self.skip_whitespace();

            let op = if self.consume_if("=") {
                CompareOp::Eq
            } else if self.consume_if("!=") {
                CompareOp::Ne
            } else if self.consume_if("<=") {
                CompareOp::Le
            } else if self.consume_if(">=") {
                CompareOp::Ge
            } else if self.consume_if("<") {
                CompareOp::Lt
            } else if self.consume_if(">") {
                CompareOp::Gt
            } else {
                return Err(self.error("Expected comparison operator"));
            };

            self.skip_whitespace();
            let value = self.parse_term()?;

            return Ok(SparqlFilter::Compare(var, op, value));
        }

        Err(self.error("Invalid filter expression"))
    }

    // Helper methods

    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.pos += 1;
            } else if c == '#' {
                // Skip comment
                while let Some(c) = self.peek() {
                    self.pos += 1;
                    if c == '\n' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn is_at_end(&self) -> bool {
        self.pos >= self.input.len()
    }

    fn consume_if(&mut self, s: &str) -> bool {
        self.skip_whitespace();
        if self.input[self.pos..].starts_with(s) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: char) -> Result<(), SparqlError> {
        self.skip_whitespace();
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.error(&format!("Expected '{}', found {:?}", c, self.peek())))
        }
    }

    fn peek_keyword(&self, keyword: &str) -> bool {
        let remaining = &self.input[self.pos..].trim_start();
        if remaining.len() >= keyword.len() {
            let word = &remaining[..keyword.len()];
            word.eq_ignore_ascii_case(keyword)
                && remaining
                    .chars()
                    .nth(keyword.len())
                    .map(|c| !c.is_alphanumeric())
                    .unwrap_or(true)
        } else {
            false
        }
    }

    fn consume_keyword(&mut self, keyword: &str) -> Result<(), SparqlError> {
        self.skip_whitespace();
        if self.peek_keyword(keyword) {
            self.pos += self.input[self.pos..].len() - self.input[self.pos..].trim_start().len();
            self.pos += keyword.len();
            Ok(())
        } else {
            Err(self.error(&format!("Expected keyword '{}'", keyword)))
        }
    }

    fn skip_identifier(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn parse_variable(&mut self) -> Result<String, SparqlError> {
        self.skip_whitespace();
        if self.peek() != Some('?') && self.peek() != Some('$') {
            return Err(self.error("Expected variable starting with ? or $"));
        }
        self.pos += 1;

        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' {
                self.pos += 1;
            } else {
                break;
            }
        }

        Ok(self.input[start..self.pos].to_string())
    }

    fn parse_prefix_name(&mut self) -> Result<String, SparqlError> {
        self.skip_whitespace();
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                self.pos += 1;
            } else {
                break;
            }
        }
        Ok(self.input[start..self.pos].to_string())
    }

    fn parse_local_name(&mut self) -> Result<String, SparqlError> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' || c == '-' || c == '.' {
                self.pos += 1;
            } else {
                break;
            }
        }
        Ok(self.input[start..self.pos].to_string())
    }

    fn parse_iri(&mut self) -> Result<String, SparqlError> {
        self.skip_whitespace();
        self.expect('<')?;
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c == '>' {
                let iri = self.input[start..self.pos].to_string();
                self.pos += 1;
                return Ok(iri);
            }
            self.pos += 1;
        }
        Err(self.error("Unterminated IRI"))
    }

    fn parse_string(&mut self) -> Result<String, SparqlError> {
        self.skip_whitespace();
        let quote = self.peek();
        if quote != Some('"') && quote != Some('\'') {
            return Err(self.error("Expected string"));
        }
        self.pos += 1;

        let start = self.pos;
        while let Some(c) = self.peek() {
            if Some(c) == quote {
                let s = self.input[start..self.pos].to_string();
                self.pos += 1;
                return Ok(s);
            }
            if c == '\\' {
                self.pos += 2;
            } else {
                self.pos += 1;
            }
        }
        Err(self.error("Unterminated string"))
    }

    fn parse_integer(&mut self) -> Result<i64, SparqlError> {
        self.skip_whitespace();
        let start = self.pos;
        if self.peek() == Some('-') || self.peek() == Some('+') {
            self.pos += 1;
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                self.pos += 1;
            } else {
                break;
            }
        }
        let s = &self.input[start..self.pos];
        s.parse()
            .map_err(|_| self.error(&format!("Invalid integer: {}", s)))
    }

    fn parse_number(&mut self) -> Result<f64, SparqlError> {
        self.skip_whitespace();
        let start = self.pos;
        if self.peek() == Some('-') || self.peek() == Some('+') {
            self.pos += 1;
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let s = &self.input[start..self.pos];
        s.parse()
            .map_err(|_| self.error(&format!("Invalid number: {}", s)))
    }

    fn error(&self, message: &str) -> SparqlError {
        SparqlError {
            message: message.to_string(),
            position: self.pos,
        }
    }
}

impl SparqlQuery {
    /// Convert SPARQL query to QueryExpr
    pub fn to_query_expr(&self) -> QueryExpr {
        let mut nodes: Vec<NodePattern> = Vec::new();
        let mut edges: Vec<EdgePattern> = Vec::new();
        let mut filters: Vec<Filter> = Vec::new();
        let mut var_to_alias: HashMap<String, String> = HashMap::new();
        let mut alias_counter = 0;

        // Helper to get or create alias for a variable
        let mut get_alias = |var: &str| -> String {
            if let Some(alias) = var_to_alias.get(var) {
                alias.clone()
            } else {
                let alias = format!("n{}", alias_counter);
                alias_counter += 1;
                var_to_alias.insert(var.to_string(), alias.clone());
                nodes.push(NodePattern {
                    alias: alias.clone(),
                    node_type: None,
                    properties: Vec::new(),
                });
                alias
            }
        };

        // Convert triple patterns to edges
        for pattern in &self.where_patterns {
            let subject_alias = match &pattern.subject {
                SparqlTerm::Variable(v) => get_alias(v),
                _ => continue, // Skip non-variable subjects for now
            };

            let predicate_label = match &pattern.predicate {
                SparqlTerm::PrefixedName(_, local) => Some(local.clone()),
                SparqlTerm::A => Some("type".to_string()),
                SparqlTerm::Iri(iri) => {
                    // Extract local name from IRI
                    iri.rsplit('/')
                        .next()
                        .or_else(|| iri.rsplit('#').next())
                        .map(|s| s.to_string())
                }
                _ => None,
            };

            // Map predicate label to GraphEdgeType
            let edge_type =
                predicate_label
                    .as_ref()
                    .and_then(|l| match l.to_lowercase().as_str() {
                        "hasservice" | "has_service" => Some(GraphEdgeType::HasService),
                        "hasendpoint" | "has_endpoint" => Some(GraphEdgeType::HasEndpoint),
                        "usestech" | "uses_tech" => Some(GraphEdgeType::UsesTech),
                        "authaccess" | "auth_access" => Some(GraphEdgeType::AuthAccess),
                        "affectedby" | "affected_by" => Some(GraphEdgeType::AffectedBy),
                        "contains" => Some(GraphEdgeType::Contains),
                        "connectsto" | "connects_to" => Some(GraphEdgeType::ConnectsTo),
                        "relatedto" | "related_to" => Some(GraphEdgeType::RelatedTo),
                        "hasuser" | "has_user" => Some(GraphEdgeType::HasUser),
                        "hascert" | "has_cert" => Some(GraphEdgeType::HasCert),
                        _ => None,
                    });

            match &pattern.object {
                SparqlTerm::Variable(v) => {
                    let object_alias = get_alias(v);
                    edges.push(EdgePattern {
                        alias: None,
                        from: subject_alias.clone(),
                        to: object_alias,
                        edge_type,
                        direction: EdgeDirection::Outgoing,
                        min_hops: 1,
                        max_hops: 1,
                    });
                }
                SparqlTerm::Literal(lit) | SparqlTerm::TypedLiteral(lit, _) => {
                    // Object is a literal - add as property filter
                    if let Some(pred) = predicate_label {
                        filters.push(Filter::Compare {
                            field: FieldRef::NodeProperty {
                                alias: subject_alias.clone(),
                                property: pred,
                            },
                            op: CompareOp::Eq,
                            value: Value::Text(lit.clone()),
                        });
                    }
                }
                _ => {}
            }
        }

        // Convert SPARQL filters
        for filter in &self.filters {
            if let Some(f) = convert_sparql_filter(filter) {
                filters.push(f);
            }
        }

        // Build projections
        let projections = if self.select.contains(&"*".to_string()) {
            // Return all node IDs for * projection
            nodes
                .iter()
                .map(|n| {
                    Projection::from_field(FieldRef::NodeId {
                        alias: n.alias.clone(),
                    })
                })
                .collect()
        } else {
            self.select
                .iter()
                .filter_map(|v| {
                    var_to_alias.get(v).map(|alias| {
                        Projection::from_field(FieldRef::NodeId {
                            alias: alias.clone(),
                        })
                    })
                })
                .collect()
        };

        // Fold multiple filters into nested And
        let combined_filter = if filters.is_empty() {
            None
        } else {
            let mut iter = filters.into_iter();
            let first = iter.next().unwrap();
            Some(iter.fold(first, |acc, f| Filter::And(Box::new(acc), Box::new(f))))
        };

        QueryExpr::Graph(GraphQuery {
            alias: None,
            pattern: GraphPattern { nodes, edges },
            filter: combined_filter,
            return_: projections,
        })
    }
}

/// Convert a SPARQL filter to our Filter type
fn convert_sparql_filter(filter: &SparqlFilter) -> Option<Filter> {
    // Helper to create FieldRef from SPARQL variable name
    let var_to_field = |var: &str| -> FieldRef {
        // Strip ? prefix if present
        let clean = var.trim_start_matches('?');
        FieldRef::NodeProperty {
            alias: clean.to_string(),
            property: "value".to_string(), // Default property
        }
    };

    match filter {
        SparqlFilter::Compare(var, op, term) => {
            let value = match term {
                SparqlTerm::Literal(s) => Value::Text(s.clone()),
                SparqlTerm::Number(n) => Value::Float(*n),
                SparqlTerm::Boolean(b) => Value::Boolean(*b),
                _ => return None,
            };
            Some(Filter::Compare {
                field: var_to_field(var),
                op: *op,
                value,
            })
        }
        SparqlFilter::Bound(var) => Some(Filter::IsNotNull(var_to_field(var))),
        SparqlFilter::NotBound(var) => Some(Filter::IsNull(var_to_field(var))),
        SparqlFilter::Contains(var, pattern) => Some(Filter::Like {
            field: var_to_field(var),
            pattern: format!("%{}%", pattern),
        }),
        SparqlFilter::StrStarts(var, prefix) => Some(Filter::StartsWith {
            field: var_to_field(var),
            prefix: prefix.clone(),
        }),
        SparqlFilter::StrEnds(var, suffix) => Some(Filter::EndsWith {
            field: var_to_field(var),
            suffix: suffix.clone(),
        }),
        SparqlFilter::And(a, b) => {
            let fa = convert_sparql_filter(a)?;
            let fb = convert_sparql_filter(b)?;
            Some(Filter::And(Box::new(fa), Box::new(fb)))
        }
        SparqlFilter::Or(a, b) => {
            let fa = convert_sparql_filter(a)?;
            let fb = convert_sparql_filter(b)?;
            Some(Filter::Or(Box::new(fa), Box::new(fb)))
        }
        SparqlFilter::Not(inner) => {
            let fi = convert_sparql_filter(inner)?;
            Some(Filter::Not(Box::new(fi)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_select() {
        let q = SparqlParser::parse("SELECT ?host WHERE { ?host :hasIP ?ip }").unwrap();
        assert_eq!(q.select, vec!["host"]);
        assert_eq!(q.where_patterns.len(), 1);
    }

    #[test]
    fn test_parse_with_prefix() {
        let q = SparqlParser::parse(
            "PREFIX ex: <http://example.org/> SELECT ?x WHERE { ?x ex:type ?t }",
        )
        .unwrap();
        assert!(q.prefixes.contains_key("ex"));
        assert_eq!(q.select, vec!["x"]);
    }

    #[test]
    fn test_parse_multiple_patterns() {
        let q = SparqlParser::parse(
            "SELECT ?host ?ip WHERE { ?host :hasIP ?ip . ?host :hasName ?name }",
        )
        .unwrap();
        assert_eq!(q.where_patterns.len(), 2);
    }

    #[test]
    fn test_parse_with_limit() {
        let q = SparqlParser::parse("SELECT ?x WHERE { ?x :type ?t } LIMIT 10").unwrap();
        assert_eq!(q.limit, Some(10));
    }

    #[test]
    fn test_parse_with_filter() {
        let q = SparqlParser::parse("SELECT ?host WHERE { ?host :port ?p } FILTER (?p > 1000)")
            .unwrap();
        assert_eq!(q.filters.len(), 1);
    }

    #[test]
    fn test_parse_select_star() {
        let q = SparqlParser::parse("SELECT * WHERE { ?s ?p ?o }").unwrap();
        assert!(q.select.contains(&"*".to_string()));
    }

    #[test]
    fn test_to_query_expr() {
        let q = SparqlParser::parse("SELECT ?host ?ip WHERE { ?host :hasIP ?ip }").unwrap();
        let expr = q.to_query_expr();
        assert!(matches!(expr, QueryExpr::Graph(_)));
    }
}
