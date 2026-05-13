//! RQL Parser
//!
//! Parses RQL (RedDB Query Language) tokens into a unified AST.
//! Supports SQL-like table queries, Cypher-like graph patterns, and joins.
//!
//! # Supported Syntax
//!
//! ## Table Queries
//! ```text
//! SELECT [columns] FROM table [WHERE condition] [ORDER BY ...] [LIMIT n]
//! ```
//!
//! ## Graph Queries
//! ```text
//! MATCH pattern [WHERE condition] RETURN projection
//! ```
//!
//! ## Join Queries
//! ```text
//! FROM table [alias] JOIN GRAPH pattern ON condition [RETURN projection]
//! ```
//!
//! ## Path Queries
//! ```text
//! PATH FROM selector TO selector [VIA edge_types] [RETURN projection]
//! ```
//!
//! ## Vector Queries
//! ```text
//! VECTOR SEARCH collection SIMILAR TO [...] [WHERE ...] [METRIC ...] LIMIT k
//! ```
//!
//! ## Hybrid Queries
//! ```text
//! HYBRID (structured query) VECTOR SEARCH ... FUSION strategy LIMIT n
//! ```

mod auth_ddl;
mod config;
mod cte;
mod ddl;
mod dml;
mod error;
mod expr;
mod filter;
mod graph;
mod graph_commands;
mod hybrid;
mod index_ddl;
mod join;
mod kv;
pub mod limits;
mod migration;
mod path;
mod probabilistic_commands;
mod queue;
mod search_commands;
mod table;
mod timeseries;
mod tree;
mod vector;

#[cfg(test)]
mod json_literal_table;
#[cfg(test)]
mod property_tests;
#[cfg(test)]
mod tests;

pub use error::{ParseError, ParseErrorKind, SafeTokenDisplay};
pub use limits::ParserLimits;

use super::ast::{QueryExpr, QueryWithCte, Span};
use super::lexer::{Lexer, Position, Spanned, Token};
use crate::storage::schema::Value;
use limits::DepthCounter;

/// RQL Parser
pub struct Parser<'a> {
    lexer: Lexer<'a>,
    /// Current token
    pub(crate) current: Spanned,
    /// Recursion-depth tracker. Each enter/exit of a recursive
    /// descent point should bracket itself with [`enter_depth`] /
    /// [`exit_depth`] (see `parse_expr_prec`).
    pub(crate) depth: DepthCounter,
    /// Tracks placeholder style used so far in this statement.
    /// Mixing `$N` and `?` in one statement is a parse error
    /// (PRD #351 / issue #354).
    pub(crate) placeholder_mode: PlaceholderMode,
    /// Counter for `?` positional placeholders, numbered 1-based
    /// in source. Unused when mode is `Dollar` or `None`.
    pub(crate) question_count: usize,
}

/// Placeholder style locked in by the first placeholder seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaceholderMode {
    None,
    Dollar,
    Question,
}

impl<'a> Parser<'a> {
    /// Create a new parser with default DoS limits.
    pub fn new(input: &'a str) -> Result<Self, ParseError> {
        Self::with_limits(input, ParserLimits::default())
    }

    /// Create a new parser with custom DoS limits.
    pub fn with_limits(input: &'a str, limits: ParserLimits) -> Result<Self, ParseError> {
        // Input-byte cap is enforced before the lexer is even
        // constructed: the lexer holds a `Chars` iterator over the
        // input slice, so refusing oversized input here keeps the
        // pathological case (10 GiB blob) cheap.
        if input.len() > limits.max_input_bytes {
            return Err(ParseError::input_too_large(
                "max_input_bytes",
                limits.max_input_bytes,
                Position::new(1, 1, 0),
            ));
        }
        let mut lexer = Lexer::with_limits(input, limits);
        let current = lexer.next_token()?;
        Ok(Self {
            lexer,
            current,
            depth: DepthCounter::new(limits.max_depth),
            placeholder_mode: PlaceholderMode::None,
            question_count: 0,
        })
    }

    /// Increment the recursion-depth counter or bail with
    /// `ParseError::DepthLimit`. Pair with [`exit_depth`] (use the
    /// `with_depth` helper for RAII-style bracketing).
    pub(crate) fn enter_depth(&mut self) -> Result<(), ParseError> {
        self.depth.depth += 1;
        if self.depth.depth > self.depth.max_depth {
            return Err(ParseError::depth_limit(
                "max_depth",
                self.depth.max_depth,
                self.position(),
            ));
        }
        Ok(())
    }

    /// Decrement the recursion-depth counter. Always called on the
    /// success path; on the error path, the counter is rebalanced
    /// when `Parser` itself is dropped (it's only consulted while
    /// parsing is in progress).
    pub(crate) fn exit_depth(&mut self) {
        if self.depth.depth > 0 {
            self.depth.depth -= 1;
        }
    }

    /// Get current position
    pub fn position(&self) -> Position {
        self.current.start
    }

    /// Advance to next token
    pub fn advance(&mut self) -> Result<Token, ParseError> {
        let old = std::mem::replace(&mut self.current, self.lexer.next_token()?);
        Ok(old.token)
    }

    /// Peek at current token
    pub fn peek(&self) -> &Token {
        &self.current.token
    }

    /// Peek one token past the current parser position without consuming it.
    pub fn peek_next(&mut self) -> Result<&Token, ParseError> {
        Ok(&self.lexer.peek_token()?.token)
    }

    /// Check if current token matches
    pub fn check(&self, expected: &Token) -> bool {
        std::mem::discriminant(&self.current.token) == std::mem::discriminant(expected)
    }

    /// Check if current token is a specific keyword
    pub fn check_keyword(&self, keyword: &Token) -> bool {
        self.check(keyword)
    }

    /// Consume a specific token or error
    pub fn expect(&mut self, expected: Token) -> Result<Token, ParseError> {
        if self.check(&expected) {
            self.advance()
        } else {
            Err(ParseError::expected(
                vec![&expected.to_string()],
                &self.current.token,
                self.position(),
            ))
        }
    }

    /// Consume an identifier and return its value
    pub fn expect_ident(&mut self) -> Result<String, ParseError> {
        match &self.current.token {
            Token::Ident(name) => {
                let name = name.clone();
                self.advance()?;
                Ok(name)
            }
            other => Err(ParseError::expected(
                vec!["identifier"],
                other,
                self.position(),
            )),
        }
    }

    /// Consume an identifier or aggregate keyword when the grammar expects
    /// a user-defined column name.
    pub fn expect_column_ident(&mut self) -> Result<String, ParseError> {
        let name = match &self.current.token {
            Token::Ident(name) => name.clone(),
            Token::Count => "count".to_string(),
            Token::Sum => "sum".to_string(),
            Token::Avg => "avg".to_string(),
            Token::Min => "min".to_string(),
            Token::Max => "max".to_string(),
            other => {
                return Err(ParseError::expected(
                    vec!["identifier"],
                    other,
                    self.position(),
                ));
            }
        };
        self.advance()?;
        Ok(name)
    }

    /// Consume an identifier or keyword (for type names where keywords are valid)
    pub fn expect_ident_or_keyword(&mut self) -> Result<String, ParseError> {
        // Get the string representation of the current token
        let name = match &self.current.token {
            Token::Ident(name) => name.clone(),
            Token::Count => "count".to_string(),
            Token::Sum => "sum".to_string(),
            Token::Avg => "avg".to_string(),
            Token::Min => "min".to_string(),
            Token::Max => "max".to_string(),
            // Keywords that can be type names (convert to uppercase for type matching)
            Token::Contains => "CONTAINS".to_string(),
            Token::Left => "LEFT".to_string(),
            Token::Right => "RIGHT".to_string(),
            Token::First => "FIRST".to_string(),
            Token::Last => "LAST".to_string(),
            Token::In => "IN".to_string(),
            Token::By => "BY".to_string(),
            // Any other keyword - use its display string
            other => other.to_string(),
        };

        // Only advance for valid type-name-like tokens
        match &self.current.token {
            // Identifiers are always valid
            Token::Ident(_) => {
                self.advance()?;
                Ok(name)
            }
            // These keywords can be type names
            Token::Contains
            | Token::Left
            | Token::Right
            | Token::First
            | Token::Last
            | Token::In
            | Token::By => {
                self.advance()?;
                Ok(name)
            }
            // Reject structural tokens that can't be type names
            Token::Eof
            | Token::LParen
            | Token::RParen
            | Token::LBracket
            | Token::RBracket
            | Token::Comma
            | Token::Dot
            | Token::Eq
            | Token::Lt
            | Token::Gt
            | Token::Le
            | Token::Ge
            | Token::Arrow
            | Token::ArrowLeft
            | Token::Dash
            | Token::Colon
            | Token::Semi
            | Token::Star
            | Token::Plus
            | Token::Slash => Err(ParseError::expected(
                vec!["identifier or type name"],
                &self.current.token,
                self.position(),
            )),
            // All other keywords can potentially be type names
            _ => {
                self.advance()?;
                Ok(name)
            }
        }
    }

    /// Try to consume a token, returning true if consumed
    pub fn consume(&mut self, expected: &Token) -> Result<bool, ParseError> {
        if self.check(expected) {
            self.advance()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Try to consume an identifier case-insensitively (for keywords not in Token enum)
    pub fn consume_ident_ci(&mut self, expected: &str) -> Result<bool, ParseError> {
        match self.peek().clone() {
            Token::Ident(name) if name.eq_ignore_ascii_case(expected) => {
                self.advance()?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Parse a complete query
    pub fn parse(&mut self) -> Result<QueryExpr, ParseError> {
        let query = self.parse_frontend_statement()?.into_query_expr();

        // Expect end of input
        if !self.check(&Token::Eof) {
            return Err(ParseError::new(
                // F-05: route the offending token through `SafeTokenDisplay`
                // so the user-controlled `Ident` / `String` / `JsonLiteral`
                // payloads are escaped before the message lands in the
                // downstream JSON / audit / log / gRPC sinks.
                format!(
                    "Unexpected token after query: {}",
                    error::SafeTokenDisplay(&self.current.token)
                ),
                self.position(),
            ));
        }

        Ok(query)
    }

    /// Parse the main query expression (without CTEs)
    pub fn parse_query_expr(&mut self) -> Result<QueryExpr, ParseError> {
        self.parse_frontend_statement()
            .map(|statement| statement.into_query_expr())
    }

    /// Parse an integer literal
    pub fn parse_integer(&mut self) -> Result<i64, ParseError> {
        match &self.current.token {
            Token::Integer(n) => {
                let n = *n;
                self.advance()?;
                Ok(n)
            }
            other => Err(ParseError::expected(
                vec!["integer"],
                other,
                self.position(),
            )),
        }
    }

    /// Parse a `$N` or `?` placeholder in a non-Expr slot (e.g. the
    /// `LIMIT` / `MIN_SCORE` slots of SEARCH SIMILAR; issue #361). The
    /// `field` name is used only to enrich placeholder-mixing errors.
    /// Returns the 0-based parameter index.
    pub fn parse_param_slot(&mut self, field: &'static str) -> Result<usize, ParseError> {
        match self.peek().clone() {
            Token::Dollar => {
                self.advance()?;
                let n = match *self.peek() {
                    Token::Integer(n) if n >= 1 => {
                        self.advance()?;
                        n as usize
                    }
                    _ => {
                        return Err(ParseError::new(
                            format!("expected `$N` (N >= 1) for {field} parameter"),
                            self.position(),
                        ));
                    }
                };
                if self.placeholder_mode == PlaceholderMode::Question {
                    return Err(ParseError::new(
                        "cannot mix `?` and `$N` placeholders in one statement".to_string(),
                        self.position(),
                    ));
                }
                self.placeholder_mode = PlaceholderMode::Dollar;
                Ok(n - 1)
            }
            Token::Question => {
                let (index, _) = self.parse_question_param_index()?;
                Ok(index)
            }
            other => Err(ParseError::expected(
                vec!["$N", "?"],
                &other,
                self.position(),
            )),
        }
    }

    /// Parse a question-style positional placeholder. Bare `?` slots are
    /// assigned left-to-right. Immediate `?N` slots use the explicit
    /// 1-based index, matching `$N` without changing the placeholder
    /// family.
    pub(crate) fn parse_question_param_index(&mut self) -> Result<(usize, Span), ParseError> {
        let start = self.position();
        let question_end = self.current.end;
        self.expect(Token::Question)?;
        if self.placeholder_mode == PlaceholderMode::Dollar {
            return Err(ParseError::new(
                "cannot mix `?` and `$N` placeholders in one statement".to_string(),
                self.position(),
            ));
        }
        self.placeholder_mode = PlaceholderMode::Question;

        if let Token::Integer(n) = *self.peek() {
            if self.current.start.offset == question_end.offset {
                if n < 1 {
                    return Err(ParseError::new(
                        "placeholder index must be >= 1".to_string(),
                        self.position(),
                    ));
                }
                let end = self.current.end;
                self.advance()?;
                let index = n as usize - 1;
                self.question_count = self.question_count.max(index + 1);
                return Ok((index, Span::new(start, end)));
            }
        }

        self.question_count += 1;
        Ok((self.question_count - 1, Span::new(start, question_end)))
    }

    /// Parse a strictly-positive integer literal (`> 0`).
    ///
    /// Surfaces a `ValueOutOfRange` error for `field` when the literal
    /// is `0`, negative, or starts with a unary `-` (which the bare
    /// `parse_integer` rejects with a confusing "expected: integer"
    /// message). Used by modifier slots like `MAX_SIZE`, `CAPACITY`,
    /// `WIDTH`, `DEPTH`, `K` where zero or negative values are
    /// semantically meaningless.
    pub fn parse_positive_integer(&mut self, field: &'static str) -> Result<i64, ParseError> {
        let pos = self.position();
        // Detect the unary-minus path up-front so we surface a
        // clearer "must be positive" message instead of the generic
        // "expected: integer".
        if matches!(self.current.token, Token::Minus | Token::Dash) {
            return Err(ParseError::value_out_of_range(
                field,
                "must be a positive integer",
                pos,
            ));
        }
        let raw = self.parse_integer()?;
        if raw <= 0 {
            return Err(ParseError::value_out_of_range(
                field,
                "must be a positive integer",
                pos,
            ));
        }
        Ok(raw)
    }

    /// Parse float literal. Accepts an optional leading unary `-`
    /// followed by a `Float` or `Integer` token so positions like
    /// vector literals (`[-0.1, …]`), `THRESHOLD -0.5`, `MIN_SCORE
    /// -0.1`, `RERANK(-0.3)`, `UNION(0.7, -0.3)`, and geo coordinates
    /// (`LATITUDE -33.86`) parse correctly. See bug #107.
    pub fn parse_float(&mut self) -> Result<f64, ParseError> {
        let negate = if matches!(self.current.token, Token::Minus | Token::Dash) {
            self.advance()?;
            true
        } else {
            false
        };
        let value = match &self.current.token {
            Token::Float(n) => {
                let n = *n;
                self.advance()?;
                n
            }
            Token::Integer(n) => {
                let n = *n as f64;
                self.advance()?;
                n
            }
            other => {
                return Err(ParseError::expected(vec!["number"], other, self.position()));
            }
        };
        Ok(if negate { -value } else { value })
    }

    /// Parse a string literal
    pub fn parse_string(&mut self) -> Result<String, ParseError> {
        match &self.current.token {
            Token::String(s) => {
                let s = s.clone();
                self.advance()?;
                Ok(s)
            }
            other => Err(ParseError::expected(vec!["string"], other, self.position())),
        }
    }

    /// Parse a value (delegates to parse_literal_value for full JSON support)
    pub fn parse_value(&mut self) -> Result<Value, ParseError> {
        self.parse_literal_value()
    }

    /// Parse value list for IN clause
    pub fn parse_value_list(&mut self) -> Result<Vec<Value>, ParseError> {
        let mut values = Vec::new();
        loop {
            values.push(self.parse_value()?);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        Ok(values)
    }

    /// Phase 1 cutover bridge: parse an expression via the new Pratt
    /// parser (`parser/expr.rs`) and try to fold it back into a
    /// literal `Value`. Used by INSERT VALUES / UPDATE SET / DEFAULT
    /// slots that still store `Value` in their AST nodes — the bridge
    /// lets them benefit from the full Expr grammar (parenthesised
    /// literals, unary minus, CAST literals) without an AST cascade.
    ///
    /// Folds these Expr shapes:
    /// - `Expr::Literal { value, .. }` → `value`
    /// - `Expr::UnaryOp { Neg, operand: Literal(Integer/Float), .. }`
    ///   → negated value
    /// - `Expr::Cast { inner: Literal(text), target, .. }` →
    ///   coerce(text, target) via schema::coerce
    ///
    /// Anything else returns an error so callers can decide whether
    /// to fall back to the legacy `parse_literal_value` path or
    /// surface a "non-literal not supported in this position" error.
    pub fn parse_expr_value(&mut self) -> Result<Value, ParseError> {
        let expr = self.parse_expr()?;
        super::sql_lowering::fold_expr_to_value(expr)
            .map_err(|msg| ParseError::new(msg, self.position()))
    }
}

/// Parse an RQL query string into a `QueryWithCte`. A leading `WITH`
/// is consumed as a CTE prelude; any other statement is returned in
/// the `QueryWithCte::simple` shape (`with_clause: None`). Callers
/// that don't care about CTEs read `.query` to recover the legacy
/// `QueryExpr` shape.
pub fn parse(input: &str) -> Result<QueryWithCte, ParseError> {
    let mut parser = Parser::new(input)?;
    parser.parse_with_cte()
}
