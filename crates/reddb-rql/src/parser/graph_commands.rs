//! Graph Command Parser: GRAPH NEIGHBORHOOD | SHORTEST_PATH | TRAVERSE | CENTRALITY | ...

use super::error::ParseError;
use super::Parser;
use crate::ast::{GraphCommand, GraphCommandOrderBy, QueryExpr};
use crate::lexer::Token;

impl<'a> Parser<'a> {
    /// Parse: GRAPH subcommand ...
    pub fn parse_graph_command(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::Graph)?;
        match self.peek().clone() {
            Token::Neighborhood => self.parse_graph_neighborhood(),
            Token::ShortestPath => self.parse_graph_shortest_path(),
            Token::Traverse => self.parse_graph_traverse(),
            Token::Centrality => self.parse_graph_centrality(),
            Token::Community => self.parse_graph_community(),
            Token::Components => self.parse_graph_components(),
            Token::Cycles => self.parse_graph_cycles(),
            Token::Clustering => self.parse_graph_clustering(),
            Token::TopologicalSort => self.parse_graph_topological_sort(),
            Token::Properties => self.parse_graph_properties(),
            _ => Err(ParseError::expected(
                vec![
                    "NEIGHBORHOOD",
                    "SHORTEST_PATH",
                    "TRAVERSE",
                    "CENTRALITY",
                    "COMMUNITY",
                    "COMPONENTS",
                    "CYCLES",
                    "CLUSTERING",
                    "TOPOLOGICAL_SORT",
                    "PROPERTIES",
                ],
                self.peek(),
                self.position(),
            )),
        }
    }

    /// Parse: GRAPH NEIGHBORHOOD 'source' [DEPTH n] [DIRECTION outgoing|incoming|both] [EDGES IN ('label', ...)]
    fn parse_graph_neighborhood(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume NEIGHBORHOOD
        let source = self.parse_string()?;
        let mut depth = 3;
        let mut direction = "outgoing".to_string();
        let mut edge_labels = None;

        loop {
            if self.consume(&Token::Depth)? {
                depth = self.parse_integer()? as u32;
            } else if self.consume(&Token::Direction)? {
                direction = self.expect_ident_or_keyword()?;
            } else if self.consume_ident_ci("EDGES")? {
                edge_labels = Some(self.parse_graph_edge_label_list()?);
            } else {
                break;
            }
        }

        Ok(QueryExpr::GraphCommand(GraphCommand::Neighborhood {
            source,
            depth,
            direction,
            edge_labels,
        }))
    }

    /// Parse: GRAPH SHORTEST_PATH [FROM] 'source' TO 'target' [ALGORITHM bfs|dijkstra] [DIRECTION dir] [ORDER BY metric [ASC|DESC]] [LIMIT n]
    fn parse_graph_shortest_path(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume SHORTEST_PATH
        let _ = self.consume(&Token::From)?; // optional FROM (docs syntax)
        let source = self.parse_string()?;
        self.expect(Token::To)?;
        let target = self.parse_string()?;
        let mut algorithm: Option<String> = None;
        let mut direction: Option<String> = None;
        let mut limit: Option<u32> = None;
        let mut order_by: Option<GraphCommandOrderBy> = None;
        loop {
            if algorithm.is_none() && self.consume(&Token::Algorithm)? {
                algorithm = Some(self.expect_ident_or_keyword()?);
            } else if direction.is_none() && self.consume(&Token::Direction)? {
                direction = Some(self.expect_ident_or_keyword()?);
            } else if order_by.is_none() && self.consume(&Token::Order)? {
                order_by = Some(self.parse_graph_order_by_tail()?);
            } else if limit.is_none() && self.consume(&Token::Limit)? {
                let n = self.parse_integer()?;
                limit = Some(n as u32);
            } else {
                break;
            }
        }
        Ok(QueryExpr::GraphCommand(GraphCommand::ShortestPath {
            source,
            target,
            algorithm: algorithm.unwrap_or_else(|| "bfs".to_string()),
            direction: direction.unwrap_or_else(|| "outgoing".to_string()),
            limit,
            order_by,
        }))
    }

    /// Parse: GRAPH TRAVERSE [FROM] 'source' [STRATEGY bfs|dfs] [DEPTH n | MAX_DEPTH n] [DIRECTION dir] [EDGES IN ('label', ...)]
    fn parse_graph_traverse(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume TRAVERSE
        let _ = self.consume(&Token::From)?; // optional FROM (docs syntax)
        let source = self.parse_string()?;
        let mut strategy = "bfs".to_string();
        let mut depth: u32 = 5;
        let mut direction = "outgoing".to_string();
        let mut edge_labels = None;
        loop {
            if self.consume(&Token::Strategy)? {
                strategy = self.expect_ident_or_keyword()?;
            } else if self.consume(&Token::Depth)? || self.consume_ident_ci("MAX_DEPTH")? {
                depth = self.parse_integer()? as u32;
            } else if self.consume(&Token::Direction)? {
                direction = self.expect_ident_or_keyword()?;
            } else if self.consume_ident_ci("EDGES")? {
                edge_labels = Some(self.parse_graph_edge_label_list()?);
            } else {
                break;
            }
        }
        Ok(QueryExpr::GraphCommand(GraphCommand::Traverse {
            source,
            strategy,
            depth,
            direction,
            edge_labels,
        }))
    }

    fn parse_graph_edge_label_list(&mut self) -> Result<Vec<String>, ParseError> {
        self.expect(Token::In)?;
        self.expect(Token::LParen)?;
        let mut labels = Vec::new();
        loop {
            labels.push(self.parse_string()?);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        self.expect(Token::RParen)?;
        Ok(labels)
    }

    /// Parse: GRAPH CENTRALITY [ALGORITHM degree|closeness|betweenness|eigenvector|pagerank] [ORDER BY metric [ASC|DESC]] [LIMIT n]
    fn parse_graph_centrality(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume CENTRALITY
        let mut algorithm: Option<String> = None;
        let mut limit: Option<u32> = None;
        let mut order_by: Option<GraphCommandOrderBy> = None;
        loop {
            if algorithm.is_none() && self.consume(&Token::Algorithm)? {
                algorithm = Some(self.expect_ident_or_keyword()?);
            } else if order_by.is_none() && self.consume(&Token::Order)? {
                order_by = Some(self.parse_graph_order_by_tail()?);
            } else if limit.is_none() && self.consume(&Token::Limit)? {
                // `parse_integer` already rejects a leading `-` at the
                // integer slot, so the value is always non-negative here.
                let n = self.parse_integer()?;
                limit = Some(n as u32);
            } else {
                break;
            }
        }
        Ok(QueryExpr::GraphCommand(GraphCommand::Centrality {
            algorithm: algorithm.unwrap_or_else(|| "degree".to_string()),
            limit,
            order_by,
        }))
    }

    /// Parse: GRAPH COMMUNITY [ALGORITHM label_propagation|louvain] [MAX_ITERATIONS n] [ORDER BY metric [ASC|DESC]] [LIMIT n] [RETURN ASSIGNMENTS]
    fn parse_graph_community(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume COMMUNITY
        let mut algorithm: Option<String> = None;
        let mut max_iterations: Option<u32> = None;
        let mut limit: Option<u32> = None;
        let mut order_by: Option<GraphCommandOrderBy> = None;
        let mut return_assignments = false;
        loop {
            if algorithm.is_none() && self.consume(&Token::Algorithm)? {
                algorithm = Some(self.expect_ident_or_keyword()?);
            } else if max_iterations.is_none() && self.consume(&Token::MaxIterations)? {
                max_iterations = Some(self.parse_integer()? as u32);
            } else if order_by.is_none() && self.consume(&Token::Order)? {
                order_by = Some(self.parse_graph_order_by_tail()?);
            } else if limit.is_none() && self.consume(&Token::Limit)? {
                let n = self.parse_integer()?;
                limit = Some(n as u32);
            } else if !return_assignments && self.consume(&Token::Return)? {
                // RETURN ASSIGNMENTS — emit per-node node→community rows (#660)
                let target = self.expect_ident_or_keyword()?;
                if !target.eq_ignore_ascii_case("assignments") {
                    return Err(ParseError::expected(
                        vec!["ASSIGNMENTS"],
                        self.peek(),
                        self.position(),
                    ));
                }
                return_assignments = true;
            } else {
                break;
            }
        }
        Ok(QueryExpr::GraphCommand(GraphCommand::Community {
            algorithm: algorithm.unwrap_or_else(|| "label_propagation".to_string()),
            max_iterations: max_iterations.unwrap_or(100),
            limit,
            order_by,
            return_assignments,
        }))
    }

    /// Parse: GRAPH COMPONENTS [MODE connected|weak|strong] [ORDER BY metric [ASC|DESC]] [LIMIT n]
    fn parse_graph_components(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume COMPONENTS
        let mut mode: Option<String> = None;
        let mut limit: Option<u32> = None;
        let mut order_by: Option<GraphCommandOrderBy> = None;
        loop {
            if mode.is_none() && self.consume(&Token::Mode)? {
                mode = Some(self.expect_ident_or_keyword()?);
            } else if order_by.is_none() && self.consume(&Token::Order)? {
                order_by = Some(self.parse_graph_order_by_tail()?);
            } else if limit.is_none() && self.consume(&Token::Limit)? {
                let n = self.parse_integer()?;
                limit = Some(n as u32);
            } else {
                break;
            }
        }
        Ok(QueryExpr::GraphCommand(GraphCommand::Components {
            mode: mode.unwrap_or_else(|| "connected".to_string()),
            limit,
            order_by,
        }))
    }

    fn parse_graph_order_by_tail(&mut self) -> Result<GraphCommandOrderBy, ParseError> {
        self.expect(Token::By)?;
        let metric = self.expect_ident_or_keyword()?;
        let ascending = if self.consume(&Token::Desc)? {
            false
        } else {
            self.consume(&Token::Asc)?;
            true
        };
        Ok(GraphCommandOrderBy { metric, ascending })
    }

    /// Parse: GRAPH CYCLES [MAX_LENGTH n]
    fn parse_graph_cycles(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume CYCLES
        let max_length = if self.consume(&Token::MaxLength)? {
            self.parse_integer()? as u32
        } else {
            10
        };
        Ok(QueryExpr::GraphCommand(GraphCommand::Cycles { max_length }))
    }

    /// Parse: GRAPH CLUSTERING
    fn parse_graph_clustering(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume CLUSTERING
        Ok(QueryExpr::GraphCommand(GraphCommand::Clustering))
    }

    /// Parse: GRAPH TOPOLOGICAL_SORT
    fn parse_graph_topological_sort(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume TOPOLOGICAL_SORT
        Ok(QueryExpr::GraphCommand(GraphCommand::TopologicalSort))
    }

    /// Parse: GRAPH PROPERTIES ['<id-or-label>']
    fn parse_graph_properties(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume PROPERTIES
        let source = if matches!(self.peek(), Token::String(_)) {
            Some(self.parse_string()?)
        } else {
            None
        };
        Ok(QueryExpr::GraphCommand(GraphCommand::Properties { source }))
    }
}
