//! Algebraic Query Operators
//!
//! Jena-inspired operator tree representing query algebra.
//!
//! # Operator Hierarchy
//!
//! ```text
//! Op
//! ├── OpBGP           - Basic Graph Pattern (triple patterns)
//! ├── OpTriple        - Single triple pattern
//! ├── OpJoin          - Join two operators
//! ├── OpLeftJoin      - Left outer join (OPTIONAL)
//! ├── OpFilter        - Filter with expression
//! ├── OpUnion         - Union of two operators
//! ├── OpProject       - Select variables
//! ├── OpDistinct      - Remove duplicates
//! ├── OpReduced       - Remove adjacent duplicates
//! ├── OpSlice         - Offset and limit
//! ├── OpOrder         - Sort results
//! ├── OpGroup         - Group by with aggregation
//! ├── OpExtend        - Assign expression to variable
//! ├── OpMinus         - Set difference
//! ├── OpTable         - Inline data
//! ├── OpSequence      - Sequential execution
//! ├── OpDisjunction   - OR pattern
//! └── OpNull          - Empty pattern
//! ```

use super::binding::{Binding, Value, Var};
use std::fmt;

/// Triple pattern for graph matching
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Triple {
    /// Subject
    pub subject: Pattern,
    /// Predicate
    pub predicate: Pattern,
    /// Object
    pub object: Pattern,
}

impl Triple {
    /// Create new triple pattern
    pub fn new(subject: Pattern, predicate: Pattern, object: Pattern) -> Self {
        Self {
            subject,
            predicate,
            object,
        }
    }

    /// Get all variables in this triple
    pub fn vars(&self) -> Vec<Var> {
        let mut vars = Vec::new();
        if let Pattern::Var(v) = &self.subject {
            vars.push(v.clone());
        }
        if let Pattern::Var(v) = &self.predicate {
            vars.push(v.clone());
        }
        if let Pattern::Var(v) = &self.object {
            vars.push(v.clone());
        }
        vars
    }

    /// Check if this triple is concrete (no variables)
    pub fn is_concrete(&self) -> bool {
        !matches!(self.subject, Pattern::Var(_))
            && !matches!(self.predicate, Pattern::Var(_))
            && !matches!(self.object, Pattern::Var(_))
    }
}

impl fmt::Display for Triple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({} {} {})", self.subject, self.predicate, self.object)
    }
}

/// Pattern element (variable or concrete value)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    /// Variable
    Var(Var),
    /// URI/Node ID
    Uri(String),
    /// Literal string
    Literal(String),
    /// Literal with datatype
    TypedLiteral(String, String),
    /// Any (wildcard)
    Any,
}

impl Pattern {
    /// Check if this is a variable
    pub fn is_var(&self) -> bool {
        matches!(self, Pattern::Var(_))
    }

    /// Get variable if present
    pub fn as_var(&self) -> Option<&Var> {
        match self {
            Pattern::Var(v) => Some(v),
            _ => None,
        }
    }

    /// Convert to Value
    pub fn to_value(&self) -> Option<Value> {
        match self {
            Pattern::Var(_) => None,
            Pattern::Uri(s) => Some(Value::Uri(s.clone())),
            Pattern::Literal(s) => Some(Value::String(s.clone())),
            Pattern::TypedLiteral(v, _) => Some(Value::String(v.clone())),
            Pattern::Any => None,
        }
    }
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Pattern::Var(v) => write!(f, "{}", v),
            Pattern::Uri(s) => write!(f, "<{}>", s),
            Pattern::Literal(s) => write!(f, "\"{}\"", s),
            Pattern::TypedLiteral(v, t) => write!(f, "\"{}\"^^<{}>", v, t),
            Pattern::Any => write!(f, "_"),
        }
    }
}

/// Core algebraic operator type
#[derive(Debug, Clone)]
pub enum Op {
    /// Basic graph pattern (set of triples)
    BGP(OpBGP),
    /// Single triple pattern
    Triple(OpTriple),
    /// Inner join
    Join(OpJoin),
    /// Left outer join (OPTIONAL)
    LeftJoin(OpLeftJoin),
    /// Right outer join
    RightJoin(OpRightJoin),
    /// Cross join (Cartesian product)
    CrossJoin(OpCrossJoin),
    /// Filter
    Filter(OpFilter),
    /// Union
    Union(OpUnion),
    /// Project to variables
    Project(OpProject),
    /// Distinct
    Distinct(OpDistinct),
    /// Reduced (remove adjacent duplicates)
    Reduced(OpReduced),
    /// Offset/Limit
    Slice(OpSlice),
    /// Order by
    Order(OpOrder),
    /// Group by
    Group(OpGroup),
    /// Extend (bind expression to variable)
    Extend(OpExtend),
    /// Set difference
    Minus(OpMinus),
    /// Set intersection
    Intersect(OpIntersect),
    /// Inline data
    Table(OpTable),
    /// Sequential execution
    Sequence(OpSequence),
    /// Disjunction (OR patterns)
    Disjunction(OpDisjunction),
    /// Empty pattern
    Null(OpNull),
}

impl Op {
    /// Get all variables in this operator
    pub fn vars(&self) -> Vec<Var> {
        match self {
            Op::BGP(op) => op.vars(),
            Op::Triple(op) => op.triple.vars(),
            Op::Join(op) => {
                let mut vars = op.left.vars();
                for v in op.right.vars() {
                    if !vars.contains(&v) {
                        vars.push(v);
                    }
                }
                vars
            }
            Op::LeftJoin(op) => {
                let mut vars = op.left.vars();
                for v in op.right.vars() {
                    if !vars.contains(&v) {
                        vars.push(v);
                    }
                }
                vars
            }
            Op::RightJoin(op) => {
                let mut vars = op.left.vars();
                for v in op.right.vars() {
                    if !vars.contains(&v) {
                        vars.push(v);
                    }
                }
                vars
            }
            Op::CrossJoin(op) => {
                let mut vars = op.left.vars();
                for v in op.right.vars() {
                    if !vars.contains(&v) {
                        vars.push(v);
                    }
                }
                vars
            }
            Op::Filter(op) => op.sub_op.vars(),
            Op::Union(op) => {
                let mut vars = op.left.vars();
                for v in op.right.vars() {
                    if !vars.contains(&v) {
                        vars.push(v);
                    }
                }
                vars
            }
            Op::Project(op) => op.vars.clone(),
            Op::Distinct(op) => op.sub_op.vars(),
            Op::Reduced(op) => op.sub_op.vars(),
            Op::Slice(op) => op.sub_op.vars(),
            Op::Order(op) => op.sub_op.vars(),
            Op::Group(op) => {
                let mut vars = op.group_vars.clone();
                for (v, _) in &op.aggregates {
                    vars.push(v.clone());
                }
                vars
            }
            Op::Extend(op) => {
                let mut vars = op.sub_op.vars();
                if !vars.contains(&op.var) {
                    vars.push(op.var.clone());
                }
                vars
            }
            Op::Minus(op) => op.left.vars(),
            Op::Intersect(op) => {
                // Intersection preserves common variables
                let left_vars = op.left.vars();
                let right_vars = op.right.vars();
                left_vars
                    .into_iter()
                    .filter(|v| right_vars.contains(v))
                    .collect()
            }
            Op::Table(op) => op.vars.clone(),
            Op::Sequence(op) => {
                let mut vars = Vec::new();
                for sub in &op.ops {
                    for v in sub.vars() {
                        if !vars.contains(&v) {
                            vars.push(v);
                        }
                    }
                }
                vars
            }
            Op::Disjunction(op) => {
                let mut vars = Vec::new();
                for sub in &op.ops {
                    for v in sub.vars() {
                        if !vars.contains(&v) {
                            vars.push(v);
                        }
                    }
                }
                vars
            }
            Op::Null(_) => Vec::new(),
        }
    }

    /// Check if this operator is a null/empty pattern
    pub fn is_null(&self) -> bool {
        matches!(self, Op::Null(_))
    }
}

/// Basic Graph Pattern - set of triple patterns
#[derive(Debug, Clone)]
pub struct OpBGP {
    /// Triple patterns
    pub triples: Vec<Triple>,
}

impl OpBGP {
    /// Create empty BGP
    pub fn new() -> Self {
        Self {
            triples: Vec::new(),
        }
    }

    /// Create from triples
    pub fn from_triples(triples: Vec<Triple>) -> Self {
        Self { triples }
    }

    /// Add triple
    pub fn add(&mut self, triple: Triple) {
        self.triples.push(triple);
    }

    /// Get all variables
    pub fn vars(&self) -> Vec<Var> {
        let mut vars = Vec::new();
        for triple in &self.triples {
            for v in triple.vars() {
                if !vars.contains(&v) {
                    vars.push(v);
                }
            }
        }
        vars
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.triples.is_empty()
    }
}

impl Default for OpBGP {
    fn default() -> Self {
        Self::new()
    }
}

/// Single triple pattern
#[derive(Debug, Clone)]
pub struct OpTriple {
    pub triple: Triple,
}

impl OpTriple {
    /// Create new triple op
    pub fn new(triple: Triple) -> Self {
        Self { triple }
    }
}

/// Join two operators
#[derive(Debug, Clone)]
pub struct OpJoin {
    pub left: Box<Op>,
    pub right: Box<Op>,
}

impl OpJoin {
    /// Create join
    pub fn new(left: Op, right: Op) -> Self {
        Self {
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    /// Create from multiple ops (left-associative join chain)
    pub fn join_all(ops: Vec<Op>) -> Op {
        if ops.is_empty() {
            return Op::Null(OpNull);
        }

        let mut result = ops.into_iter();
        let mut current = result.next().unwrap();

        for op in result {
            current = Op::Join(OpJoin::new(current, op));
        }

        current
    }
}

/// Left outer join (OPTIONAL)
#[derive(Debug, Clone)]
pub struct OpLeftJoin {
    pub left: Box<Op>,
    pub right: Box<Op>,
    /// Filter expression for the join
    pub filter: Option<FilterExpr>,
}

impl OpLeftJoin {
    /// Create left join
    pub fn new(left: Op, right: Op) -> Self {
        Self {
            left: Box::new(left),
            right: Box::new(right),
            filter: None,
        }
    }

    /// Create with filter
    pub fn with_filter(left: Op, right: Op, filter: FilterExpr) -> Self {
        Self {
            left: Box::new(left),
            right: Box::new(right),
            filter: Some(filter),
        }
    }
}

/// Right outer join
#[derive(Debug, Clone)]
pub struct OpRightJoin {
    pub left: Box<Op>,
    pub right: Box<Op>,
    /// Filter expression for the join
    pub filter: Option<FilterExpr>,
}

impl OpRightJoin {
    /// Create right join
    pub fn new(left: Op, right: Op) -> Self {
        Self {
            left: Box::new(left),
            right: Box::new(right),
            filter: None,
        }
    }

    /// Create with filter
    pub fn with_filter(left: Op, right: Op, filter: FilterExpr) -> Self {
        Self {
            left: Box::new(left),
            right: Box::new(right),
            filter: Some(filter),
        }
    }
}

/// Cross join (Cartesian product)
#[derive(Debug, Clone)]
pub struct OpCrossJoin {
    pub left: Box<Op>,
    pub right: Box<Op>,
}

impl OpCrossJoin {
    /// Create cross join
    pub fn new(left: Op, right: Op) -> Self {
        Self {
            left: Box::new(left),
            right: Box::new(right),
        }
    }
}

/// Filter expression for filtering bindings
#[derive(Debug, Clone)]
pub enum FilterExpr {
    /// Equality
    Eq(ExprTerm, ExprTerm),
    /// Not equal
    NotEq(ExprTerm, ExprTerm),
    /// Less than
    Lt(ExprTerm, ExprTerm),
    /// Less than or equal
    LtEq(ExprTerm, ExprTerm),
    /// Greater than
    Gt(ExprTerm, ExprTerm),
    /// Greater than or equal
    GtEq(ExprTerm, ExprTerm),
    /// Logical AND
    And(Box<FilterExpr>, Box<FilterExpr>),
    /// Logical OR
    Or(Box<FilterExpr>, Box<FilterExpr>),
    /// Logical NOT
    Not(Box<FilterExpr>),
    /// Bound check
    Bound(Var),
    /// Regex match
    Regex(ExprTerm, String, Option<String>),
    /// String starts with
    StartsWith(ExprTerm, String),
    /// String ends with
    EndsWith(ExprTerm, String),
    /// String contains
    Contains(ExprTerm, String),
    /// Is IRI
    IsUri(ExprTerm),
    /// Is literal
    IsLiteral(ExprTerm),
    /// Is blank node
    IsBlank(ExprTerm),
    /// In list
    In(ExprTerm, Vec<ExprTerm>),
    /// Not in list
    NotIn(ExprTerm, Vec<ExprTerm>),
    /// True constant
    True,
    /// False constant
    False,
}

impl FilterExpr {
    /// Create AND expression
    pub fn and(left: FilterExpr, right: FilterExpr) -> FilterExpr {
        FilterExpr::And(Box::new(left), Box::new(right))
    }

    /// Create OR expression
    pub fn or(left: FilterExpr, right: FilterExpr) -> FilterExpr {
        FilterExpr::Or(Box::new(left), Box::new(right))
    }

    /// Create NOT expression
    pub fn not(expr: FilterExpr) -> FilterExpr {
        FilterExpr::Not(Box::new(expr))
    }

    /// Evaluate against a binding
    pub fn evaluate(&self, binding: &Binding) -> bool {
        match self {
            FilterExpr::Eq(left, right) => {
                let l = left.evaluate(binding);
                let r = right.evaluate(binding);
                l == r
            }
            FilterExpr::NotEq(left, right) => {
                let l = left.evaluate(binding);
                let r = right.evaluate(binding);
                l != r
            }
            FilterExpr::Lt(left, right) => compare_terms(left, right, binding, |a, b| a < b),
            FilterExpr::LtEq(left, right) => compare_terms(left, right, binding, |a, b| a <= b),
            FilterExpr::Gt(left, right) => compare_terms(left, right, binding, |a, b| a > b),
            FilterExpr::GtEq(left, right) => compare_terms(left, right, binding, |a, b| a >= b),
            FilterExpr::And(left, right) => left.evaluate(binding) && right.evaluate(binding),
            FilterExpr::Or(left, right) => left.evaluate(binding) || right.evaluate(binding),
            FilterExpr::Not(expr) => !expr.evaluate(binding),
            FilterExpr::Bound(var) => binding.contains(var),
            FilterExpr::Regex(term, pattern, flags) => {
                if let Some(Value::String(s)) = term.evaluate(binding) {
                    // Simple regex matching (production would use regex crate)
                    let case_insensitive = flags.as_ref().map(|f| f.contains('i')).unwrap_or(false);
                    if case_insensitive {
                        s.to_lowercase().contains(&pattern.to_lowercase())
                    } else {
                        s.contains(pattern)
                    }
                } else {
                    false
                }
            }
            FilterExpr::StartsWith(term, prefix) => {
                if let Some(Value::String(s)) = term.evaluate(binding) {
                    s.starts_with(prefix)
                } else {
                    false
                }
            }
            FilterExpr::EndsWith(term, suffix) => {
                if let Some(Value::String(s)) = term.evaluate(binding) {
                    s.ends_with(suffix)
                } else {
                    false
                }
            }
            FilterExpr::Contains(term, substring) => {
                if let Some(Value::String(s)) = term.evaluate(binding) {
                    s.contains(substring)
                } else {
                    false
                }
            }
            FilterExpr::IsUri(term) => {
                matches!(term.evaluate(binding), Some(Value::Uri(_)))
            }
            FilterExpr::IsLiteral(term) => {
                matches!(
                    term.evaluate(binding),
                    Some(
                        Value::String(_) | Value::Integer(_) | Value::Float(_) | Value::Boolean(_)
                    )
                )
            }
            FilterExpr::IsBlank(term) => {
                if let Some(Value::Node(id)) = term.evaluate(binding) {
                    id.starts_with("_:")
                } else {
                    false
                }
            }
            FilterExpr::In(term, list) => {
                if let Some(val) = term.evaluate(binding) {
                    list.iter()
                        .any(|t| t.evaluate(binding) == Some(val.clone()))
                } else {
                    false
                }
            }
            FilterExpr::NotIn(term, list) => {
                if let Some(val) = term.evaluate(binding) {
                    !list
                        .iter()
                        .any(|t| t.evaluate(binding) == Some(val.clone()))
                } else {
                    true
                }
            }
            FilterExpr::True => true,
            FilterExpr::False => false,
        }
    }
}

/// Compare two terms with a comparison function
fn compare_terms<F>(left: &ExprTerm, right: &ExprTerm, binding: &Binding, cmp: F) -> bool
where
    F: Fn(i64, i64) -> bool,
{
    match (left.evaluate(binding), right.evaluate(binding)) {
        (Some(Value::Integer(a)), Some(Value::Integer(b))) => cmp(a, b),
        (Some(Value::Float(a)), Some(Value::Float(b))) => cmp(a as i64, b as i64),
        (Some(Value::Integer(a)), Some(Value::Float(b))) => cmp(a, b as i64),
        (Some(Value::Float(a)), Some(Value::Integer(b))) => cmp(a as i64, b),
        _ => false,
    }
}

/// Expression term
#[derive(Debug, Clone, PartialEq)]
pub enum ExprTerm {
    /// Variable reference
    Var(Var),
    /// Constant value
    Const(Value),
    /// String function result
    Str(Box<ExprTerm>),
    /// Lowercase
    LCase(Box<ExprTerm>),
    /// Uppercase
    UCase(Box<ExprTerm>),
    /// String length
    StrLen(Box<ExprTerm>),
    /// Concatenation
    Concat(Vec<ExprTerm>),
}

impl ExprTerm {
    /// Evaluate term against binding
    pub fn evaluate(&self, binding: &Binding) -> Option<Value> {
        match self {
            ExprTerm::Var(var) => binding.get(var).cloned(),
            ExprTerm::Const(val) => Some(val.clone()),
            ExprTerm::Str(inner) => inner
                .evaluate(binding)
                .map(|v| Value::String(format!("{}", v))),
            ExprTerm::LCase(inner) => {
                if let Some(Value::String(s)) = inner.evaluate(binding) {
                    Some(Value::String(s.to_lowercase()))
                } else {
                    None
                }
            }
            ExprTerm::UCase(inner) => {
                if let Some(Value::String(s)) = inner.evaluate(binding) {
                    Some(Value::String(s.to_uppercase()))
                } else {
                    None
                }
            }
            ExprTerm::StrLen(inner) => {
                if let Some(Value::String(s)) = inner.evaluate(binding) {
                    Some(Value::Integer(s.len() as i64))
                } else {
                    None
                }
            }
            ExprTerm::Concat(terms) => {
                let mut result = String::new();
                for term in terms {
                    if let Some(Value::String(s)) = term.evaluate(binding) {
                        result.push_str(&s);
                    } else if let Some(v) = term.evaluate(binding) {
                        result.push_str(&format!("{}", v));
                    }
                }
                Some(Value::String(result))
            }
        }
    }
}

/// Filter operator
#[derive(Debug, Clone)]
pub struct OpFilter {
    pub filter: FilterExpr,
    pub sub_op: Box<Op>,
}

impl OpFilter {
    /// Create filter
    pub fn new(filter: FilterExpr, sub_op: Op) -> Self {
        Self {
            filter,
            sub_op: Box::new(sub_op),
        }
    }
}

/// Union operator
#[derive(Debug, Clone)]
pub struct OpUnion {
    pub left: Box<Op>,
    pub right: Box<Op>,
}

impl OpUnion {
    /// Create union
    pub fn new(left: Op, right: Op) -> Self {
        Self {
            left: Box::new(left),
            right: Box::new(right),
        }
    }
}

/// Project operator
#[derive(Debug, Clone)]
pub struct OpProject {
    pub vars: Vec<Var>,
    pub sub_op: Box<Op>,
}

impl OpProject {
    /// Create project
    pub fn new(vars: Vec<Var>, sub_op: Op) -> Self {
        Self {
            vars,
            sub_op: Box::new(sub_op),
        }
    }
}

/// Distinct operator
#[derive(Debug, Clone)]
pub struct OpDistinct {
    pub sub_op: Box<Op>,
}

impl OpDistinct {
    /// Create distinct
    pub fn new(sub_op: Op) -> Self {
        Self {
            sub_op: Box::new(sub_op),
        }
    }
}

/// Reduced operator (adjacent duplicate removal)
#[derive(Debug, Clone)]
pub struct OpReduced {
    pub sub_op: Box<Op>,
}

impl OpReduced {
    /// Create reduced
    pub fn new(sub_op: Op) -> Self {
        Self {
            sub_op: Box::new(sub_op),
        }
    }
}

/// Slice operator (offset/limit)
#[derive(Debug, Clone)]
pub struct OpSlice {
    pub sub_op: Box<Op>,
    pub offset: u64,
    pub limit: Option<u64>,
}

impl OpSlice {
    /// Create slice
    pub fn new(sub_op: Op, offset: u64, limit: Option<u64>) -> Self {
        Self {
            sub_op: Box::new(sub_op),
            offset,
            limit,
        }
    }

    /// Create limit only
    pub fn limit(sub_op: Op, limit: u64) -> Self {
        Self::new(sub_op, 0, Some(limit))
    }

    /// Create offset only
    pub fn offset(sub_op: Op, offset: u64) -> Self {
        Self::new(sub_op, offset, None)
    }
}

/// Order key
#[derive(Debug, Clone)]
pub struct OrderKey {
    pub expr: ExprTerm,
    pub ascending: bool,
}

impl OrderKey {
    /// Create ascending key
    pub fn asc(expr: ExprTerm) -> Self {
        Self {
            expr,
            ascending: true,
        }
    }

    /// Create descending key
    pub fn desc(expr: ExprTerm) -> Self {
        Self {
            expr,
            ascending: false,
        }
    }
}

/// Order operator
#[derive(Debug, Clone)]
pub struct OpOrder {
    pub sub_op: Box<Op>,
    pub keys: Vec<OrderKey>,
}

impl OpOrder {
    /// Create order
    pub fn new(sub_op: Op, keys: Vec<OrderKey>) -> Self {
        Self {
            sub_op: Box::new(sub_op),
            keys,
        }
    }
}

/// Aggregate function
#[derive(Debug, Clone)]
pub enum Aggregate {
    Count(Option<ExprTerm>),
    CountDistinct(ExprTerm),
    Sum(ExprTerm),
    Avg(ExprTerm),
    Min(ExprTerm),
    Max(ExprTerm),
    Sample(ExprTerm),
    GroupConcat(ExprTerm, Option<String>),
}

/// Group operator
#[derive(Debug, Clone)]
pub struct OpGroup {
    pub sub_op: Box<Op>,
    pub group_vars: Vec<Var>,
    pub aggregates: Vec<(Var, Aggregate)>,
}

impl OpGroup {
    /// Create group
    pub fn new(sub_op: Op, group_vars: Vec<Var>) -> Self {
        Self {
            sub_op: Box::new(sub_op),
            group_vars,
            aggregates: Vec::new(),
        }
    }

    /// Add aggregate
    pub fn with_aggregate(mut self, var: Var, agg: Aggregate) -> Self {
        self.aggregates.push((var, agg));
        self
    }
}

/// Extend operator (bind expression to variable)
#[derive(Debug, Clone)]
pub struct OpExtend {
    pub sub_op: Box<Op>,
    pub var: Var,
    pub expr: ExprTerm,
}

impl OpExtend {
    /// Create extend
    pub fn new(sub_op: Op, var: Var, expr: ExprTerm) -> Self {
        Self {
            sub_op: Box::new(sub_op),
            var,
            expr,
        }
    }
}

/// Minus operator (set difference)
#[derive(Debug, Clone)]
pub struct OpMinus {
    pub left: Box<Op>,
    pub right: Box<Op>,
}

impl OpMinus {
    /// Create minus
    pub fn new(left: Op, right: Op) -> Self {
        Self {
            left: Box::new(left),
            right: Box::new(right),
        }
    }
}

/// Intersect operator (set intersection)
#[derive(Debug, Clone)]
pub struct OpIntersect {
    pub left: Box<Op>,
    pub right: Box<Op>,
}

impl OpIntersect {
    /// Create intersect
    pub fn new(left: Op, right: Op) -> Self {
        Self {
            left: Box::new(left),
            right: Box::new(right),
        }
    }
}

/// Table operator (inline data VALUES)
#[derive(Debug, Clone)]
pub struct OpTable {
    pub vars: Vec<Var>,
    pub rows: Vec<Vec<Option<Value>>>,
}

impl OpTable {
    /// Create table
    pub fn new(vars: Vec<Var>, rows: Vec<Vec<Option<Value>>>) -> Self {
        Self { vars, rows }
    }

    /// Create empty table
    pub fn empty() -> Self {
        Self {
            vars: Vec::new(),
            rows: Vec::new(),
        }
    }

    /// Create single-row table
    pub fn unit() -> Self {
        Self {
            vars: Vec::new(),
            rows: vec![vec![]],
        }
    }
}

/// Sequence operator
#[derive(Debug, Clone)]
pub struct OpSequence {
    pub ops: Vec<Op>,
}

impl OpSequence {
    /// Create sequence
    pub fn new(ops: Vec<Op>) -> Self {
        Self { ops }
    }
}

/// Disjunction operator (OR patterns)
#[derive(Debug, Clone)]
pub struct OpDisjunction {
    pub ops: Vec<Op>,
}

impl OpDisjunction {
    /// Create disjunction
    pub fn new(ops: Vec<Op>) -> Self {
        Self { ops }
    }
}

/// Null operator (empty pattern)
#[derive(Debug, Clone, Copy)]
pub struct OpNull;

impl OpNull {
    /// Create null op
    pub fn new() -> Self {
        Self
    }
}

impl Default for OpNull {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_triple_pattern() {
        let triple = Triple::new(
            Pattern::Var(Var::new("s")),
            Pattern::Uri("http://example.org/knows".to_string()),
            Pattern::Var(Var::new("o")),
        );

        assert_eq!(triple.vars().len(), 2);
        assert!(!triple.is_concrete());
    }

    #[test]
    fn test_bgp() {
        let mut bgp = OpBGP::new();
        bgp.add(Triple::new(
            Pattern::Var(Var::new("s")),
            Pattern::Uri("http://example.org/name".to_string()),
            Pattern::Var(Var::new("name")),
        ));
        bgp.add(Triple::new(
            Pattern::Var(Var::new("s")),
            Pattern::Uri("http://example.org/age".to_string()),
            Pattern::Var(Var::new("age")),
        ));

        assert_eq!(bgp.triples.len(), 2);
        assert_eq!(bgp.vars().len(), 3); // s, name, age
    }

    #[test]
    fn test_filter_expr() {
        let binding = Binding::one(Var::new("x"), Value::Integer(10));

        let expr = FilterExpr::Gt(
            ExprTerm::Var(Var::new("x")),
            ExprTerm::Const(Value::Integer(5)),
        );

        assert!(expr.evaluate(&binding));

        let expr2 = FilterExpr::Lt(
            ExprTerm::Var(Var::new("x")),
            ExprTerm::Const(Value::Integer(5)),
        );

        assert!(!expr2.evaluate(&binding));
    }

    #[test]
    fn test_filter_and_or() {
        let binding = Binding::two(
            Var::new("x"),
            Value::Integer(10),
            Var::new("y"),
            Value::Integer(20),
        );

        let expr = FilterExpr::and(
            FilterExpr::Gt(
                ExprTerm::Var(Var::new("x")),
                ExprTerm::Const(Value::Integer(5)),
            ),
            FilterExpr::Lt(
                ExprTerm::Var(Var::new("y")),
                ExprTerm::Const(Value::Integer(30)),
            ),
        );

        assert!(expr.evaluate(&binding));
    }

    #[test]
    fn test_join_all() {
        let op1 = Op::BGP(OpBGP::new());
        let op2 = Op::BGP(OpBGP::new());
        let op3 = Op::BGP(OpBGP::new());

        let joined = OpJoin::join_all(vec![op1, op2, op3]);
        assert!(matches!(joined, Op::Join(_)));
    }

    #[test]
    fn test_op_vars() {
        let mut bgp = OpBGP::new();
        bgp.add(Triple::new(
            Pattern::Var(Var::new("s")),
            Pattern::Uri("pred".to_string()),
            Pattern::Var(Var::new("o")),
        ));

        let filter = Op::Filter(OpFilter::new(FilterExpr::True, Op::BGP(bgp)));

        let vars = filter.vars();
        assert!(vars.contains(&Var::new("s")));
        assert!(vars.contains(&Var::new("o")));
    }

    #[test]
    fn test_table_op() {
        let table = OpTable::new(
            vec![Var::new("x"), Var::new("y")],
            vec![
                vec![Some(Value::Integer(1)), Some(Value::Integer(2))],
                vec![Some(Value::Integer(3)), None],
            ],
        );

        assert_eq!(table.vars.len(), 2);
        assert_eq!(table.rows.len(), 2);
    }

    #[test]
    fn test_string_functions() {
        let binding = Binding::one(Var::new("s"), Value::String("Hello World".to_string()));

        let lower = ExprTerm::LCase(Box::new(ExprTerm::Var(Var::new("s"))));
        assert_eq!(
            lower.evaluate(&binding),
            Some(Value::String("hello world".to_string()))
        );

        let upper = ExprTerm::UCase(Box::new(ExprTerm::Var(Var::new("s"))));
        assert_eq!(
            upper.evaluate(&binding),
            Some(Value::String("HELLO WORLD".to_string()))
        );

        let len = ExprTerm::StrLen(Box::new(ExprTerm::Var(Var::new("s"))));
        assert_eq!(len.evaluate(&binding), Some(Value::Integer(11)));
    }
}
