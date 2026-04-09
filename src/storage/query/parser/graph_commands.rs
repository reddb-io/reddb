//! Graph Command Parser: GRAPH NEIGHBORHOOD | SHORTEST_PATH | TRAVERSE | CENTRALITY | ...

use super::super::ast::{GraphCommand, QueryExpr};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;

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
                ],
                self.peek(),
                self.position(),
            )),
        }
    }

    /// Parse: GRAPH NEIGHBORHOOD 'source' [DEPTH n] [DIRECTION outgoing|incoming|both]
    fn parse_graph_neighborhood(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume NEIGHBORHOOD
        let source = self.parse_string()?;
        let depth = if self.consume(&Token::Depth)? {
            self.parse_integer()? as u32
        } else {
            3
        };
        let direction = if self.consume(&Token::Direction)? {
            self.expect_ident_or_keyword()?
        } else {
            "outgoing".to_string()
        };
        Ok(QueryExpr::GraphCommand(GraphCommand::Neighborhood {
            source,
            depth,
            direction,
        }))
    }

    /// Parse: GRAPH SHORTEST_PATH 'source' TO 'target' [ALGORITHM bfs|dijkstra] [DIRECTION dir]
    fn parse_graph_shortest_path(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume SHORTEST_PATH
        let source = self.parse_string()?;
        self.expect(Token::To)?;
        let target = self.parse_string()?;
        let algorithm = if self.consume(&Token::Algorithm)? {
            self.expect_ident_or_keyword()?
        } else {
            "bfs".to_string()
        };
        let direction = if self.consume(&Token::Direction)? {
            self.expect_ident_or_keyword()?
        } else {
            "outgoing".to_string()
        };
        Ok(QueryExpr::GraphCommand(GraphCommand::ShortestPath {
            source,
            target,
            algorithm,
            direction,
        }))
    }

    /// Parse: GRAPH TRAVERSE 'source' [STRATEGY bfs|dfs] [DEPTH n] [DIRECTION dir]
    fn parse_graph_traverse(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume TRAVERSE
        let source = self.parse_string()?;
        let strategy = if self.consume(&Token::Strategy)? {
            self.expect_ident_or_keyword()?
        } else {
            "bfs".to_string()
        };
        let depth = if self.consume(&Token::Depth)? {
            self.parse_integer()? as u32
        } else {
            5
        };
        let direction = if self.consume(&Token::Direction)? {
            self.expect_ident_or_keyword()?
        } else {
            "outgoing".to_string()
        };
        Ok(QueryExpr::GraphCommand(GraphCommand::Traverse {
            source,
            strategy,
            depth,
            direction,
        }))
    }

    /// Parse: GRAPH CENTRALITY [ALGORITHM degree|closeness|betweenness|eigenvector|pagerank]
    fn parse_graph_centrality(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume CENTRALITY
        let algorithm = if self.consume(&Token::Algorithm)? {
            self.expect_ident_or_keyword()?
        } else {
            "degree".to_string()
        };
        Ok(QueryExpr::GraphCommand(GraphCommand::Centrality {
            algorithm,
        }))
    }

    /// Parse: GRAPH COMMUNITY [ALGORITHM label_propagation|louvain] [MAX_ITERATIONS n]
    fn parse_graph_community(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume COMMUNITY
        let algorithm = if self.consume(&Token::Algorithm)? {
            self.expect_ident_or_keyword()?
        } else {
            "label_propagation".to_string()
        };
        let max_iterations = if self.consume(&Token::MaxIterations)? {
            self.parse_integer()? as u32
        } else {
            100
        };
        Ok(QueryExpr::GraphCommand(GraphCommand::Community {
            algorithm,
            max_iterations,
        }))
    }

    /// Parse: GRAPH COMPONENTS [MODE connected|weak|strong]
    fn parse_graph_components(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume COMPONENTS
        let mode = if self.consume(&Token::Mode)? {
            self.expect_ident_or_keyword()?
        } else {
            "connected".to_string()
        };
        Ok(QueryExpr::GraphCommand(GraphCommand::Components { mode }))
    }

    /// Parse: GRAPH CYCLES [MAX_LENGTH n]
    fn parse_graph_cycles(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume CYCLES
        let max_length = if self.consume(&Token::MaxLength)? {
            self.parse_integer()? as u32
        } else {
            10
        };
        Ok(QueryExpr::GraphCommand(GraphCommand::Cycles {
            max_length,
        }))
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
}
