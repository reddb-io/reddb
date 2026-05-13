//! Graph query parsing (MATCH pattern)

use super::super::ast::{
    CompareOp, EdgeDirection, EdgePattern, FieldRef, GraphPattern, GraphQuery, NodePattern,
    Projection, PropertyFilter, QueryExpr,
};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;

impl<'a> Parser<'a> {
    /// Parse MATCH ... RETURN query
    pub fn parse_match_query(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::Match)?;

        let pattern = self.parse_graph_pattern()?;

        let filter = if self.consume(&Token::Where)? {
            Some(self.parse_filter()?)
        } else {
            None
        };

        self.expect(Token::Return)?;
        let return_ = self.parse_return_list()?;
        let limit = self.parse_match_limit()?;

        Ok(QueryExpr::Graph(GraphQuery {
            alias: None,
            pattern,
            filter,
            return_,
            limit,
        }))
    }

    fn parse_match_limit(&mut self) -> Result<Option<u64>, ParseError> {
        if !self.consume(&Token::Limit)? && !self.consume_ident_ci("LIMIT")? {
            return Ok(None);
        }

        let pos = self.position();
        if matches!(self.current.token, Token::Minus | Token::Dash) {
            return Err(ParseError::value_out_of_range(
                "MATCH LIMIT",
                "must be a non-negative integer",
                pos,
            ));
        }

        let raw = self.parse_integer()?;
        if raw < 0 {
            return Err(ParseError::value_out_of_range(
                "MATCH LIMIT",
                "must be a non-negative integer",
                pos,
            ));
        }
        Ok(Some(raw as u64))
    }

    /// Parse graph pattern: (a)-[r]->(b)
    pub fn parse_graph_pattern(&mut self) -> Result<GraphPattern, ParseError> {
        let mut pattern = GraphPattern::new();

        // Parse first node
        let first_node = self.parse_node_pattern()?;
        pattern.nodes.push(first_node);

        // Parse chain of edges and nodes
        while self.peek() == &Token::Dash || self.peek() == &Token::ArrowLeft {
            let (edge, next_node) =
                self.parse_edge_and_node(pattern.nodes.last().unwrap().alias.clone())?;
            pattern.edges.push(edge);
            pattern.nodes.push(next_node);
        }

        Ok(pattern)
    }

    /// Parse node pattern: (alias:Type {props})
    pub fn parse_node_pattern(&mut self) -> Result<NodePattern, ParseError> {
        self.expect(Token::LParen)?;

        let alias = self.expect_ident()?;

        // Label filter is a free-form string; resolution against the
        // graph's `LabelRegistry` happens at execution time, not here.
        let node_label = if self.consume(&Token::Colon)? {
            Some(self.expect_ident_or_keyword()?.to_lowercase())
        } else {
            None
        };

        let properties = if self.consume(&Token::LBrace)? {
            self.parse_property_filters()?
        } else {
            Vec::new()
        };

        self.expect(Token::RParen)?;

        Ok(NodePattern {
            alias,
            node_label,
            properties,
        })
    }

    /// Parse edge and next node: -[r:TYPE*min..max]->(b)
    fn parse_edge_and_node(
        &mut self,
        from_alias: String,
    ) -> Result<(EdgePattern, NodePattern), ParseError> {
        // Determine direction
        let incoming = self.consume(&Token::ArrowLeft)?;
        if !incoming {
            self.expect(Token::Dash)?;
        }

        // Parse edge pattern
        self.expect(Token::LBracket)?;

        let alias = if let Token::Ident(name) = self.peek() {
            let name = name.clone();
            self.advance()?;
            Some(name)
        } else {
            None
        };

        let edge_label = if self.consume(&Token::Colon)? {
            Some(self.expect_ident_or_keyword()?.to_lowercase())
        } else {
            None
        };

        // Variable length: *min..max
        let (min_hops, max_hops) = if self.consume(&Token::Star)? {
            if let Token::Integer(_) = self.peek() {
                let min = self.parse_integer()? as u32;
                if self.consume(&Token::DotDot)? {
                    let max = self.parse_integer()? as u32;
                    (min, max)
                } else {
                    (min, min)
                }
            } else {
                (1, u32::MAX) // * means any length
            }
        } else {
            (1, 1) // Default: exactly 1 hop
        };

        self.expect(Token::RBracket)?;

        // Determine final direction
        let direction = if incoming {
            self.expect(Token::Dash)?;
            EdgeDirection::Incoming
        } else if self.consume(&Token::Arrow)? {
            EdgeDirection::Outgoing
        } else {
            self.expect(Token::Dash)?;
            EdgeDirection::Both
        };

        // Parse next node
        let next_node = self.parse_node_pattern()?;

        let edge = EdgePattern {
            alias,
            from: from_alias,
            to: next_node.alias.clone(),
            edge_label,
            direction,
            min_hops,
            max_hops,
        };

        Ok((edge, next_node))
    }

    /// Parse property filters in braces: {name: 'value', age: 25}
    pub fn parse_property_filters(&mut self) -> Result<Vec<PropertyFilter>, ParseError> {
        let mut filters = Vec::new();

        loop {
            let name = self.expect_ident()?;
            self.expect(Token::Colon)?;
            let value = self.parse_value()?;

            filters.push(PropertyFilter {
                name,
                op: CompareOp::Eq,
                value,
            });

            if !self.consume(&Token::Comma)? {
                break;
            }
        }

        self.expect(Token::RBrace)?;
        Ok(filters)
    }

    /// Parse RETURN list
    pub fn parse_return_list(&mut self) -> Result<Vec<Projection>, ParseError> {
        let mut projections = Vec::new();
        loop {
            let proj = self.parse_graph_projection()?;
            projections.push(proj);

            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        Ok(projections)
    }

    /// Parse a graph projection (can be node alias, node.property, etc.)
    fn parse_graph_projection(&mut self) -> Result<Projection, ParseError> {
        let first = self.expect_ident()?;

        let field = if self.consume(&Token::Dot)? {
            let prop = self.expect_ident()?;
            FieldRef::NodeProperty {
                alias: first,
                property: prop,
            }
        } else {
            // Just the alias, refers to the whole node
            FieldRef::NodeId { alias: first }
        };

        let alias = if self.consume(&Token::As)? {
            Some(self.expect_ident()?)
        } else {
            None
        };

        Ok(Projection::Field(field, alias))
    }

    /// Normalize a parsed node-type token to its label-string form. The
    /// pentest-flavoured aliases (`VULN`, `TECH`, `CERT`) are kept so
    /// existing query strings continue to parse, but the result is just
    /// the canonical lowercase label and is no longer constrained to a
    /// closed enum.
    pub fn parse_node_label(&self, name: &str) -> Result<String, ParseError> {
        let canonical = match name.to_uppercase().as_str() {
            "HOST" => "host",
            "SERVICE" => "service",
            "CREDENTIAL" => "credential",
            "VULNERABILITY" | "VULN" => "vulnerability",
            "ENDPOINT" => "endpoint",
            "TECHNOLOGY" | "TECH" => "technology",
            "USER" => "user",
            "DOMAIN" => "domain",
            "CERTIFICATE" | "CERT" => "certificate",
            // Forward unknown labels verbatim (lowercased) — the registry
            // resolves them at execution time, or a later validation
            // pass can reject them.
            other => return Ok(other.to_lowercase()),
        };
        Ok(canonical.to_string())
    }

    /// Edge label counterpart to [`parse_node_label`].
    pub fn parse_edge_label(&self, name: &str) -> Result<String, ParseError> {
        let canonical = match name.to_uppercase().as_str() {
            "HAS_SERVICE" => "has_service",
            "HAS_ENDPOINT" => "has_endpoint",
            "USES_TECH" | "USES_TECHNOLOGY" => "uses_tech",
            "AUTH_ACCESS" | "AUTH" => "auth_access",
            "AFFECTED_BY" => "affected_by",
            "CONTAINS" => "contains",
            "CONNECTS_TO" | "CONNECTS" => "connects_to",
            "RELATED_TO" | "RELATED" => "related_to",
            "HAS_USER" => "has_user",
            "HAS_CERT" | "HAS_CERTIFICATE" => "has_cert",
            other => return Ok(other.to_lowercase()),
        };
        Ok(canonical.to_string())
    }
}
