//! Auth-related DDL parsers — `GRANT`, `REVOKE`, `ALTER USER`.
//!
//! These statements live alongside the rest of DDL but their AST nodes
//! and downstream dispatch are in `crate::auth::privileges`. The
//! parser is intentionally thin: every shape the user types maps
//! directly onto the [`GrantStmt`] / [`RevokeStmt`] / [`AlterUserStmt`]
//! AST so the runtime can apply the change in one match arm.
//!
//! Grammar (conservative — defers the long-tail PG modifiers):
//! ```text
//!   GRANT { privilege_list | ALL [PRIVILEGES] }
//!         [ ( column_list ) ]
//!         ON [ TABLE | SCHEMA | DATABASE | FUNCTION ] object_list
//!         TO grant_principal_list
//!         [ WITH GRANT OPTION ]
//!
//!   REVOKE [ GRANT OPTION FOR ] { privilege_list | ALL [PRIVILEGES] }
//!         [ ( column_list ) ]
//!         ON [ TABLE | SCHEMA | DATABASE | FUNCTION ] object_list
//!         FROM grant_principal_list
//!
//!   ALTER USER name
//!         [ VALID UNTIL 'timestamp' ]
//!         [ CONNECTION LIMIT n ]
//!         [ ENABLE | DISABLE ]
//!         [ SET search_path = 'csv' ]
//!         [ PASSWORD 'plaintext' ]
//! ```
//!
//! `name` accepts `tenant.username` form so a platform admin can target
//! a tenant-scoped account. `PUBLIC` is recognised as a reserved
//! principal.

use crate::storage::query::ast::{
    AlterUserAttribute, AlterUserStmt, GrantObject, GrantObjectKind, GrantPrincipalRef, GrantStmt,
    PolicyPrincipalRef, PolicyResourceRef, PolicyUserRef, QueryExpr, RevokeStmt,
};
use crate::storage::query::lexer::Token;
use crate::storage::query::parser::{ParseError, Parser};

impl<'a> Parser<'a> {
    /// Parse a `GRANT` statement. Caller must have already verified the
    /// current token is the `GRANT` ident (it is not a lexer keyword —
    /// the lexer maps it to `Token::Ident("GRANT")`).
    pub fn parse_grant_statement(&mut self) -> Result<GrantStmt, ParseError> {
        // Eat `GRANT`.
        self.advance()?;

        let (actions, all, columns) = self.parse_privilege_list()?;
        self.expect(Token::On)?;
        let object_kind = self.parse_grant_object_kind()?;
        let objects = self.parse_grant_object_list(&object_kind)?;
        self.expect(Token::To)?;
        let principals = self.parse_grant_principal_list()?;

        let with_grant_option = self.consume_grant_option_suffix()?;

        Ok(GrantStmt {
            actions,
            columns,
            object_kind,
            objects,
            principals,
            with_grant_option,
            all,
        })
    }

    /// Parse a `REVOKE` statement. Caller must have already verified the
    /// current token is the `REVOKE` ident.
    pub fn parse_revoke_statement(&mut self) -> Result<RevokeStmt, ParseError> {
        // Eat `REVOKE`.
        self.advance()?;

        // Optional `GRANT OPTION FOR`.
        let grant_option_for = self.consume_grant_option_for_prefix()?;

        let (actions, all, columns) = self.parse_privilege_list()?;
        self.expect(Token::On)?;
        let object_kind = self.parse_grant_object_kind()?;
        let objects = self.parse_grant_object_list(&object_kind)?;
        self.expect(Token::From)?;
        let principals = self.parse_grant_principal_list()?;

        Ok(RevokeStmt {
            actions,
            columns,
            object_kind,
            objects,
            principals,
            grant_option_for,
            all,
        })
    }

    /// Parse `ALTER USER name <attrs>`. Caller has just consumed
    /// `Token::Alter`.
    pub fn parse_alter_user_statement(&mut self) -> Result<AlterUserStmt, ParseError> {
        // `ALTER` was already consumed by the dispatcher; expect USER ident.
        if !self.consume_ident_ci("USER")? {
            return Err(ParseError::expected(
                vec!["USER"],
                self.peek(),
                self.position(),
            ));
        }
        let (tenant, username) = self.parse_user_name()?;

        let mut attributes = Vec::new();
        loop {
            if self.consume_ident_ci("VALID")? {
                if !self.consume_ident_ci("UNTIL")? {
                    return Err(ParseError::expected(
                        vec!["UNTIL"],
                        self.peek(),
                        self.position(),
                    ));
                }
                let ts = self.parse_string()?;
                attributes.push(AlterUserAttribute::ValidUntil(ts));
            } else if self.consume_ident_ci("CONNECTION")? {
                if !self.consume(&Token::Limit)? && !self.consume_ident_ci("LIMIT")? {
                    return Err(ParseError::expected(
                        vec!["LIMIT"],
                        self.peek(),
                        self.position(),
                    ));
                }
                let n = self.parse_integer()?;
                attributes.push(AlterUserAttribute::ConnectionLimit(n));
            } else if self.consume(&Token::Enable)? {
                attributes.push(AlterUserAttribute::Enable);
            } else if self.consume(&Token::Disable)? {
                attributes.push(AlterUserAttribute::Disable);
            } else if self.consume(&Token::Set)? {
                // SET search_path = 'csv'  |  SET search_path TO 'csv'
                if !self.consume_ident_ci("SEARCH_PATH")? {
                    return Err(ParseError::expected(
                        vec!["search_path"],
                        self.peek(),
                        self.position(),
                    ));
                }
                if !self.consume(&Token::Eq)? && !self.consume(&Token::To)? {
                    return Err(ParseError::expected(
                        vec!["="],
                        self.peek(),
                        self.position(),
                    ));
                }
                let value = self.parse_string()?;
                attributes.push(AlterUserAttribute::SetSearchPath(value));
            } else if self.consume(&Token::Add)? || self.consume_ident_ci("ADD")? {
                if !self.consume(&Token::Group)? && !self.consume_ident_ci("GROUP")? {
                    return Err(ParseError::expected(
                        vec!["GROUP"],
                        self.peek(),
                        self.position(),
                    ));
                }
                let group = self.expect_ident()?;
                attributes.push(AlterUserAttribute::AddGroup(group));
            } else if self.consume(&Token::Drop)? || self.consume_ident_ci("DROP")? {
                if !self.consume(&Token::Group)? && !self.consume_ident_ci("GROUP")? {
                    return Err(ParseError::expected(
                        vec!["GROUP"],
                        self.peek(),
                        self.position(),
                    ));
                }
                let group = self.expect_ident()?;
                attributes.push(AlterUserAttribute::DropGroup(group));
            } else if self.consume_ident_ci("PASSWORD")? {
                let pw = self.parse_string()?;
                attributes.push(AlterUserAttribute::Password(pw));
            } else {
                break;
            }
        }

        if attributes.is_empty() {
            return Err(ParseError::expected(
                vec![
                    "VALID",
                    "CONNECTION",
                    "ENABLE",
                    "DISABLE",
                    "SET",
                    "ADD",
                    "DROP",
                    "PASSWORD",
                ],
                self.peek(),
                self.position(),
            ));
        }

        Ok(AlterUserStmt {
            tenant,
            username,
            attributes,
        })
    }

    // -----------------------------------------------------------------
    // IAM policy DDL — CREATE / DROP / ATTACH / DETACH / SHOW / SIMULATE
    // -----------------------------------------------------------------

    /// Parse `CREATE POLICY '<id>' AS '<json>'`. Caller has consumed
    /// `CREATE POLICY` already and confirmed the next token is a
    /// string literal (the IAM-flavoured form). Returns the
    /// `QueryExpr::CreateIamPolicy` variant.
    pub fn parse_create_iam_policy_after_keywords(&mut self) -> Result<QueryExpr, ParseError> {
        let id = self.parse_string()?;
        if !self.consume(&Token::As)? && !self.consume_ident_ci("AS")? {
            return Err(ParseError::expected(
                vec!["AS"],
                self.peek(),
                self.position(),
            ));
        }
        let json = self.parse_string()?;
        Ok(QueryExpr::CreateIamPolicy { id, json })
    }

    /// Parse `DROP POLICY '<id>'`. Caller has consumed `DROP POLICY`
    /// and verified the next token is a string literal.
    pub fn parse_drop_iam_policy_after_keywords(&mut self) -> Result<QueryExpr, ParseError> {
        let id = self.parse_string()?;
        Ok(QueryExpr::DropIamPolicy { id })
    }

    /// Parse `ATTACH POLICY '<id>' TO { USER | GROUP } <name>`.
    /// Caller has consumed nothing — leading `ATTACH` is still on
    /// the token stream.
    pub fn parse_attach_policy(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // ATTACH
        if !self.consume(&Token::Policy)? && !self.consume_ident_ci("POLICY")? {
            return Err(ParseError::expected(
                vec!["POLICY"],
                self.peek(),
                self.position(),
            ));
        }
        let policy_id = self.parse_string()?;
        self.expect(Token::To)?;
        let principal = self.parse_iam_principal_kind()?;
        Ok(QueryExpr::AttachPolicy {
            policy_id,
            principal,
        })
    }

    /// Parse `DETACH POLICY '<id>' FROM { USER | GROUP } <name>`.
    pub fn parse_detach_policy(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // DETACH
        if !self.consume(&Token::Policy)? && !self.consume_ident_ci("POLICY")? {
            return Err(ParseError::expected(
                vec!["POLICY"],
                self.peek(),
                self.position(),
            ));
        }
        let policy_id = self.parse_string()?;
        self.expect(Token::From)?;
        let principal = self.parse_iam_principal_kind()?;
        Ok(QueryExpr::DetachPolicy {
            policy_id,
            principal,
        })
    }

    /// Parse `SIMULATE <name> ACTION <verb> ON <kind>:<name>`.
    pub fn parse_simulate_policy(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // ident "SIMULATE"
        let user = self.parse_iam_user_ref()?;
        if !self.consume_ident_ci("ACTION")? {
            return Err(ParseError::expected(
                vec!["ACTION"],
                self.peek(),
                self.position(),
            ));
        }
        let action = self.parse_iam_action_token()?;
        self.expect(Token::On)?;
        let resource = self.parse_iam_resource_ref()?;
        Ok(QueryExpr::SimulatePolicy {
            user,
            action,
            resource,
        })
    }

    /// Parse `SHOW POLICIES [FOR USER <name> | FOR GROUP <name>]` or
    /// `SHOW EFFECTIVE PERMISSIONS FOR <name> [ON <kind>:<name>]`.
    /// Caller has just consumed `SHOW`.
    pub fn parse_show_iam_after_show(&mut self) -> Result<Option<QueryExpr>, ParseError> {
        // Disambiguate: SHOW POLICIES vs SHOW EFFECTIVE
        if self.consume_ident_ci("POLICIES")? {
            // Optional FOR USER / FOR GROUP
            if self.consume(&Token::For)? || self.consume_ident_ci("FOR")? {
                let principal = self.parse_iam_principal_kind()?;
                return Ok(Some(QueryExpr::ShowPolicies {
                    filter: Some(principal),
                }));
            }
            return Ok(Some(QueryExpr::ShowPolicies { filter: None }));
        }
        if self.consume_ident_ci("EFFECTIVE")? {
            if !self.consume_ident_ci("PERMISSIONS")? {
                return Err(ParseError::expected(
                    vec!["PERMISSIONS"],
                    self.peek(),
                    self.position(),
                ));
            }
            if !self.consume(&Token::For)? && !self.consume_ident_ci("FOR")? {
                return Err(ParseError::expected(
                    vec!["FOR"],
                    self.peek(),
                    self.position(),
                ));
            }
            let user = self.parse_iam_user_ref()?;
            let resource = if self.consume(&Token::On)? || self.consume_ident_ci("ON")? {
                Some(self.parse_iam_resource_ref()?)
            } else {
                None
            };
            return Ok(Some(QueryExpr::ShowEffectivePermissions { user, resource }));
        }
        Ok(None)
    }

    // ----- helpers used by the IAM policy parsers -----

    fn parse_iam_principal_kind(&mut self) -> Result<PolicyPrincipalRef, ParseError> {
        if self.consume_ident_ci("USER")? {
            let user = self.parse_iam_user_ref()?;
            Ok(PolicyPrincipalRef::User(user))
        } else if self.consume(&Token::Group)? || self.consume_ident_ci("GROUP")? {
            let g = self.expect_ident()?;
            Ok(PolicyPrincipalRef::Group(g))
        } else {
            Err(ParseError::expected(
                vec!["USER", "GROUP"],
                self.peek(),
                self.position(),
            ))
        }
    }

    fn parse_iam_user_ref(&mut self) -> Result<PolicyUserRef, ParseError> {
        let (tenant, username) = self.parse_user_name()?;
        Ok(PolicyUserRef { tenant, username })
    }

    fn parse_iam_resource_ref(&mut self) -> Result<PolicyResourceRef, ParseError> {
        // Two accepted forms:
        //   * `<kind>:<name>` as one string literal
        //   * `<kind>:<dotted_name>` as `kind ':' part ('.' part)*`
        if matches!(self.peek(), Token::String(_)) {
            let raw = self.parse_string()?;
            let (kind, name) = raw.split_once(':').ok_or_else(|| {
                ParseError::new(
                    format!("resource must be `kind:name`, got `{raw}`"),
                    self.position(),
                )
            })?;
            return Ok(PolicyResourceRef {
                kind: kind.to_string(),
                name: name.to_string(),
            });
        }
        // Normalise both halves to lowercase so the kernel's allowlist
        // (`table`, `function`, …) lines up regardless of how the SQL
        // tokens were cased / promoted by the lexer.
        let kind = self.expect_ident_or_keyword()?.to_ascii_lowercase();
        if !self.consume(&Token::Colon)? {
            return Err(ParseError::expected(
                vec![":"],
                self.peek(),
                self.position(),
            ));
        }
        // Accept dotted resource names — `public.orders` arrives as
        // `Ident("public")`, `Dot`, `Ident("orders")` from the lexer.
        let mut name = self.expect_ident_or_keyword()?;
        while self.consume(&Token::Dot)? {
            let next = self.expect_ident_or_keyword()?;
            name.push('.');
            name.push_str(&next);
        }
        Ok(PolicyResourceRef { kind, name })
    }

    fn parse_iam_action_token(&mut self) -> Result<String, ParseError> {
        if matches!(self.peek(), Token::String(_)) {
            return self.parse_string();
        }
        // SELECT / INSERT / UPDATE / DELETE are real tokens; everything
        // else is exposed as an `Ident` by the lexer.
        match self.peek() {
            Token::Select => {
                self.advance()?;
                Ok("select".into())
            }
            Token::Insert => {
                self.advance()?;
                Ok("insert".into())
            }
            Token::Update => {
                self.advance()?;
                Ok("update".into())
            }
            Token::Delete => {
                self.advance()?;
                Ok("delete".into())
            }
            Token::Ident(_) => {
                let raw = self.expect_ident()?;
                Ok(raw.to_ascii_lowercase())
            }
            other => Err(ParseError::expected(
                vec!["action keyword"],
                other,
                self.position(),
            )),
        }
    }

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    /// Parse a comma-separated privilege list (`SELECT, INSERT, ...`)
    /// or `ALL [PRIVILEGES]`. Returns `(actions, is_all, columns?)`.
    /// Column-level lists are accepted at parse time but enforcement is
    /// deferred — see `auth::privileges` module docstring.
    fn parse_privilege_list(
        &mut self,
    ) -> Result<(Vec<String>, bool, Option<Vec<String>>), ParseError> {
        // ALL [PRIVILEGES]
        if self.consume(&Token::All)? || self.consume_ident_ci("ALL")? {
            let _ = self.consume_ident_ci("PRIVILEGES")?;
            let columns = self.parse_optional_column_list()?;
            return Ok((vec!["ALL".to_string()], true, columns));
        }

        // Privilege list.
        let mut actions = Vec::new();
        loop {
            actions.push(self.parse_privilege_keyword()?);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        let columns = self.parse_optional_column_list()?;
        Ok((actions, false, columns))
    }

    /// Recognise SELECT / INSERT / UPDATE / DELETE / TRUNCATE /
    /// REFERENCES / EXECUTE / USAGE. SELECT/INSERT/UPDATE/DELETE are
    /// real tokens; the rest are idents.
    fn parse_privilege_keyword(&mut self) -> Result<String, ParseError> {
        match self.peek() {
            Token::Select => {
                self.advance()?;
                Ok("SELECT".to_string())
            }
            Token::Insert => {
                self.advance()?;
                Ok("INSERT".to_string())
            }
            Token::Update => {
                self.advance()?;
                Ok("UPDATE".to_string())
            }
            Token::Delete => {
                self.advance()?;
                Ok("DELETE".to_string())
            }
            Token::Ident(name)
                if matches!(
                    name.to_ascii_uppercase().as_str(),
                    "TRUNCATE" | "REFERENCES" | "EXECUTE" | "USAGE"
                ) =>
            {
                let upper = name.to_ascii_uppercase();
                self.advance()?;
                Ok(upper)
            }
            other => Err(ParseError::expected(
                vec![
                    "SELECT",
                    "INSERT",
                    "UPDATE",
                    "DELETE",
                    "TRUNCATE",
                    "REFERENCES",
                    "EXECUTE",
                    "USAGE",
                ],
                other,
                self.position(),
            )),
        }
    }

    /// Optional `( col1, col2, ... )` after a privilege list. Returns
    /// `None` when the next token isn't `(`.
    fn parse_optional_column_list(&mut self) -> Result<Option<Vec<String>>, ParseError> {
        if !self.check(&Token::LParen) {
            return Ok(None);
        }
        self.expect(Token::LParen)?;
        let mut cols = Vec::new();
        loop {
            cols.push(self.expect_ident()?);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        self.expect(Token::RParen)?;
        Ok(Some(cols))
    }

    /// Parse the optional `[ TABLE | SCHEMA | DATABASE | FUNCTION ]`
    /// keyword between `ON` and the object list. Defaults to `TABLE`
    /// when absent (matches PG).
    fn parse_grant_object_kind(&mut self) -> Result<GrantObjectKind, ParseError> {
        if self.consume(&Token::Table)? {
            Ok(GrantObjectKind::Table)
        } else if self.consume(&Token::Schema)? {
            Ok(GrantObjectKind::Schema)
        } else if self.consume_ident_ci("DATABASE")? {
            Ok(GrantObjectKind::Database)
        } else if self.consume_ident_ci("FUNCTION")? {
            Ok(GrantObjectKind::Function)
        } else {
            // Default: TABLE
            Ok(GrantObjectKind::Table)
        }
    }

    /// Parse a comma-separated list of `[schema.]name` objects.
    fn parse_grant_object_list(
        &mut self,
        kind: &GrantObjectKind,
    ) -> Result<Vec<GrantObject>, ParseError> {
        let mut out = Vec::new();
        loop {
            // DATABASE objects use the database name as the object —
            // accept a single ident.
            if matches!(kind, GrantObjectKind::Database) {
                let name = self.expect_ident()?;
                out.push(GrantObject { schema: None, name });
            } else {
                let first = self.expect_ident()?;
                let (schema, name) = if self.consume(&Token::Dot)? {
                    let second = self.expect_ident_or_keyword()?;
                    (Some(first), second)
                } else {
                    (None, first)
                };
                out.push(GrantObject { schema, name });
            }
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        Ok(out)
    }

    /// Parse a comma-separated principal list. Each principal is one of:
    ///   * `PUBLIC` — every authenticated user.
    ///   * `GROUP groupname` — role-as-group (parsed, not enforced).
    ///   * `username` or `tenant.username` — a specific user.
    fn parse_grant_principal_list(&mut self) -> Result<Vec<GrantPrincipalRef>, ParseError> {
        let mut out = Vec::new();
        loop {
            if self.consume_ident_ci("PUBLIC")? {
                out.push(GrantPrincipalRef::Public);
            } else if self.consume_ident_ci("GROUP")? {
                let g = self.expect_ident()?;
                out.push(GrantPrincipalRef::Group(g));
            } else {
                let (tenant, name) = self.parse_user_name()?;
                out.push(GrantPrincipalRef::User { tenant, name });
            }
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        Ok(out)
    }

    /// Parse a `user` or `tenant.user` form. Returns `(tenant, name)`.
    fn parse_user_name(&mut self) -> Result<(Option<String>, String), ParseError> {
        let first = self.expect_ident()?;
        if self.consume(&Token::Dot)? {
            let name = self.expect_ident()?;
            Ok((Some(first), name))
        } else {
            Ok((None, first))
        }
    }

    /// Recognise the optional `WITH GRANT OPTION` suffix on a GRANT.
    fn consume_grant_option_suffix(&mut self) -> Result<bool, ParseError> {
        if self.consume(&Token::With)? {
            if !self.consume_ident_ci("GRANT")? {
                return Err(ParseError::expected(
                    vec!["GRANT"],
                    self.peek(),
                    self.position(),
                ));
            }
            if !self.consume_ident_ci("OPTION")? {
                return Err(ParseError::expected(
                    vec!["OPTION"],
                    self.peek(),
                    self.position(),
                ));
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Recognise the optional `GRANT OPTION FOR` prefix on a REVOKE.
    fn consume_grant_option_for_prefix(&mut self) -> Result<bool, ParseError> {
        // `GRANT` is an ident, not a keyword — we must peek the ident
        // text without consuming until we know the full prefix matches.
        let saved_pos = self.position();
        if !matches!(self.peek(), Token::Ident(s) if s.eq_ignore_ascii_case("GRANT")) {
            return Ok(false);
        }
        // Consume GRANT.
        self.advance()?;
        if !self.consume_ident_ci("OPTION")? {
            // Not the prefix we expected — but `REVOKE GRANT ...`
            // makes no other sense, so this is a parse error rather
            // than a non-match.
            return Err(ParseError::expected(vec!["OPTION"], self.peek(), saved_pos));
        }
        if !self.consume(&Token::For)? && !self.consume_ident_ci("FOR")? {
            return Err(ParseError::expected(vec!["FOR"], self.peek(), saved_pos));
        }
        Ok(true)
    }
}
