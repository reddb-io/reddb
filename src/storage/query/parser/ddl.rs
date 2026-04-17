//! DDL SQL Parser: CREATE TABLE, DROP TABLE, ALTER TABLE

use super::super::ast::{
    AlterOperation, AlterTableQuery, CreateColumnDef, CreateTableQuery, DropTableQuery,
    ExplainAlterQuery, ExplainFormat, PartitionKind, PartitionSpec, QueryExpr,
};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;
use crate::storage::schema::{SqlTypeName, TypeModifier};

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

        let mut default_ttl_ms = None;
        let mut context_index_fields = Vec::new();
        let mut timestamps = false;

        while self.consume(&Token::With)? {
            if self.consume_ident_ci("CONTEXT")? {
                // Consume INDEX token (reserved keyword)
                if !self.consume(&Token::Index)? {
                    return Err(ParseError::expected(
                        vec!["INDEX"],
                        self.peek(),
                        self.position(),
                    ));
                }
                self.expect(Token::On)?;
                self.expect(Token::LParen)?;
                loop {
                    context_index_fields.push(self.expect_ident()?);
                    if !self.consume(&Token::Comma)? {
                        break;
                    }
                }
                self.expect(Token::RParen)?;
            } else if self.consume_ident_ci("TIMESTAMPS")? {
                timestamps = self.parse_bool_assign()?;
            } else {
                default_ttl_ms = self.parse_create_table_ttl_clause()?;
            }
        }

        Ok(QueryExpr::CreateTable(CreateTableQuery {
            name,
            columns,
            if_not_exists,
            default_ttl_ms,
            context_index_fields,
            timestamps,
            partition_by: None,
            tenant_by: None,
        }))
    }

    /// Parse: DROP TABLE [IF EXISTS] name
    pub fn parse_drop_table_query(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::Drop)?;
        self.expect(Token::Table)?;
        self.parse_drop_table_body()
    }

    /// Parse the body of CREATE TABLE after CREATE TABLE has been consumed
    pub fn parse_create_table_body(&mut self) -> Result<QueryExpr, ParseError> {
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

        let mut default_ttl_ms = None;
        let mut context_index_fields = Vec::new();
        let mut timestamps = false;
        let mut tenant_by: Option<String> = None;

        while self.consume(&Token::With)? {
            if self.consume_ident_ci("CONTEXT")? {
                if !self.consume(&Token::Index)? {
                    return Err(ParseError::expected(
                        vec!["INDEX"],
                        self.peek(),
                        self.position(),
                    ));
                }
                self.expect(Token::On)?;
                self.expect(Token::LParen)?;
                loop {
                    context_index_fields.push(self.expect_ident()?);
                    if !self.consume(&Token::Comma)? {
                        break;
                    }
                }
                self.expect(Token::RParen)?;
            } else if self.consume_ident_ci("TIMESTAMPS")? {
                timestamps = self.parse_bool_assign()?;
            } else if self.consume_ident_ci("TENANT_BY")? {
                // `WITH (tenant_by = 'col')` form — accepts `=` optional
                // and expects a string literal column name.
                let _ = self.consume(&Token::Eq)?;
                let value = self.parse_literal_value()?;
                match value {
                    Value::Text(col) => tenant_by = Some(col),
                    other => {
                        return Err(ParseError::new(
                            format!(
                                "WITH tenant_by expects a text literal, got {other:?}"
                            ),
                            self.position(),
                        ));
                    }
                }
            } else {
                default_ttl_ms = self.parse_create_table_ttl_clause()?;
            }
        }

        // Optional `PARTITION BY RANGE|LIST|HASH (col)` clause (Phase 2.2).
        let partition_by = if self.consume(&Token::Partition)? {
            self.expect(Token::By)?;
            let kind = if self.consume(&Token::Range)? {
                PartitionKind::Range
            } else if self.consume(&Token::List)? {
                PartitionKind::List
            } else if self.consume(&Token::Hash)? {
                PartitionKind::Hash
            } else {
                return Err(ParseError::expected(
                    vec!["RANGE", "LIST", "HASH"],
                    self.peek(),
                    self.position(),
                ));
            };
            self.expect(Token::LParen)?;
            let column = self.expect_ident()?;
            self.expect(Token::RParen)?;
            Some(PartitionSpec { kind, column })
        } else {
            None
        };

        // Shorthand: `TENANT BY (col)` trailing clause (after partition
        // spec if both are used). More ergonomic than the WITH form for
        // a feature that's usually declared once.
        if tenant_by.is_none() && self.consume_ident_ci("TENANT")? {
            self.expect(Token::By)?;
            self.expect(Token::LParen)?;
            let col = self.expect_ident()?;
            self.expect(Token::RParen)?;
            tenant_by = Some(col);
        }

        Ok(QueryExpr::CreateTable(CreateTableQuery {
            name,
            columns,
            if_not_exists,
            default_ttl_ms,
            context_index_fields,
            timestamps,
            partition_by,
            tenant_by,
        }))
    }

    /// Parse: EXPLAIN ALTER FOR CREATE TABLE name (...) [FORMAT JSON|SQL]
    ///
    /// Pure read: does not execute DDL. Returns a schema-diff rendering of the
    /// difference between the table's current contract and the target CREATE
    /// TABLE body.
    pub fn parse_explain_alter_query(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::Explain)?;
        self.expect(Token::Alter)?;
        self.expect(Token::For)?;
        self.expect(Token::Create)?;
        self.expect(Token::Table)?;

        let body = self.parse_create_table_body()?;
        let target = match body {
            QueryExpr::CreateTable(t) => t,
            _ => {
                return Err(ParseError::new(
                    "EXPLAIN ALTER FOR CREATE TABLE body must be a CREATE TABLE statement"
                        .to_string(),
                    self.position(),
                ));
            }
        };

        let format = if self.consume(&Token::Format)? {
            if self.consume(&Token::Json)? {
                ExplainFormat::Json
            } else if self.consume_ident_ci("SQL")? {
                ExplainFormat::Sql
            } else {
                return Err(ParseError::expected(
                    vec!["JSON", "SQL"],
                    self.peek(),
                    self.position(),
                ));
            }
        } else {
            ExplainFormat::Sql
        };

        Ok(QueryExpr::ExplainAlter(ExplainAlterQuery {
            target,
            format,
        }))
    }

    /// Parse the body of DROP TABLE after DROP TABLE has been consumed
    pub fn parse_drop_table_body(&mut self) -> Result<QueryExpr, ParseError> {
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
        } else if self.consume(&Token::Attach)? {
            // ATTACH PARTITION child FOR VALUES ...
            self.expect(Token::Partition)?;
            let child = self.expect_ident()?;
            self.expect(Token::For)?;
            // Accept `VALUES` as an ident since the grammar doesn't have it
            // as a reserved keyword everywhere. Collect the remaining tokens
            // as a raw bound string for round-trip persistence.
            if !self.consume_ident_ci("VALUES")? && !self.consume(&Token::Values)? {
                return Err(ParseError::expected(
                    vec!["VALUES"],
                    self.peek(),
                    self.position(),
                ));
            }
            let bound = self.collect_remaining_tokens_as_string()?;
            Ok(AlterOperation::AttachPartition { child, bound })
        } else if self.consume(&Token::Detach)? {
            // DETACH PARTITION child
            self.expect(Token::Partition)?;
            let child = self.expect_ident()?;
            Ok(AlterOperation::DetachPartition { child })
        } else if self.consume(&Token::Enable)? {
            // ENABLE ROW LEVEL SECURITY  |  ENABLE TENANCY ON (col)
            if self.consume_ident_ci("TENANCY")? {
                self.expect(Token::On)?;
                self.expect(Token::LParen)?;
                let col = self.expect_ident()?;
                self.expect(Token::RParen)?;
                Ok(AlterOperation::EnableTenancy { column: col })
            } else {
                self.expect(Token::Row)?;
                self.expect(Token::Level)?;
                self.expect(Token::Security)?;
                Ok(AlterOperation::EnableRowLevelSecurity)
            }
        } else if self.consume(&Token::Disable)? {
            // DISABLE ROW LEVEL SECURITY  |  DISABLE TENANCY
            if self.consume_ident_ci("TENANCY")? {
                Ok(AlterOperation::DisableTenancy)
            } else {
                self.expect(Token::Row)?;
                self.expect(Token::Level)?;
                self.expect(Token::Security)?;
                Ok(AlterOperation::DisableRowLevelSecurity)
            }
        } else {
            Err(ParseError::expected(
                vec![
                    "ADD", "DROP", "RENAME", "ATTACH", "DETACH", "ENABLE", "DISABLE",
                ],
                self.peek(),
                self.position(),
            ))
        }
    }

    /// Capture remaining tokens as a display-joined string.
    ///
    /// Used by `ATTACH PARTITION ... FOR VALUES <bound>` to round-trip the
    /// bound clause into storage without needing a dedicated per-kind AST.
    fn collect_remaining_tokens_as_string(&mut self) -> Result<String, ParseError> {
        let mut parts: Vec<String> = Vec::new();
        while !self.check(&Token::Eof) && !self.check(&Token::Comma) {
            parts.push(self.peek().to_string());
            self.advance()?;
        }
        Ok(parts.join(" "))
    }

    /// Parse a single column definition: name TYPE [NOT NULL] [DEFAULT=val] [COMPRESS:N] [UNIQUE] [PRIMARY KEY]
    fn parse_column_def(&mut self) -> Result<CreateColumnDef, ParseError> {
        let name = self.expect_ident()?;
        let sql_type = self.parse_column_type()?;
        let data_type = sql_type.to_string();

        let mut def = CreateColumnDef {
            name,
            data_type,
            sql_type: sql_type.clone(),
            not_null: false,
            default: None,
            compress: None,
            unique: false,
            primary_key: false,
            enum_variants: sql_type.enum_variants().unwrap_or_default(),
            array_element: sql_type.array_element_type(),
            decimal_precision: sql_type.decimal_precision(),
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
    fn parse_column_type(&mut self) -> Result<SqlTypeName, ParseError> {
        let type_name = self.expect_ident_or_keyword()?;
        if self.consume(&Token::LParen)? {
            let inner = self.parse_type_params()?;
            self.expect(Token::RParen)?;
            Ok(SqlTypeName::new(type_name).with_modifiers(inner))
        } else {
            Ok(SqlTypeName::new(type_name))
        }
    }

    /// Parse type parameters inside parentheses: 'a','b' or TEXT or 2
    fn parse_type_params(&mut self) -> Result<Vec<TypeModifier>, ParseError> {
        let mut parts = Vec::new();
        loop {
            match self.peek().clone() {
                Token::String(s) => {
                    let s = s.clone();
                    self.advance()?;
                    parts.push(TypeModifier::StringLiteral(s));
                }
                Token::Integer(n) => {
                    self.advance()?;
                    parts.push(TypeModifier::Number(n as u32));
                }
                _ => {
                    parts.push(TypeModifier::Type(Box::new(self.parse_column_type()?)));
                }
            }
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        Ok(parts)
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

    fn check_ttl_keyword(&self) -> bool {
        matches!(self.peek(), Token::Ident(name) if name.eq_ignore_ascii_case("ttl"))
    }

    /// Parse `= true` / `= false` after a `WITH <option>` keyword.
    /// Used for boolean table options like `WITH TIMESTAMPS = true`.
    fn parse_bool_assign(&mut self) -> Result<bool, ParseError> {
        self.expect(Token::Eq)?;
        match self.peek() {
            Token::True => {
                self.advance()?;
                Ok(true)
            }
            Token::False => {
                self.advance()?;
                Ok(false)
            }
            other => Err(ParseError::expected(
                vec!["true", "false"],
                other,
                self.position(),
            )),
        }
    }

    fn expect_ident_ci_ddl(&mut self, expected: &str) -> Result<(), ParseError> {
        if self.consume_ident_ci(expected)? {
            Ok(())
        } else {
            Err(ParseError::expected(
                vec![expected],
                self.peek(),
                self.position(),
            ))
        }
    }

    fn parse_create_table_ttl_clause(&mut self) -> Result<Option<u64>, ParseError> {
        let option_name = self.expect_ident_or_keyword()?;
        if !option_name.eq_ignore_ascii_case("ttl") {
            return Err(ParseError::new(
                format!("unsupported CREATE TABLE option '{option_name}', expected TTL"),
                self.position(),
            ));
        }

        let ttl_value = self.parse_float()?;
        let ttl_unit = match self.peek() {
            Token::Ident(unit) => {
                let unit = unit.clone();
                self.advance()?;
                unit
            }
            _ => "s".to_string(),
        };

        let multiplier_ms = match ttl_unit.to_ascii_lowercase().as_str() {
            "ms" | "msec" | "millisecond" | "milliseconds" => 1.0,
            "s" | "sec" | "secs" | "second" | "seconds" => 1_000.0,
            "m" | "min" | "mins" | "minute" | "minutes" => 60_000.0,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3_600_000.0,
            "d" | "day" | "days" => 86_400_000.0,
            other => {
                return Err(ParseError::new(
                    format!("unsupported TTL unit '{other}'"),
                    self.position(),
                ))
            }
        };

        if !ttl_value.is_finite() || ttl_value < 0.0 {
            return Err(ParseError::new(
                "TTL must be a finite, non-negative duration".to_string(),
                self.position(),
            ));
        }

        let ttl_ms = ttl_value * multiplier_ms;
        if ttl_ms > u64::MAX as f64 {
            return Err(ParseError::new(
                "TTL duration is too large".to_string(),
                self.position(),
            ));
        }
        if ttl_ms.fract().abs() >= f64::EPSILON {
            return Err(ParseError::new(
                "TTL duration must resolve to a whole number of milliseconds".to_string(),
                self.position(),
            ));
        }

        Ok(Some(ttl_ms as u64))
    }

    /// Try to match IF NOT EXISTS sequence
    pub(super) fn match_if_not_exists(&mut self) -> Result<bool, ParseError> {
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
    pub(super) fn match_if_exists(&mut self) -> Result<bool, ParseError> {
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
