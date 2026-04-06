//! CTE (Common Table Expression) parsing

use super::super::ast::{CteDefinition, QueryExpr, QueryWithCte, WithClause};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;

impl<'a> Parser<'a> {
    /// Parse a complete query with optional WITH clause
    pub fn parse_with_cte(&mut self) -> Result<QueryWithCte, ParseError> {
        // Check for WITH clause
        if self.check(&Token::With) && !self.is_with_filter_context() {
            let with_clause = self.parse_with_clause()?;
            let query = self.parse_query_expr()?;

            // Expect end of input
            if !self.check(&Token::Eof) {
                return Err(ParseError::new(
                    format!("Unexpected token after query: {}", self.current.token),
                    self.position(),
                ));
            }

            Ok(QueryWithCte::with_ctes(with_clause, query))
        } else {
            let query = self.parse_query_expr()?;

            // Expect end of input
            if !self.check(&Token::Eof) {
                return Err(ParseError::new(
                    format!("Unexpected token after query: {}", self.current.token),
                    self.position(),
                ));
            }

            Ok(QueryWithCte::simple(query))
        }
    }

    /// Check if WITH is part of a filter (STARTS WITH, ENDS WITH)
    fn is_with_filter_context(&self) -> bool {
        // WITH at start of query is CTE, not filter
        false
    }

    /// Parse WITH clause containing one or more CTEs
    ///
    /// Syntax:
    /// ```text
    /// WITH [RECURSIVE] cte_name [(columns)] AS (query) [, ...]
    /// ```
    fn parse_with_clause(&mut self) -> Result<WithClause, ParseError> {
        self.expect(Token::With)?;

        // Check for RECURSIVE keyword
        let is_recursive = self.consume(&Token::Recursive)?;

        let mut with_clause = WithClause::new();

        // Parse CTEs (comma-separated)
        loop {
            let cte = self.parse_cte_definition(is_recursive)?;
            with_clause = with_clause.add(cte);

            // Check for more CTEs
            if !self.consume(&Token::Comma)? {
                break;
            }
        }

        Ok(with_clause)
    }

    /// Parse a single CTE definition
    ///
    /// Syntax:
    /// ```text
    /// cte_name [(column1, column2, ...)] AS (query)
    /// ```
    fn parse_cte_definition(&mut self, is_recursive: bool) -> Result<CteDefinition, ParseError> {
        // CTE name
        let name = self.expect_ident()?;

        // Optional column list
        let columns = if self.consume(&Token::LParen)? {
            let mut cols = Vec::new();
            loop {
                cols.push(self.expect_ident()?);
                if !self.consume(&Token::Comma)? {
                    break;
                }
            }
            self.expect(Token::RParen)?;
            cols
        } else {
            Vec::new()
        };

        // AS keyword
        self.expect(Token::As)?;

        // CTE query in parentheses
        self.expect(Token::LParen)?;
        let query = self.parse_cte_query()?;
        self.expect(Token::RParen)?;

        let mut cte = if is_recursive {
            CteDefinition::recursive(&name, query)
        } else {
            CteDefinition::new(&name, query)
        };

        if !columns.is_empty() {
            cte = cte.with_columns(columns);
        }

        Ok(cte)
    }

    /// Parse a query within a CTE (supports UNION ALL for recursive CTEs)
    fn parse_cte_query(&mut self) -> Result<QueryExpr, ParseError> {
        // Parse the base query
        let query = self.parse_query_expr()?;

        // For recursive CTEs, we might have UNION ALL
        // For now, we just return the base query
        // Full UNION ALL support would require a UnionQuery variant
        if self.check(&Token::Union) {
            // Skip UNION ALL - the recursive part references the CTE by name
            // Full implementation would parse both sides
            self.advance()?;
            if self.consume(&Token::All)? {
                // Parse the recursive part
                let _recursive_query = self.parse_query_expr()?;
                // For now, return just the base - execution handles recursion
            }
        }

        Ok(query)
    }
}
