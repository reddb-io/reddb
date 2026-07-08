//! DDL Parser for CREATE INDEX and DROP INDEX

use super::error::ParseError;
use super::Parser;
use crate::ast::{CreateIndexQuery, DropIndexQuery, IndexMethod, QueryExpr, DEFAULT_H3_RESOLUTION};
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

    /// Parse index method identifier: HASH | BTREE | BITMAP | SPATIAL | H3.
    /// `HASH` is also a reserved keyword token, so we match both the
    /// keyword form and the ident form — otherwise `USING HASH`
    /// fails with "Unexpected token: HASH" even though the parser
    /// lists HASH as an expected option.
    ///
    /// `H3` additionally accepts an optional parenthesised resolution
    /// (`USING H3 (12)`); when omitted it defaults to
    /// [`DEFAULT_H3_RESOLUTION`]. The column list is already consumed
    /// before `USING`, so the trailing `(...)` is unambiguously the
    /// resolution argument.
    fn parse_index_method(&mut self) -> Result<IndexMethod, ParseError> {
        let peeked = self.peek().clone();
        if matches!(peeked, Token::Hash) {
            self.advance()?;
            return Ok(IndexMethod::Hash);
        }
        match peeked {
            Token::Ident(ref name) => {
                let upper = name.to_ascii_uppercase();
                if upper == "H3" {
                    self.advance()?;
                    let resolution = self.parse_h3_resolution_opt()?;
                    return Ok(IndexMethod::H3 { resolution });
                }
                let method = match upper.as_str() {
                    "HASH" => IndexMethod::Hash,
                    "BTREE" => IndexMethod::BTree,
                    "BITMAP" => IndexMethod::Bitmap,
                    "RTREE" => {
                        return Err(ParseError::new(
                            "USING RTREE was removed: the in-RAM R-tree indexed nothing and served no queries. Use USING H3 — same SEARCH SPATIAL surface, disk-resident, maintained on every write. Example: CREATE INDEX idx_loc ON events (gpsLocation) USING H3",
                            self.position(),
                        ));
                    }
                    "SPATIAL" => IndexMethod::Spatial,
                    _ => {
                        return Err(ParseError::new(
                            format!(
                                "unknown index method '{}', expected HASH, BTREE, BITMAP, SPATIAL, or H3",
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
                vec!["HASH", "BTREE", "BITMAP", "SPATIAL", "H3"],
                &other,
                self.position(),
            )),
        }
    }

    /// Parse the optional `(resolution)` argument that follows `USING H3`.
    /// Resolution must be an integer in H3's valid `0..=15` range; absence
    /// of the parenthesised argument yields [`DEFAULT_H3_RESOLUTION`].
    fn parse_h3_resolution_opt(&mut self) -> Result<u8, ParseError> {
        if !self.consume(&Token::LParen)? {
            return Ok(DEFAULT_H3_RESOLUTION);
        }
        let resolution = match self.peek().clone() {
            Token::Integer(n) if (0..=15).contains(&n) => {
                self.advance()?;
                n as u8
            }
            other => {
                return Err(ParseError::new(
                    format!("H3 resolution must be an integer 0..=15, got {other:?}"),
                    self.position(),
                ));
            }
        };
        self.expect(Token::RParen)?;
        Ok(resolution)
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
            ("hash", IndexMethod::Hash),
        ] {
            let query = parse_create_index(&format!(
                "CREATE INDEX idx_{method} ON users (email) USING {method}"
            ));
            assert_eq!(query.method, expected);
        }
    }

    #[test]
    fn parse_create_index_using_rtree_reports_didactic_removal() {
        let mut p = parser("CREATE INDEX idx_loc ON events (gpsLocation) USING RTREE");
        p.expect(Token::Create).expect("CREATE");
        let err = p.parse_create_index_query().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("USING RTREE was removed"), "{msg}");
        assert!(msg.contains("Use USING H3"), "{msg}");
        assert!(
            msg.contains("CREATE INDEX idx_loc ON events (gpsLocation) USING H3"),
            "{msg}"
        );
    }

    #[test]
    fn parse_create_index_using_h3_defaults_resolution() {
        let query = parse_create_index("CREATE INDEX idx_loc ON places (loc) USING H3");
        assert_eq!(query.name, "idx_loc");
        assert_eq!(query.table, "places");
        assert_eq!(query.columns, vec!["loc"]);
        assert_eq!(
            query.method,
            IndexMethod::H3 {
                resolution: DEFAULT_H3_RESOLUTION
            }
        );
    }

    #[test]
    fn parse_create_index_using_h3_explicit_resolution() {
        let query = parse_create_index("CREATE INDEX idx_loc ON places (loc) USING H3 (12)");
        assert_eq!(query.method, IndexMethod::H3 { resolution: 12 });

        // Lowercase method ident is accepted too.
        let query = parse_create_index("CREATE INDEX idx_loc ON places (loc) USING h3 (0)");
        assert_eq!(query.method, IndexMethod::H3 { resolution: 0 });
    }

    #[test]
    fn parse_create_index_using_h3_rejects_out_of_range_resolution() {
        let mut p = parser("CREATE INDEX idx ON places (loc) USING H3 (16)");
        p.expect(Token::Create).expect("CREATE");
        let err = p.parse_create_index_query().unwrap_err();
        assert!(
            err.to_string().contains("H3 resolution must be an integer"),
            "{err}"
        );
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
                .contains("expected: HASH, BTREE, BITMAP, SPATIAL"),
            "{err}"
        );
    }

    #[test]
    fn parse_create_index_using_spatial_is_generic_spatial_method() {
        // `USING SPATIAL` is the generic spatial request; the engine maps
        // it to the default spatial backend (H3 as of #1578). The parser
        // only needs to surface the generic `Spatial` method here.
        let query = parse_create_index("CREATE INDEX gix ON places (loc) USING SPATIAL");
        assert_eq!(query.method, IndexMethod::Spatial);
        // Lowercase ident is accepted too.
        let query = parse_create_index("CREATE INDEX gix ON places (loc) USING spatial");
        assert_eq!(query.method, IndexMethod::Spatial);
    }
}
