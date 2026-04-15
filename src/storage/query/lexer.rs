//! RQL Lexer
//!
//! Tokenizes RQL (RedDB Query Language) strings for parsing.
//! Supports both SQL-like table queries and Cypher-like graph patterns.
//!
//! # Token Types
//!
//! - Keywords: SELECT, FROM, WHERE, MATCH, RETURN, JOIN, GRAPH, PATH, etc.
//! - Literals: strings, integers, floats, booleans
//! - Identifiers: table names, column names, aliases
//! - Operators: comparison, arithmetic, logical
//! - Graph syntax: arrows (->), edge brackets ([-])

use std::fmt;
use std::iter::Peekable;
use std::str::Chars;

/// Token types for RQL
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Keywords
    Select,
    From,
    Where,
    And,
    Or,
    Not,
    Match,
    Return,
    Join,
    Graph,
    Path,
    To,
    Via,
    On,
    As,
    Is,
    Null,
    Between,
    Like,
    In,
    Order,
    By,
    Asc,
    Desc,
    Nulls,
    First,
    Last,
    Limit,
    Offset,
    Inner,
    Left,
    Right,
    Outer,
    Full,
    Cross,
    Starts,
    Ends,
    With,
    Contains,
    True,
    False,
    Enrich,
    Group,
    Count,
    Sum,
    Avg,
    Min,
    Max,
    Distinct,

    // Vector query keywords
    Vector,
    Search,
    Similar,
    Collection,
    Metric,
    Threshold,
    K,
    Hybrid,
    Fusion,
    Rerank,
    Rrf,
    Intersection,
    Union,
    Recursive,
    All,
    Weight,
    L2,
    Cosine,
    InnerProduct,
    Include,
    Metadata,
    Vectors,

    // DML/DDL keywords
    Insert,
    Into,
    Values,
    Update,
    Set,
    Delete,
    Create,
    Table,
    Drop,
    Alter,
    Add,
    Column,
    Primary,
    // EXPLAIN ALTER FOR — schema diff command
    Explain,
    For,
    Format,
    Json,
    Key,
    Default,
    Compress,
    Index,
    Unique,
    If,
    Exists,
    Returning,
    Cascade,
    Rename,
    Using,

    // Entity type keywords
    Node,
    Edge,
    Document,
    Kv,

    // Time-series & Queue keywords
    Timeseries,
    Retention,
    Queue,
    Push,
    Pop,
    Peek,
    Purge,
    Ack,
    Nack,
    Priority,

    // Graph command keywords
    Neighborhood,
    ShortestPath,
    Centrality,
    Community,
    Components,
    Cycles,
    Traverse,
    Depth,
    Direction,
    Algorithm,
    Strategy,
    MaxIterations,
    MaxLength,
    Mode,
    Clustering,
    TopologicalSort,
    Properties,
    Text,
    Fuzzy,
    MinScore,

    // Literals
    String(String),
    Integer(i64),
    Float(f64),

    // Identifiers
    Ident(String),

    // Operators
    Eq,      // =
    Ne,      // <> or !=
    Lt,      // <
    Le,      // <=
    Gt,      // >
    Ge,      // >=
    Plus,    // +
    Minus,   // -
    Star,    // *
    Slash,   // /
    Percent, // %

    // Delimiters
    LParen,   // (
    RParen,   // )
    LBracket, // [
    RBracket, // ]
    LBrace,   // {
    RBrace,   // }
    Comma,    // ,
    Dot,      // .
    Colon,    // :
    Semi,     // ;

    // Graph syntax
    Arrow,      // ->
    ArrowLeft,  // <-
    Dash,       // -
    DotDot,     // ..
    Pipe,       // |
    DoublePipe, // ||

    // End of input
    Eof,
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Token::Select => write!(f, "SELECT"),
            Token::From => write!(f, "FROM"),
            Token::Where => write!(f, "WHERE"),
            Token::And => write!(f, "AND"),
            Token::Or => write!(f, "OR"),
            Token::Not => write!(f, "NOT"),
            Token::Match => write!(f, "MATCH"),
            Token::Return => write!(f, "RETURN"),
            Token::Join => write!(f, "JOIN"),
            Token::Graph => write!(f, "GRAPH"),
            Token::Path => write!(f, "PATH"),
            Token::To => write!(f, "TO"),
            Token::Via => write!(f, "VIA"),
            Token::On => write!(f, "ON"),
            Token::As => write!(f, "AS"),
            Token::Is => write!(f, "IS"),
            Token::Null => write!(f, "NULL"),
            Token::Between => write!(f, "BETWEEN"),
            Token::Like => write!(f, "LIKE"),
            Token::In => write!(f, "IN"),
            Token::Order => write!(f, "ORDER"),
            Token::By => write!(f, "BY"),
            Token::Asc => write!(f, "ASC"),
            Token::Desc => write!(f, "DESC"),
            Token::Nulls => write!(f, "NULLS"),
            Token::First => write!(f, "FIRST"),
            Token::Last => write!(f, "LAST"),
            Token::Limit => write!(f, "LIMIT"),
            Token::Offset => write!(f, "OFFSET"),
            Token::Inner => write!(f, "INNER"),
            Token::Left => write!(f, "LEFT"),
            Token::Right => write!(f, "RIGHT"),
            Token::Outer => write!(f, "OUTER"),
            Token::Full => write!(f, "FULL"),
            Token::Cross => write!(f, "CROSS"),
            Token::Starts => write!(f, "STARTS"),
            Token::Ends => write!(f, "ENDS"),
            Token::With => write!(f, "WITH"),
            Token::Contains => write!(f, "CONTAINS"),
            Token::True => write!(f, "TRUE"),
            Token::False => write!(f, "FALSE"),
            Token::Enrich => write!(f, "ENRICH"),
            Token::Group => write!(f, "GROUP"),
            Token::Count => write!(f, "COUNT"),
            Token::Sum => write!(f, "SUM"),
            Token::Avg => write!(f, "AVG"),
            Token::Min => write!(f, "MIN"),
            Token::Max => write!(f, "MAX"),
            Token::Distinct => write!(f, "DISTINCT"),
            Token::Vector => write!(f, "VECTOR"),
            Token::Search => write!(f, "SEARCH"),
            Token::Similar => write!(f, "SIMILAR"),
            Token::Collection => write!(f, "COLLECTION"),
            Token::Metric => write!(f, "METRIC"),
            Token::Threshold => write!(f, "THRESHOLD"),
            Token::K => write!(f, "K"),
            Token::Hybrid => write!(f, "HYBRID"),
            Token::Fusion => write!(f, "FUSION"),
            Token::Rerank => write!(f, "RERANK"),
            Token::Rrf => write!(f, "RRF"),
            Token::Intersection => write!(f, "INTERSECTION"),
            Token::Union => write!(f, "UNION"),
            Token::Recursive => write!(f, "RECURSIVE"),
            Token::All => write!(f, "ALL"),
            Token::Weight => write!(f, "WEIGHT"),
            Token::L2 => write!(f, "L2"),
            Token::Cosine => write!(f, "COSINE"),
            Token::InnerProduct => write!(f, "INNER_PRODUCT"),
            Token::Include => write!(f, "INCLUDE"),
            Token::Metadata => write!(f, "METADATA"),
            Token::Vectors => write!(f, "VECTORS"),
            Token::Explain => write!(f, "EXPLAIN"),
            Token::For => write!(f, "FOR"),
            Token::Format => write!(f, "FORMAT"),
            Token::Json => write!(f, "JSON"),
            Token::Insert => write!(f, "INSERT"),
            Token::Into => write!(f, "INTO"),
            Token::Values => write!(f, "VALUES"),
            Token::Update => write!(f, "UPDATE"),
            Token::Set => write!(f, "SET"),
            Token::Delete => write!(f, "DELETE"),
            Token::Create => write!(f, "CREATE"),
            Token::Table => write!(f, "TABLE"),
            Token::Drop => write!(f, "DROP"),
            Token::Alter => write!(f, "ALTER"),
            Token::Add => write!(f, "ADD"),
            Token::Column => write!(f, "COLUMN"),
            Token::Primary => write!(f, "PRIMARY"),
            Token::Key => write!(f, "KEY"),
            Token::Default => write!(f, "DEFAULT"),
            Token::Compress => write!(f, "COMPRESS"),
            Token::Index => write!(f, "INDEX"),
            Token::Unique => write!(f, "UNIQUE"),
            Token::If => write!(f, "IF"),
            Token::Exists => write!(f, "EXISTS"),
            Token::Returning => write!(f, "RETURNING"),
            Token::Cascade => write!(f, "CASCADE"),
            Token::Rename => write!(f, "RENAME"),
            Token::Using => write!(f, "USING"),
            Token::Node => write!(f, "NODE"),
            Token::Edge => write!(f, "EDGE"),
            Token::Document => write!(f, "DOCUMENT"),
            Token::Kv => write!(f, "KV"),
            Token::Timeseries => write!(f, "TIMESERIES"),
            Token::Retention => write!(f, "RETENTION"),
            Token::Queue => write!(f, "QUEUE"),
            Token::Push => write!(f, "PUSH"),
            Token::Pop => write!(f, "POP"),
            Token::Peek => write!(f, "PEEK"),
            Token::Purge => write!(f, "PURGE"),
            Token::Ack => write!(f, "ACK"),
            Token::Nack => write!(f, "NACK"),
            Token::Priority => write!(f, "PRIORITY"),
            Token::Neighborhood => write!(f, "NEIGHBORHOOD"),
            Token::ShortestPath => write!(f, "SHORTEST_PATH"),
            Token::Centrality => write!(f, "CENTRALITY"),
            Token::Community => write!(f, "COMMUNITY"),
            Token::Components => write!(f, "COMPONENTS"),
            Token::Cycles => write!(f, "CYCLES"),
            Token::Traverse => write!(f, "TRAVERSE"),
            Token::Depth => write!(f, "DEPTH"),
            Token::Direction => write!(f, "DIRECTION"),
            Token::Algorithm => write!(f, "ALGORITHM"),
            Token::Strategy => write!(f, "STRATEGY"),
            Token::MaxIterations => write!(f, "MAX_ITERATIONS"),
            Token::MaxLength => write!(f, "MAX_LENGTH"),
            Token::Mode => write!(f, "MODE"),
            Token::Clustering => write!(f, "CLUSTERING"),
            Token::TopologicalSort => write!(f, "TOPOLOGICAL_SORT"),
            Token::Properties => write!(f, "PROPERTIES"),
            Token::Text => write!(f, "TEXT"),
            Token::Fuzzy => write!(f, "FUZZY"),
            Token::MinScore => write!(f, "MIN_SCORE"),
            Token::String(s) => write!(f, "'{}'", s),
            Token::Integer(n) => write!(f, "{}", n),
            Token::Float(n) => write!(f, "{}", n),
            Token::Ident(s) => write!(f, "{}", s),
            Token::Eq => write!(f, "="),
            Token::Ne => write!(f, "<>"),
            Token::Lt => write!(f, "<"),
            Token::Le => write!(f, "<="),
            Token::Gt => write!(f, ">"),
            Token::Ge => write!(f, ">="),
            Token::Plus => write!(f, "+"),
            Token::Minus => write!(f, "-"),
            Token::Star => write!(f, "*"),
            Token::Slash => write!(f, "/"),
            Token::Percent => write!(f, "%"),
            Token::LParen => write!(f, "("),
            Token::RParen => write!(f, ")"),
            Token::LBracket => write!(f, "["),
            Token::RBracket => write!(f, "]"),
            Token::LBrace => write!(f, "{{"),
            Token::RBrace => write!(f, "}}"),
            Token::Comma => write!(f, ","),
            Token::Dot => write!(f, "."),
            Token::Colon => write!(f, ":"),
            Token::Semi => write!(f, ";"),
            Token::Arrow => write!(f, "->"),
            Token::ArrowLeft => write!(f, "<-"),
            Token::Dash => write!(f, "-"),
            Token::DotDot => write!(f, ".."),
            Token::Pipe => write!(f, "|"),
            Token::DoublePipe => write!(f, "||"),
            Token::Eof => write!(f, "EOF"),
        }
    }
}

/// Position in source code
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Position {
    /// Line number (1-indexed)
    pub line: u32,
    /// Column number (1-indexed)
    pub column: u32,
    /// Byte offset from start
    pub offset: u32,
}

impl Position {
    /// Create a new position
    pub fn new(line: u32, column: u32, offset: u32) -> Self {
        Self {
            line,
            column,
            offset,
        }
    }
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

/// A token with its position in source
#[derive(Debug, Clone)]
pub struct Spanned {
    /// The token
    pub token: Token,
    /// Start position
    pub start: Position,
    /// End position
    pub end: Position,
}

impl Spanned {
    /// Create a new spanned token
    pub fn new(token: Token, start: Position, end: Position) -> Self {
        Self { token, start, end }
    }
}

/// Lexer error
#[derive(Debug, Clone)]
pub struct LexerError {
    /// Error message
    pub message: String,
    /// Position where error occurred
    pub position: Position,
}

impl LexerError {
    /// Create a new lexer error
    pub fn new(message: impl Into<String>, position: Position) -> Self {
        Self {
            message: message.into(),
            position,
        }
    }
}

impl fmt::Display for LexerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Lexer error at {}: {}", self.position, self.message)
    }
}

impl std::error::Error for LexerError {}

/// RQL Lexer
pub struct Lexer<'a> {
    /// Input characters
    chars: Peekable<Chars<'a>>,
    /// Current position
    line: u32,
    column: u32,
    offset: u32,
    /// Peeked token
    peeked: Option<Spanned>,
    /// Put-back buffer for characters we need to "unconsume"
    putback: Option<(char, Position)>,
}

impl<'a> Lexer<'a> {
    /// Create a new lexer for the given input
    pub fn new(input: &'a str) -> Self {
        Self {
            chars: input.chars().peekable(),
            line: 1,
            column: 1,
            offset: 0,
            peeked: None,
            putback: None,
        }
    }

    /// Get current position
    fn position(&self) -> Position {
        Position::new(self.line, self.column, self.offset)
    }

    /// Put a character back into the stream
    fn unget(&mut self, ch: char, pos: Position) {
        self.putback = Some((ch, pos));
    }

    /// Advance and get next character
    fn advance(&mut self) -> Option<char> {
        // Check putback buffer first
        if let Some((ch, pos)) = self.putback.take() {
            // When we re-consume from putback, update position to after the char
            self.line = pos.line;
            self.column = pos.column + 1;
            self.offset = pos.offset + ch.len_utf8() as u32;
            return Some(ch);
        }

        let ch = self.chars.next()?;
        self.offset += ch.len_utf8() as u32;
        if ch == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(ch)
    }

    /// Peek at next character
    fn peek(&mut self) -> Option<char> {
        // Check putback buffer first
        if let Some((ch, _)) = &self.putback {
            return Some(*ch);
        }
        self.chars.peek().copied()
    }

    /// Skip whitespace and comments
    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() {
                self.advance();
            } else if ch == '-' {
                // Could be comment (--) or operator
                let pos = self.position();
                self.advance();
                if self.peek() == Some('-') {
                    // Line comment
                    self.advance();
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.advance();
                    }
                } else {
                    // Not a comment, put back - by restoring state
                    // Since we can't put back, we'll handle this in next_token
                    self.line = pos.line;
                    self.column = pos.column;
                    self.offset = pos.offset;
                    // Need to reset chars iterator - this is tricky
                    // Instead, we'll handle -- in scan_operator
                    break;
                }
            } else {
                break;
            }
        }
    }

    /// Peek at the next token without consuming it
    pub fn peek_token(&mut self) -> Result<&Spanned, LexerError> {
        if self.peeked.is_none() {
            self.peeked = Some(self.next_token_internal()?);
        }
        Ok(self.peeked.as_ref().unwrap())
    }

    /// Get the next token
    pub fn next_token(&mut self) -> Result<Spanned, LexerError> {
        if let Some(tok) = self.peeked.take() {
            return Ok(tok);
        }
        self.next_token_internal()
    }

    /// Internal implementation of next_token
    fn next_token_internal(&mut self) -> Result<Spanned, LexerError> {
        self.skip_whitespace_simple();

        let start = self.position();

        let ch = match self.peek() {
            Some(c) => c,
            None => {
                return Ok(Spanned::new(Token::Eof, start, start));
            }
        };

        // Dispatch based on first character
        let token = match ch {
            // String literals
            '\'' | '"' => self.scan_string()?,

            // Numbers
            '0'..='9' => self.scan_number()?,

            // Identifiers and keywords
            'a'..='z' | 'A'..='Z' | '_' => self.scan_identifier()?,

            // Operators and delimiters
            '=' => {
                self.advance();
                Token::Eq
            }
            '<' => self.scan_less_than()?,
            '>' => self.scan_greater_than()?,
            '!' => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    Token::Ne
                } else {
                    return Err(LexerError::new("Expected '=' after '!'", start));
                }
            }
            '+' => {
                self.advance();
                Token::Plus
            }
            '-' => self.scan_minus()?,
            '*' => {
                self.advance();
                Token::Star
            }
            '/' => {
                self.advance();
                Token::Slash
            }
            '%' => {
                self.advance();
                Token::Percent
            }
            '(' => {
                self.advance();
                Token::LParen
            }
            ')' => {
                self.advance();
                Token::RParen
            }
            '[' => {
                self.advance();
                Token::LBracket
            }
            ']' => {
                self.advance();
                Token::RBracket
            }
            '{' => {
                self.advance();
                Token::LBrace
            }
            '}' => {
                self.advance();
                Token::RBrace
            }
            ',' => {
                self.advance();
                Token::Comma
            }
            '.' => self.scan_dot()?,
            ':' => {
                self.advance();
                Token::Colon
            }
            ';' => {
                self.advance();
                Token::Semi
            }
            '|' => {
                self.advance();
                if self.peek() == Some('|') {
                    self.advance();
                    Token::DoublePipe
                } else {
                    Token::Pipe
                }
            }
            _ => {
                return Err(LexerError::new(
                    format!("Unexpected character: '{}'", ch),
                    start,
                ));
            }
        };

        let end = self.position();
        Ok(Spanned::new(token, start, end))
    }

    /// Simple whitespace skip (no comment handling to avoid complexity)
    fn skip_whitespace_simple(&mut self) {
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() {
                self.advance();
            } else {
                break;
            }
        }
    }

    /// Scan a string literal
    fn scan_string(&mut self) -> Result<Token, LexerError> {
        let quote = self.advance().unwrap(); // ' or "
        let start = self.position();
        let mut value = String::new();

        loop {
            match self.peek() {
                None => {
                    return Err(LexerError::new("Unterminated string", start));
                }
                Some(c) if c == quote => {
                    self.advance();
                    // Check for escaped quote ('')
                    if self.peek() == Some(quote) {
                        self.advance();
                        value.push(quote);
                    } else {
                        break;
                    }
                }
                Some('\\') => {
                    self.advance();
                    match self.peek() {
                        Some('n') => {
                            self.advance();
                            value.push('\n');
                        }
                        Some('r') => {
                            self.advance();
                            value.push('\r');
                        }
                        Some('t') => {
                            self.advance();
                            value.push('\t');
                        }
                        Some('\\') => {
                            self.advance();
                            value.push('\\');
                        }
                        Some(c) if c == quote => {
                            self.advance();
                            value.push(quote);
                        }
                        Some(c) => {
                            // Unknown escape, keep as-is
                            value.push('\\');
                            value.push(c);
                            self.advance();
                        }
                        None => {
                            return Err(LexerError::new("Unterminated string", start));
                        }
                    }
                }
                Some(c) => {
                    self.advance();
                    value.push(c);
                }
            }
        }

        Ok(Token::String(value))
    }

    /// Scan a number (integer or float)
    fn scan_number(&mut self) -> Result<Token, LexerError> {
        let mut value = String::new();
        let mut is_float = false;

        // Integer part
        while let Some(ch) = self.peek() {
            if ch.is_ascii_digit() {
                value.push(ch);
                self.advance();
            } else {
                break;
            }
        }

        // Check for decimal point
        if self.peek() == Some('.') {
            // Look ahead to distinguish from .. and method calls
            let dot_pos = self.position();
            self.advance(); // consume the first '.'

            if self.peek() == Some('.') {
                // It's ".." - put back the first dot using unget
                self.unget('.', dot_pos);
                // Return integer without the dot
            } else if self.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                is_float = true;
                value.push('.');
                while let Some(ch) = self.peek() {
                    if ch.is_ascii_digit() {
                        value.push(ch);
                        self.advance();
                    } else {
                        break;
                    }
                }
            } else {
                // Just a dot after number (like `x.method`), put it back
                self.unget('.', dot_pos);
            }
        }

        // Check for exponent
        if self.peek() == Some('e') || self.peek() == Some('E') {
            is_float = true;
            value.push(self.advance().unwrap());

            if self.peek() == Some('+') || self.peek() == Some('-') {
                value.push(self.advance().unwrap());
            }

            while let Some(ch) = self.peek() {
                if ch.is_ascii_digit() {
                    value.push(ch);
                    self.advance();
                } else {
                    break;
                }
            }
        }

        if is_float {
            match value.parse::<f64>() {
                Ok(n) => Ok(Token::Float(n)),
                Err(_) => Err(LexerError::new(
                    format!("Invalid float: {}", value),
                    self.position(),
                )),
            }
        } else {
            match value.parse::<i64>() {
                Ok(n) => Ok(Token::Integer(n)),
                Err(_) => Err(LexerError::new(
                    format!("Invalid integer: {}", value),
                    self.position(),
                )),
            }
        }
    }

    /// Scan an identifier or keyword
    fn scan_identifier(&mut self) -> Result<Token, LexerError> {
        let mut value = String::new();

        while let Some(ch) = self.peek() {
            if ch.is_alphanumeric() || ch == '_' {
                value.push(ch);
                self.advance();
            } else {
                break;
            }
        }

        // Check for keywords (case-insensitive)
        let token = match value.to_uppercase().as_str() {
            "SELECT" => Token::Select,
            "FROM" => Token::From,
            "WHERE" => Token::Where,
            "AND" => Token::And,
            "OR" => Token::Or,
            "NOT" => Token::Not,
            "MATCH" => Token::Match,
            "RETURN" => Token::Return,
            "JOIN" => Token::Join,
            "GRAPH" => Token::Graph,
            "PATH" => Token::Path,
            "TO" => Token::To,
            "VIA" => Token::Via,
            "ON" => Token::On,
            "AS" => Token::As,
            "IS" => Token::Is,
            "NULL" => Token::Null,
            "BETWEEN" => Token::Between,
            "LIKE" => Token::Like,
            "IN" => Token::In,
            "ORDER" => Token::Order,
            "BY" => Token::By,
            "ASC" => Token::Asc,
            "DESC" => Token::Desc,
            "NULLS" => Token::Nulls,
            "FIRST" => Token::First,
            "LAST" => Token::Last,
            "LIMIT" => Token::Limit,
            "OFFSET" => Token::Offset,
            "INNER" => Token::Inner,
            "LEFT" => Token::Left,
            "RIGHT" => Token::Right,
            "OUTER" => Token::Outer,
            "FULL" => Token::Full,
            "CROSS" => Token::Cross,
            "STARTS" => Token::Starts,
            "ENDS" => Token::Ends,
            "WITH" => Token::With,
            "CONTAINS" => Token::Contains,
            "TRUE" => Token::True,
            "FALSE" => Token::False,
            "ENRICH" => Token::Enrich,
            "GROUP" => Token::Group,
            "COUNT" => Token::Count,
            "SUM" => Token::Sum,
            "AVG" => Token::Avg,
            "MIN" => Token::Min,
            "MAX" => Token::Max,
            "DISTINCT" => Token::Distinct,
            "VECTOR" => Token::Vector,
            "SEARCH" => Token::Search,
            "SIMILAR" => Token::Similar,
            "COLLECTION" => Token::Collection,
            "METRIC" => Token::Metric,
            "THRESHOLD" => Token::Threshold,
            "K" => Token::K,
            "HYBRID" => Token::Hybrid,
            "FUSION" => Token::Fusion,
            "RERANK" => Token::Rerank,
            "RRF" => Token::Rrf,
            "INTERSECTION" => Token::Intersection,
            "UNION" => Token::Union,
            "RECURSIVE" => Token::Recursive,
            "ALL" => Token::All,
            "WEIGHT" => Token::Weight,
            "L2" => Token::L2,
            "COSINE" => Token::Cosine,
            "INNER_PRODUCT" | "INNERPRODUCT" => Token::InnerProduct,
            "INCLUDE" => Token::Include,
            "METADATA" => Token::Metadata,
            "VECTORS" => Token::Vectors,
            "EXPLAIN" => Token::Explain,
            "FOR" => Token::For,
            "FORMAT" => Token::Format,
            "JSON" => Token::Json,
            "INSERT" => Token::Insert,
            "INTO" => Token::Into,
            "VALUES" => Token::Values,
            "UPDATE" => Token::Update,
            "SET" => Token::Set,
            "DELETE" => Token::Delete,
            "CREATE" => Token::Create,
            "TABLE" => Token::Table,
            "DROP" => Token::Drop,
            "ALTER" => Token::Alter,
            "ADD" => Token::Add,
            "COLUMN" => Token::Column,
            "PRIMARY" => Token::Primary,
            "KEY" => Token::Key,
            "DEFAULT" => Token::Default,
            "COMPRESS" => Token::Compress,
            "INDEX" => Token::Index,
            "UNIQUE" => Token::Unique,
            "IF" => Token::If,
            "EXISTS" => Token::Exists,
            "RETURNING" => Token::Returning,
            "CASCADE" => Token::Cascade,
            "RENAME" => Token::Rename,
            "USING" => Token::Using,
            "NODE" => Token::Node,
            "EDGE" => Token::Edge,
            "DOCUMENT" => Token::Document,
            "KV" => Token::Kv,
            "TIMESERIES" => Token::Timeseries,
            "RETENTION" => Token::Retention,
            "QUEUE" => Token::Queue,
            "PUSH" => Token::Push,
            "POP" => Token::Pop,
            "PEEK" => Token::Peek,
            "PURGE" => Token::Purge,
            "ACK" => Token::Ack,
            "NACK" => Token::Nack,
            "PRIORITY" => Token::Priority,
            "LPUSH" => Token::Ident("LPUSH".to_string()),
            "RPUSH" => Token::Ident("RPUSH".to_string()),
            "LPOP" => Token::Ident("LPOP".to_string()),
            "RPOP" => Token::Ident("RPOP".to_string()),
            "NEIGHBORHOOD" => Token::Neighborhood,
            "SHORTEST_PATH" | "SHORTESTPATH" => Token::ShortestPath,
            "CENTRALITY" => Token::Centrality,
            "COMMUNITY" => Token::Community,
            "COMPONENTS" => Token::Components,
            "CYCLES" => Token::Cycles,
            "TRAVERSE" => Token::Traverse,
            "DEPTH" => Token::Depth,
            "DIRECTION" => Token::Direction,
            "ALGORITHM" => Token::Algorithm,
            "STRATEGY" => Token::Strategy,
            "MAX_ITERATIONS" | "MAXITERATIONS" => Token::MaxIterations,
            "MAX_LENGTH" | "MAXLENGTH" => Token::MaxLength,
            "MODE" => Token::Mode,
            "CLUSTERING" => Token::Clustering,
            "TOPOLOGICAL_SORT" | "TOPOLOGICALSORT" => Token::TopologicalSort,
            "PROPERTIES" => Token::Properties,
            "TEXT" => Token::Text,
            "FUZZY" => Token::Fuzzy,
            "MIN_SCORE" | "MINSCORE" => Token::MinScore,
            _ => Token::Ident(value),
        };

        Ok(token)
    }

    /// Scan less-than variants: <, <=, <>, <-
    fn scan_less_than(&mut self) -> Result<Token, LexerError> {
        self.advance(); // consume '<'
        match self.peek() {
            Some('=') => {
                self.advance();
                Ok(Token::Le)
            }
            Some('>') => {
                self.advance();
                Ok(Token::Ne)
            }
            Some('-') => {
                self.advance();
                Ok(Token::ArrowLeft)
            }
            _ => Ok(Token::Lt),
        }
    }

    /// Scan greater-than variants: >, >=
    fn scan_greater_than(&mut self) -> Result<Token, LexerError> {
        self.advance(); // consume '>'
        if self.peek() == Some('=') {
            self.advance();
            Ok(Token::Ge)
        } else {
            Ok(Token::Gt)
        }
    }

    /// Scan minus variants: -, ->, --comment
    fn scan_minus(&mut self) -> Result<Token, LexerError> {
        self.advance(); // consume '-'
        match self.peek() {
            Some('>') => {
                self.advance();
                Ok(Token::Arrow)
            }
            Some('-') => {
                // Line comment, skip to end of line
                self.advance();
                while let Some(c) = self.peek() {
                    if c == '\n' {
                        break;
                    }
                    self.advance();
                }
                // Recursively get next token
                self.skip_whitespace_simple();
                if self.peek().is_none() {
                    Ok(Token::Eof)
                } else {
                    let next = self.next_token_internal()?;
                    Ok(next.token)
                }
            }
            _ => Ok(Token::Dash),
        }
    }

    /// Scan dot variants: ., ..
    fn scan_dot(&mut self) -> Result<Token, LexerError> {
        self.advance(); // consume '.'
        if self.peek() == Some('.') {
            self.advance();
            Ok(Token::DotDot)
        } else {
            Ok(Token::Dot)
        }
    }

    /// Tokenize entire input
    pub fn tokenize(&mut self) -> Result<Vec<Spanned>, LexerError> {
        let mut tokens = Vec::new();
        loop {
            let tok = self.next_token()?;
            let is_eof = tok.token == Token::Eof;
            tokens.push(tok);
            if is_eof {
                break;
            }
        }
        Ok(tokens)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn tokenize(input: &str) -> Vec<Token> {
        let mut lexer = Lexer::new(input);
        lexer
            .tokenize()
            .unwrap()
            .into_iter()
            .map(|s| s.token)
            .collect()
    }

    #[test]
    fn test_keywords() {
        let tokens = tokenize("SELECT FROM WHERE AND OR NOT");
        assert_eq!(
            tokens,
            vec![
                Token::Select,
                Token::From,
                Token::Where,
                Token::And,
                Token::Or,
                Token::Not,
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_identifiers() {
        let tokens = tokenize("hosts users ip_address");
        assert_eq!(
            tokens,
            vec![
                Token::Ident("hosts".into()),
                Token::Ident("users".into()),
                Token::Ident("ip_address".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_numbers() {
        let tokens = tokenize("42 2.5 1e10 2.5e-3");
        assert_eq!(
            tokens,
            vec![
                Token::Integer(42),
                Token::Float(2.5),
                Token::Float(1e10),
                Token::Float(2.5e-3),
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_strings() {
        let tokens = tokenize("'hello' \"world\" 'it''s'");
        assert_eq!(
            tokens,
            vec![
                Token::String("hello".into()),
                Token::String("world".into()),
                Token::String("it's".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_operators() {
        let tokens = tokenize("= <> < <= > >= != + - * /");
        assert_eq!(
            tokens,
            vec![
                Token::Eq,
                Token::Ne,
                Token::Lt,
                Token::Le,
                Token::Gt,
                Token::Ge,
                Token::Ne,
                Token::Plus,
                Token::Dash,
                Token::Star,
                Token::Slash,
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_delimiters() {
        let tokens = tokenize("( ) [ ] { } , . : ;");
        assert_eq!(
            tokens,
            vec![
                Token::LParen,
                Token::RParen,
                Token::LBracket,
                Token::RBracket,
                Token::LBrace,
                Token::RBrace,
                Token::Comma,
                Token::Dot,
                Token::Colon,
                Token::Semi,
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_graph_syntax() {
        let tokens = tokenize("-> <- - ..");
        assert_eq!(
            tokens,
            vec![
                Token::Arrow,
                Token::ArrowLeft,
                Token::Dash,
                Token::DotDot,
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_table_query() {
        let tokens = tokenize("SELECT ip, hostname FROM hosts WHERE os = 'Linux' LIMIT 10");
        assert_eq!(
            tokens,
            vec![
                Token::Select,
                Token::Ident("ip".into()),
                Token::Comma,
                Token::Ident("hostname".into()),
                Token::From,
                Token::Ident("hosts".into()),
                Token::Where,
                Token::Ident("os".into()),
                Token::Eq,
                Token::String("Linux".into()),
                Token::Limit,
                Token::Integer(10),
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_graph_query() {
        let tokens = tokenize("MATCH (h:Host)-[:HAS_SERVICE]->(s:Service) RETURN h, s");
        assert_eq!(
            tokens,
            vec![
                Token::Match,
                Token::LParen,
                Token::Ident("h".into()),
                Token::Colon,
                Token::Ident("Host".into()),
                Token::RParen,
                Token::Dash,
                Token::LBracket,
                Token::Colon,
                Token::Ident("HAS_SERVICE".into()),
                Token::RBracket,
                Token::Arrow,
                Token::LParen,
                Token::Ident("s".into()),
                Token::Colon,
                Token::Ident("Service".into()),
                Token::RParen,
                Token::Return,
                Token::Ident("h".into()),
                Token::Comma,
                Token::Ident("s".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_join_query() {
        let tokens = tokenize("FROM hosts h JOIN GRAPH (h)-[:HAS_VULN]->(v) ON h.ip = v.id");
        assert_eq!(
            tokens,
            vec![
                Token::From,
                Token::Ident("hosts".into()),
                Token::Ident("h".into()),
                Token::Join,
                Token::Graph,
                Token::LParen,
                Token::Ident("h".into()),
                Token::RParen,
                Token::Dash,
                Token::LBracket,
                Token::Colon,
                Token::Ident("HAS_VULN".into()),
                Token::RBracket,
                Token::Arrow,
                Token::LParen,
                Token::Ident("v".into()),
                Token::RParen,
                Token::On,
                Token::Ident("h".into()),
                Token::Dot,
                Token::Ident("ip".into()),
                Token::Eq,
                Token::Ident("v".into()),
                Token::Dot,
                Token::Ident("id".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_path_query() {
        let tokens = tokenize("PATH FROM host('192.168.1.1') TO host('10.0.0.1') VIA [:AUTH]");
        assert_eq!(
            tokens,
            vec![
                Token::Path,
                Token::From,
                Token::Ident("host".into()),
                Token::LParen,
                Token::String("192.168.1.1".into()),
                Token::RParen,
                Token::To,
                Token::Ident("host".into()),
                Token::LParen,
                Token::String("10.0.0.1".into()),
                Token::RParen,
                Token::Via,
                Token::LBracket,
                Token::Colon,
                Token::Ident("AUTH".into()),
                Token::RBracket,
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_variable_length_pattern() {
        let tokens = tokenize("(a)-[*1..5]->(b)");
        assert_eq!(
            tokens,
            vec![
                Token::LParen,
                Token::Ident("a".into()),
                Token::RParen,
                Token::Dash,
                Token::LBracket,
                Token::Star,
                Token::Integer(1),
                Token::DotDot,
                Token::Integer(5),
                Token::RBracket,
                Token::Arrow,
                Token::LParen,
                Token::Ident("b".into()),
                Token::RParen,
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_case_insensitive_keywords() {
        let tokens = tokenize("select FROM Where AND");
        assert_eq!(
            tokens,
            vec![
                Token::Select,
                Token::From,
                Token::Where,
                Token::And,
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_comments() {
        let tokens = tokenize("SELECT -- this is a comment\nip FROM hosts");
        assert_eq!(
            tokens,
            vec![
                Token::Select,
                Token::Ident("ip".into()),
                Token::From,
                Token::Ident("hosts".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn test_escaped_strings() {
        let tokens = tokenize(r"'hello\nworld' 'tab\there'");
        assert_eq!(
            tokens,
            vec![
                Token::String("hello\nworld".into()),
                Token::String("tab\there".into()),
                Token::Eof
            ]
        );
    }
}
