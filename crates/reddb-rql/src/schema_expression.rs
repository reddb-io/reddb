//! Persisted row expressions. Parse once at DDL/reopen, share the AST on writes.
use std::collections::BTreeSet;
use std::sync::Arc;

use crate::ast::{Expr, FieldRef};
use crate::lexer::Token;
use crate::parser::{Parser, ParserLimits};

#[derive(Debug, Clone)]
pub struct SchemaExpression {
    source: Arc<str>,
    expression: Arc<Expr>,
    dependencies: Arc<[String]>,
}

impl SchemaExpression {
    pub fn parse(source: String) -> Result<Self, String> {
        let limits = ParserLimits {
            max_tokens: 1024,
            max_input_bytes: 65_536,
            ..ParserLimits::default()
        };
        let mut parser = Parser::with_limits(&source, limits).map_err(|error| error.to_string())?;
        let expression = parser.parse_expr().map_err(|error| error.to_string())?;
        if !matches!(parser.peek(), Token::Eof) {
            return Err("unexpected trailing input in schema expression".to_string());
        }
        // Iterative validation also bounds left-deep trees: parser nesting limits
        // alone do not bound a long chain of left-associative binary operators.
        let mut pending = vec![(&expression, 1usize)];
        let mut dependencies = BTreeSet::new();
        while let Some((node, depth)) = pending.pop() {
            if depth > 64 {
                return Err("schema expression exceeds depth limit 64".to_string());
            }
            let mut push = |child| pending.push((child, depth + 1));
            match node {
                Expr::Literal { .. } => {}
                Expr::Column { field: FieldRef::TableColumn { table, column }, .. } if table.is_empty() => {
                    dependencies.insert(column.clone());
                }
                Expr::BinaryOp { lhs, rhs, .. } => { push(lhs); push(rhs); }
                Expr::UnaryOp { operand, .. } | Expr::IsNull { operand, .. } => push(operand),
                Expr::Cast { inner, .. } => push(inner),
                Expr::Between { target, low, high, .. } => { push(target); push(low); push(high); }
                Expr::InList { target, values, .. } => { push(target); for value in values { push(value); } }
                Expr::Case { branches, else_, .. } => {
                    for (condition, value) in branches { push(condition); push(value); }
                    if let Some(value) = else_ { push(value); }
                }
                // A deliberately closed effect boundary until the function catalog
                // provides verified volatility/effect metadata for every callable.
                _ => return Err("schema expressions require local fields and deterministic scalar operators; functions, parameters and subqueries are unsupported".to_string()),
            }
        }
        Ok(Self {
            source: source.into(),
            expression: Arc::new(expression),
            dependencies: dependencies.into_iter().collect::<Vec<_>>().into(),
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn expression(&self) -> &Expr {
        &self.expression
    }
    pub fn dependencies(&self) -> &[String] {
        &self.dependencies
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_source_and_dependencies_and_rejects_external_effects() {
        let expression =
            SchemaExpression::parse("price * quantity + tax".into()).expect("valid scalar");
        assert_eq!(expression.dependencies(), ["price", "quantity", "tax"]);
        for source in [
            "random()",
            "$1",
            "other.price",
            "price; DELETE FROM orders",
            "(SELECT price FROM orders)",
        ] {
            assert!(SchemaExpression::parse(source.into()).is_err(), "{source}");
        }
    }

    #[test]
    fn bounds_left_deep_expression_trees() {
        let source = vec!["1"; 66].join(" + ");
        assert!(SchemaExpression::parse(source).is_err());
    }

    #[test]
    fn parses_stored_and_check_and_preserves_quoted_source() {
        for sql in [
            "CREATE TABLE orders (price INTEGER, quantity INTEGER CHECK (quantity > 0), total INTEGER GENERATED ALWAYS AS (price * quantity) STORED)",
            "CREATE TABLE names (label TEXT GENERATED ALWAYS AS ('it''s (ready)') STORED CHECK (label <> ''))",
        ] {
            crate::parser::parse(sql).expect(sql);
        }
        for sql in [
            "CREATE TABLE orders (total INTEGER GENERATED ALWAYS AS (1) VIRTUAL)",
            "CREATE TABLE orders (quantity INTEGER CHECK (quantity > 0) CHECK (quantity < 9))",
        ] {
            assert!(crate::parser::parse(sql).is_err(), "{sql}");
        }
    }
}
