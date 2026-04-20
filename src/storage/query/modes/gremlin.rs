//! Gremlin Traversal Parser
//!
//! Parses TinkerPop-style Gremlin queries like:
//! - `g.V().hasLabel('host').out('connects')`
//! - `g.V('host:10.0.0.1').repeat(out()).times(3).path()`
//! - `__.out('knows').has('name', 'bob')`
//!
//! # Supported Steps
//!
//! ## Source Steps
//! - `V()`, `V(id)` - Get vertices
//! - `E()`, `E(id)` - Get edges
//!
//! ## Traversal Steps
//! - `out(label?)`, `in(label?)`, `both(label?)` - Traverse edges
//! - `outE(label?)`, `inE(label?)`, `bothE(label?)` - Get edges
//! - `outV()`, `inV()`, `bothV()`, `otherV()` - Edge to vertex
//!
//! ## Filter Steps
//! - `has(key, value)`, `has(key)`, `hasNot(key)`
//! - `hasLabel(label)`, `hasId(id)`
//! - `where(predicate)`, `filter(traversal)`
//! - `dedup()`, `limit(n)`, `skip(n)`, `range(from, to)`
//!
//! ## Map Steps
//! - `values(keys...)`, `valueMap(keys...)`
//! - `id()`, `label()`, `properties(keys...)`
//! - `count()`, `sum()`, `min()`, `max()`, `mean()`
//! - `select(labels...)`, `project(keys...)`
//! - `path()`, `simplePath()`, `cyclicPath()`
//!
//! ## Branch Steps
//! - `repeat(traversal).times(n)`, `repeat(traversal).until(predicate)`
//! - `union(traversal...)`, `choose(predicate, true_traversal, false_traversal)`
//! - `coalesce(traversal...)`
//!
//! ## Side Effect Steps
//! - `as(label)`, `by(key|traversal)`
//! - `aggregate(label)`, `store(label)`
//! - `group()`, `groupCount()`

use crate::storage::engine::graph_store::{GraphEdgeType, GraphNodeType};
use crate::storage::query::ast::{
    CompareOp, EdgeDirection, EdgePattern, FieldRef, Filter, GraphPattern, GraphQuery, NodePattern,
    Projection, PropertyFilter, QueryExpr,
};
use crate::storage::schema::Value;

/// Gremlin parse error
#[derive(Debug, Clone)]
pub struct GremlinError {
    pub message: String,
    pub position: usize,
}

impl std::fmt::Display for GremlinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Gremlin error at {}: {}", self.position, self.message)
    }
}

impl std::error::Error for GremlinError {}

/// A Gremlin traversal is a sequence of steps
#[derive(Debug, Clone)]
pub struct GremlinTraversal {
    /// The source (g or __)
    pub source: TraversalSource,
    /// Steps in the traversal
    pub steps: Vec<GremlinStep>,
}

/// Traversal source
#[derive(Debug, Clone, PartialEq)]
pub enum TraversalSource {
    /// Graph source: g.V(), g.E()
    Graph,
    /// Anonymous traversal: __.out()
    Anonymous,
}

/// A single step in a Gremlin traversal
#[derive(Debug, Clone)]
pub enum GremlinStep {
    // Source steps
    V(Option<String>), // V() or V(id)
    E(Option<String>), // E() or E(id)

    // Traversal steps
    Out(Option<String>),   // out() or out('label')
    In(Option<String>),    // in() or in('label')
    Both(Option<String>),  // both() or both('label')
    OutE(Option<String>),  // outE() or outE('label')
    InE(Option<String>),   // inE() or inE('label')
    BothE(Option<String>), // bothE() or bothE('label')
    OutV,                  // outV()
    InV,                   // inV()
    BothV,                 // bothV()
    OtherV,                // otherV()

    // Filter steps
    Has(String, Option<GremlinValue>), // has('key') or has('key', value)
    HasNot(String),                    // hasNot('key')
    HasLabel(String),                  // hasLabel('label')
    HasId(String),                     // hasId('id')
    Where(Box<GremlinTraversal>),      // where(traversal)
    Filter(Box<GremlinTraversal>),     // filter(traversal)
    Dedup,                             // dedup()
    Limit(u64),                        // limit(n)
    Skip(u64),                         // skip(n)
    Range(u64, u64),                   // range(from, to)

    // Map steps
    Values(Vec<String>),     // values('key1', 'key2')
    ValueMap(Vec<String>),   // valueMap('key1', 'key2')
    Id,                      // id()
    Label,                   // label()
    Properties(Vec<String>), // properties('key1', 'key2')
    Count,                   // count()
    Sum,                     // sum()
    Min,                     // min()
    Max,                     // max()
    Mean,                    // mean()
    Select(Vec<String>),     // select('a', 'b')
    Project(Vec<String>),    // project('key1', 'key2')
    Path,                    // path()
    SimplePath,              // simplePath()
    CyclicPath,              // cyclicPath()

    // Branch steps
    Repeat(Box<GremlinTraversal>), // repeat(traversal)
    Times(u32),                    // times(n) - modifier for repeat
    Until(Box<GremlinTraversal>),  // until(predicate) - modifier for repeat
    Emit,                          // emit() - modifier for repeat
    Union(Vec<GremlinTraversal>),  // union(t1, t2, ...)
    Choose(
        Box<GremlinTraversal>,
        Box<GremlinTraversal>,
        Option<Box<GremlinTraversal>>,
    ),
    Coalesce(Vec<GremlinTraversal>), // coalesce(t1, t2, ...)

    // Side effect steps
    As(String),        // as('label')
    By(ByModifier),    // by('key') or by(traversal)
    Aggregate(String), // aggregate('label')
    Store(String),     // store('label')
    Group,             // group()
    GroupCount,        // groupCount()

    // Terminal steps
    ToList, // toList()
    ToSet,  // toSet()
    Next,   // next()
    Fold,   // fold()
}

/// Value in Gremlin predicates
#[derive(Debug, Clone, PartialEq)]
pub enum GremlinValue {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Predicate(GremlinPredicate),
}

/// Gremlin predicates for comparisons
#[derive(Debug, Clone, PartialEq)]
pub enum GremlinPredicate {
    Eq(Box<GremlinValue>),                         // eq(value)
    Neq(Box<GremlinValue>),                        // neq(value)
    Lt(Box<GremlinValue>),                         // lt(value)
    Lte(Box<GremlinValue>),                        // lte(value)
    Gt(Box<GremlinValue>),                         // gt(value)
    Gte(Box<GremlinValue>),                        // gte(value)
    Between(Box<GremlinValue>, Box<GremlinValue>), // between(a, b)
    Inside(Box<GremlinValue>, Box<GremlinValue>),  // inside(a, b)
    Outside(Box<GremlinValue>, Box<GremlinValue>), // outside(a, b)
    Within(Vec<GremlinValue>),                     // within(a, b, c)
    Without(Vec<GremlinValue>),                    // without(a, b, c)
    StartingWith(String),                          // startingWith('prefix')
    EndingWith(String),                            // endingWith('suffix')
    Containing(String),                            // containing('substring')
    Regex(String),                                 // regex('pattern')
}

/// By modifier for grouping/ordering
#[derive(Debug, Clone)]
pub enum ByModifier {
    Key(String),
    Traversal(Box<GremlinTraversal>),
    Order(OrderDirection),
}

/// Order direction for by()
#[derive(Debug, Clone)]
pub enum OrderDirection {
    Asc,
    Desc,
}

/// Gremlin parser
pub struct GremlinParser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> GremlinParser<'a> {
    /// Create a new parser
    pub fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    /// Parse a Gremlin query string
    pub fn parse(input: &str) -> Result<GremlinTraversal, GremlinError> {
        let mut parser = GremlinParser::new(input);
        parser.parse_traversal()
    }

    /// Parse a full traversal
    fn parse_traversal(&mut self) -> Result<GremlinTraversal, GremlinError> {
        self.skip_whitespace();

        // Determine source
        let source = if self.consume_if("g.") {
            TraversalSource::Graph
        } else if self.consume_if("__.") {
            TraversalSource::Anonymous
        } else if self.consume_if("__") {
            // Just __ without dot - valid anonymous start
            TraversalSource::Anonymous
        } else {
            return Err(self.error("Expected 'g.' or '__' at start of traversal"));
        };

        let mut steps = Vec::new();

        // Parse steps
        loop {
            self.skip_whitespace();

            if self.is_at_end() || self.peek() == Some(')') || self.peek() == Some(',') {
                break;
            }

            // Skip dots between steps
            self.consume_if(".");

            self.skip_whitespace();

            if self.is_at_end() || self.peek() == Some(')') || self.peek() == Some(',') {
                break;
            }

            let step = self.parse_step()?;
            steps.push(step);
        }

        Ok(GremlinTraversal { source, steps })
    }

    /// Parse a single step
    fn parse_step(&mut self) -> Result<GremlinStep, GremlinError> {
        let name = self.parse_identifier()?;

        match name.as_str() {
            // Source steps
            "V" => {
                self.expect('(')?;
                let id = self.parse_optional_string_arg()?;
                self.expect(')')?;
                Ok(GremlinStep::V(id))
            }
            "E" => {
                self.expect('(')?;
                let id = self.parse_optional_string_arg()?;
                self.expect(')')?;
                Ok(GremlinStep::E(id))
            }

            // Traversal steps
            "out" => {
                self.expect('(')?;
                let label = self.parse_optional_string_arg()?;
                self.expect(')')?;
                Ok(GremlinStep::Out(label))
            }
            "in" => {
                self.expect('(')?;
                let label = self.parse_optional_string_arg()?;
                self.expect(')')?;
                Ok(GremlinStep::In(label))
            }
            "both" => {
                self.expect('(')?;
                let label = self.parse_optional_string_arg()?;
                self.expect(')')?;
                Ok(GremlinStep::Both(label))
            }
            "outE" => {
                self.expect('(')?;
                let label = self.parse_optional_string_arg()?;
                self.expect(')')?;
                Ok(GremlinStep::OutE(label))
            }
            "inE" => {
                self.expect('(')?;
                let label = self.parse_optional_string_arg()?;
                self.expect(')')?;
                Ok(GremlinStep::InE(label))
            }
            "bothE" => {
                self.expect('(')?;
                let label = self.parse_optional_string_arg()?;
                self.expect(')')?;
                Ok(GremlinStep::BothE(label))
            }
            "outV" => {
                self.expect('(')?;
                self.expect(')')?;
                Ok(GremlinStep::OutV)
            }
            "inV" => {
                self.expect('(')?;
                self.expect(')')?;
                Ok(GremlinStep::InV)
            }
            "bothV" => {
                self.expect('(')?;
                self.expect(')')?;
                Ok(GremlinStep::BothV)
            }
            "otherV" => {
                self.expect('(')?;
                self.expect(')')?;
                Ok(GremlinStep::OtherV)
            }

            // Filter steps
            "has" => {
                self.expect('(')?;
                let key = self.parse_string()?;
                self.skip_whitespace();
                let value = if self.consume_if(",") {
                    self.skip_whitespace();
                    Some(self.parse_value()?)
                } else {
                    None
                };
                self.expect(')')?;
                Ok(GremlinStep::Has(key, value))
            }
            "hasNot" => {
                self.expect('(')?;
                let key = self.parse_string()?;
                self.expect(')')?;
                Ok(GremlinStep::HasNot(key))
            }
            "hasLabel" => {
                self.expect('(')?;
                let label = self.parse_string()?;
                self.expect(')')?;
                Ok(GremlinStep::HasLabel(label))
            }
            "hasId" => {
                self.expect('(')?;
                let id = self.parse_string()?;
                self.expect(')')?;
                Ok(GremlinStep::HasId(id))
            }
            "dedup" => {
                self.expect('(')?;
                self.expect(')')?;
                Ok(GremlinStep::Dedup)
            }
            "limit" => {
                self.expect('(')?;
                let n = self.parse_integer()? as u64;
                self.expect(')')?;
                Ok(GremlinStep::Limit(n))
            }
            "skip" => {
                self.expect('(')?;
                let n = self.parse_integer()? as u64;
                self.expect(')')?;
                Ok(GremlinStep::Skip(n))
            }
            "range" => {
                self.expect('(')?;
                let from = self.parse_integer()? as u64;
                self.expect(',')?;
                self.skip_whitespace();
                let to = self.parse_integer()? as u64;
                self.expect(')')?;
                Ok(GremlinStep::Range(from, to))
            }

            // Map steps
            "values" => {
                self.expect('(')?;
                let keys = self.parse_string_list()?;
                self.expect(')')?;
                Ok(GremlinStep::Values(keys))
            }
            "valueMap" => {
                self.expect('(')?;
                let keys = self.parse_string_list()?;
                self.expect(')')?;
                Ok(GremlinStep::ValueMap(keys))
            }
            "id" => {
                self.expect('(')?;
                self.expect(')')?;
                Ok(GremlinStep::Id)
            }
            "label" => {
                self.expect('(')?;
                self.expect(')')?;
                Ok(GremlinStep::Label)
            }
            "properties" => {
                self.expect('(')?;
                let keys = self.parse_string_list()?;
                self.expect(')')?;
                Ok(GremlinStep::Properties(keys))
            }
            "count" => {
                self.expect('(')?;
                self.expect(')')?;
                Ok(GremlinStep::Count)
            }
            "sum" => {
                self.expect('(')?;
                self.expect(')')?;
                Ok(GremlinStep::Sum)
            }
            "min" => {
                self.expect('(')?;
                self.expect(')')?;
                Ok(GremlinStep::Min)
            }
            "max" => {
                self.expect('(')?;
                self.expect(')')?;
                Ok(GremlinStep::Max)
            }
            "mean" => {
                self.expect('(')?;
                self.expect(')')?;
                Ok(GremlinStep::Mean)
            }
            "select" => {
                self.expect('(')?;
                let labels = self.parse_string_list()?;
                self.expect(')')?;
                Ok(GremlinStep::Select(labels))
            }
            "project" => {
                self.expect('(')?;
                let keys = self.parse_string_list()?;
                self.expect(')')?;
                Ok(GremlinStep::Project(keys))
            }
            "path" => {
                self.expect('(')?;
                self.expect(')')?;
                Ok(GremlinStep::Path)
            }
            "simplePath" => {
                self.expect('(')?;
                self.expect(')')?;
                Ok(GremlinStep::SimplePath)
            }
            "cyclicPath" => {
                self.expect('(')?;
                self.expect(')')?;
                Ok(GremlinStep::CyclicPath)
            }

            // Branch steps
            "repeat" => {
                self.expect('(')?;
                let inner = self.parse_inner_traversal()?;
                self.expect(')')?;
                Ok(GremlinStep::Repeat(Box::new(inner)))
            }
            "times" => {
                self.expect('(')?;
                let n = self.parse_integer()? as u32;
                self.expect(')')?;
                Ok(GremlinStep::Times(n))
            }
            "until" => {
                self.expect('(')?;
                let inner = self.parse_inner_traversal()?;
                self.expect(')')?;
                Ok(GremlinStep::Until(Box::new(inner)))
            }
            "emit" => {
                self.expect('(')?;
                self.expect(')')?;
                Ok(GremlinStep::Emit)
            }

            // Side effect steps
            "as" => {
                self.expect('(')?;
                let label = self.parse_string()?;
                self.expect(')')?;
                Ok(GremlinStep::As(label))
            }
            "aggregate" => {
                self.expect('(')?;
                let label = self.parse_string()?;
                self.expect(')')?;
                Ok(GremlinStep::Aggregate(label))
            }
            "store" => {
                self.expect('(')?;
                let label = self.parse_string()?;
                self.expect(')')?;
                Ok(GremlinStep::Store(label))
            }
            "group" => {
                self.expect('(')?;
                self.expect(')')?;
                Ok(GremlinStep::Group)
            }
            "groupCount" => {
                self.expect('(')?;
                self.expect(')')?;
                Ok(GremlinStep::GroupCount)
            }

            // Terminal steps
            "toList" => {
                self.expect('(')?;
                self.expect(')')?;
                Ok(GremlinStep::ToList)
            }
            "toSet" => {
                self.expect('(')?;
                self.expect(')')?;
                Ok(GremlinStep::ToSet)
            }
            "next" => {
                self.expect('(')?;
                self.expect(')')?;
                Ok(GremlinStep::Next)
            }
            "fold" => {
                self.expect('(')?;
                self.expect(')')?;
                Ok(GremlinStep::Fold)
            }

            _ => Err(self.error(&format!("Unknown step: {}", name))),
        }
    }

    /// Parse an inner traversal (for repeat, where, etc.)
    fn parse_inner_traversal(&mut self) -> Result<GremlinTraversal, GremlinError> {
        self.skip_whitespace();

        // Check if it starts with __ or g.
        if self.input[self.pos..].starts_with("__") || self.input[self.pos..].starts_with("g.") {
            return self.parse_traversal();
        }

        // Otherwise, create anonymous traversal from steps
        let mut steps = Vec::new();

        loop {
            self.skip_whitespace();

            if self.is_at_end() || self.peek() == Some(')') {
                break;
            }

            // Skip dots between steps
            self.consume_if(".");

            self.skip_whitespace();

            if self.is_at_end() || self.peek() == Some(')') {
                break;
            }

            let step = self.parse_step()?;
            steps.push(step);
        }

        Ok(GremlinTraversal {
            source: TraversalSource::Anonymous,
            steps,
        })
    }

    // Helper methods

    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.pos += 1;
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
        if self.input[self.pos..].starts_with(s) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: char) -> Result<(), GremlinError> {
        self.skip_whitespace();
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.error(&format!("Expected '{}', found {:?}", c, self.peek())))
        }
    }

    fn parse_identifier(&mut self) -> Result<String, GremlinError> {
        self.skip_whitespace();

        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' {
                self.pos += 1;
            } else {
                break;
            }
        }

        if self.pos == start {
            Err(self.error("Expected identifier"))
        } else {
            Ok(self.input[start..self.pos].to_string())
        }
    }

    fn parse_string(&mut self) -> Result<String, GremlinError> {
        self.skip_whitespace();

        let quote = self.peek();
        if quote != Some('\'') && quote != Some('"') {
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
                self.pos += 2; // Skip escape
            } else {
                self.pos += 1;
            }
        }

        Err(self.error("Unterminated string"))
    }

    fn parse_optional_string_arg(&mut self) -> Result<Option<String>, GremlinError> {
        self.skip_whitespace();
        if self.peek() == Some(')') {
            Ok(None)
        } else if self.peek() == Some('\'') || self.peek() == Some('"') {
            Ok(Some(self.parse_string()?))
        } else {
            // Could be unquoted ID
            let start = self.pos;
            while let Some(c) = self.peek() {
                if c.is_alphanumeric() || c == '_' || c == ':' || c == '.' || c == '-' {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            if self.pos > start {
                Ok(Some(self.input[start..self.pos].to_string()))
            } else {
                Ok(None)
            }
        }
    }

    fn parse_string_list(&mut self) -> Result<Vec<String>, GremlinError> {
        let mut result = Vec::new();

        self.skip_whitespace();
        if self.peek() == Some(')') {
            return Ok(result);
        }

        loop {
            self.skip_whitespace();
            if self.peek() == Some(')') {
                break;
            }

            result.push(self.parse_string()?);

            self.skip_whitespace();
            if !self.consume_if(",") {
                break;
            }
        }

        Ok(result)
    }

    fn parse_integer(&mut self) -> Result<i64, GremlinError> {
        self.skip_whitespace();

        let start = self.pos;
        if self.peek() == Some('-') {
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

    fn parse_value(&mut self) -> Result<GremlinValue, GremlinError> {
        self.skip_whitespace();

        // String
        if self.peek() == Some('\'') || self.peek() == Some('"') {
            return Ok(GremlinValue::String(self.parse_string()?));
        }

        // Boolean
        if self.consume_if("true") {
            return Ok(GremlinValue::Boolean(true));
        }
        if self.consume_if("false") {
            return Ok(GremlinValue::Boolean(false));
        }

        // Number
        let start = self.pos;
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == '.' {
                self.pos += 1;
            } else {
                break;
            }
        }

        let s = &self.input[start..self.pos];
        if s.contains('.') {
            let f: f64 = s
                .parse()
                .map_err(|_| self.error(&format!("Invalid float: {}", s)))?;
            Ok(GremlinValue::Float(f))
        } else {
            let i: i64 = s
                .parse()
                .map_err(|_| self.error(&format!("Invalid integer: {}", s)))?;
            Ok(GremlinValue::Integer(i))
        }
    }

    fn error(&self, message: &str) -> GremlinError {
        GremlinError {
            message: message.to_string(),
            position: self.pos,
        }
    }
}

impl GremlinTraversal {
    /// Convert Gremlin traversal to QueryExpr
    pub fn to_query_expr(&self) -> QueryExpr {
        // Build graph pattern from steps
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut filters = Vec::new();
        let mut projections = Vec::new();

        let mut current_alias = "n0".to_string();
        let mut alias_counter = 0;

        for step in &self.steps {
            match step {
                GremlinStep::V(id) => {
                    let mut node = NodePattern {
                        alias: current_alias.clone(),
                        node_type: None,
                        properties: Vec::new(),
                    };
                    if let Some(id) = id {
                        node.properties.push(PropertyFilter {
                            name: "id".to_string(),
                            op: CompareOp::Eq,
                            value: Value::text(id.clone()),
                        });
                    }
                    nodes.push(node);
                }
                GremlinStep::HasLabel(label) => {
                    if let Some(last) = nodes.last_mut() {
                        // Map label string to GraphNodeType
                        last.node_type = match label.to_lowercase().as_str() {
                            "host" => Some(GraphNodeType::Host),
                            "service" => Some(GraphNodeType::Service),
                            "credential" => Some(GraphNodeType::Credential),
                            "vulnerability" | "vuln" => Some(GraphNodeType::Vulnerability),
                            "endpoint" => Some(GraphNodeType::Endpoint),
                            "technology" | "tech" => Some(GraphNodeType::Technology),
                            "user" => Some(GraphNodeType::User),
                            "domain" => Some(GraphNodeType::Domain),
                            "certificate" | "cert" => Some(GraphNodeType::Certificate),
                            _ => None,
                        };
                    }
                }
                GremlinStep::Has(key, value) => {
                    let field_ref = FieldRef::NodeProperty {
                        alias: current_alias.clone(),
                        property: key.clone(),
                    };
                    let filter = if let Some(val) = value {
                        Filter::Compare {
                            field: field_ref,
                            op: CompareOp::Eq,
                            value: match val {
                                GremlinValue::String(s) => Value::text(s.clone()),
                                GremlinValue::Integer(i) => Value::Integer(*i),
                                GremlinValue::Float(f) => Value::Float(*f),
                                GremlinValue::Boolean(b) => Value::Boolean(*b),
                                GremlinValue::Predicate(_) => Value::Null, // Predicates handled separately
                            },
                        }
                    } else {
                        Filter::IsNotNull(field_ref)
                    };
                    filters.push(filter);
                }
                GremlinStep::Out(label) | GremlinStep::In(label) | GremlinStep::Both(label) => {
                    alias_counter += 1;
                    let new_alias = format!("n{}", alias_counter);

                    let direction = match step {
                        GremlinStep::Out(_) => EdgeDirection::Outgoing,
                        GremlinStep::In(_) => EdgeDirection::Incoming,
                        GremlinStep::Both(_) => EdgeDirection::Both,
                        _ => EdgeDirection::Outgoing,
                    };

                    // Map edge label to GraphEdgeType
                    let edge_type = label
                        .as_ref()
                        .and_then(|l| match l.to_lowercase().as_str() {
                            "has_service" | "hasservice" => Some(GraphEdgeType::HasService),
                            "has_endpoint" | "hasendpoint" => Some(GraphEdgeType::HasEndpoint),
                            "uses_tech" | "usestech" => Some(GraphEdgeType::UsesTech),
                            "auth_access" | "authaccess" => Some(GraphEdgeType::AuthAccess),
                            "affected_by" | "affectedby" => Some(GraphEdgeType::AffectedBy),
                            "contains" => Some(GraphEdgeType::Contains),
                            "connects_to" | "connectsto" | "connects" => {
                                Some(GraphEdgeType::ConnectsTo)
                            }
                            "related_to" | "relatedto" => Some(GraphEdgeType::RelatedTo),
                            "has_user" | "hasuser" => Some(GraphEdgeType::HasUser),
                            "has_cert" | "hascert" => Some(GraphEdgeType::HasCert),
                            _ => None,
                        });

                    edges.push(EdgePattern {
                        alias: None,
                        from: current_alias.clone(),
                        to: new_alias.clone(),
                        edge_type,
                        direction,
                        min_hops: 1,
                        max_hops: 1,
                    });

                    nodes.push(NodePattern {
                        alias: new_alias.clone(),
                        node_type: None,
                        properties: Vec::new(),
                    });

                    current_alias = new_alias;
                }
                GremlinStep::Limit(_n) => {
                    // Note: limit is handled at execution time, not in GraphQuery
                    // Store in execution context if needed
                }
                GremlinStep::Values(keys) => {
                    for key in keys {
                        projections.push(Projection::from_field(FieldRef::NodeProperty {
                            alias: current_alias.clone(),
                            property: key.clone(),
                        }));
                    }
                }
                GremlinStep::Count => {
                    // Count is an aggregation, add a marker projection
                    projections.push(Projection::Field(
                        FieldRef::NodeId {
                            alias: current_alias.clone(),
                        },
                        Some("count".to_string()),
                    ));
                }
                GremlinStep::As(label) => {
                    if let Some(last) = nodes.last_mut() {
                        last.alias = label.clone();
                        current_alias = label.clone();
                    }
                }
                _ => {}
            }
        }

        // If no projections, return all node properties
        if projections.is_empty() {
            projections.push(Projection::from_field(FieldRef::NodeId {
                alias: current_alias.clone(),
            }));
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_v() {
        let t = GremlinParser::parse("g.V()").unwrap();
        assert_eq!(t.source, TraversalSource::Graph);
        assert_eq!(t.steps.len(), 1);
        assert!(matches!(t.steps[0], GremlinStep::V(None)));
    }

    #[test]
    fn test_parse_v_with_id() {
        let t = GremlinParser::parse("g.V('host:10.0.0.1')").unwrap();
        assert!(matches!(&t.steps[0], GremlinStep::V(Some(id)) if id == "host:10.0.0.1"));
    }

    #[test]
    fn test_parse_has_label() {
        let t = GremlinParser::parse("g.V().hasLabel('host')").unwrap();
        assert_eq!(t.steps.len(), 2);
        assert!(matches!(&t.steps[1], GremlinStep::HasLabel(l) if l == "host"));
    }

    #[test]
    fn test_parse_has_key_value() {
        let t = GremlinParser::parse("g.V().has('name', 'alice')").unwrap();
        assert!(matches!(
            &t.steps[1],
            GremlinStep::Has(k, Some(GremlinValue::String(v))) if k == "name" && v == "alice"
        ));
    }

    #[test]
    fn test_parse_out() {
        let t = GremlinParser::parse("g.V().out('knows')").unwrap();
        assert!(matches!(&t.steps[1], GremlinStep::Out(Some(l)) if l == "knows"));
    }

    #[test]
    fn test_parse_chain() {
        let t =
            GremlinParser::parse("g.V().hasLabel('host').out('connects').has('port', 22)").unwrap();
        assert_eq!(t.steps.len(), 4);
    }

    #[test]
    fn test_parse_limit() {
        let t = GremlinParser::parse("g.V().limit(10)").unwrap();
        assert!(matches!(t.steps[1], GremlinStep::Limit(10)));
    }

    #[test]
    fn test_parse_count() {
        let t = GremlinParser::parse("g.V().count()").unwrap();
        assert!(matches!(t.steps[1], GremlinStep::Count));
    }

    #[test]
    fn test_parse_repeat_times() {
        let t = GremlinParser::parse("g.V().repeat(out()).times(3)").unwrap();
        assert_eq!(t.steps.len(), 3);
        assert!(matches!(&t.steps[1], GremlinStep::Repeat(_)));
        assert!(matches!(t.steps[2], GremlinStep::Times(3)));
    }

    #[test]
    fn test_parse_anonymous() {
        let t = GremlinParser::parse("__.out('knows')").unwrap();
        assert_eq!(t.source, TraversalSource::Anonymous);
        assert!(matches!(&t.steps[0], GremlinStep::Out(Some(l)) if l == "knows"));
    }

    #[test]
    fn test_parse_values() {
        let t = GremlinParser::parse("g.V().values('name', 'age')").unwrap();
        assert!(matches!(&t.steps[1], GremlinStep::Values(keys) if keys.len() == 2));
    }

    #[test]
    fn test_to_query_expr() {
        let t = GremlinParser::parse("g.V().hasLabel('host').out('connects').limit(10)").unwrap();
        let expr = t.to_query_expr();
        assert!(matches!(expr, QueryExpr::Graph(_)));
    }
}
