//! Parser for migration SQL statements.

use super::super::ast::{
    ApplyMigrationQuery, ApplyMigrationTarget, CreateMigrationQuery, ExplainMigrationQuery,
    QueryExpr, RollbackMigrationQuery,
};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;

impl<'a> Parser<'a> {
    /// Parse: CREATE MIGRATION name [DEPENDS ON dep1, dep2] [BATCH n ROWS] [NO ROLLBACK] body_sql
    ///
    /// Called after CREATE has been consumed and MIGRATION ident detected.
    pub fn parse_create_migration_body(&mut self) -> Result<QueryExpr, ParseError> {
        let name = self.expect_ident()?;

        let mut depends_on: Vec<String> = Vec::new();
        let mut batch_size: Option<u64> = None;
        let mut no_rollback = false;

        // Parse optional clauses in any order before the body
        loop {
            if self.consume_ident_ci("DEPENDS")? {
                // `ON` is lexed as `Token::On` (reserved keyword), not as
                // an identifier — `consume_ident_ci("ON")` would silently
                // miss it and the next `expect_ident()` would surface
                // "expected identifier, got ON". Require the typed
                // keyword so the dependency list actually parses.
                self.expect(Token::On)?;
                loop {
                    depends_on.push(self.expect_ident()?);
                    if !self.consume(&Token::Comma)? {
                        break;
                    }
                }
            } else if self.consume_ident_ci("BATCH")? {
                if let Token::Integer(n) = self.peek().clone() {
                    self.advance()?;
                    batch_size = Some(n as u64);
                }
                self.consume_ident_ci("ROWS")?;
            } else if self.consume_ident_ci("NO")? {
                self.consume_ident_ci("ROLLBACK")?;
                no_rollback = true;
            } else {
                break;
            }
        }

        // Optional `AS` keyword separating the metadata clauses from the
        // body. SQL convention; without consuming it the body string
        // begins with the literal "AS " token, which then doesn't
        // round-trip through the query-mode detector when the engine
        // re-executes the body in `apply_batched`. `AS` is lexed as
        // `Token::As`, not as an identifier, so use `consume(&Token::As)`.
        let _ = self.consume(&Token::As)?;

        // Everything remaining until EOF is the body
        let body = self.collect_remaining_input();

        Ok(QueryExpr::CreateMigration(CreateMigrationQuery {
            name,
            body,
            depends_on,
            batch_size,
            no_rollback,
        }))
    }

    /// Parse: APPLY MIGRATION name | APPLY MIGRATION * [FOR TENANT id]
    pub fn parse_apply_migration(&mut self) -> Result<QueryExpr, ParseError> {
        // APPLY has already been consumed
        self.consume_ident_ci("MIGRATION")?;

        let target = if self.consume(&Token::Star)? {
            ApplyMigrationTarget::All
        } else {
            let name = self.expect_ident()?;
            ApplyMigrationTarget::Named(name)
        };

        // `FOR` is lexed as `Token::For` (reserved keyword), not as an
        // identifier — `consume_ident_ci("FOR")` never matched it and
        // the suffix was silently dropped, so the `for_tenant` slot
        // stayed `None` while `Token::For` leaked back to the
        // top-level parser as "Unexpected token after query".
        let for_tenant = if self.consume(&Token::For)? {
            // Once FOR is committed, TENANT must follow — bail
            // explicitly if it doesn't, instead of silently accepting
            // arbitrary identifiers as the tenant id.
            if !self.consume_ident_ci("TENANT")? {
                return Err(ParseError::expected(
                    vec!["TENANT"],
                    self.peek(),
                    self.position(),
                ));
            }
            Some(self.expect_string_or_ident()?)
        } else {
            None
        };

        Ok(QueryExpr::ApplyMigration(ApplyMigrationQuery {
            target,
            for_tenant,
        }))
    }

    /// Parse: ROLLBACK MIGRATION name  (called after ROLLBACK is consumed)
    pub fn parse_rollback_migration_after_keyword(&mut self) -> Result<QueryExpr, ParseError> {
        self.consume_ident_ci("MIGRATION")?;
        let name = self.expect_ident()?;
        Ok(QueryExpr::RollbackMigration(RollbackMigrationQuery {
            name,
        }))
    }

    /// Parse: EXPLAIN MIGRATION name  (called after EXPLAIN is consumed)
    pub fn parse_explain_migration_after_keyword(&mut self) -> Result<QueryExpr, ParseError> {
        self.consume_ident_ci("MIGRATION")?;
        let name = self.expect_ident()?;
        Ok(QueryExpr::ExplainMigration(ExplainMigrationQuery { name }))
    }

    /// Collect all remaining tokens into a single string (joined with spaces).
    /// Used to capture the raw SQL body of a migration.
    pub fn collect_remaining_input(&mut self) -> String {
        let mut parts: Vec<String> = Vec::new();
        loop {
            if self.check(&Token::Eof) {
                break;
            }
            parts.push(self.current.token.to_string());
            // Advance, ignoring errors (at worst we stop early)
            if self.advance().is_err() {
                break;
            }
        }
        parts.join(" ")
    }

    /// Try to consume a bare identifier or a single-quoted string literal.
    pub fn expect_string_or_ident(&mut self) -> Result<String, ParseError> {
        match self.peek().clone() {
            Token::String(s) => {
                self.advance()?;
                Ok(s)
            }
            Token::Ident(_) => self.expect_ident(),
            other => Err(ParseError::expected(
                vec!["string or identifier"],
                &other,
                self.position(),
            )),
        }
    }
}
