//! Filter types and where clause builder
//!
//! Provides fluent filter construction for queries.

/// A filter condition
#[derive(Debug, Clone)]
pub struct Filter {
    pub field: String,
    pub op: FilterOp,
    pub value: FilterValue,
}

/// Filter operation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOp {
    Equals,
    NotEquals,
    GreaterThan,
    GreaterThanOrEquals,
    LessThan,
    LessThanOrEquals,
    Contains,
    StartsWith,
    EndsWith,
    In,
    Between,
    IsNull,
    IsNotNull,
}

/// Filter value
#[derive(Debug, Clone)]
pub enum FilterValue {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
    List(Vec<FilterValue>),
    Range(Box<FilterValue>, Box<FilterValue>),
}

// Convenient From implementations
impl From<&str> for FilterValue {
    fn from(s: &str) -> Self {
        FilterValue::String(s.to_string())
    }
}

impl From<String> for FilterValue {
    fn from(s: String) -> Self {
        FilterValue::String(s)
    }
}

impl From<i32> for FilterValue {
    fn from(v: i32) -> Self {
        FilterValue::Int(v as i64)
    }
}

impl From<i64> for FilterValue {
    fn from(v: i64) -> Self {
        FilterValue::Int(v)
    }
}

impl From<f32> for FilterValue {
    fn from(v: f32) -> Self {
        FilterValue::Float(v as f64)
    }
}

impl From<f64> for FilterValue {
    fn from(v: f64) -> Self {
        FilterValue::Float(v)
    }
}

impl From<bool> for FilterValue {
    fn from(v: bool) -> Self {
        FilterValue::Bool(v)
    }
}

/// Trait for builders that can accept filters
pub trait FilterAcceptor {
    fn add_filter(&mut self, filter: Filter);
}

/// Fluent where clause for filter conditions
#[derive(Debug)]
pub struct WhereClause<B> {
    builder: B,
    field: String,
}

impl<B> WhereClause<B> {
    pub fn new(builder: B, field: String) -> Self {
        Self { builder, field }
    }
}

impl<B: FilterAcceptor> WhereClause<B> {
    /// Equal to value
    pub fn equals<V: Into<FilterValue>>(mut self, value: V) -> B {
        self.builder.add_filter(Filter {
            field: self.field,
            op: FilterOp::Equals,
            value: value.into(),
        });
        self.builder
    }

    /// Shorthand for equals
    pub fn eq<V: Into<FilterValue>>(self, value: V) -> B {
        self.equals(value)
    }

    /// Not equal to value
    pub fn not_equals<V: Into<FilterValue>>(mut self, value: V) -> B {
        self.builder.add_filter(Filter {
            field: self.field,
            op: FilterOp::NotEquals,
            value: value.into(),
        });
        self.builder
    }

    /// Greater than value
    pub fn greater_than<V: Into<FilterValue>>(mut self, value: V) -> B {
        self.builder.add_filter(Filter {
            field: self.field,
            op: FilterOp::GreaterThan,
            value: value.into(),
        });
        self.builder
    }

    /// Shorthand for greater_than
    pub fn gt<V: Into<FilterValue>>(self, value: V) -> B {
        self.greater_than(value)
    }

    /// Greater than or equal
    pub fn gte<V: Into<FilterValue>>(mut self, value: V) -> B {
        self.builder.add_filter(Filter {
            field: self.field,
            op: FilterOp::GreaterThanOrEquals,
            value: value.into(),
        });
        self.builder
    }

    /// Less than value
    pub fn less_than<V: Into<FilterValue>>(mut self, value: V) -> B {
        self.builder.add_filter(Filter {
            field: self.field,
            op: FilterOp::LessThan,
            value: value.into(),
        });
        self.builder
    }

    /// Shorthand for less_than
    pub fn lt<V: Into<FilterValue>>(self, value: V) -> B {
        self.less_than(value)
    }

    /// Less than or equal
    pub fn lte<V: Into<FilterValue>>(mut self, value: V) -> B {
        self.builder.add_filter(Filter {
            field: self.field,
            op: FilterOp::LessThanOrEquals,
            value: value.into(),
        });
        self.builder
    }

    /// Contains substring
    pub fn contains(mut self, substr: impl Into<String>) -> B {
        self.builder.add_filter(Filter {
            field: self.field,
            op: FilterOp::Contains,
            value: FilterValue::String(substr.into()),
        });
        self.builder
    }

    /// Starts with prefix
    pub fn starts_with(mut self, prefix: impl Into<String>) -> B {
        self.builder.add_filter(Filter {
            field: self.field,
            op: FilterOp::StartsWith,
            value: FilterValue::String(prefix.into()),
        });
        self.builder
    }

    /// Ends with suffix
    pub fn ends_with(mut self, suffix: impl Into<String>) -> B {
        self.builder.add_filter(Filter {
            field: self.field,
            op: FilterOp::EndsWith,
            value: FilterValue::String(suffix.into()),
        });
        self.builder
    }

    /// Value is in list
    pub fn in_list(mut self, values: Vec<FilterValue>) -> B {
        self.builder.add_filter(Filter {
            field: self.field,
            op: FilterOp::In,
            value: FilterValue::List(values),
        });
        self.builder
    }

    /// Between two values (inclusive)
    pub fn between<V: Into<FilterValue>>(mut self, low: V, high: V) -> B {
        self.builder.add_filter(Filter {
            field: self.field,
            op: FilterOp::Between,
            value: FilterValue::Range(Box::new(low.into()), Box::new(high.into())),
        });
        self.builder
    }

    /// Is null
    pub fn is_null(mut self) -> B {
        self.builder.add_filter(Filter {
            field: self.field,
            op: FilterOp::IsNull,
            value: FilterValue::Null,
        });
        self.builder
    }

    /// Is not null
    pub fn is_not_null(mut self) -> B {
        self.builder.add_filter(Filter {
            field: self.field,
            op: FilterOp::IsNotNull,
            value: FilterValue::Null,
        });
        self.builder
    }
}
