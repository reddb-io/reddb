//! DDL Parser for CREATE INDEX and DROP INDEX

use super::error::ParseError;
use super::Parser;
use crate::ast::{CreateIndexQuery, DropIndexQuery, IndexMethod, QueryExpr};
use crate::lexer::Token;

impl<'a> Parser<'a> {
    /// Parse: CREATE [UNIQUE] INDEX [IF NOT EXISTS] name ON table (col1, ...) [USING method]
    ///
    /// Called after `Token::Create` has been consumed and we've peeked `Token::Index`
    /// or `Token::Unique`.
    pub fn parse_create_index_query(&mut self) -> Result<QueryExpr, ParseError> {
        // CREATE has already been consumed by the dispatcher

        let unique = self.consume(&Token::Unique)?;

        self.expect(Token::Index)?;

        let if_not_exists = self.match_if_not_exists()?;

        let name = self.expect_ident()?;

        self.expect(Token::On)?;

        let table = self.expect_ident()?;

        // Parse column list: (col1, col2, ...)
        self.expect(Token::LParen)?;
        let mut columns = Vec::new();
        loop {
            columns.push(self.parse_index_column_path()?);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        self.expect(Token::RParen)?;

        // Parse optional USING method
        let method = if self.consume(&Token::Using)? {
            self.parse_index_method()?
        } else {
            IndexMethod::BTree // default
        };

        Ok(QueryExpr::CreateIndex(CreateIndexQuery {
            name,
            table,
            columns,
            method,
            unique,
            if_not_exists,
        }))
    }

    /// Parse: DROP INDEX [IF EXISTS] name ON table
    ///
    /// Called after `Token::Drop` has been consumed and we've peeked `Token::Index`.
    pub fn parse_drop_index_query(&mut self) -> Result<QueryExpr, ParseError> {
        // DROP has already been consumed by the dispatcher

        self.expect(Token::Index)?;

        let if_exists = self.match_if_exists()?;

        let name = self.expect_ident()?;

        self.expect(Token::On)?;

        let table = self.expect_ident()?;

        Ok(QueryExpr::DropIndex(DropIndexQuery {
            name,
            table,
            if_exists,
        }))
    }

    /// Parse index method identifier: HASH | BTREE | BITMAP | RTREE.
    /// `HASH` is also a reserved keyword token, so we match both the
    /// keyword form and the ident form — otherwise `USING HASH`
    /// fails with "Unexpected token: HASH" even though the parser
    /// lists HASH as an expected option.
    fn parse_index_method(&mut self) -> Result<IndexMethod, ParseError> {
        let peeked = self.peek().clone();
        if matches!(peeked, Token::Hash) {
            self.advance()?;
            return Ok(IndexMethod::Hash);
        }
        match peeked {
            Token::Ident(ref name) => {
                let method = match name.to_ascii_uppercase().as_str() {
                    "HASH" => IndexMethod::Hash,
                    "BTREE" => IndexMethod::BTree,
                    "BITMAP" => IndexMethod::Bitmap,
                    "RTREE" => IndexMethod::RTree,
                    _ => {
                        return Err(ParseError::new(
                            format!(
                                "unknown index method '{}', expected HASH, BTREE, BITMAP, or RTREE",
                                name
                            ),
                            self.position(),
                        ));
                    }
                };
                self.advance()?;
                Ok(method)
            }
            other => Err(ParseError::expected(
                vec!["HASH", "BTREE", "BITMAP", "RTREE"],
                &other,
                self.position(),
            )),
        }
    }

    fn parse_index_column_path(&mut self) -> Result<String, ParseError> {
        let mut column = self.expect_ident_or_keyword()?;
        while self.consume(&Token::Dot)? {
            column.push('.');
            column.push_str(&self.expect_ident_or_keyword()?);
        }
        Ok(column)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser(input: &str) -> Parser<'_> {
        Parser::new(input).unwrap_or_else(|err| panic!("failed to lex {input:?}: {err:?}"))
    }

    fn parse_create_index(input: &str) -> CreateIndexQuery {
        let mut parser = parser(input);
        parser.expect(Token::Create).expect("CREATE");
        let QueryExpr::CreateIndex(query) = parser
            .parse_create_index_query()
            .unwrap_or_else(|err| panic!("failed to parse {input:?}: {err:?}"))
        else {
            panic!("Expected CreateIndexQuery");
        };
        assert!(
            matches!(parser.peek(), Token::Eof),
            "CREATE INDEX parse did not consume all input: {input:?}"
        );
        query
    }

    fn parse_drop_index(input: &str) -> DropIndexQuery {
        let mut parser = parser(input);
        parser.expect(Token::Drop).expect("DROP");
        let QueryExpr::DropIndex(query) = parser
            .parse_drop_index_query()
            .unwrap_or_else(|err| panic!("failed to parse {input:?}: {err:?}"))
        else {
            panic!("Expected DropIndexQuery");
        };
        assert!(
            matches!(parser.peek(), Token::Eof),
            "DROP INDEX parse did not consume all input: {input:?}"
        );
        query
    }

    #[test]
    fn parse_create_index_defaults_and_options() {
        let query = parse_create_index("CREATE INDEX IF NOT EXISTS idx ON users (email, type)");
        assert_eq!(query.name, "idx");
        assert_eq!(query.table, "users");
        assert_eq!(query.columns, vec!["email", "type"]);
        assert_eq!(query.method, IndexMethod::BTree);
        assert!(query.if_not_exists);
        assert!(!query.unique);

        let query = parse_create_index("CREATE UNIQUE INDEX idx_orders ON orders (id) USING HASH");
        assert!(query.unique);
        assert_eq!(query.method, IndexMethod::Hash);
    }

    #[test]
    fn parse_create_index_on_document_path() {
        let query = parse_create_index("CREATE INDEX idx_docs_tier ON docs (body.service.tier)");
        assert_eq!(query.name, "idx_docs_tier");
        assert_eq!(query.table, "docs");
        assert_eq!(query.columns, vec!["body.service.tier"]);
        assert_eq!(query.method, IndexMethod::BTree);
    }

    #[test]
    fn parse_index_method_accepts_all_ident_variants() {
        for (method, expected) in [
            ("BTREE", IndexMethod::BTree),
            ("BITMAP", IndexMethod::Bitmap),
            ("RTREE", IndexMethod::RTree),
            ("hash", IndexMethod::Hash),
        ] {
            let query = parse_create_index(&format!(
                "CREATE INDEX idx_{method} ON users (email) USING {method}"
            ));
            assert_eq!(query.method, expected);
        }
    }

    #[test]
    fn parse_drop_index_body_covers_if_exists() {
        let query = parse_drop_index("DROP INDEX IF EXISTS idx_email ON users");
        assert_eq!(query.name, "idx_email");
        assert_eq!(query.table, "users");
        assert!(query.if_exists);

        let query = parse_drop_index("DROP INDEX idx_email ON users");
        assert!(!query.if_exists);
    }

    #[test]
    fn parse_index_method_reports_unknown_ident_and_non_ident_errors() {
        let mut p = parser("CREATE INDEX idx ON users (email) USING GIN");
        p.expect(Token::Create).expect("CREATE");
        let err = p.parse_create_index_query().unwrap_err();
        assert!(
            err.to_string().contains("unknown index method 'GIN'"),
            "{err}"
        );

        let mut p = parser("CREATE INDEX idx ON users (email) USING 42");
        p.expect(Token::Create).expect("CREATE");
        let err = p.parse_create_index_query().unwrap_err();
        assert!(
            err.to_string()
                .contains("expected: HASH, BTREE, BITMAP, RTREE"),
            "{err}"
        );
    }
}
