//! Table query parsing (SELECT ... FROM ...)

use super::super::ast::{OrderByClause, Projection, QueryExpr, TableQuery};
use super::super::lexer::Token;
use super::error::ParseError;

fn is_scalar_function(name: &str) -> bool {
    matches!(
        name,
        "GEO_DISTANCE"
            | "GEO_DISTANCE_VINCENTY"
            | "GEO_BEARING"
            | "GEO_MIDPOINT"
            | "HAVERSINE"
            | "VINCENTY"
            | "UPPER"
            | "LOWER"
            | "LENGTH"
            | "ABS"
            | "ROUND"
            | "COALESCE"
    )
}
use super::Parser;

impl<'a> Parser<'a> {
    /// Parse SELECT ... FROM ... query
    pub fn parse_select_query(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::Select)?;

        // Parse column list
        let columns = self.parse_projection_list()?;

        // Parse optional table source. If omitted, default to `ANY` so the query
        // can return mixed entities (table, document, graph, and vector) by default.
        let has_from = self.consume(&Token::From)?;
        let table = if has_from {
            if self.consume(&Token::Star)? {
                "*".to_string()
            } else if self.consume(&Token::All)? {
                "all".to_string()
            } else {
                self.expect_ident()?
            }
        } else {
            "any".to_string()
        };

        // Parse optional alias (only when a FROM clause exists).
        let alias = if !has_from {
            None
        } else if self.consume(&Token::As)?
            || (self.check(&Token::Ident("".into())) && !self.is_clause_keyword())
        {
            Some(self.expect_ident()?)
        } else {
            None
        };

        let mut query = TableQuery {
            table,
            alias,
            columns,
            filter: None,
            group_by: Vec::new(),
            having: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            expand: None,
        };

        // Parse optional clauses
        self.parse_table_clauses(&mut query)?;

        Ok(QueryExpr::Table(query))
    }
}

impl<'a> Parser<'a> {
    /// Check if current identifier is a clause keyword
    pub fn is_clause_keyword(&self) -> bool {
        matches!(
            self.peek(),
            Token::Where
                | Token::Order
                | Token::Limit
                | Token::Offset
                | Token::Join
                | Token::Inner
                | Token::Left
                | Token::Right
        )
    }

    /// Parse projection list (column selections)
    pub fn parse_projection_list(&mut self) -> Result<Vec<Projection>, ParseError> {
        // Handle SELECT *
        if self.consume(&Token::Star)? {
            return Ok(Vec::new()); // Empty means all columns
        }

        let mut projections = Vec::new();
        loop {
            let proj = self.parse_projection()?;
            projections.push(proj);

            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        Ok(projections)
    }

    /// Parse a single projection — supports columns, aggregate functions, and scalar functions
    fn parse_projection(&mut self) -> Result<Projection, ParseError> {
        // Check for aggregate functions: COUNT(*), AVG(col), SUM(col), MIN(col), MAX(col)
        let is_agg = matches!(
            self.peek(),
            Token::Count | Token::Sum | Token::Avg | Token::Min | Token::Max
        );
        if is_agg {
            let func_name = self.advance()?.to_string().to_uppercase();
            self.expect(Token::LParen)?;
            let args = if self.consume(&Token::Star)? {
                vec![Projection::All]
            } else {
                let col = self.expect_ident_or_keyword()?;
                vec![Projection::Column(col)]
            };
            self.expect(Token::RParen)?;
            if self.consume(&Token::As)? {
                let _alias = self.expect_ident()?;
            }
            return Ok(Projection::Function(func_name, args));
        }

        // Check for scalar function: IDENT(args) — e.g. GEO_DISTANCE(col, POINT(x,y))
        if let Token::Ident(ref name) = self.peek() {
            let upper = name.to_uppercase();
            if is_scalar_function(&upper) {
                self.advance()?; // consume function name
                self.expect(Token::LParen)?;
                let args = self.parse_function_args()?;
                self.expect(Token::RParen)?;
                let alias = if self.consume(&Token::As)? {
                    Some(self.expect_ident()?)
                } else {
                    None
                };
                return Ok(if let Some(a) = alias {
                    Projection::Function(format!("{}:{}", upper, a), args)
                } else {
                    Projection::Function(upper, args)
                });
            }
        }

        let field = self.parse_field_ref()?;
        let alias = if self.consume(&Token::As)? {
            Some(self.expect_ident()?)
        } else {
            None
        };
        Ok(Projection::Field(field, alias))
    }

    /// Parse comma-separated function arguments (columns, literals, POINT())
    fn parse_function_args(&mut self) -> Result<Vec<Projection>, ParseError> {
        let mut args = Vec::new();
        if self.peek() == &Token::RParen {
            return Ok(args);
        }
        loop {
            // POINT(lat, lon) → encoded as Column("POINT:lat:lon")
            if let Token::Ident(ref name) = self.peek() {
                if name.eq_ignore_ascii_case("POINT") {
                    self.advance()?; // consume POINT
                    self.expect(Token::LParen)?;
                    let lat = self.parse_numeric_literal()?;
                    self.expect(Token::Comma)?;
                    let lon = self.parse_numeric_literal()?;
                    self.expect(Token::RParen)?;
                    args.push(Projection::Column(format!("POINT:{}:{}", lat, lon)));
                    if !self.consume(&Token::Comma)? {
                        break;
                    }
                    continue;
                }
            }
            // Numeric literal
            if matches!(self.peek(), Token::Float(_)) || self.peek() == &Token::Minus {
                let val = self.parse_numeric_literal()?;
                args.push(Projection::Column(format!("LIT:{}", val)));
                if !self.consume(&Token::Comma)? {
                    break;
                }
                continue;
            }
            // String literal
            if let Token::String(s) = self.peek().clone() {
                self.advance()?;
                args.push(Projection::Column(format!("LIT:{}", s)));
                if !self.consume(&Token::Comma)? {
                    break;
                }
                continue;
            }
            // Column reference
            let col = self.expect_ident_or_keyword()?;
            args.push(Projection::Column(col));
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        Ok(args)
    }

    /// Parse a numeric literal (float, positive or negative)
    fn parse_numeric_literal(&mut self) -> Result<f64, ParseError> {
        let negative = self.consume(&Token::Minus)?;
        match self.advance()? {
            Token::Float(n) => Ok(if negative { -n } else { n }),
            other => Err(ParseError::new(
                format!("expected number, got {}", other),
                self.position(),
            )),
        }
    }

    /// Parse table query clauses (WHERE, GROUP BY, HAVING, ORDER BY, LIMIT, OFFSET)
    pub fn parse_table_clauses(&mut self, query: &mut TableQuery) -> Result<(), ParseError> {
        // WHERE clause
        if self.consume(&Token::Where)? {
            query.filter = Some(self.parse_filter()?);
        }

        // GROUP BY clause
        if self.consume(&Token::Group)? {
            self.expect(Token::By)?;
            query.group_by = self.parse_group_by_list()?;
        }

        // HAVING clause (only valid after GROUP BY)
        if !query.group_by.is_empty() && self.consume_ident_ci("HAVING")? {
            query.having = Some(self.parse_filter()?);
        }

        // ORDER BY clause
        if self.consume(&Token::Order)? {
            self.expect(Token::By)?;
            query.order_by = self.parse_order_by_list()?;
        }

        // LIMIT clause
        if self.consume(&Token::Limit)? {
            query.limit = Some(self.parse_integer()? as u64);
        }

        // OFFSET clause
        if self.consume(&Token::Offset)? {
            query.offset = Some(self.parse_integer()? as u64);
        }

        // WITH EXPAND clause
        if self.consume(&Token::With)? && self.consume_ident_ci("EXPAND")? {
            query.expand = Some(self.parse_expand_options()?);
        }

        Ok(())
    }

    /// Parse EXPAND options: GRAPH [DEPTH n], CROSS_REFS, ALL
    fn parse_expand_options(
        &mut self,
    ) -> Result<crate::storage::query::ast::ExpandOptions, ParseError> {
        use crate::storage::query::ast::ExpandOptions;
        let mut opts = ExpandOptions::default();

        loop {
            if self.consume(&Token::Graph)? || self.consume_ident_ci("GRAPH")? {
                opts.graph = true;
                opts.graph_depth = if self.consume(&Token::Depth)? {
                    self.parse_integer()? as usize
                } else {
                    1
                };
            } else if self.consume_ident_ci("CROSS_REFS")?
                || self.consume_ident_ci("CROSSREFS")?
                || self.consume_ident_ci("REFS")?
            {
                opts.cross_refs = true;
            } else if self.consume(&Token::All)? || self.consume_ident_ci("ALL")? {
                opts.graph = true;
                opts.cross_refs = true;
                opts.graph_depth = 1;
            } else {
                break;
            }
            if !self.consume(&Token::Comma)? {
                break;
            }
        }

        if !opts.graph && !opts.cross_refs {
            opts.graph = true;
            opts.cross_refs = true;
            opts.graph_depth = 1;
        }

        Ok(opts)
    }

    /// Parse GROUP BY field list
    pub fn parse_group_by_list(&mut self) -> Result<Vec<String>, ParseError> {
        let mut fields = Vec::new();
        loop {
            fields.push(self.expect_ident()?);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        Ok(fields)
    }

    /// Parse ORDER BY list
    pub fn parse_order_by_list(&mut self) -> Result<Vec<OrderByClause>, ParseError> {
        let mut clauses = Vec::new();
        loop {
            let field = self.parse_field_ref()?;
            let ascending = if self.consume(&Token::Desc)? {
                false
            } else {
                self.consume(&Token::Asc)?;
                true
            };

            let nulls_first = if self.consume(&Token::Nulls)? {
                if self.consume(&Token::First)? {
                    true
                } else {
                    self.expect(Token::Last)?;
                    false
                }
            } else {
                !ascending // Default: nulls last for ASC, first for DESC
            };

            clauses.push(OrderByClause {
                field,
                ascending,
                nulls_first,
            });

            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        Ok(clauses)
    }
}
