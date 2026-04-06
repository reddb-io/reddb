//! Filter Predicates for Query Engine
//!
//! Provides filter operations for querying data including comparison,
//! logical, and pattern matching operations.

use crate::storage::schema::Value;
use std::fmt;

/// Filter operation type
#[derive(Debug, Clone, PartialEq)]
pub enum FilterOp {
    /// Equal to
    Eq,
    /// Not equal to
    Ne,
    /// Less than
    Lt,
    /// Less than or equal
    Le,
    /// Greater than
    Gt,
    /// Greater than or equal
    Ge,
    /// Between two values (inclusive)
    Between,
    /// In a set of values
    In,
    /// Not in a set of values
    NotIn,
    /// Like pattern matching (SQL-style with % and _)
    Like,
    /// Not like pattern
    NotLike,
    /// Is null
    IsNull,
    /// Is not null
    IsNotNull,
    /// Contains (for text or arrays)
    Contains,
    /// Starts with
    StartsWith,
    /// Ends with
    EndsWith,
}

impl fmt::Display for FilterOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FilterOp::Eq => write!(f, "="),
            FilterOp::Ne => write!(f, "!="),
            FilterOp::Lt => write!(f, "<"),
            FilterOp::Le => write!(f, "<="),
            FilterOp::Gt => write!(f, ">"),
            FilterOp::Ge => write!(f, ">="),
            FilterOp::Between => write!(f, "BETWEEN"),
            FilterOp::In => write!(f, "IN"),
            FilterOp::NotIn => write!(f, "NOT IN"),
            FilterOp::Like => write!(f, "LIKE"),
            FilterOp::NotLike => write!(f, "NOT LIKE"),
            FilterOp::IsNull => write!(f, "IS NULL"),
            FilterOp::IsNotNull => write!(f, "IS NOT NULL"),
            FilterOp::Contains => write!(f, "CONTAINS"),
            FilterOp::StartsWith => write!(f, "STARTS WITH"),
            FilterOp::EndsWith => write!(f, "ENDS WITH"),
        }
    }
}

/// A single predicate condition
#[derive(Debug, Clone)]
pub struct Predicate {
    /// Column name
    pub column: String,
    /// Filter operation
    pub op: FilterOp,
    /// Value(s) to compare against
    pub value: PredicateValue,
}

/// Value type for predicates
#[derive(Debug, Clone)]
pub enum PredicateValue {
    /// Single value
    Single(Value),
    /// Range of values (for BETWEEN)
    Range(Value, Value),
    /// List of values (for IN)
    List(Vec<Value>),
    /// Pattern string (for LIKE)
    Pattern(String),
    /// No value (for IS NULL)
    None,
}

impl Predicate {
    /// Create an equality predicate
    pub fn eq(column: impl Into<String>, value: Value) -> Self {
        Self {
            column: column.into(),
            op: FilterOp::Eq,
            value: PredicateValue::Single(value),
        }
    }

    /// Create a not-equal predicate
    pub fn ne(column: impl Into<String>, value: Value) -> Self {
        Self {
            column: column.into(),
            op: FilterOp::Ne,
            value: PredicateValue::Single(value),
        }
    }

    /// Create a less-than predicate
    pub fn lt(column: impl Into<String>, value: Value) -> Self {
        Self {
            column: column.into(),
            op: FilterOp::Lt,
            value: PredicateValue::Single(value),
        }
    }

    /// Create a less-than-or-equal predicate
    pub fn le(column: impl Into<String>, value: Value) -> Self {
        Self {
            column: column.into(),
            op: FilterOp::Le,
            value: PredicateValue::Single(value),
        }
    }

    /// Create a greater-than predicate
    pub fn gt(column: impl Into<String>, value: Value) -> Self {
        Self {
            column: column.into(),
            op: FilterOp::Gt,
            value: PredicateValue::Single(value),
        }
    }

    /// Create a greater-than-or-equal predicate
    pub fn ge(column: impl Into<String>, value: Value) -> Self {
        Self {
            column: column.into(),
            op: FilterOp::Ge,
            value: PredicateValue::Single(value),
        }
    }

    /// Create a between predicate (inclusive)
    pub fn between(column: impl Into<String>, low: Value, high: Value) -> Self {
        Self {
            column: column.into(),
            op: FilterOp::Between,
            value: PredicateValue::Range(low, high),
        }
    }

    /// Create an IN predicate
    pub fn in_list(column: impl Into<String>, values: Vec<Value>) -> Self {
        Self {
            column: column.into(),
            op: FilterOp::In,
            value: PredicateValue::List(values),
        }
    }

    /// Create a NOT IN predicate
    pub fn not_in(column: impl Into<String>, values: Vec<Value>) -> Self {
        Self {
            column: column.into(),
            op: FilterOp::NotIn,
            value: PredicateValue::List(values),
        }
    }

    /// Create a LIKE predicate
    pub fn like(column: impl Into<String>, pattern: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            op: FilterOp::Like,
            value: PredicateValue::Pattern(pattern.into()),
        }
    }

    /// Create a NOT LIKE predicate
    pub fn not_like(column: impl Into<String>, pattern: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            op: FilterOp::NotLike,
            value: PredicateValue::Pattern(pattern.into()),
        }
    }

    /// Create an IS NULL predicate
    pub fn is_null(column: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            op: FilterOp::IsNull,
            value: PredicateValue::None,
        }
    }

    /// Create an IS NOT NULL predicate
    pub fn is_not_null(column: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            op: FilterOp::IsNotNull,
            value: PredicateValue::None,
        }
    }

    /// Create a CONTAINS predicate
    pub fn contains(column: impl Into<String>, value: Value) -> Self {
        Self {
            column: column.into(),
            op: FilterOp::Contains,
            value: PredicateValue::Single(value),
        }
    }

    /// Create a STARTS WITH predicate
    pub fn starts_with(column: impl Into<String>, prefix: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            op: FilterOp::StartsWith,
            value: PredicateValue::Pattern(prefix.into()),
        }
    }

    /// Create an ENDS WITH predicate
    pub fn ends_with(column: impl Into<String>, suffix: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            op: FilterOp::EndsWith,
            value: PredicateValue::Pattern(suffix.into()),
        }
    }

    /// Evaluate predicate against a value
    pub fn evaluate(&self, column_value: &Value) -> bool {
        match (&self.op, &self.value) {
            (FilterOp::Eq, PredicateValue::Single(v)) => column_value == v,
            (FilterOp::Ne, PredicateValue::Single(v)) => column_value != v,
            (FilterOp::Lt, PredicateValue::Single(v)) => {
                compare_values(column_value, v) == Some(std::cmp::Ordering::Less)
            }
            (FilterOp::Le, PredicateValue::Single(v)) => {
                matches!(
                    compare_values(column_value, v),
                    Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
                )
            }
            (FilterOp::Gt, PredicateValue::Single(v)) => {
                compare_values(column_value, v) == Some(std::cmp::Ordering::Greater)
            }
            (FilterOp::Ge, PredicateValue::Single(v)) => {
                matches!(
                    compare_values(column_value, v),
                    Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                )
            }
            (FilterOp::Between, PredicateValue::Range(low, high)) => {
                matches!(
                    compare_values(column_value, low),
                    Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                ) && matches!(
                    compare_values(column_value, high),
                    Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
                )
            }
            (FilterOp::In, PredicateValue::List(values)) => values.contains(column_value),
            (FilterOp::NotIn, PredicateValue::List(values)) => !values.contains(column_value),
            (FilterOp::Like, PredicateValue::Pattern(pattern)) => match column_value {
                Value::Text(s) => match_like_pattern(s, pattern),
                _ => false,
            },
            (FilterOp::NotLike, PredicateValue::Pattern(pattern)) => match column_value {
                Value::Text(s) => !match_like_pattern(s, pattern),
                _ => true,
            },
            (FilterOp::IsNull, PredicateValue::None) => matches!(column_value, Value::Null),
            (FilterOp::IsNotNull, PredicateValue::None) => !matches!(column_value, Value::Null),
            (FilterOp::Contains, PredicateValue::Single(v)) => match (column_value, v) {
                (Value::Text(haystack), Value::Text(needle)) => haystack.contains(needle.as_str()),
                _ => false,
            },
            (FilterOp::StartsWith, PredicateValue::Pattern(prefix)) => match column_value {
                Value::Text(s) => s.starts_with(prefix),
                _ => false,
            },
            (FilterOp::EndsWith, PredicateValue::Pattern(suffix)) => match column_value {
                Value::Text(s) => s.ends_with(suffix),
                _ => false,
            },
            _ => false,
        }
    }
}

/// Compare two values, returning ordering if comparable
fn compare_values(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::Integer(a), Value::Integer(b)) => Some(a.cmp(b)),
        (Value::UnsignedInteger(a), Value::UnsignedInteger(b)) => Some(a.cmp(b)),
        (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
        (Value::Text(a), Value::Text(b)) => Some(a.cmp(b)),
        (Value::Timestamp(a), Value::Timestamp(b)) => Some(a.cmp(b)),
        (Value::Duration(a), Value::Duration(b)) => Some(a.cmp(b)),
        // Cross-type numeric comparisons
        (Value::Integer(a), Value::Float(b)) => (*a as f64).partial_cmp(b),
        (Value::Float(a), Value::Integer(b)) => a.partial_cmp(&(*b as f64)),
        (Value::UnsignedInteger(a), Value::Float(b)) => (*a as f64).partial_cmp(b),
        (Value::Float(a), Value::UnsignedInteger(b)) => a.partial_cmp(&(*b as f64)),
        _ => None,
    }
}

/// Match SQL LIKE pattern
///
/// % matches any sequence of characters
/// _ matches any single character
fn match_like_pattern(text: &str, pattern: &str) -> bool {
    let mut text_chars = text.chars().peekable();
    let mut pattern_chars = pattern.chars().peekable();

    match_like_recursive(&mut text_chars, &mut pattern_chars)
}

fn match_like_recursive<I: Iterator<Item = char> + Clone>(
    text: &mut std::iter::Peekable<I>,
    pattern: &mut std::iter::Peekable<I>,
) -> bool {
    loop {
        match (pattern.peek().cloned(), text.peek().cloned()) {
            // Pattern exhausted
            (None, None) => return true,
            (None, Some(_)) => return false,

            // Wildcard %: match zero or more characters
            (Some('%'), _) => {
                pattern.next();
                // Skip consecutive %
                while pattern.peek() == Some(&'%') {
                    pattern.next();
                }
                // If % is at end, match everything
                if pattern.peek().is_none() {
                    return true;
                }
                // Try matching from each position
                loop {
                    let mut text_clone = text.clone();
                    let mut pattern_clone = pattern.clone();
                    if match_like_recursive(&mut text_clone, &mut pattern_clone) {
                        return true;
                    }
                    if text.next().is_none() {
                        return false;
                    }
                }
            }

            // Single char wildcard _
            (Some('_'), Some(_)) => {
                pattern.next();
                text.next();
            }
            (Some('_'), None) => return false,

            // Literal match
            (Some(p), Some(t)) => {
                if p.to_lowercase().next() == t.to_lowercase().next() {
                    pattern.next();
                    text.next();
                } else {
                    return false;
                }
            }

            // Pattern has more but text exhausted
            (Some(_), None) => return false,
        }
    }
}

/// Composite filter with logical operations
#[derive(Debug, Clone)]
pub enum Filter {
    /// Single predicate
    Predicate(Predicate),
    /// Logical AND of filters
    And(Vec<Filter>),
    /// Logical OR of filters
    Or(Vec<Filter>),
    /// Logical NOT of filter
    Not(Box<Filter>),
}

impl Filter {
    /// Create filter from predicate
    pub fn from_predicate(predicate: Predicate) -> Self {
        Filter::Predicate(predicate)
    }

    /// Create equality filter
    pub fn eq(column: impl Into<String>, value: Value) -> Self {
        Filter::Predicate(Predicate::eq(column, value))
    }

    /// Create not-equal filter
    pub fn ne(column: impl Into<String>, value: Value) -> Self {
        Filter::Predicate(Predicate::ne(column, value))
    }

    /// Create less-than filter
    pub fn lt(column: impl Into<String>, value: Value) -> Self {
        Filter::Predicate(Predicate::lt(column, value))
    }

    /// Create less-than-or-equal filter
    pub fn le(column: impl Into<String>, value: Value) -> Self {
        Filter::Predicate(Predicate::le(column, value))
    }

    /// Create greater-than filter
    pub fn gt(column: impl Into<String>, value: Value) -> Self {
        Filter::Predicate(Predicate::gt(column, value))
    }

    /// Create greater-than-or-equal filter
    pub fn ge(column: impl Into<String>, value: Value) -> Self {
        Filter::Predicate(Predicate::ge(column, value))
    }

    /// Create between filter
    pub fn between(column: impl Into<String>, low: Value, high: Value) -> Self {
        Filter::Predicate(Predicate::between(column, low, high))
    }

    /// Create IN filter
    pub fn in_list(column: impl Into<String>, values: Vec<Value>) -> Self {
        Filter::Predicate(Predicate::in_list(column, values))
    }

    /// Create LIKE filter
    pub fn like(column: impl Into<String>, pattern: impl Into<String>) -> Self {
        Filter::Predicate(Predicate::like(column, pattern))
    }

    /// Create IS NULL filter
    pub fn is_null(column: impl Into<String>) -> Self {
        Filter::Predicate(Predicate::is_null(column))
    }

    /// Create IS NOT NULL filter
    pub fn is_not_null(column: impl Into<String>) -> Self {
        Filter::Predicate(Predicate::is_not_null(column))
    }

    /// Create AND filter
    pub fn and(filters: Vec<Filter>) -> Self {
        Filter::And(filters)
    }

    /// Create OR filter
    pub fn or(filters: Vec<Filter>) -> Self {
        Filter::Or(filters)
    }

    /// Create NOT filter
    pub fn not(filter: Filter) -> Self {
        Filter::Not(Box::new(filter))
    }

    /// Combine with another filter using AND
    pub fn and_filter(self, other: Filter) -> Self {
        match self {
            Filter::And(mut filters) => {
                filters.push(other);
                Filter::And(filters)
            }
            _ => Filter::And(vec![self, other]),
        }
    }

    /// Combine with another filter using OR
    pub fn or_filter(self, other: Filter) -> Self {
        match self {
            Filter::Or(mut filters) => {
                filters.push(other);
                Filter::Or(filters)
            }
            _ => Filter::Or(vec![self, other]),
        }
    }

    /// Evaluate filter against a row (column name -> value map)
    pub fn evaluate(&self, get_value: &impl Fn(&str) -> Option<Value>) -> bool {
        match self {
            Filter::Predicate(pred) => {
                if let Some(value) = get_value(&pred.column) {
                    pred.evaluate(&value)
                } else {
                    // Column not found - treat as NULL
                    matches!(pred.op, FilterOp::IsNull)
                }
            }
            Filter::And(filters) => filters.iter().all(|f| f.evaluate(get_value)),
            Filter::Or(filters) => filters.iter().any(|f| f.evaluate(get_value)),
            Filter::Not(filter) => !filter.evaluate(get_value),
        }
    }

    /// Check if filter references a specific column
    pub fn references_column(&self, column: &str) -> bool {
        match self {
            Filter::Predicate(pred) => pred.column == column,
            Filter::And(filters) | Filter::Or(filters) => {
                filters.iter().any(|f| f.references_column(column))
            }
            Filter::Not(filter) => filter.references_column(column),
        }
    }

    /// Get all referenced columns
    pub fn referenced_columns(&self) -> Vec<&str> {
        let mut columns = Vec::new();
        self.collect_columns(&mut columns);
        columns
    }

    fn collect_columns<'a>(&'a self, columns: &mut Vec<&'a str>) {
        match self {
            Filter::Predicate(pred) => {
                if !columns.contains(&pred.column.as_str()) {
                    columns.push(&pred.column);
                }
            }
            Filter::And(filters) | Filter::Or(filters) => {
                for f in filters {
                    f.collect_columns(columns);
                }
            }
            Filter::Not(filter) => filter.collect_columns(columns),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_row() -> impl Fn(&str) -> Option<Value> {
        |col: &str| -> Option<Value> {
            match col {
                "name" => Some(Value::Text("Alice".to_string())),
                "age" => Some(Value::Integer(25)),
                "score" => Some(Value::Float(95.5)),
                "active" => Some(Value::Boolean(true)),
                "email" => Some(Value::Text("alice@example.com".to_string())),
                "nullable" => Some(Value::Null),
                _ => None,
            }
        }
    }

    #[test]
    fn test_predicate_eq() {
        let pred = Predicate::eq("name", Value::Text("Alice".to_string()));
        assert!(pred.evaluate(&Value::Text("Alice".to_string())));
        assert!(!pred.evaluate(&Value::Text("Bob".to_string())));
    }

    #[test]
    fn test_predicate_ne() {
        let pred = Predicate::ne("name", Value::Text("Bob".to_string()));
        assert!(pred.evaluate(&Value::Text("Alice".to_string())));
        assert!(!pred.evaluate(&Value::Text("Bob".to_string())));
    }

    #[test]
    fn test_predicate_lt() {
        let pred = Predicate::lt("age", Value::Integer(30));
        assert!(pred.evaluate(&Value::Integer(25)));
        assert!(!pred.evaluate(&Value::Integer(30)));
        assert!(!pred.evaluate(&Value::Integer(35)));
    }

    #[test]
    fn test_predicate_le() {
        let pred = Predicate::le("age", Value::Integer(30));
        assert!(pred.evaluate(&Value::Integer(25)));
        assert!(pred.evaluate(&Value::Integer(30)));
        assert!(!pred.evaluate(&Value::Integer(35)));
    }

    #[test]
    fn test_predicate_gt() {
        let pred = Predicate::gt("age", Value::Integer(20));
        assert!(pred.evaluate(&Value::Integer(25)));
        assert!(!pred.evaluate(&Value::Integer(20)));
        assert!(!pred.evaluate(&Value::Integer(15)));
    }

    #[test]
    fn test_predicate_ge() {
        let pred = Predicate::ge("age", Value::Integer(20));
        assert!(pred.evaluate(&Value::Integer(25)));
        assert!(pred.evaluate(&Value::Integer(20)));
        assert!(!pred.evaluate(&Value::Integer(15)));
    }

    #[test]
    fn test_predicate_between() {
        let pred = Predicate::between("age", Value::Integer(20), Value::Integer(30));
        assert!(pred.evaluate(&Value::Integer(25)));
        assert!(pred.evaluate(&Value::Integer(20)));
        assert!(pred.evaluate(&Value::Integer(30)));
        assert!(!pred.evaluate(&Value::Integer(19)));
        assert!(!pred.evaluate(&Value::Integer(31)));
    }

    #[test]
    fn test_predicate_in() {
        let pred = Predicate::in_list(
            "name",
            vec![
                Value::Text("Alice".to_string()),
                Value::Text("Bob".to_string()),
            ],
        );
        assert!(pred.evaluate(&Value::Text("Alice".to_string())));
        assert!(pred.evaluate(&Value::Text("Bob".to_string())));
        assert!(!pred.evaluate(&Value::Text("Charlie".to_string())));
    }

    #[test]
    fn test_predicate_not_in() {
        let pred = Predicate::not_in("name", vec![Value::Text("Alice".to_string())]);
        assert!(!pred.evaluate(&Value::Text("Alice".to_string())));
        assert!(pred.evaluate(&Value::Text("Bob".to_string())));
    }

    #[test]
    fn test_predicate_like() {
        // % matches any sequence
        let pred = Predicate::like("email", "%@example.com");
        assert!(pred.evaluate(&Value::Text("alice@example.com".to_string())));
        assert!(pred.evaluate(&Value::Text("bob@example.com".to_string())));
        assert!(!pred.evaluate(&Value::Text("alice@gmail.com".to_string())));

        // _ matches single char
        let pred2 = Predicate::like("name", "A_ice");
        assert!(pred2.evaluate(&Value::Text("Alice".to_string())));
        assert!(!pred2.evaluate(&Value::Text("Alicia".to_string())));

        // Combined
        let pred3 = Predicate::like("email", "a%@%.com");
        assert!(pred3.evaluate(&Value::Text("alice@example.com".to_string())));
    }

    #[test]
    fn test_predicate_is_null() {
        let pred = Predicate::is_null("nullable");
        assert!(pred.evaluate(&Value::Null));
        assert!(!pred.evaluate(&Value::Integer(0)));
    }

    #[test]
    fn test_predicate_is_not_null() {
        let pred = Predicate::is_not_null("name");
        assert!(pred.evaluate(&Value::Text("Alice".to_string())));
        assert!(!pred.evaluate(&Value::Null));
    }

    #[test]
    fn test_predicate_contains() {
        let pred = Predicate::contains("email", Value::Text("@example".to_string()));
        assert!(pred.evaluate(&Value::Text("alice@example.com".to_string())));
        assert!(!pred.evaluate(&Value::Text("alice@gmail.com".to_string())));
    }

    #[test]
    fn test_predicate_starts_with() {
        let pred = Predicate::starts_with("name", "Al");
        assert!(pred.evaluate(&Value::Text("Alice".to_string())));
        assert!(pred.evaluate(&Value::Text("Albert".to_string())));
        assert!(!pred.evaluate(&Value::Text("Bob".to_string())));
    }

    #[test]
    fn test_predicate_ends_with() {
        let pred = Predicate::ends_with("email", ".com");
        assert!(pred.evaluate(&Value::Text("alice@example.com".to_string())));
        assert!(!pred.evaluate(&Value::Text("alice@example.org".to_string())));
    }

    #[test]
    fn test_filter_evaluate() {
        let row = make_row();

        // Simple equality
        let filter = Filter::eq("name", Value::Text("Alice".to_string()));
        assert!(filter.evaluate(&row));

        // Comparison
        let filter = Filter::gt("age", Value::Integer(20));
        assert!(filter.evaluate(&row));
    }

    #[test]
    fn test_filter_and() {
        let row = make_row();

        let filter = Filter::and(vec![
            Filter::eq("name", Value::Text("Alice".to_string())),
            Filter::gt("age", Value::Integer(20)),
        ]);
        assert!(filter.evaluate(&row));

        let filter = Filter::and(vec![
            Filter::eq("name", Value::Text("Alice".to_string())),
            Filter::gt("age", Value::Integer(30)), // Alice is 25, fails
        ]);
        assert!(!filter.evaluate(&row));
    }

    #[test]
    fn test_filter_or() {
        let row = make_row();

        let filter = Filter::or(vec![
            Filter::eq("name", Value::Text("Bob".to_string())), // Fails
            Filter::gt("age", Value::Integer(20)),              // Passes
        ]);
        assert!(filter.evaluate(&row));

        let filter = Filter::or(vec![
            Filter::eq("name", Value::Text("Bob".to_string())),
            Filter::lt("age", Value::Integer(20)),
        ]);
        assert!(!filter.evaluate(&row));
    }

    #[test]
    fn test_filter_not() {
        let row = make_row();

        let filter = Filter::not(Filter::eq("name", Value::Text("Bob".to_string())));
        assert!(filter.evaluate(&row)); // Alice != Bob

        let filter = Filter::not(Filter::eq("name", Value::Text("Alice".to_string())));
        assert!(!filter.evaluate(&row));
    }

    #[test]
    fn test_filter_chain() {
        let row = make_row();

        let filter = Filter::eq("name", Value::Text("Alice".to_string()))
            .and_filter(Filter::gt("age", Value::Integer(20)))
            .and_filter(Filter::lt("age", Value::Integer(30)));

        assert!(filter.evaluate(&row));
    }

    #[test]
    fn test_filter_referenced_columns() {
        let filter = Filter::and(vec![
            Filter::eq("name", Value::Text("Alice".to_string())),
            Filter::or(vec![
                Filter::gt("age", Value::Integer(20)),
                Filter::lt("score", Value::Float(100.0)),
            ]),
        ]);

        let columns = filter.referenced_columns();
        assert!(columns.contains(&"name"));
        assert!(columns.contains(&"age"));
        assert!(columns.contains(&"score"));
        assert_eq!(columns.len(), 3);
    }

    #[test]
    fn test_cross_type_comparison() {
        // Integer vs Float
        let pred = Predicate::lt("value", Value::Float(30.5));
        assert!(pred.evaluate(&Value::Integer(30)));
        assert!(!pred.evaluate(&Value::Integer(31)));
    }

    #[test]
    fn test_like_edge_cases() {
        // Empty pattern
        let pred = Predicate::like("text", "");
        assert!(pred.evaluate(&Value::Text("".to_string())));
        assert!(!pred.evaluate(&Value::Text("a".to_string())));

        // Only %
        let pred = Predicate::like("text", "%");
        assert!(pred.evaluate(&Value::Text("anything".to_string())));
        assert!(pred.evaluate(&Value::Text("".to_string())));

        // Multiple %
        let pred = Predicate::like("text", "a%b%c");
        assert!(pred.evaluate(&Value::Text("abc".to_string())));
        assert!(pred.evaluate(&Value::Text("aXXbYYc".to_string())));
        assert!(!pred.evaluate(&Value::Text("ab".to_string())));
    }
}
