//! DDL SQL Parser: CREATE TABLE, DROP TABLE, ALTER TABLE

use super::super::ast::{
    AlterOperation, AlterTableQuery, CreateColumnDef, CreateTableQuery, DropTableQuery, QueryExpr,
};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;

impl<'a> Parser<'a> {
    /// Parse: CREATE TABLE [IF NOT EXISTS] name (col1 TYPE [modifiers], ...)
    pub fn parse_create_table_query(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::Create)?;
        self.expect(Token::Table)?;

        let if_not_exists = self.match_if_not_exists()?;
        let name = self.expect_ident()?;

        self.expect(Token::LParen)?;
        let mut columns = Vec::new();
        loop {
            let col = self.parse_column_def()?;
            columns.push(col);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        self.expect(Token::RParen)?;

        Ok(QueryExpr::CreateTable(CreateTableQuery {
            name,
            columns,
            if_not_exists,
        }))
    }

    /// Parse: DROP TABLE [IF EXISTS] name
    pub fn parse_drop_table_query(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::Drop)?;
        self.expect(Token::Table)?;

        let if_exists = self.match_if_exists()?;
        let name = self.expect_ident()?;

        Ok(QueryExpr::DropTable(DropTableQuery { name, if_exists }))
    }

    /// Parse: ALTER TABLE name ADD/DROP/RENAME COLUMN ...
    pub fn parse_alter_table_query(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::Alter)?;
        self.expect(Token::Table)?;
        let name = self.expect_ident()?;

        let mut operations = Vec::new();
        loop {
            let op = self.parse_alter_operation()?;
            operations.push(op);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }

        Ok(QueryExpr::AlterTable(AlterTableQuery { name, operations }))
    }

    /// Parse a single ALTER TABLE operation
    fn parse_alter_operation(&mut self) -> Result<AlterOperation, ParseError> {
        if self.consume(&Token::Add)? {
            // ADD COLUMN definition
            let _ = self.consume(&Token::Column)?; // COLUMN keyword is optional
            let col_def = self.parse_column_def()?;
            Ok(AlterOperation::AddColumn(col_def))
        } else if self.consume(&Token::Drop)? {
            // DROP COLUMN name
            let _ = self.consume(&Token::Column)?; // COLUMN keyword is optional
            let col_name = self.expect_ident()?;
            Ok(AlterOperation::DropColumn(col_name))
        } else if self.consume(&Token::Rename)? {
            // RENAME COLUMN from TO to
            let _ = self.consume(&Token::Column)?; // COLUMN keyword is optional
            let from = self.expect_ident()?;
            self.expect(Token::To)?;
            let to = self.expect_ident()?;
            Ok(AlterOperation::RenameColumn { from, to })
        } else {
            Err(ParseError::expected(
                vec!["ADD", "DROP", "RENAME"],
                self.peek(),
                self.position(),
            ))
        }
    }

    /// Parse a single column definition: name TYPE [NOT NULL] [DEFAULT=val] [COMPRESS:N] [UNIQUE] [PRIMARY KEY]
    fn parse_column_def(&mut self) -> Result<CreateColumnDef, ParseError> {
        let name = self.expect_ident()?;
        let data_type = self.parse_column_type()?;

        let mut def = CreateColumnDef {
            name,
            data_type,
            not_null: false,
            default: None,
            compress: None,
            unique: false,
            primary_key: false,
            enum_variants: Vec::new(),
            array_element: None,
            decimal_precision: None,
        };

        // Parse modifiers in any order
        loop {
            if self.match_not_null()? {
                def.not_null = true;
            } else if self.consume(&Token::Default)? {
                self.expect(Token::Eq)?;
                def.default = Some(self.parse_literal_string_for_ddl()?);
            } else if self.consume(&Token::Compress)? {
                self.expect(Token::Colon)?;
                def.compress = Some(self.parse_integer()? as u8);
            } else if self.consume(&Token::Unique)? {
                def.unique = true;
            } else if self.match_primary_key()? {
                def.primary_key = true;
            } else {
                break;
            }
        }

        Ok(def)
    }

    /// Parse column type: TEXT, INTEGER, EMAIL, ENUM('a','b','c'), ARRAY(TEXT), DECIMAL(2)
    fn parse_column_type(&mut self) -> Result<String, ParseError> {
        let type_name = self.expect_ident_or_keyword()?;
        // Handle parameterized types
        if self.consume(&Token::LParen)? {
            let inner = self.parse_type_params()?;
            self.expect(Token::RParen)?;
            Ok(format!("{}({})", type_name, inner))
        } else {
            Ok(type_name)
        }
    }

    /// Parse type parameters inside parentheses: 'a','b' or TEXT or 2
    fn parse_type_params(&mut self) -> Result<String, ParseError> {
        let mut parts = Vec::new();
        loop {
            match self.peek().clone() {
                Token::String(s) => {
                    let s = s.clone();
                    self.advance()?;
                    parts.push(format!("'{}'", s));
                }
                Token::Integer(n) => {
                    self.advance()?;
                    parts.push(n.to_string());
                }
                Token::Ident(s) => {
                    let s = s.clone();
                    self.advance()?;
                    parts.push(s);
                }
                _ => {
                    // Also accept keywords as type names inside params
                    let name = self.expect_ident_or_keyword()?;
                    parts.push(name);
                }
            }
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        Ok(parts.join(","))
    }

    /// Parse a literal string value for DDL DEFAULT expressions
    fn parse_literal_string_for_ddl(&mut self) -> Result<String, ParseError> {
        match self.peek().clone() {
            Token::String(s) => {
                let s = s.clone();
                self.advance()?;
                Ok(s)
            }
            Token::Integer(n) => {
                self.advance()?;
                Ok(n.to_string())
            }
            Token::Float(n) => {
                self.advance()?;
                Ok(n.to_string())
            }
            Token::True => {
                self.advance()?;
                Ok("true".to_string())
            }
            Token::False => {
                self.advance()?;
                Ok("false".to_string())
            }
            Token::Null => {
                self.advance()?;
                Ok("null".to_string())
            }
            ref other => Err(ParseError::expected(
                vec!["string", "number", "true", "false", "null"],
                other,
                self.position(),
            )),
        }
    }

    /// Try to match IF NOT EXISTS sequence
    fn match_if_not_exists(&mut self) -> Result<bool, ParseError> {
        if self.check(&Token::If) {
            self.advance()?;
            self.expect(Token::Not)?;
            self.expect(Token::Exists)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Try to match IF EXISTS sequence
    fn match_if_exists(&mut self) -> Result<bool, ParseError> {
        if self.check(&Token::If) {
            self.advance()?;
            self.expect(Token::Exists)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Try to match NOT NULL sequence
    fn match_not_null(&mut self) -> Result<bool, ParseError> {
        if self.check(&Token::Not) {
            // Peek ahead - only consume if followed by NULL
            // We need to be careful: save state and try
            self.advance()?; // consume NOT
            if self.check(&Token::Null) {
                self.advance()?; // consume NULL
                Ok(true)
            } else {
                // This is tricky - NOT was consumed but next isn't NULL.
                // In column modifier context, NOT should only appear before NULL.
                // Return error for clarity.
                Err(ParseError::expected(
                    vec!["NULL (after NOT)"],
                    self.peek(),
                    self.position(),
                ))
            }
        } else {
            Ok(false)
        }
    }

    /// Try to match PRIMARY KEY sequence
    fn match_primary_key(&mut self) -> Result<bool, ParseError> {
        if self.check(&Token::Primary) {
            self.advance()?;
            self.expect(Token::Key)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
