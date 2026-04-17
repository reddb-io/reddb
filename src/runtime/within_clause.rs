//! `WITHIN TENANT '<id>' [USER '<u>'] [AS ROLE '<r>'] <stmt>` —
//! a per-statement scope override for tenant + auth identity.
//!
//! Designed for SaaS deployments where one process / one connection
//! pool serves many tenants. The clause carries the scope inline with
//! the query, so:
//!   * no thread-local state survives the call
//!   * connection pools cannot leak tenant context between checkouts
//!   * async runtimes that move tasks between threads stay correct
//!     (the scope lives in a stack pushed/popped by the same execute
//!     call — no `.await` in between)
//!   * clients can use prepared statements normally
//!
//! Values: string literal (`'acme'`) or `NULL` (clears just that field).

use crate::storage::query::lexer::{Lexer, Token};

/// Tri-state for a single overridable field. `Inherit` means the
/// `WITHIN` clause didn't mention this field and the runtime should
/// fall through to its prior source (session-level or auth-installed).
/// `Clear` means the clause explicitly set the field to NULL — this
/// must hide the inherited value, not fall through. `Set(v)` carries
/// the literal value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldOverride {
    Inherit,
    Clear,
    Set(String),
}

impl Default for FieldOverride {
    fn default() -> Self {
        Self::Inherit
    }
}

impl FieldOverride {
    /// Is this override active — i.e. it should win over any
    /// inherited value?
    pub fn is_active(&self) -> bool {
        !matches!(self, Self::Inherit)
    }

    /// Resolve the override against an inherited value. `Inherit`
    /// passes the inherited through; `Clear` returns `None`; `Set`
    /// returns its literal.
    pub fn resolve(&self, inherited: Option<String>) -> Option<String> {
        match self {
            Self::Inherit => inherited,
            Self::Clear => None,
            Self::Set(v) => Some(v.clone()),
        }
    }
}

/// Per-statement scope override extracted from a `WITHIN ...` prefix.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeOverride {
    pub tenant: FieldOverride,
    pub user: FieldOverride,
    pub role: FieldOverride,
}

impl ScopeOverride {
    pub fn is_empty(&self) -> bool {
        !self.tenant.is_active() && !self.user.is_active() && !self.role.is_active()
    }
}

/// Try to recognise a `WITHIN TENANT ... <stmt>` prefix at the start
/// of `input`. Returns the parsed scope plus the remainder slice
/// (the inner statement), or `None` when the input doesn't start with
/// `WITHIN`. A malformed `WITHIN ...` clause returns `Err`.
pub fn try_strip_within_prefix(input: &str) -> Result<Option<(ScopeOverride, &str)>, String> {
    let trimmed = input.trim_start();
    let first_word = trimmed
        .split(|c: char| c.is_whitespace())
        .next()
        .unwrap_or("");
    if !first_word.eq_ignore_ascii_case("WITHIN") {
        return Ok(None);
    }

    let mut lexer = Lexer::new(input);
    expect_ident(&mut lexer, "WITHIN")?;

    let mut scope = ScopeOverride::default();
    let mut tenant_seen = false;

    loop {
        let spanned = lexer.next_token().map_err(|e| e.to_string())?;
        match spanned.token {
            Token::Ident(ref name) if name.eq_ignore_ascii_case("TENANT") => {
                if tenant_seen {
                    return Err("duplicate TENANT clause in WITHIN prefix".into());
                }
                tenant_seen = true;
                scope.tenant = parse_value(&mut lexer)?;
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("USER") => {
                if scope.user.is_active() {
                    return Err("duplicate USER clause in WITHIN prefix".into());
                }
                scope.user = parse_value(&mut lexer)?;
            }
            Token::As => {
                expect_ident(&mut lexer, "ROLE")?;
                if scope.role.is_active() {
                    return Err("duplicate AS ROLE clause in WITHIN prefix".into());
                }
                scope.role = parse_value(&mut lexer)?;
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("ROLE") => {
                if scope.role.is_active() {
                    return Err("duplicate ROLE clause in WITHIN prefix".into());
                }
                scope.role = parse_value(&mut lexer)?;
            }
            // Anything else is the start of the inner statement — peel
            // back to the offset where this token began so the inner
            // query string slice keeps the leading keyword intact.
            _ => {
                if !tenant_seen {
                    return Err(
                        "WITHIN clause requires at least TENANT '<id>' (or NULL)".into()
                    );
                }
                let offset = spanned.start.offset as usize;
                if offset > input.len() {
                    return Err("internal: WITHIN clause offset out of range".into());
                }
                let inner = input[offset..].trim_start();
                if inner.is_empty() {
                    return Err("WITHIN clause has no inner statement to execute".into());
                }
                return Ok(Some((scope, inner)));
            }
        }
    }
}

fn expect_ident(lexer: &mut Lexer<'_>, expected: &str) -> Result<(), String> {
    let spanned = lexer.next_token().map_err(|e| e.to_string())?;
    match spanned.token {
        Token::Ident(name) if name.eq_ignore_ascii_case(expected) => Ok(()),
        other => Err(format!(
            "expected `{expected}` in WITHIN prefix, got {other:?}"
        )),
    }
}

fn parse_value(lexer: &mut Lexer<'_>) -> Result<FieldOverride, String> {
    let spanned = lexer.next_token().map_err(|e| e.to_string())?;
    match spanned.token {
        Token::String(s) => Ok(FieldOverride::Set(s)),
        Token::Null => Ok(FieldOverride::Clear),
        other => Err(format!(
            "WITHIN clause value must be a string literal or NULL, got {other:?}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_within_prefix_returns_none() {
        assert!(try_strip_within_prefix("SELECT * FROM x")
            .unwrap()
            .is_none());
        assert!(try_strip_within_prefix("  SELECT * FROM x")
            .unwrap()
            .is_none());
    }

    #[test]
    fn parses_tenant_only() {
        let (scope, inner) =
            try_strip_within_prefix("WITHIN TENANT 'acme' SELECT * FROM x")
                .unwrap()
                .unwrap();
        assert_eq!(scope.tenant, FieldOverride::Set("acme".into()));
        assert_eq!(scope.user, FieldOverride::Inherit);
        assert_eq!(scope.role, FieldOverride::Inherit);
        assert_eq!(inner, "SELECT * FROM x");
    }

    #[test]
    fn parses_full_clause() {
        let (scope, inner) = try_strip_within_prefix(
            "WITHIN TENANT 'acme' USER 'filipe' AS ROLE 'admin' SELECT * FROM x",
        )
        .unwrap()
        .unwrap();
        assert_eq!(scope.tenant, FieldOverride::Set("acme".into()));
        assert_eq!(scope.user, FieldOverride::Set("filipe".into()));
        assert_eq!(scope.role, FieldOverride::Set("admin".into()));
        assert_eq!(inner, "SELECT * FROM x");
    }

    #[test]
    fn null_tenant_clears() {
        let (scope, _) = try_strip_within_prefix("WITHIN TENANT NULL SELECT 1")
            .unwrap()
            .unwrap();
        assert_eq!(scope.tenant, FieldOverride::Clear);
    }

    #[test]
    fn rejects_missing_tenant() {
        assert!(try_strip_within_prefix("WITHIN USER 'x' SELECT 1").is_err());
    }

    #[test]
    fn rejects_duplicate_clause() {
        assert!(try_strip_within_prefix(
            "WITHIN TENANT 'a' TENANT 'b' SELECT 1"
        )
        .is_err());
    }

    #[test]
    fn case_insensitive() {
        let (scope, inner) =
            try_strip_within_prefix("within tenant 'acme' select * from x")
                .unwrap()
                .unwrap();
        assert_eq!(scope.tenant, FieldOverride::Set("acme".into()));
        assert_eq!(inner, "select * from x");
    }
}
