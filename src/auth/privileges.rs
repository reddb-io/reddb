//! Granular RBAC: per-table/action privileges plus user attributes.
//!
//! RedDB's baseline auth model exposes three fixed roles (`Read` / `Write`
//! / `Admin`). Postgres users expect finer-grained `GRANT`/`REVOKE` on
//! individual tables, schemas, and functions, plus account attributes
//! such as `VALID UNTIL` and `CONNECTION LIMIT`. This module adds those
//! pieces alongside the existing role model — the legacy fast-path still
//! applies when no grants exist for the principal (back-compat for
//! deployments that pre-date this work).
//!
//! # Resolution algorithm
//! `check_grant(ctx, action, resource)` walks the ACL in this order:
//!
//! 1. `Admin` role bypasses every check. (Fast path.)
//! 2. If the principal has zero grants AND zero `Public` grants exist,
//!    fall back to the legacy `Role::Write` / `Role::Read` rule. This is
//!    the back-compat shim for already-deployed instances that have
//!    not issued a single GRANT — they keep working without surprise.
//! 3. Otherwise scan the principal's own grants, then group memberships
//!    (reserved for future role-as-group support), then `Public` grants.
//!    A grant matches when:
//!      * its `Resource` equals the requested `Resource`, OR the request
//!        is for a `Table` whose schema matches a `Schema(s)` grant, OR
//!        a `Database` grant covers everything.
//!      * its `actions` set contains the requested action OR `All`.
//! 4. If any matching grant is found, allow; otherwise deny (fail-closed).
//!
//! # Tenant scoping
//! Grants carry an implicit tenant that is taken from the user's tenant
//! at GRANT time (the user record owns the tenant; see `AuthStore`).
//! `check_grant` rejects cross-tenant matches: if the request's tenant
//! does not equal the grant's tenant the grant is skipped. A `None`
//! tenant on either side is treated as the global/platform tenant and
//! only matches another `None`.

use std::collections::{BTreeSet, HashMap};

use super::{Role, UserId};

// ---------------------------------------------------------------------------
// Action
// ---------------------------------------------------------------------------

/// SQL action covered by a grant. Mirrors PG's privilege vocabulary.
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Ord, PartialOrd)]
pub enum Action {
    /// SELECT on a table / view.
    Select,
    /// INSERT on a table.
    Insert,
    /// UPDATE on a table.
    Update,
    /// DELETE on a table.
    Delete,
    /// TRUNCATE TABLE.
    Truncate,
    /// REFERENCES (FK target privilege).
    References,
    /// EXECUTE on a function.
    Execute,
    /// USAGE on a schema or sequence.
    Usage,
    /// All privileges (PG `ALL [PRIVILEGES]`).
    All,
}

impl Action {
    /// Parse a privilege keyword (case-insensitive). Returns `None` for
    /// unrecognised tokens so the parser can produce a precise error.
    pub fn from_keyword(kw: &str) -> Option<Self> {
        match kw.to_ascii_uppercase().as_str() {
            "SELECT" => Some(Self::Select),
            "INSERT" => Some(Self::Insert),
            "UPDATE" => Some(Self::Update),
            "DELETE" => Some(Self::Delete),
            "TRUNCATE" => Some(Self::Truncate),
            "REFERENCES" => Some(Self::References),
            "EXECUTE" => Some(Self::Execute),
            "USAGE" => Some(Self::Usage),
            "ALL" => Some(Self::All),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Select => "SELECT",
            Self::Insert => "INSERT",
            Self::Update => "UPDATE",
            Self::Delete => "DELETE",
            Self::Truncate => "TRUNCATE",
            Self::References => "REFERENCES",
            Self::Execute => "EXECUTE",
            Self::Usage => "USAGE",
            Self::All => "ALL",
        }
    }
}

// ---------------------------------------------------------------------------
// Resource
// ---------------------------------------------------------------------------

/// Object the grant covers. Schemas and tables form a hierarchy: a grant
/// on `Schema("public")` implicitly covers every table in `public`.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Ord, PartialOrd)]
pub enum Resource {
    /// Cluster-wide (PG: `ON DATABASE`).
    Database,
    /// All objects within a schema.
    Schema(String),
    /// A specific table (optionally schema-qualified).
    Table {
        schema: Option<String>,
        table: String,
    },
    /// A function (UDF / aggregate).
    Function {
        schema: Option<String>,
        name: String,
    },
}

impl Resource {
    /// Construct a `Table` resource from a possibly dotted name.
    /// `"public.users"` → `Table { schema: Some("public"), table: "users" }`.
    /// `"users"` → `Table { schema: None, table: "users" }`.
    pub fn table_from_name(name: &str) -> Self {
        match name.split_once('.') {
            Some((schema, table)) => Self::Table {
                schema: Some(schema.to_string()),
                table: table.to_string(),
            },
            None => Self::Table {
                schema: None,
                table: name.to_string(),
            },
        }
    }

    /// Does `self` (a grant resource) cover `requested`?
    /// `Database` covers everything; `Schema(s)` covers any
    /// `Table { schema: Some(s), .. }`; everything else requires
    /// equality.
    pub fn covers(&self, requested: &Resource) -> bool {
        match (self, requested) {
            (Resource::Database, _) => true,
            (Resource::Schema(s), Resource::Table { schema, .. }) => {
                schema.as_deref() == Some(s.as_str())
            }
            (Resource::Schema(s), Resource::Function { schema, .. }) => {
                schema.as_deref() == Some(s.as_str())
            }
            (a, b) => a == b,
        }
    }
}

// ---------------------------------------------------------------------------
// GrantPrincipal
// ---------------------------------------------------------------------------

/// Who the grant applies to.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum GrantPrincipal {
    /// A specific user (tenant-scoped via `UserId`).
    User(UserId),
    /// A named group (role-as-group, future expansion).
    Group(String),
    /// Everyone — equivalent to PG's `PUBLIC`.
    Public,
}

impl GrantPrincipal {
    pub fn as_user(&self) -> Option<&UserId> {
        if let GrantPrincipal::User(u) = self {
            Some(u)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Grant
// ---------------------------------------------------------------------------

/// A single GRANT row.
#[derive(Debug, Clone)]
pub struct Grant {
    pub principal: GrantPrincipal,
    pub resource: Resource,
    pub actions: BTreeSet<Action>,
    /// `WITH GRANT OPTION` — recipient may re-grant.
    pub with_grant_option: bool,
    /// Username of the grantor.
    pub granted_by: String,
    /// Timestamp (ms since epoch) for audit.
    pub granted_at: u128,
    /// Tenant the grant lives in. `None` = global/platform tenant.
    pub tenant: Option<String>,
    /// Optional column list for column-level privileges. `None`
    /// means the grant covers all columns. Storage-only (the
    /// AST/parser populates it; enforcement is deferred — see
    /// the module docstring).
    pub columns: Option<Vec<String>>,
}

impl Grant {
    /// Convenience constructor — most callers want a single-action grant.
    pub fn single(
        principal: GrantPrincipal,
        resource: Resource,
        action: Action,
        granted_by: String,
        granted_at: u128,
        tenant: Option<String>,
    ) -> Self {
        let mut actions = BTreeSet::new();
        actions.insert(action);
        Self {
            principal,
            resource,
            actions,
            with_grant_option: false,
            granted_by,
            granted_at,
            tenant,
            columns: None,
        }
    }

    /// True iff this grant authorises `action` on `resource` for the
    /// given `tenant`.
    pub fn authorises(&self, action: Action, resource: &Resource, tenant: Option<&str>) -> bool {
        if self.tenant.as_deref() != tenant {
            return false;
        }
        if !self.resource.covers(resource) {
            return false;
        }
        self.actions.contains(&action) || self.actions.contains(&Action::All)
    }
}

// ---------------------------------------------------------------------------
// UserAttributes
// ---------------------------------------------------------------------------

/// Per-user account attributes that PG exposes via `ALTER USER`. None of
/// these are tied to the underlying password hash — they live alongside
/// the `User` record so they can be modified without rotating credentials.
#[derive(Debug, Clone, Default)]
pub struct UserAttributes {
    /// Account expiry (ms since epoch). Logins after this point are
    /// rejected with `InvalidCredentials`.
    pub valid_until: Option<u128>,
    /// Maximum concurrent sessions. `None` = unlimited.
    pub connection_limit: Option<u32>,
    /// `SET search_path = ...` style default applied at connection time.
    pub search_path: Option<String>,
    /// IAM policy groups this user belongs to.
    pub groups: Vec<String>,
}

// ---------------------------------------------------------------------------
// AuthzContext + check_grant
// ---------------------------------------------------------------------------

/// Caller identity threaded through the privilege check.
#[derive(Debug, Clone)]
pub struct AuthzContext<'a> {
    /// Username of the principal making the request.
    pub principal: &'a str,
    /// The principal's effective role (legacy fast-path input).
    pub effective_role: Role,
    /// Tenant the request runs under. `None` = global/platform tenant.
    pub tenant: Option<&'a str>,
}

/// Privilege-check error.
#[derive(Debug, Clone)]
pub enum AuthzError {
    /// Action denied by the privilege engine.
    PermissionDenied {
        action: Action,
        resource: Resource,
        principal: String,
    },
    /// Tenant mismatch — request tenant does not match grant tenant.
    CrossTenantDenied { action: Action, principal: String },
}

impl std::fmt::Display for AuthzError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthzError::PermissionDenied {
                action,
                resource,
                principal,
            } => write!(
                f,
                "permission denied: principal={principal} action={a} resource={r:?}",
                a = action.as_str(),
                r = resource
            ),
            AuthzError::CrossTenantDenied { action, principal } => write!(
                f,
                "cross-tenant denied: principal={principal} action={a}",
                a = action.as_str()
            ),
        }
    }
}

impl std::error::Error for AuthzError {}

/// Inputs to `check_grant`. Decoupled from `AuthStore` so unit tests
/// can construct fixtures without booting a vault.
pub struct GrantsView<'a> {
    /// Grants owned directly by the principal.
    pub user_grants: &'a [Grant],
    /// Grants applied to PUBLIC (everyone).
    pub public_grants: &'a [Grant],
}

/// Core privilege check. See module docstring for the resolution order.
///
/// **Fail-closed.** Any internal ambiguity (e.g. an unparseable resource)
/// produces `Err(PermissionDenied)` — never an `Ok`.
pub fn check_grant(
    ctx: &AuthzContext<'_>,
    action: Action,
    resource: &Resource,
    grants: &GrantsView<'_>,
) -> Result<(), AuthzError> {
    // 1. Admin bypass — keeps the existing 3-role model intact for
    //    operators who haven't switched to per-object grants yet.
    if ctx.effective_role == Role::Admin {
        return Ok(());
    }

    // 2. Legacy fallback: when no grants are configured anywhere, defer
    //    to the role-wide rule so an upgraded instance keeps working.
    let no_grants_at_all = grants.user_grants.is_empty() && grants.public_grants.is_empty();
    if no_grants_at_all {
        let allowed = match action {
            Action::Select | Action::Usage | Action::Execute => ctx.effective_role >= Role::Read,
            Action::Insert | Action::Update | Action::Delete | Action::Truncate => {
                ctx.effective_role >= Role::Write
            }
            Action::References => ctx.effective_role >= Role::Read,
            // ALL only makes sense for Admin, which already returned above.
            Action::All => false,
        };
        return if allowed {
            Ok(())
        } else {
            Err(AuthzError::PermissionDenied {
                action,
                resource: resource.clone(),
                principal: ctx.principal.to_string(),
            })
        };
    }

    // 3. Walk per-user grants, then PUBLIC grants. First match wins.
    let scan = |g: &Grant| g.authorises(action, resource, ctx.tenant);
    if grants.user_grants.iter().any(scan) || grants.public_grants.iter().any(scan) {
        return Ok(());
    }

    Err(AuthzError::PermissionDenied {
        action,
        resource: resource.clone(),
        principal: ctx.principal.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Pre-resolved permission cache (per user)
// ---------------------------------------------------------------------------

/// Compact (resource, action) lookup pre-built from a user's grants
/// + PUBLIC grants. The privilege check first probes this cache before
/// falling back to the linear scan above. Invalidated on every
/// GRANT / REVOKE / ALTER USER.
#[derive(Debug, Default, Clone)]
pub struct PermissionCache {
    /// Set of (resource, action) tuples authorised. `Action::All` is
    /// expanded into one entry per concrete action so lookups stay O(1).
    entries: HashMap<(Resource, Action), ()>,
}

impl PermissionCache {
    pub fn build(user_grants: &[Grant], public_grants: &[Grant]) -> Self {
        let mut entries: HashMap<(Resource, Action), ()> = HashMap::new();
        for g in user_grants.iter().chain(public_grants.iter()) {
            for a in concrete_actions(&g.actions) {
                entries.insert((g.resource.clone(), a), ());
            }
        }
        Self { entries }
    }

    /// O(1) cache check. Returns `true` if the cache contains an exact
    /// (resource, action) match. Caller must still consult `check_grant`
    /// for hierarchical lookups (Schema covers Table, etc.).
    pub fn allows(&self, resource: &Resource, action: Action) -> bool {
        self.entries.contains_key(&(resource.clone(), action))
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn concrete_actions(set: &BTreeSet<Action>) -> Vec<Action> {
    if set.contains(&Action::All) {
        return vec![
            Action::Select,
            Action::Insert,
            Action::Update,
            Action::Delete,
            Action::Truncate,
            Action::References,
            Action::Execute,
            Action::Usage,
        ];
    }
    set.iter().copied().collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn t(name: &str) -> Resource {
        Resource::Table {
            schema: None,
            table: name.into(),
        }
    }

    fn grant_for(user: &str, res: Resource, action: Action) -> Grant {
        Grant::single(
            GrantPrincipal::User(UserId::platform(user)),
            res,
            action,
            "admin".into(),
            0,
            None,
        )
    }

    fn ctx<'a>(user: &'a str, role: Role) -> AuthzContext<'a> {
        AuthzContext {
            principal: user,
            effective_role: role,
            tenant: None,
        }
    }

    #[test]
    fn admin_bypasses_every_check() {
        let view = GrantsView {
            user_grants: &[],
            public_grants: &[],
        };
        let ctx = ctx("root", Role::Admin);
        assert!(check_grant(&ctx, Action::Delete, &t("anything"), &view).is_ok());
    }

    #[test]
    fn legacy_fallback_when_no_grants_exist() {
        let view = GrantsView {
            user_grants: &[],
            public_grants: &[],
        };
        // Read role can SELECT, can't INSERT.
        assert!(check_grant(&ctx("alice", Role::Read), Action::Select, &t("u"), &view).is_ok());
        assert!(check_grant(&ctx("alice", Role::Read), Action::Insert, &t("u"), &view).is_err());
        // Write role can INSERT.
        assert!(check_grant(&ctx("bob", Role::Write), Action::Insert, &t("u"), &view).is_ok());
    }

    #[test]
    fn user_grant_allows_action() {
        let g = grant_for("alice", t("orders"), Action::Select);
        let view = GrantsView {
            user_grants: std::slice::from_ref(&g),
            public_grants: &[],
        };
        assert!(check_grant(
            &ctx("alice", Role::Read),
            Action::Select,
            &t("orders"),
            &view
        )
        .is_ok());
        // Different table — denied.
        assert!(check_grant(
            &ctx("alice", Role::Read),
            Action::Select,
            &t("hosts"),
            &view
        )
        .is_err());
        // Same table, different action — denied.
        assert!(check_grant(
            &ctx("alice", Role::Read),
            Action::Insert,
            &t("orders"),
            &view
        )
        .is_err());
    }

    #[test]
    fn schema_grant_covers_tables_in_schema() {
        let g = Grant::single(
            GrantPrincipal::User(UserId::platform("alice")),
            Resource::Schema("acme".into()),
            Action::Select,
            "admin".into(),
            0,
            None,
        );
        let view = GrantsView {
            user_grants: std::slice::from_ref(&g),
            public_grants: &[],
        };
        let r = Resource::Table {
            schema: Some("acme".into()),
            table: "x".into(),
        };
        assert!(check_grant(&ctx("alice", Role::Read), Action::Select, &r, &view).is_ok());
        // Different schema — denied.
        let bad = Resource::Table {
            schema: Some("public".into()),
            table: "x".into(),
        };
        assert!(check_grant(&ctx("alice", Role::Read), Action::Select, &bad, &view).is_err());
    }

    #[test]
    fn public_grant_applies_to_everyone() {
        let g = Grant::single(
            GrantPrincipal::Public,
            t("welcome"),
            Action::Select,
            "admin".into(),
            0,
            None,
        );
        let view = GrantsView {
            user_grants: &[],
            public_grants: std::slice::from_ref(&g),
        };
        assert!(check_grant(
            &ctx("anyone", Role::Read),
            Action::Select,
            &t("welcome"),
            &view
        )
        .is_ok());
    }

    #[test]
    fn all_action_authorises_everything() {
        let mut actions = BTreeSet::new();
        actions.insert(Action::All);
        let g = Grant {
            principal: GrantPrincipal::User(UserId::platform("alice")),
            resource: t("orders"),
            actions,
            with_grant_option: true,
            granted_by: "admin".into(),
            granted_at: 0,
            tenant: None,
            columns: None,
        };
        let view = GrantsView {
            user_grants: std::slice::from_ref(&g),
            public_grants: &[],
        };
        for a in [
            Action::Select,
            Action::Insert,
            Action::Update,
            Action::Delete,
            Action::Truncate,
        ] {
            assert!(check_grant(&ctx("alice", Role::Read), a, &t("orders"), &view).is_ok());
        }
    }

    #[test]
    fn cross_tenant_grant_does_not_match() {
        let g = Grant::single(
            GrantPrincipal::User(UserId::platform("alice")),
            t("orders"),
            Action::Select,
            "admin".into(),
            0,
            Some("acme".into()),
        );
        let view = GrantsView {
            user_grants: std::slice::from_ref(&g),
            public_grants: &[],
        };
        let mut ctx = ctx("alice", Role::Read);
        ctx.tenant = Some("globex");
        assert!(check_grant(&ctx, Action::Select, &t("orders"), &view).is_err());
        ctx.tenant = Some("acme");
        assert!(check_grant(&ctx, Action::Select, &t("orders"), &view).is_ok());
    }

    #[test]
    fn permission_cache_expands_all() {
        let mut actions = BTreeSet::new();
        actions.insert(Action::All);
        let g = Grant {
            principal: GrantPrincipal::User(UserId::platform("alice")),
            resource: t("orders"),
            actions,
            with_grant_option: false,
            granted_by: "admin".into(),
            granted_at: 0,
            tenant: None,
            columns: None,
        };
        let cache = PermissionCache::build(std::slice::from_ref(&g), &[]);
        assert!(cache.allows(&t("orders"), Action::Select));
        assert!(cache.allows(&t("orders"), Action::Insert));
        assert!(cache.allows(&t("orders"), Action::Delete));
        assert!(!cache.allows(&t("nope"), Action::Select));
    }

    #[test]
    fn resource_table_from_dotted_name() {
        let r = Resource::table_from_name("public.users");
        assert_eq!(
            r,
            Resource::Table {
                schema: Some("public".into()),
                table: "users".into()
            }
        );
        let r = Resource::table_from_name("users");
        assert_eq!(
            r,
            Resource::Table {
                schema: None,
                table: "users".into()
            }
        );
    }

    #[test]
    fn database_grant_covers_anything() {
        let g = Grant::single(
            GrantPrincipal::User(UserId::platform("alice")),
            Resource::Database,
            Action::Select,
            "admin".into(),
            0,
            None,
        );
        let view = GrantsView {
            user_grants: std::slice::from_ref(&g),
            public_grants: &[],
        };
        assert!(check_grant(
            &ctx("alice", Role::Read),
            Action::Select,
            &t("anything"),
            &view
        )
        .is_ok());
    }
}
