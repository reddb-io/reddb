//! DDL SQL Parser: CREATE TABLE, DROP TABLE, ALTER TABLE

use super::error::ParseError;
use super::Parser;
use crate::ast::{
    AlterOperation, AlterTableQuery, CreateCollectionQuery, CreateColumnDef, CreateTableQuery,
    CreateVectorQuery, DropCollectionQuery, DropDocumentQuery, DropGraphQuery, DropKvQuery,
    DropTableQuery, DropVectorQuery, ExplainAlterQuery, ExplainFormat, PartitionKind,
    PartitionSpec, QueryExpr, TruncateQuery,
};
use crate::lexer::Token;
use reddb_types::catalog::{CollectionModel, SubscriptionDescriptor, SubscriptionOperation};
use reddb_types::types::{SqlTypeName, TypeModifier, Value};

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
        let mut context_index_enabled = false;
        let mut timestamps = false;
        let mut subscriptions = Vec::new();

        while self.consume(&Token::With)? {
            if self.consume_ident_ci("EVENTS")? {
                subscriptions.push(self.parse_subscription_descriptor(name.clone())?);
            } else if self.consume_ident_ci("CONTEXT_INDEX")? {
                context_index_enabled = self.parse_bool_assign()?;
            } else if self.consume_ident_ci("CONTEXT")? {
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
                context_index_enabled = true;
            } else if self.consume_ident_ci("TIMESTAMPS")? {
                timestamps = self.parse_bool_assign()?;
            } else {
                default_ttl_ms = self.parse_create_table_ttl_clause()?;
            }
        }

        Ok(QueryExpr::CreateTable(CreateTableQuery {
            collection_model: CollectionModel::Table,
            name,
            columns,
            if_not_exists,
            default_ttl_ms,
            metrics_rollup_policies: Vec::new(),
            context_index_fields,
            context_index_enabled,
            timestamps,
            partition_by: None,
            tenant_by: None,
            append_only: false,
            subscriptions,
            analytics_config: Vec::new(),
            vault_own_master_key: false,
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
        let mut context_index_enabled = false;
        let mut timestamps = false;
        let mut tenant_by: Option<String> = None;
        let mut append_only = false;
        let mut subscriptions = Vec::new();

        while self.consume(&Token::With)? {
            if self.consume_ident_ci("EVENTS")? {
                subscriptions.push(self.parse_subscription_descriptor(name.clone())?);
                continue;
            }
            // Accept both spellings:
            //   WITH key = value
            //   WITH (key = value, key = value)
            // Postgres / ClickHouse use the parenthesised form; the
            // bare form is our legacy shorthand. The parenthesised
            // form collects options separated by commas until `)`.
            let has_parens = self.consume(&Token::LParen)?;

            loop {
                if self.consume_ident_ci("CONTEXT_INDEX")? {
                    context_index_enabled = self.parse_bool_assign()?;
                } else if self.consume_ident_ci("CONTEXT")? {
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
                    context_index_enabled = true;
                } else if self.consume_ident_ci("TIMESTAMPS")? {
                    timestamps = self.parse_bool_assign()?;
                } else if self.consume_ident_ci("APPEND_ONLY")? {
                    append_only = self.parse_bool_assign()?;
                } else if self.consume_ident_ci("TENANT_BY")? {
                    // `WITH (tenant_by = 'col')` form — accepts `=` optional
                    // and expects a string literal column name.
                    let _ = self.consume(&Token::Eq)?;
                    let value = self.parse_literal_value()?;
                    match value {
                        Value::Text(col) => tenant_by = Some(col.to_string()),
                        other => {
                            return Err(ParseError::new(
                                format!("WITH tenant_by expects a text literal, got {other:?}"),
                                self.position(),
                            ));
                        }
                    }
                } else {
                    default_ttl_ms = self.parse_create_table_ttl_clause()?;
                }
                if has_parens {
                    if self.consume(&Token::Comma)? {
                        continue;
                    }
                    self.expect(Token::RParen)?;
                }
                break;
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

        // Shorthand: trailing `APPEND ONLY` keyword pair (PG / ClickHouse
        // style). Accepted after partition spec / tenant spec / or on
        // its own. `WITH (append_only = true)` is the other form and
        // handled above.
        if !append_only && self.consume_ident_ci("APPEND")? {
            if !self.consume_ident_ci("ONLY")? {
                return Err(ParseError::expected(
                    vec!["ONLY"],
                    self.peek(),
                    self.position(),
                ));
            }
            append_only = true;
        }

        // Shorthand: `TENANT BY (col)` or `TENANT BY (root.sub.path)`
        // trailing clause (after partition spec if both are used).
        //
        // Dotted paths let non-table models declare tenancy over their
        // natural nested structures — `metadata.tenant` for vectors,
        // `payload.tenant` for queue messages, `tags.cluster` for
        // timeseries, `properties.org` for graphs. The read-path
        // resolver already navigates these paths via
        // `resolve_runtime_document_path`; here we just store the
        // dotted string and let the policy evaluator do the rest.
        if tenant_by.is_none() && self.consume_ident_ci("TENANT")? {
            self.expect(Token::By)?;
            self.expect(Token::LParen)?;
            // Allow keyword-idents (`metadata`, `type`, `data`) as
            // column names — SQL treats them as bare identifiers in
            // this context.
            let mut path = self.expect_ident_or_keyword()?;
            while self.consume(&Token::Dot)? {
                let next = self.expect_ident_or_keyword()?;
                path = format!("{path}.{next}");
            }
            self.expect(Token::RParen)?;
            tenant_by = Some(path);
        }

        Ok(QueryExpr::CreateTable(CreateTableQuery {
            collection_model: CollectionModel::Table,
            name,
            columns,
            if_not_exists,
            default_ttl_ms,
            metrics_rollup_policies: Vec::new(),
            context_index_fields,
            context_index_enabled,
            timestamps,
            partition_by,
            tenant_by,
            append_only,
            subscriptions,
            analytics_config: Vec::new(),
            vault_own_master_key: false,
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
        let name = self.parse_drop_collection_name()?;
        Ok(QueryExpr::DropTable(DropTableQuery { name, if_exists }))
    }

    pub fn parse_drop_graph_body(&mut self) -> Result<QueryExpr, ParseError> {
        let if_exists = self.match_if_exists()?;
        let name = self.parse_drop_collection_name()?;
        Ok(QueryExpr::DropGraph(DropGraphQuery { name, if_exists }))
    }

    pub fn parse_drop_vector_body(&mut self) -> Result<QueryExpr, ParseError> {
        let if_exists = self.match_if_exists()?;
        let name = self.parse_drop_collection_name()?;
        Ok(QueryExpr::DropVector(DropVectorQuery { name, if_exists }))
    }

    pub fn parse_drop_document_body(&mut self) -> Result<QueryExpr, ParseError> {
        let if_exists = self.match_if_exists()?;
        let name = self.parse_drop_collection_name()?;
        Ok(QueryExpr::DropDocument(DropDocumentQuery {
            name,
            if_exists,
        }))
    }

    pub fn parse_create_keyed_body(
        &mut self,
        model: CollectionModel,
    ) -> Result<QueryExpr, ParseError> {
        let if_not_exists = self.match_if_not_exists()?;
        let name = self.parse_drop_collection_name()?;
        let vault_own_master_key =
            if model == CollectionModel::Vault && self.consume(&Token::With)? {
                if !self.consume_ident_ci("OWN")? {
                    return Err(ParseError::expected(
                        vec!["OWN"],
                        self.peek(),
                        self.position(),
                    ));
                }
                if !self.consume_ident_ci("MASTER")? {
                    return Err(ParseError::expected(
                        vec!["MASTER"],
                        self.peek(),
                        self.position(),
                    ));
                }
                if !self.consume(&Token::Key)? && !self.consume_ident_ci("KEY")? {
                    return Err(ParseError::expected(
                        vec!["KEY"],
                        self.peek(),
                        self.position(),
                    ));
                }
                true
            } else {
                false
            };
        // `CREATE GRAPH <name> WITH ANALYTICS (...)` — the analytics opt-in
        // is graph-only (issue #800). Other keyed models reject the clause so
        // a misplaced `WITH ANALYTICS` fails loudly instead of being ignored.
        let analytics_config = if model == CollectionModel::Graph && self.consume(&Token::With)? {
            if !self.consume_ident_ci("ANALYTICS")? {
                return Err(ParseError::expected(
                    vec!["ANALYTICS"],
                    self.peek(),
                    self.position(),
                ));
            }
            self.parse_analytics_clause()?
        } else {
            Vec::new()
        };
        Ok(QueryExpr::CreateTable(CreateTableQuery {
            collection_model: model,
            name,
            columns: Vec::new(),
            if_not_exists,
            default_ttl_ms: None,
            metrics_rollup_policies: Vec::new(),
            context_index_fields: Vec::new(),
            context_index_enabled: false,
            timestamps: false,
            partition_by: None,
            tenant_by: None,
            append_only: false,
            subscriptions: Vec::new(),
            analytics_config,
            vault_own_master_key,
        }))
    }

    /// Parse the `( <output> [ ( <key> = <value> [, ...] ) ] [, ...] )` body of
    /// a `WITH ANALYTICS` clause (issue #800). Recognised outputs are
    /// `communities`, `components`, `centrality`; recognised options are
    /// `using`, `resolution`, `max_iterations`, `tolerance`. Unknown output
    /// names and option keys are rejected with a clear, structured error.
    fn parse_analytics_clause(
        &mut self,
    ) -> Result<Vec<reddb_types::catalog::AnalyticsViewDescriptor>, ParseError> {
        use reddb_types::catalog::{AnalyticsOutput, AnalyticsViewDescriptor};

        self.expect(Token::LParen)?;
        let mut views: Vec<AnalyticsViewDescriptor> = Vec::new();
        loop {
            let output_name = self.parse_analytics_output_name()?;
            let output = AnalyticsOutput::from_str(&output_name).ok_or_else(|| {
                ParseError::new(
                    format!(
                        "unknown analytics output '{output_name}': expected communities, components, or centrality"
                    ),
                    self.position(),
                )
            })?;
            if views.iter().any(|view| view.output == output) {
                return Err(ParseError::new(
                    format!("duplicate analytics output '{output_name}'"),
                    self.position(),
                ));
            }
            let mut view = AnalyticsViewDescriptor {
                output,
                algorithm: None,
                resolution: None,
                max_iterations: None,
                tolerance: None,
            };
            if self.consume(&Token::LParen)? {
                loop {
                    let key = self.parse_analytics_option_key()?;
                    self.expect(Token::Eq)?;
                    match key.as_str() {
                        "using" => {
                            view.algorithm =
                                Some(self.expect_ident_or_keyword()?.to_ascii_lowercase());
                        }
                        "resolution" => view.resolution = Some(self.parse_float()?),
                        "max_iterations" => view.max_iterations = Some(self.parse_integer()?),
                        "tolerance" => view.tolerance = Some(self.parse_float()?),
                        other => {
                            return Err(ParseError::new(
                                format!(
                                    "unknown analytics option '{other}': expected using, resolution, max_iterations, or tolerance"
                                ),
                                self.position(),
                            ))
                        }
                    }
                    if !self.consume(&Token::Comma)? {
                        break;
                    }
                }
                self.expect(Token::RParen)?;
            }
            views.push(view);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        self.expect(Token::RParen)?;
        if views.is_empty() {
            return Err(ParseError::new(
                "WITH ANALYTICS requires at least one output".to_string(),
                self.position(),
            ));
        }
        Ok(views)
    }

    /// Read one analytics output name, normalising the keyword-lexed outputs
    /// (`components`, `centrality`) back to their lowercase spelling so they
    /// compare uniformly with the ident-lexed `communities`.
    fn parse_analytics_output_name(&mut self) -> Result<String, ParseError> {
        match self.peek() {
            Token::Components => {
                self.advance()?;
                Ok("components".to_string())
            }
            Token::Centrality => {
                self.advance()?;
                Ok("centrality".to_string())
            }
            _ => Ok(self.expect_ident()?.to_ascii_lowercase()),
        }
    }

    /// Read one analytics option key, normalising the keyword-lexed keys
    /// (`using`, `max_iterations`) back to their lowercase spelling.
    fn parse_analytics_option_key(&mut self) -> Result<String, ParseError> {
        match self.peek() {
            Token::Using => {
                self.advance()?;
                Ok("using".to_string())
            }
            Token::MaxIterations => {
                self.advance()?;
                Ok("max_iterations".to_string())
            }
            _ => Ok(self.expect_ident()?.to_ascii_lowercase()),
        }
    }

    pub fn parse_create_collection_model_body(
        &mut self,
        model: CollectionModel,
    ) -> Result<QueryExpr, ParseError> {
        self.parse_create_keyed_body(model)
    }

    pub fn parse_create_collection_body(&mut self) -> Result<QueryExpr, ParseError> {
        let if_not_exists = self.match_if_not_exists()?;
        let name = self.parse_drop_collection_name()?;
        if !self.consume_ident_ci("KIND")? {
            return Err(ParseError::expected(
                vec!["KIND"],
                self.peek(),
                self.position(),
            ));
        }
        let mut kind = self.expect_ident_or_keyword()?.to_ascii_lowercase();
        while self.consume(&Token::Dot)? {
            let part = self.expect_ident_or_keyword()?.to_ascii_lowercase();
            kind.push('.');
            kind.push_str(&part);
        }
        let (vector_dimension, vector_metric) = if kind == "vector.turbo" {
            if !self.consume_ident_ci("DIM")? {
                return Err(ParseError::expected(
                    vec!["DIM"],
                    self.peek(),
                    self.position(),
                ));
            }
            let dimension = self.parse_integer()?;
            if dimension <= 0 {
                return Err(ParseError::new(
                    "VECTOR DIM must be a positive integer".to_string(),
                    self.position(),
                ));
            }
            let metric = if self.consume(&Token::Metric)? {
                self.parse_distance_metric()?
            } else {
                reddb_types::distance::DistanceMetric::Cosine
            };
            (Some(dimension as usize), Some(metric))
        } else {
            (None, None)
        };
        let allowed_signers = if self.consume_ident_ci("SIGNED_BY")? {
            self.parse_signed_by_list()?
        } else {
            Vec::new()
        };
        Ok(QueryExpr::CreateCollection(CreateCollectionQuery {
            name,
            kind,
            if_not_exists,
            vector_dimension,
            vector_metric,
            allowed_signers,
        }))
    }

    /// Parse a single `'hex32'` string literal as a 32-byte Ed25519
    /// pubkey. Used by `ALTER COLLECTION ... ADD|REVOKE SIGNER 'hex'`
    /// (issue #522).
    fn parse_single_signer_hex(&mut self) -> Result<[u8; 32], ParseError> {
        let hex = match self.peek().clone() {
            Token::String(s) => {
                self.advance()?;
                s
            }
            _ => {
                return Err(ParseError::expected(
                    vec!["string literal (ed25519 pubkey hex)"],
                    self.peek(),
                    self.position(),
                ));
            }
        };
        decode_hex_32(&hex).map_err(|msg| {
            ParseError::new(
                format!("SIGNER pubkey '{hex}' invalid: {msg}"),
                self.position(),
            )
        })
    }

    /// Parse `( 'hex32', 'hex32', ... )` — Ed25519 pubkey list. Each entry
    /// must decode to exactly 32 bytes. Used by both `CREATE COLLECTION ...
    /// SIGNED_BY (...)` and (in a later iteration) `ALTER COLLECTION` signer
    /// mutations. Issue #520.
    fn parse_signed_by_list(&mut self) -> Result<Vec<[u8; 32]>, ParseError> {
        self.expect(Token::LParen)?;
        let mut out = Vec::new();
        loop {
            let hex = match self.peek().clone() {
                Token::String(s) => {
                    self.advance()?;
                    s
                }
                _ => {
                    return Err(ParseError::expected(
                        vec!["string literal (ed25519 pubkey hex)"],
                        self.peek(),
                        self.position(),
                    ));
                }
            };
            let bytes = decode_hex_32(&hex).map_err(|msg| {
                ParseError::new(
                    format!("SIGNED_BY pubkey '{hex}' invalid: {msg}"),
                    self.position(),
                )
            })?;
            out.push(bytes);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        self.expect(Token::RParen)?;
        if out.is_empty() {
            return Err(ParseError::new(
                "SIGNED_BY list must contain at least one pubkey".to_string(),
                self.position(),
            ));
        }
        Ok(out)
    }

    pub fn parse_create_vector_body(&mut self) -> Result<QueryExpr, ParseError> {
        let if_not_exists = self.match_if_not_exists()?;
        let name = self.parse_drop_collection_name()?;
        if !self.consume_ident_ci("DIM")? {
            return Err(ParseError::expected(
                vec!["DIM"],
                self.peek(),
                self.position(),
            ));
        }
        let dimension = self.parse_integer()?;
        if dimension <= 0 {
            return Err(ParseError::new(
                "VECTOR DIM must be a positive integer".to_string(),
                self.position(),
            ));
        }
        let metric = if self.consume(&Token::Metric)? {
            self.parse_distance_metric()?
        } else {
            reddb_types::distance::DistanceMetric::Cosine
        };
        Ok(QueryExpr::CreateVector(CreateVectorQuery {
            name,
            dimension: dimension as usize,
            metric,
            if_not_exists,
        }))
    }

    pub fn parse_drop_keyed_body(
        &mut self,
        model: CollectionModel,
    ) -> Result<QueryExpr, ParseError> {
        let if_exists = self.match_if_exists()?;
        let name = self.parse_drop_collection_name()?;
        Ok(QueryExpr::DropKv(DropKvQuery {
            name,
            if_exists,
            model,
        }))
    }

    pub fn parse_drop_kv_body(&mut self) -> Result<QueryExpr, ParseError> {
        self.parse_drop_keyed_body(CollectionModel::Kv)
    }

    pub fn parse_drop_collection_body(&mut self) -> Result<QueryExpr, ParseError> {
        self.parse_drop_collection_model_body(None)
    }

    pub fn parse_drop_collection_model_body(
        &mut self,
        model: Option<CollectionModel>,
    ) -> Result<QueryExpr, ParseError> {
        let if_exists = self.match_if_exists()?;
        let name = self.parse_drop_collection_name()?;
        Ok(QueryExpr::DropCollection(DropCollectionQuery {
            name,
            if_exists,
            model,
        }))
    }

    pub fn parse_truncate_body(
        &mut self,
        model: Option<CollectionModel>,
    ) -> Result<QueryExpr, ParseError> {
        let if_exists = self.match_if_exists()?;
        let name = self.parse_drop_collection_name()?;
        Ok(QueryExpr::Truncate(TruncateQuery {
            name,
            model,
            if_exists,
        }))
    }

    pub(crate) fn parse_drop_collection_name(&mut self) -> Result<String, ParseError> {
        let mut name = self.expect_ident()?;
        while self.consume(&Token::Dot)? {
            if self.consume(&Token::Star)? {
                name.push_str(".*");
                break;
            }
            let next = self.expect_ident_or_keyword()?;
            name = format!("{name}.{next}");
        }
        Ok(name)
    }

    /// Parse: ALTER TABLE name ADD/DROP/RENAME COLUMN ...
    ///
    /// Also accepts `ALTER COLLECTION name ADD|REVOKE SIGNER 'hex'`
    /// (issue #522) — collection-level signer registry mutations share
    /// the AlterTable AST so the existing executor dispatch path picks
    /// them up without a new top-level variant.
    pub fn parse_alter_table_query(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::Alter)?;
        if !self.consume(&Token::Table)?
            && !self.consume(&Token::Collection)?
            && !self.consume_ident_ci("COLLECTION")?
        {
            return Err(ParseError::expected(
                vec!["TABLE", "COLLECTION"],
                self.peek(),
                self.position(),
            ));
        }
        let name = self.expect_ident()?;

        let mut operations = Vec::new();
        loop {
            let op = self.parse_alter_operation(&name)?;
            operations.push(op);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }

        Ok(QueryExpr::AlterTable(AlterTableQuery { name, operations }))
    }

    /// Parse: `ALTER GRAPH <name> ADD ANALYTICS ( <output> [, ...] )`
    /// and `ALTER GRAPH <name> DROP ANALYTICS <output>` (issue #801).
    ///
    /// Lifecycle management of the `WITH ANALYTICS` configuration declared at
    /// `CREATE GRAPH` time (#800), without recreating the collection. Shares
    /// the `AlterTable` AST so the existing executor dispatch path picks the
    /// mutations up; the executor validates the target is a graph and that the
    /// dropped output is actually enabled.
    pub fn parse_alter_graph_query(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::Alter)?;
        self.expect(Token::Graph)?;
        let name = self.expect_ident()?;

        let mut operations = Vec::new();
        loop {
            operations.push(self.parse_alter_graph_operation()?);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }

        Ok(QueryExpr::AlterTable(AlterTableQuery { name, operations }))
    }

    /// Parse a single `ALTER GRAPH` analytics operation: either
    /// `ADD ANALYTICS ( ... )` or `DROP ANALYTICS <output>`.
    fn parse_alter_graph_operation(&mut self) -> Result<AlterOperation, ParseError> {
        if self.consume(&Token::Add)? {
            if !self.consume_ident_ci("ANALYTICS")? {
                return Err(ParseError::expected(
                    vec!["ANALYTICS"],
                    self.peek(),
                    self.position(),
                ));
            }
            // Reuse the `WITH ANALYTICS (...)` body grammar verbatim so the
            // ADD form accepts the exact same outputs and options as CREATE.
            let views = self.parse_analytics_clause()?;
            Ok(AlterOperation::AddAnalytics(views))
        } else if self.consume(&Token::Drop)? {
            if !self.consume_ident_ci("ANALYTICS")? {
                return Err(ParseError::expected(
                    vec!["ANALYTICS"],
                    self.peek(),
                    self.position(),
                ));
            }
            let output_name = self.parse_analytics_output_name()?;
            let output = reddb_types::catalog::AnalyticsOutput::from_str(&output_name).ok_or_else(|| {
                ParseError::new(
                    format!(
                        "unknown analytics output '{output_name}': expected communities, components, or centrality"
                    ),
                    self.position(),
                )
            })?;
            Ok(AlterOperation::DropAnalytics(output))
        } else {
            Err(ParseError::expected(
                vec!["ADD", "DROP"],
                self.peek(),
                self.position(),
            ))
        }
    }

    /// Parse a single ALTER TABLE operation
    fn parse_alter_operation(&mut self, table_name: &str) -> Result<AlterOperation, ParseError> {
        if self.consume(&Token::Add)? {
            if self.consume_ident_ci("SUBSCRIPTION")? {
                // ADD SUBSCRIPTION name TO queue [REDACT (...)] [WHERE ...]
                let sub_name = self.expect_ident()?;
                let descriptor = self.parse_subscription_descriptor(table_name.to_string())?;
                Ok(AlterOperation::AddSubscription {
                    name: sub_name,
                    descriptor,
                })
            } else if self.consume_ident_ci("SIGNER")? {
                // ADD SIGNER 'hex_pubkey' — issue #522.
                let pubkey = self.parse_single_signer_hex()?;
                Ok(AlterOperation::AddSigner { pubkey })
            } else {
                // ADD COLUMN definition (COLUMN keyword is optional)
                let _ = self.consume(&Token::Column)?;
                let col_def = self.parse_column_def()?;
                Ok(AlterOperation::AddColumn(col_def))
            }
        } else if self.consume_ident_ci("REVOKE")? {
            // REVOKE SIGNER 'hex_pubkey' — issue #522.
            if !self.consume_ident_ci("SIGNER")? {
                return Err(ParseError::expected(
                    vec!["SIGNER"],
                    self.peek(),
                    self.position(),
                ));
            }
            let pubkey = self.parse_single_signer_hex()?;
            Ok(AlterOperation::RevokeSigner { pubkey })
        } else if self.consume(&Token::Drop)? {
            if self.consume_ident_ci("SUBSCRIPTION")? {
                // DROP SUBSCRIPTION name
                let sub_name = self.expect_ident()?;
                Ok(AlterOperation::DropSubscription { name: sub_name })
            } else {
                // DROP COLUMN name (COLUMN keyword is optional)
                let _ = self.consume(&Token::Column)?;
                let col_name = self.expect_ident()?;
                Ok(AlterOperation::DropColumn(col_name))
            }
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
            // ENABLE EVENTS | ENABLE ROW LEVEL SECURITY | ENABLE TENANCY ON (col)
            if self.consume_ident_ci("EVENTS")? {
                Ok(AlterOperation::EnableEvents(
                    self.parse_subscription_descriptor(table_name.to_string())?,
                ))
            } else if self.consume_ident_ci("TENANCY")? {
                self.expect(Token::On)?;
                self.expect(Token::LParen)?;
                // Dotted paths allowed (`metadata.tenant`, `payload.org`).
                let mut path = self.expect_ident_or_keyword()?;
                while self.consume(&Token::Dot)? {
                    let next = self.expect_ident_or_keyword()?;
                    path = format!("{path}.{next}");
                }
                self.expect(Token::RParen)?;
                Ok(AlterOperation::EnableTenancy { column: path })
            } else {
                self.expect(Token::Row)?;
                self.expect(Token::Level)?;
                self.expect(Token::Security)?;
                Ok(AlterOperation::EnableRowLevelSecurity)
            }
        } else if self.consume(&Token::Disable)? {
            // DISABLE EVENTS | DISABLE ROW LEVEL SECURITY | DISABLE TENANCY
            if self.consume_ident_ci("EVENTS")? {
                Ok(AlterOperation::DisableEvents)
            } else if self.consume_ident_ci("TENANCY")? {
                Ok(AlterOperation::DisableTenancy)
            } else {
                self.expect(Token::Row)?;
                self.expect(Token::Level)?;
                self.expect(Token::Security)?;
                Ok(AlterOperation::DisableRowLevelSecurity)
            }
        } else if self.consume(&Token::Set)? || self.consume_ident_ci("SET")? {
            // SET APPEND_ONLY = true|false | SET VERSIONED = true|false
            // SET RETENTION <duration> (issue #580)
            if self.consume_ident_ci("APPEND_ONLY")? {
                let on = self.parse_bool_assign()?;
                Ok(AlterOperation::SetAppendOnly(on))
            } else if self.consume_ident_ci("VERSIONED")? {
                let on = self.parse_bool_assign()?;
                Ok(AlterOperation::SetVersioned(on))
            } else if self.consume(&Token::Retention)? {
                // `SET RETENTION <duration>` — reuse the same float+unit
                // grammar the timeseries CREATE clause uses so `7 DAYS`,
                // `30 m`, `1 h`, `90 d` all parse identically.
                let value = self.parse_float()?;
                let unit = self.parse_duration_unit()?;
                Ok(AlterOperation::SetRetention {
                    duration_ms: (value * unit) as u64,
                })
            } else {
                Err(ParseError::expected(
                    vec!["APPEND_ONLY", "VERSIONED", "RETENTION"],
                    self.peek(),
                    self.position(),
                ))
            }
        } else if self.consume_ident_ci("UNSET")? {
            // `UNSET RETENTION` — clears the declarative retention policy.
            if self.consume(&Token::Retention)? {
                Ok(AlterOperation::UnsetRetention)
            } else {
                Err(ParseError::expected(
                    vec!["RETENTION"],
                    self.peek(),
                    self.position(),
                ))
            }
        } else {
            Err(ParseError::expected(
                vec![
                    "ADD", "DROP", "RENAME", "ATTACH", "DETACH", "ENABLE", "DISABLE", "SET",
                    "UNSET",
                ],
                self.peek(),
                self.position(),
            ))
        }
    }

    fn parse_subscription_descriptor(
        &mut self,
        source: String,
    ) -> Result<SubscriptionDescriptor, ParseError> {
        let mut ops_filter = Vec::new();
        if self.consume(&Token::LParen)? {
            loop {
                let op = if self.consume(&Token::Insert)? {
                    SubscriptionOperation::Insert
                } else if self.consume(&Token::Update)? {
                    SubscriptionOperation::Update
                } else if self.consume(&Token::Delete)? {
                    SubscriptionOperation::Delete
                } else {
                    return Err(ParseError::expected(
                        vec!["INSERT", "UPDATE", "DELETE"],
                        self.peek(),
                        self.position(),
                    ));
                };
                ops_filter.push(op);
                if !self.consume(&Token::Comma)? {
                    break;
                }
            }
            self.expect(Token::RParen)?;
        }

        let target_queue = if self.consume(&Token::To)? {
            self.expect_ident()?
        } else {
            format!("{source}_events")
        };

        let mut redact_fields = Vec::new();
        if self.consume_ident_ci("REDACT")? {
            self.expect(Token::LParen)?;
            loop {
                redact_fields.push(self.parse_dotted_redact_path()?);
                if !self.consume(&Token::Comma)? {
                    break;
                }
            }
            self.expect(Token::RParen)?;
        }

        let where_filter = if self.consume(&Token::Where)? {
            Some(self.collect_subscription_where_filter()?)
        } else {
            None
        };

        // ON ALL TENANTS: opt-in cluster-wide subscription (requires capability check at execution)
        let all_tenants = if self.consume(&Token::On)? {
            self.expect(Token::All)?;
            if !self.consume_ident_ci("TENANTS")? {
                return Err(ParseError::expected(
                    vec!["TENANTS"],
                    self.peek(),
                    self.position(),
                ));
            }
            true
        } else {
            false
        };

        // REQUIRES CAPABILITY '...' — parsed and discarded; enforcement is at execution time
        if self.consume_ident_ci("REQUIRES")? {
            self.consume_ident_ci("CAPABILITY")?;
            // consume the capability string literal token
            self.advance()?;
        }

        Ok(SubscriptionDescriptor {
            name: String::new(),
            source,
            target_queue,
            ops_filter,
            where_filter,
            redact_fields,
            enabled: true,
            all_tenants,
        })
    }

    /// Parse a dotted redact path: `field`, `obj.field`, `obj.*.field`, etc.
    fn parse_dotted_redact_path(&mut self) -> Result<String, ParseError> {
        let mut parts = Vec::new();
        if self.consume(&Token::Star)? {
            parts.push("*".to_string());
        } else {
            parts.push(self.expect_ident_or_keyword()?);
        }
        while self.consume(&Token::Dot)? {
            if self.consume(&Token::Star)? {
                parts.push("*".to_string());
            } else {
                parts.push(self.expect_ident_or_keyword()?);
            }
        }
        Ok(parts.join("."))
    }

    fn collect_subscription_where_filter(&mut self) -> Result<String, ParseError> {
        let mut parts = Vec::new();
        while !self.check(&Token::Eof) && !self.check(&Token::Comma) {
            parts.push(self.peek().to_string());
            self.advance()?;
        }
        if parts.is_empty() {
            return Err(ParseError::expected(
                vec!["predicate"],
                self.peek(),
                self.position(),
            ));
        }
        Ok(parts.join(" "))
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
        let name = self.expect_column_ident()?;
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
                // F-05: `option_name` is caller-controlled identifier text.
                // Render via `{:?}` so embedded CR/LF/NUL/quotes are escaped
                // before the message reaches downstream serialization sinks.
                format!(
                    "unsupported CREATE TABLE option {option_name:?}; supported options: TTL <duration> [ms|s|m|h|d] (e.g. `WITH TTL 30 m`)"
                ),
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
                    // F-05: render `other` via `{:?}` so caller-controlled
                    // bytes (CR / LF / NUL / quotes) are escaped before
                    // reaching downstream serialization sinks.
                    format!(
                        "unsupported TTL unit {other:?}; supported units: ms, s, m, h, d (e.g. `WITH TTL 30 m`)"
                    ),
                    self.position(),
                ));
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
    pub(crate) fn match_if_not_exists(&mut self) -> Result<bool, ParseError> {
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
    pub(crate) fn match_if_exists(&mut self) -> Result<bool, ParseError> {
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

/// Decode a 64-char lowercase/uppercase hex string into a 32-byte array.
/// Returns a human-readable error message on length or character violations.
/// Used by `SIGNED_BY` clause parsing (issue #520) to surface bad pubkeys
/// at parse-time rather than downstream in the engine.
fn decode_hex_32(s: &str) -> Result<[u8; 32], String> {
    if s.len() != 64 {
        return Err(format!("expected 64 hex chars, got {}", s.len()));
    }
    let mut out = [0u8; 32];
    let bytes = s.as_bytes();
    for i in 0..32 {
        let hi = hex_nibble(bytes[i * 2])?;
        let lo = hex_nibble(bytes[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

fn hex_nibble(c: u8) -> Result<u8, String> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(format!("non-hex char: {:?}", c as char)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reddb_types::catalog::{AnalyticsOutput, CollectionModel, SubscriptionOperation};

    fn parser(input: &str) -> Parser<'_> {
        Parser::new(input).unwrap_or_else(|err| panic!("failed to lex {input:?}: {err:?}"))
    }

    #[test]
    fn parse_create_table_body_parenthesized_options_and_trailing_clauses() {
        let QueryExpr::CreateTable(table) = parser(
            "IF NOT EXISTS events (id INT, tenant_meta TEXT) \
             WITH (tenant_by = 'tenant_id', append_only = true, timestamps = false) \
             PARTITION BY HASH (id) TENANT BY (tenant_meta.tenant)",
        )
        .parse_create_table_body()
        .expect("create table body") else {
            panic!("Expected CreateTableQuery");
        };

        assert_eq!(table.name, "events");
        assert!(table.if_not_exists);
        assert!(table.append_only);
        assert!(!table.timestamps);
        assert_eq!(table.tenant_by.as_deref(), Some("tenant_id"));
        assert_eq!(
            table
                .partition_by
                .as_ref()
                .map(|spec| (spec.kind, spec.column.as_str())),
            Some((PartitionKind::Hash, "id"))
        );

        let err = parser("bad (id INT) WITH (tenant_by = 42)")
            .parse_create_table_body()
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("WITH tenant_by expects a text literal"),
            "{err}"
        );
    }

    #[test]
    fn parse_keyed_bodies_cover_vault_analytics_and_dotted_drop_names() {
        let QueryExpr::CreateTable(vault) =
            parser("IF NOT EXISTS tenant.secrets WITH OWN MASTER KEY")
                .parse_create_keyed_body(CollectionModel::Vault)
                .expect("create vault")
        else {
            panic!("Expected CreateTableQuery");
        };
        assert_eq!(vault.collection_model, CollectionModel::Vault);
        assert_eq!(vault.name, "tenant.secrets");
        assert!(vault.if_not_exists);
        assert!(vault.vault_own_master_key);

        let QueryExpr::CreateTable(graph) = parser(
            "g WITH ANALYTICS (centrality (using = pagerank, max_iterations = 12, tolerance = 0.001))",
        )
        .parse_create_keyed_body(CollectionModel::Graph)
        .expect("create graph")
        else {
            panic!("Expected CreateTableQuery");
        };
        assert_eq!(graph.analytics_config.len(), 1);
        let view = &graph.analytics_config[0];
        assert_eq!(view.output, AnalyticsOutput::Centrality);
        assert_eq!(view.algorithm.as_deref(), Some("pagerank"));
        assert_eq!(view.max_iterations, Some(12));
        assert_eq!(view.tolerance, Some(0.001));

        let err = parser("g WITH OTHER")
            .parse_create_keyed_body(CollectionModel::Graph)
            .unwrap_err();
        assert!(err.to_string().contains("expected: ANALYTICS"), "{err}");

        assert!(parser("CREATE KV cache WITH ANALYTICS (components)")
            .parse()
            .unwrap_err()
            .to_string()
            .contains("Unexpected token after query"));

        let QueryExpr::DropKv(drop) = parser("IF EXISTS tenant.cache.*")
            .parse_drop_keyed_body(CollectionModel::Kv)
            .expect("drop kv")
        else {
            panic!("Expected DropKvQuery");
        };
        assert_eq!(drop.name, "tenant.cache.*");
        assert!(drop.if_exists);
        assert_eq!(drop.model, CollectionModel::Kv);
    }

    #[test]
    fn parse_collection_signed_by_list_and_errors() {
        let pk_a = "aa".repeat(32);
        let pk_b = "BB".repeat(32);
        let QueryExpr::CreateCollection(collection) =
            parser(&format!("signed KIND graph SIGNED_BY ('{pk_a}', '{pk_b}')"))
                .parse_create_collection_body()
                .expect("create collection")
        else {
            panic!("Expected CreateCollectionQuery");
        };
        assert_eq!(collection.allowed_signers, vec![[0xaau8; 32], [0xBBu8; 32]]);

        let err = parser("signed KIND graph SIGNED_BY (42)")
            .parse_create_collection_body()
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("string literal (ed25519 pubkey hex)"),
            "{err}"
        );

        let err = parser("signed KIND graph SIGNED_BY ('deadbeef')")
            .parse_create_collection_body()
            .unwrap_err();
        assert!(err.to_string().contains("expected 64 hex chars"), "{err}");
    }

    #[test]
    fn parse_alter_operations_cover_subscriptions_partitions_tenancy_and_signers() {
        let pk = "11".repeat(32);
        let QueryExpr::AlterTable(alter) = parser(&format!(
            "ALTER COLLECTION audit \
             ADD SUBSCRIPTION pii TO audit_events REDACT (payload.ssn, *.secret) WHERE level = 'warn', \
             DROP SUBSCRIPTION pii, \
             ADD SIGNER '{pk}', \
             REVOKE SIGNER '{pk}', \
             ATTACH PARTITION audit_2026 FOR VALUES FROM (2026) TO (2027), \
             DETACH PARTITION audit_2026, \
             ENABLE EVENTS (INSERT, UPDATE) TO table_events ON ALL TENANTS, \
             DISABLE EVENTS, \
             ENABLE TENANCY ON (metadata.tenant), \
             DISABLE TENANCY, \
             SET APPEND_ONLY = true, \
             SET VERSIONED = false, \
             SET RETENTION 2 h, \
             UNSET RETENTION"
        ))
        .parse_alter_table_query()
        .expect("alter collection")
        else {
            panic!("Expected AlterTableQuery");
        };

        assert_eq!(alter.name, "audit");
        assert_eq!(alter.operations.len(), 14);
        match &alter.operations[0] {
            AlterOperation::AddSubscription { name, descriptor } => {
                assert_eq!(name, "pii");
                assert_eq!(descriptor.target_queue, "audit_events");
                assert_eq!(descriptor.redact_fields, vec!["payload.ssn", "*.secret"]);
                assert_eq!(descriptor.where_filter.as_deref(), Some("LEVEL = 'warn'"));
            }
            other => panic!("expected AddSubscription, got {other:?}"),
        }
        assert!(matches!(
            &alter.operations[1],
            AlterOperation::DropSubscription { name } if name == "pii"
        ));
        assert!(matches!(
            &alter.operations[2],
            AlterOperation::AddSigner { pubkey } if *pubkey == [0x11; 32]
        ));
        assert!(matches!(
            &alter.operations[3],
            AlterOperation::RevokeSigner { pubkey } if *pubkey == [0x11; 32]
        ));
        assert!(matches!(
            &alter.operations[4],
            AlterOperation::AttachPartition { child, bound }
                if child == "audit_2026" && bound == "FROM ( 2026 ) TO ( 2027 )"
        ));
        assert!(matches!(
            &alter.operations[5],
            AlterOperation::DetachPartition { child } if child == "audit_2026"
        ));
        match &alter.operations[6] {
            AlterOperation::EnableEvents(descriptor) => {
                assert_eq!(
                    descriptor.ops_filter,
                    vec![SubscriptionOperation::Insert, SubscriptionOperation::Update]
                );
                assert_eq!(descriptor.target_queue, "table_events");
                assert!(descriptor.all_tenants);
            }
            other => panic!("expected EnableEvents, got {other:?}"),
        }
        assert!(matches!(
            &alter.operations[7],
            AlterOperation::DisableEvents
        ));
        assert!(matches!(
            &alter.operations[8],
            AlterOperation::EnableTenancy { column } if column == "METADATA.tenant"
        ));
        assert!(matches!(
            &alter.operations[9],
            AlterOperation::DisableTenancy
        ));
        assert!(matches!(
            &alter.operations[10],
            AlterOperation::SetAppendOnly(true)
        ));
        assert!(matches!(
            &alter.operations[11],
            AlterOperation::SetVersioned(false)
        ));
        assert!(matches!(
            &alter.operations[12],
            AlterOperation::SetRetention { duration_ms } if *duration_ms == 7_200_000
        ));
        assert!(matches!(
            &alter.operations[13],
            AlterOperation::UnsetRetention
        ));
    }

    #[test]
    fn parse_alter_graph_analytics_keyword_errors() {
        let err = parser("ALTER GRAPH g ADD centrality")
            .parse_alter_graph_query()
            .unwrap_err();
        assert!(err.to_string().contains("expected: ANALYTICS"), "{err}");

        let err = parser("ALTER GRAPH g DROP centrality")
            .parse_alter_graph_query()
            .unwrap_err();
        assert!(err.to_string().contains("expected: ANALYTICS"), "{err}");
    }

    #[test]
    fn decode_hex_32_reports_length_and_character_errors() {
        assert_eq!(decode_hex_32(&"0f".repeat(32)).unwrap(), [0x0f; 32]);
        assert_eq!(
            decode_hex_32("deadbeef").unwrap_err(),
            "expected 64 hex chars, got 8"
        );
        assert!(decode_hex_32(&"gg".repeat(32))
            .unwrap_err()
            .contains("non-hex char"));
    }
}
