//! Action catalog — the single source of truth for policy action names.
//!
//! Historically two hand-rolled slices duplicated the list of recognised
//! policy actions: `ACTION_ALLOWLIST` in [`crate::auth::policies`] (used to
//! validate policy documents) and `ACTIONS` in
//! [`crate::runtime::red_schema`] (used to populate the
//! `red.control_capabilities` virtual table). Drift between the two was a
//! latent bug — a typo in one but not the other meant either an action
//! advertised through the catalog could not be put into a policy, or a
//! policy could grant an action that the catalog never advertised.
//!
//! This module consolidates the list into a single static slice. Both
//! consumers now read from [`ACTIONS`]. Each entry carries:
//!
//! * `name` — the action verb (e.g. `policy:put`, `*`, `admin:*`).
//! * `category` — coarse grouping ([`ActionCategory`]).
//! * `lifecycle_state` — [`LifecycleState::Active`],
//!   [`LifecycleState::Deprecated`] (with a `replacement` and
//!   `since_version`), or [`LifecycleState::Removed`].
//! * `gates_description` — short human-readable note about what the action
//!   gates. Used by the (forthcoming) `red.policy.actions` virtual table.
//!
//! Lifecycle semantics:
//! * `Active` and `Deprecated` entries are both accepted by policy
//!   validation. Deprecated entries will (in the linter slice) produce a
//!   diagnostic with the `replacement` hint, but they still validate.
//! * `Removed` entries are rejected by validation. Keeping them in the
//!   catalog (rather than just deleting them) lets the linter produce a
//!   "this action was removed in version X, use Y instead" diagnostic
//!   rather than a generic "unknown action" error.

/// Coarse category for an action verb. Used by the (forthcoming) admin
/// virtual table; the policy evaluator does not consult it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionCategory {
    /// Data-manipulation verbs (`select`, `insert`, `update`, ...).
    Dml,
    /// Data-definition verbs (`create`, `drop`, `alter`).
    Ddl,
    /// Schema-level grants (`references`, `usage`).
    Schema,
    /// Stored function execution.
    Function,
    /// Privilege-management verbs (`grant`, `revoke`).
    Mgmt,
    /// Policy lifecycle verbs (`policy:put`, ...).
    Policy,
    /// Admin verbs (`admin:bootstrap`, ...).
    Admin,
    /// Runtime config verbs (`config:read`, ...).
    Config,
    /// Vault verbs (`vault:read`, ...).
    Vault,
    /// Wildcard / namespace-wildcard entries (`*`, `admin:*`).
    Wildcard,
    /// AI / analytics-facing actions (none today; reserved).
    Ai,
    /// Ephemeral notification verbs (`notify`, `notify:cross-tenant`).
    /// Gates the pub/sub primitive defined in `crate::notifications`.
    Notification,
    /// Catch-all for actions that don't fit a tighter category yet
    /// (`evidence:export`, `red.registry:register`, `kv:invalidate`).
    Other,
}

impl ActionCategory {
    /// Stable lowercase identifier used by the SQL virtual table and
    /// the `GET /admin/policies/actions` HTTP surface. Operators read
    /// these strings, so they are part of the public contract.
    pub fn as_str(&self) -> &'static str {
        match self {
            ActionCategory::Dml => "dml",
            ActionCategory::Ddl => "ddl",
            ActionCategory::Schema => "schema",
            ActionCategory::Function => "function",
            ActionCategory::Mgmt => "mgmt",
            ActionCategory::Policy => "policy",
            ActionCategory::Admin => "admin",
            ActionCategory::Config => "config",
            ActionCategory::Vault => "vault",
            ActionCategory::Wildcard => "wildcard",
            ActionCategory::Ai => "ai",
            ActionCategory::Notification => "notification",
            ActionCategory::Other => "other",
        }
    }
}

/// Lifecycle state for a catalog entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleState {
    /// Currently the canonical name for this capability.
    Active,
    /// Still accepted by validation, but a newer name is preferred.
    Deprecated {
        /// Recommended replacement action verb, if one exists.
        replacement: Option<&'static str>,
        /// Version at which the action was deprecated.
        since_version: &'static str,
    },
    /// No longer accepted. Kept in the catalog so the linter can produce
    /// a targeted "removed in version X" diagnostic instead of a generic
    /// "unknown action" error.
    Removed,
}

/// One entry in the action catalog.
#[derive(Debug, Clone)]
pub struct ActionEntry {
    pub name: &'static str,
    pub category: ActionCategory,
    pub lifecycle_state: LifecycleState,
    pub gates_description: &'static str,
}

/// Canonical action catalog. Order matters: the control-capabilities
/// virtual table emits rows in this order, so tests that assert
/// row-order parity with the prior hand-rolled slice depend on it.
///
/// To add a new action: append (or insert) an entry here. To deprecate
/// one: change its `lifecycle_state` to `Deprecated { … }` — do not
/// delete the row. To remove one: change it to `Removed` (and only
/// delete after a release cycle).
pub const ACTIONS: &[ActionEntry] = &[
    // -- DML / DDL / privilege management --------------------------------
    ActionEntry {
        name: "select",
        category: ActionCategory::Dml,
        lifecycle_state: LifecycleState::Active,
        gates_description: "read rows from a collection",
    },
    ActionEntry {
        name: "write",
        category: ActionCategory::Dml,
        lifecycle_state: LifecycleState::Active,
        gates_description: "any mutating DML (insert/update/delete)",
    },
    ActionEntry {
        name: "insert",
        category: ActionCategory::Dml,
        lifecycle_state: LifecycleState::Active,
        gates_description: "insert rows into a collection",
    },
    ActionEntry {
        name: "update",
        category: ActionCategory::Dml,
        lifecycle_state: LifecycleState::Active,
        gates_description: "update rows in a collection",
    },
    ActionEntry {
        name: "delete",
        category: ActionCategory::Dml,
        lifecycle_state: LifecycleState::Active,
        gates_description: "delete rows from a collection",
    },
    ActionEntry {
        name: "truncate",
        category: ActionCategory::Dml,
        lifecycle_state: LifecycleState::Active,
        gates_description: "truncate a collection",
    },
    ActionEntry {
        name: "references",
        category: ActionCategory::Schema,
        lifecycle_state: LifecycleState::Active,
        gates_description: "declare a foreign key referencing a table",
    },
    ActionEntry {
        name: "execute",
        category: ActionCategory::Function,
        lifecycle_state: LifecycleState::Active,
        gates_description: "execute a stored function",
    },
    ActionEntry {
        name: "usage",
        category: ActionCategory::Schema,
        lifecycle_state: LifecycleState::Active,
        gates_description: "use a schema namespace",
    },
    ActionEntry {
        name: "grant",
        category: ActionCategory::Mgmt,
        lifecycle_state: LifecycleState::Active,
        gates_description: "grant privileges to another principal",
    },
    ActionEntry {
        name: "revoke",
        category: ActionCategory::Mgmt,
        lifecycle_state: LifecycleState::Active,
        gates_description: "revoke privileges from another principal",
    },
    ActionEntry {
        name: "create",
        category: ActionCategory::Ddl,
        lifecycle_state: LifecycleState::Active,
        gates_description: "create a database object",
    },
    ActionEntry {
        name: "drop",
        category: ActionCategory::Ddl,
        lifecycle_state: LifecycleState::Active,
        gates_description: "drop a database object",
    },
    ActionEntry {
        name: "alter",
        category: ActionCategory::Ddl,
        lifecycle_state: LifecycleState::Active,
        gates_description: "alter a database object",
    },
    // -- Policy lifecycle ------------------------------------------------
    ActionEntry {
        name: "policy:put",
        category: ActionCategory::Policy,
        lifecycle_state: LifecycleState::Active,
        gates_description: "create or update a managed policy document",
    },
    ActionEntry {
        name: "policy:drop",
        category: ActionCategory::Policy,
        lifecycle_state: LifecycleState::Active,
        gates_description: "delete a managed policy document",
    },
    ActionEntry {
        name: "policy:attach",
        category: ActionCategory::Policy,
        lifecycle_state: LifecycleState::Active,
        gates_description: "attach a policy to a principal",
    },
    ActionEntry {
        name: "policy:detach",
        category: ActionCategory::Policy,
        lifecycle_state: LifecycleState::Active,
        gates_description: "detach a policy from a principal",
    },
    ActionEntry {
        name: "policy:simulate",
        category: ActionCategory::Policy,
        lifecycle_state: LifecycleState::Active,
        gates_description: "run the policy simulator",
    },
    // -- KV --------------------------------------------------------------
    ActionEntry {
        name: "kv:invalidate",
        category: ActionCategory::Other,
        lifecycle_state: LifecycleState::Active,
        gates_description: "invalidate cached KV entries",
    },
    // -- Admin -----------------------------------------------------------
    ActionEntry {
        name: "admin:bootstrap",
        category: ActionCategory::Admin,
        lifecycle_state: LifecycleState::Active,
        gates_description: "execute the bootstrap workflow",
    },
    ActionEntry {
        name: "admin:audit-read",
        category: ActionCategory::Admin,
        lifecycle_state: LifecycleState::Active,
        gates_description: "read the platform audit log",
    },
    ActionEntry {
        name: "admin:reload",
        category: ActionCategory::Admin,
        lifecycle_state: LifecycleState::Active,
        gates_description: "reload runtime configuration",
    },
    ActionEntry {
        name: "admin:lease-promote",
        category: ActionCategory::Admin,
        lifecycle_state: LifecycleState::Active,
        gates_description: "promote a standby instance via lease handoff",
    },
    // -- Runtime config --------------------------------------------------
    ActionEntry {
        name: "config:read",
        category: ActionCategory::Config,
        lifecycle_state: LifecycleState::Active,
        gates_description: "read runtime configuration values",
    },
    ActionEntry {
        name: "config:write",
        category: ActionCategory::Config,
        lifecycle_state: LifecycleState::Active,
        gates_description: "mutate runtime configuration values",
    },
    ActionEntry {
        name: "config:*",
        category: ActionCategory::Wildcard,
        lifecycle_state: LifecycleState::Active,
        gates_description: "any runtime configuration verb",
    },
    // -- Vault -----------------------------------------------------------
    ActionEntry {
        name: "vault:read_metadata",
        category: ActionCategory::Vault,
        lifecycle_state: LifecycleState::Active,
        gates_description: "read vault entry metadata (no plaintext)",
    },
    ActionEntry {
        name: "vault:read",
        category: ActionCategory::Vault,
        lifecycle_state: LifecycleState::Active,
        gates_description: "reveal vault entry plaintext",
    },
    ActionEntry {
        name: "vault:write",
        category: ActionCategory::Vault,
        lifecycle_state: LifecycleState::Active,
        gates_description: "write or rotate vault entries",
    },
    ActionEntry {
        name: "vault:unseal",
        category: ActionCategory::Vault,
        lifecycle_state: LifecycleState::Active,
        gates_description: "unseal the vault master key for this session",
    },
    // Deprecated: `vault:unseal_history` was the previous name for
    // reading the audit trail of unseal events. The capability is now
    // surfaced through `vault:read_metadata` on the unseal-events
    // resource, so the dedicated verb is retained for back-compat but
    // policy authors should migrate.
    ActionEntry {
        name: "vault:unseal_history",
        category: ActionCategory::Vault,
        lifecycle_state: LifecycleState::Deprecated {
            replacement: Some("vault:read_metadata"),
            since_version: "0.5.0",
        },
        gates_description: "read the vault unseal-event audit trail",
    },
    ActionEntry {
        name: "vault:purge",
        category: ActionCategory::Vault,
        lifecycle_state: LifecycleState::Active,
        gates_description: "purge (destructively remove) vault entries",
    },
    // -- Evidence --------------------------------------------------------
    ActionEntry {
        name: "evidence:export",
        category: ActionCategory::Other,
        lifecycle_state: LifecycleState::Active,
        gates_description: "export evidence bundles",
    },
    ActionEntry {
        name: "evidence:*",
        category: ActionCategory::Wildcard,
        lifecycle_state: LifecycleState::Active,
        gates_description: "any evidence-pipeline verb",
    },
    // -- Registry --------------------------------------------------------
    ActionEntry {
        name: "red.registry:register",
        category: ActionCategory::Other,
        lifecycle_state: LifecycleState::Active,
        gates_description: "register a new managed-config schema",
    },
    ActionEntry {
        name: "red.registry:supersede",
        category: ActionCategory::Other,
        lifecycle_state: LifecycleState::Active,
        gates_description: "supersede an existing managed-config schema",
    },
    ActionEntry {
        name: "red.registry:*",
        category: ActionCategory::Wildcard,
        lifecycle_state: LifecycleState::Active,
        gates_description: "any registry verb",
    },
    // -- AI provider gate (S3 / #711) ------------------------------------
    // The `ai:provider:<token>` namespace lets operators express "role X
    // cannot use AI provider Y" without denying `insert` on entire
    // collections. The gate runs at the SQL planner before the AI
    // credential resolver — see `runtime::ai::provider_gate`. Tokens
    // mirror `AiProvider::token()` exactly.
    ActionEntry {
        name: "ai:provider:openai",
        category: ActionCategory::Ai,
        lifecycle_state: LifecycleState::Active,
        gates_description: "use the OpenAI provider for ASK / AUTO EMBED / SEARCH SIMILAR",
    },
    ActionEntry {
        name: "ai:provider:anthropic",
        category: ActionCategory::Ai,
        lifecycle_state: LifecycleState::Active,
        gates_description: "use the Anthropic provider for ASK / AUTO EMBED / SEARCH SIMILAR",
    },
    ActionEntry {
        name: "ai:provider:groq",
        category: ActionCategory::Ai,
        lifecycle_state: LifecycleState::Active,
        gates_description: "use the Groq provider for ASK / AUTO EMBED / SEARCH SIMILAR",
    },
    ActionEntry {
        name: "ai:provider:openrouter",
        category: ActionCategory::Ai,
        lifecycle_state: LifecycleState::Active,
        gates_description: "use the OpenRouter provider for ASK / AUTO EMBED / SEARCH SIMILAR",
    },
    ActionEntry {
        name: "ai:provider:together",
        category: ActionCategory::Ai,
        lifecycle_state: LifecycleState::Active,
        gates_description: "use the Together provider for ASK / AUTO EMBED / SEARCH SIMILAR",
    },
    ActionEntry {
        name: "ai:provider:venice",
        category: ActionCategory::Ai,
        lifecycle_state: LifecycleState::Active,
        gates_description: "use the Venice provider for ASK / AUTO EMBED / SEARCH SIMILAR",
    },
    ActionEntry {
        name: "ai:provider:ollama",
        category: ActionCategory::Ai,
        lifecycle_state: LifecycleState::Active,
        gates_description: "use the Ollama provider for ASK / AUTO EMBED / SEARCH SIMILAR",
    },
    ActionEntry {
        name: "ai:provider:deepseek",
        category: ActionCategory::Ai,
        lifecycle_state: LifecycleState::Active,
        gates_description: "use the DeepSeek provider for ASK / AUTO EMBED / SEARCH SIMILAR",
    },
    ActionEntry {
        name: "ai:provider:huggingface",
        category: ActionCategory::Ai,
        lifecycle_state: LifecycleState::Active,
        gates_description: "use the HuggingFace provider for ASK / AUTO EMBED / SEARCH SIMILAR",
    },
    ActionEntry {
        name: "ai:provider:local",
        category: ActionCategory::Ai,
        lifecycle_state: LifecycleState::Active,
        gates_description: "use the local (in-process) embedding provider",
    },
    ActionEntry {
        name: "ai:provider:*",
        category: ActionCategory::Wildcard,
        lifecycle_state: LifecycleState::Active,
        gates_description: "use any AI provider (provider-gate wildcard)",
    },
    ActionEntry {
        name: "ai:*",
        category: ActionCategory::Wildcard,
        lifecycle_state: LifecycleState::Active,
        gates_description: "any AI-namespace verb",
    },
    // -- Ephemeral notifications (#720 / PRD #718) -----------------------
    // RedDB-native pub/sub primitive. `notify` gates publish/subscribe
    // inside the principal's own tenant; `notify:cross-tenant` is the
    // explicit capability required to address another tenant's channel
    // or the platform-global namespace. See `crate::notifications`.
    ActionEntry {
        name: "notify",
        category: ActionCategory::Notification,
        lifecycle_state: LifecycleState::Active,
        gates_description:
            "publish to / subscribe to ephemeral notification channels in the principal's own tenant",
    },
    ActionEntry {
        name: "notify:cross-tenant",
        category: ActionCategory::Notification,
        lifecycle_state: LifecycleState::Active,
        gates_description:
            "address ephemeral notification channels in another tenant or the global namespace",
    },
    ActionEntry {
        name: "notify:*",
        category: ActionCategory::Wildcard,
        lifecycle_state: LifecycleState::Active,
        gates_description: "any ephemeral notification verb",
    },
    // -- Wildcards (kept last for legacy ordering) -----------------------
    ActionEntry {
        name: "*",
        category: ActionCategory::Wildcard,
        lifecycle_state: LifecycleState::Active,
        gates_description: "any action (escape hatch — audit usage carefully)",
    },
    ActionEntry {
        name: "admin:*",
        category: ActionCategory::Wildcard,
        lifecycle_state: LifecycleState::Active,
        gates_description: "any admin verb",
    },
    ActionEntry {
        name: "vault:*",
        category: ActionCategory::Wildcard,
        lifecycle_state: LifecycleState::Active,
        gates_description: "any vault verb",
    },
    ActionEntry {
        name: "kv:*",
        category: ActionCategory::Wildcard,
        lifecycle_state: LifecycleState::Active,
        gates_description: "any KV verb",
    },
    ActionEntry {
        name: "policy:*",
        category: ActionCategory::Wildcard,
        lifecycle_state: LifecycleState::Active,
        gates_description: "any policy lifecycle verb",
    },
];

/// Returns `true` if `name` is recognised by the catalog and is not in
/// the `Removed` lifecycle state. `Active` and `Deprecated` entries both
/// validate.
pub fn is_valid_action(name: &str) -> bool {
    ACTIONS
        .iter()
        .any(|e| e.name == name && !matches!(e.lifecycle_state, LifecycleState::Removed))
}

/// Lookup an entry by exact name. Returns `None` for unknown names.
pub fn lookup(name: &str) -> Option<&'static ActionEntry> {
    ACTIONS.iter().find(|e| e.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// The pre-catalog allowlist that lived in `auth::policies`. The
    /// catalog must accept every one of these (modulo any explicit
    /// `Removed` entries) so existing policies that used to validate
    /// continue to validate.
    const HISTORICAL_ALLOWLIST: &[&str] = &[
        "select",
        "write",
        "insert",
        "update",
        "delete",
        "truncate",
        "references",
        "execute",
        "usage",
        "grant",
        "revoke",
        "create",
        "drop",
        "alter",
        "policy:put",
        "policy:drop",
        "policy:attach",
        "policy:detach",
        "policy:simulate",
        "kv:invalidate",
        "admin:bootstrap",
        "admin:audit-read",
        "admin:reload",
        "admin:lease-promote",
        "config:read",
        "config:write",
        "config:*",
        "vault:read_metadata",
        "vault:read",
        "vault:write",
        "vault:unseal",
        "vault:unseal_history",
        "vault:purge",
        "evidence:export",
        "evidence:*",
        "red.registry:register",
        "red.registry:supersede",
        "red.registry:*",
        "*",
        "admin:*",
        "vault:*",
        "kv:*",
        "policy:*",
    ];

    #[test]
    fn no_duplicate_names() {
        let mut seen = HashSet::new();
        for entry in ACTIONS {
            assert!(
                seen.insert(entry.name),
                "duplicate action name in catalog: {}",
                entry.name
            );
        }
    }

    #[test]
    fn covers_historical_allowlist() {
        let names: HashSet<&'static str> = ACTIONS.iter().map(|e| e.name).collect();
        for action in HISTORICAL_ALLOWLIST {
            assert!(
                names.contains(action),
                "catalog missing historically-accepted action: {action}",
            );
        }
    }

    #[test]
    fn historical_allowlist_still_validates() {
        for action in HISTORICAL_ALLOWLIST {
            assert!(
                is_valid_action(action),
                "action {action} was accepted before the catalog and must still validate",
            );
        }
    }

    #[test]
    fn has_at_least_one_deprecated_entry() {
        let count = ACTIONS
            .iter()
            .filter(|e| matches!(e.lifecycle_state, LifecycleState::Deprecated { .. }))
            .count();
        assert!(
            count >= 1,
            "catalog must demonstrate the Deprecated lifecycle state with at least one entry",
        );
    }

    #[test]
    fn removed_entries_are_rejected() {
        // No `Removed` entries today, but the predicate must enforce the
        // rule if/when one is added.
        for entry in ACTIONS {
            if matches!(entry.lifecycle_state, LifecycleState::Removed) {
                assert!(
                    !is_valid_action(entry.name),
                    "Removed entry {} must not validate",
                    entry.name,
                );
            }
        }
    }

    #[test]
    fn lookup_finds_known_entries() {
        assert!(lookup("policy:put").is_some());
        assert!(lookup("definitely-not-an-action").is_none());
    }
}
