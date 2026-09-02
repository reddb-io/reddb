//! What an MCP session is allowed to do.
//!
//! The MCP server runs every tool as an implicit admin: it is a stdio
//! process the operator started, so there is no second principal to
//! authenticate. That is defensible for the *transport*, but it makes the
//! tool list the entire security boundary — and that list includes
//! `reddb_query` (arbitrary SQL and DDL), `reddb_drop_collection`,
//! `reddb_vault_unseal` (returns plaintext secrets) and
//! `reddb_auth_bootstrap` (returns an admin API key plus the vault
//! certificate).
//!
//! The model driving those tools reads rows, documents and error strings
//! that other people wrote. Prompt injection in that content therefore
//! reaches `DROP COLLECTION` and the vault with no confirmation step. The
//! fix is not to authenticate the model — it is to stop handing every
//! session the full set by default.
//!
//! A session now runs at one of three levels, lowest first:
//!
//!   * [`McpCapability::ReadOnly`] (the default) — reads, introspection and
//!     planning. Nothing that mutates state and nothing that returns a
//!     credential.
//!   * [`McpCapability::Write`] (`--allow-write`) — adds inserts, updates,
//!     deletes, KV and config writes, and DDL.
//!   * [`McpCapability::Admin`] (`--allow-admin`) — adds the vault and the
//!     auth tools.
//!
//! `reddb_query` takes arbitrary SQL, so its classification cannot come
//! from the tool name alone; [`statement_capability`] classifies the
//! statement itself and the query tool checks the result against the
//! session's level.

use std::fmt;

/// What a session may do, ordered from least to most privileged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum McpCapability {
    /// Reads and introspection only. The default for every session.
    #[default]
    ReadOnly,
    /// Reads plus data and schema mutation.
    Write,
    /// Everything, including the vault and principal management.
    Admin,
}

impl McpCapability {
    /// Resolve the level from the two opt-in flags, highest wins.
    pub fn from_flags(allow_write: bool, allow_admin: bool) -> Self {
        if allow_admin {
            Self::Admin
        } else if allow_write {
            Self::Write
        } else {
            Self::ReadOnly
        }
    }

    /// Whether this level satisfies `required`.
    pub fn permits(self, required: McpCapability) -> bool {
        self >= required
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Write => "write",
            Self::Admin => "admin",
        }
    }

    /// The flag an operator would add to reach this level.
    fn enabling_flag(self) -> &'static str {
        match self {
            Self::ReadOnly => "(no flag needed)",
            Self::Write => "--allow-write",
            Self::Admin => "--allow-admin",
        }
    }
}

impl fmt::Display for McpCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The level a tool requires. Unknown names are treated as `Admin` so a
/// tool added later is refused by default rather than silently exposed to
/// read-only sessions.
pub fn tool_capability(tool_name: &str) -> McpCapability {
    match tool_name {
        // Reads, introspection, planning.
        "reddb_query"
        | "reddb_collections"
        | "reddb_kv_get"
        | "reddb_config_get"
        | "reddb_config_resolve"
        | "reddb_search_vector"
        | "reddb_search_text"
        | "reddb_health"
        | "reddb_graph_traverse"
        | "reddb_graph_shortest_path"
        | "reddb_graph_centrality"
        | "reddb_graph_community"
        | "reddb_graph_components"
        | "reddb_graph_cycles"
        | "reddb_graph_clustering"
        | "reddb_scan"
        | "reddb_rql_validate"
        | "reddb_rql_explain"
        | "reddb_type_of"
        | "reddb_explain_connection" => McpCapability::ReadOnly,

        // Data and schema mutation.
        "reddb_insert_row"
        | "reddb_insert_node"
        | "reddb_insert_edge"
        | "reddb_insert_vector"
        | "reddb_insert_document"
        | "reddb_kv_set"
        | "reddb_kv_invalidate_tags"
        | "reddb_config_put"
        | "reddb_delete"
        | "reddb_update"
        | "reddb_create_collection"
        | "reddb_drop_collection" => McpCapability::Write,

        // Secrets and principals.
        "reddb_vault_get"
        | "reddb_vault_put"
        | "reddb_vault_unseal"
        | "reddb_auth_bootstrap"
        | "reddb_auth_create_user"
        | "reddb_auth_login"
        | "reddb_auth_create_api_key"
        | "reddb_auth_list_users" => McpCapability::Admin,

        // Default-deny for anything not classified above.
        _ => McpCapability::Admin,
    }
}

/// The level a statement passed to `reddb_query` requires.
///
/// `reddb_query` is read-only *as a tool*, but takes arbitrary SQL, so the
/// statement decides. Classification is deliberately conservative: only a
/// recognised read verb is `ReadOnly`, anything touching the vault, secrets
/// or principals is `Admin`, and anything unrecognised is `Write` — a
/// statement this classifier does not know must not slip through a
/// read-only session.
pub fn statement_capability(sql: &str) -> McpCapability {
    let trimmed = strip_leading_noise(sql);
    let upper: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase();
    let mut words = upper.split_whitespace();
    let first = words.next().unwrap_or("");
    let second = words.next().unwrap_or("");

    // Vault, secrets and principals are admin regardless of the verb.
    if matches!(first, "VAULT" | "GRANT" | "REVOKE")
        || matches!(second, "SECRET" | "SECRETS" | "USER" | "POLICY")
        || (first == "SHOW" && matches!(second, "SECRET" | "SECRETS" | "POLICIES" | "USERS"))
    {
        return McpCapability::Admin;
    }

    match first {
        "SELECT" | "WITH" | "SHOW" | "DESCRIBE" | "DESC" | "EXPLAIN" | "RANK" | "LIST" | "GET"
        | "HISTORY" => McpCapability::ReadOnly,
        // `KV GET` reads; every other KV verb writes.
        "KV" if matches!(second, "GET" | "LIST") => McpCapability::ReadOnly,
        _ => McpCapability::Write,
    }
}

/// Drop leading whitespace and line comments so `-- x\nDROP ...` is
/// classified on the statement rather than on the comment.
fn strip_leading_noise(sql: &str) -> &str {
    let mut rest = sql.trim_start();
    while let Some(after) = rest.strip_prefix("--") {
        rest = after
            .split_once('\n')
            .map(|(_, tail)| tail)
            .unwrap_or("")
            .trim_start();
    }
    rest
}

/// The refusal handed back when a session lacks the level a call needs.
pub fn refusal(subject: &str, granted: McpCapability, required: McpCapability) -> String {
    format!(
        "{subject} requires the `{required}` capability but this MCP session is `{granted}`. \
         Restart the server with `{}` to enable it.",
        required.enabling_flag()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_session_is_read_only() {
        assert_eq!(McpCapability::default(), McpCapability::ReadOnly);
        assert_eq!(
            McpCapability::from_flags(false, false),
            McpCapability::ReadOnly
        );
        assert_eq!(McpCapability::from_flags(true, false), McpCapability::Write);
        assert_eq!(McpCapability::from_flags(false, true), McpCapability::Admin);
        // The stronger flag wins rather than combining into something odd.
        assert_eq!(McpCapability::from_flags(true, true), McpCapability::Admin);
    }

    #[test]
    fn levels_are_ordered_and_inclusive() {
        assert!(McpCapability::Admin.permits(McpCapability::Write));
        assert!(McpCapability::Write.permits(McpCapability::ReadOnly));
        assert!(!McpCapability::ReadOnly.permits(McpCapability::Write));
        assert!(!McpCapability::Write.permits(McpCapability::Admin));
    }

    #[test]
    fn destructive_and_credential_tools_are_not_read_only() {
        for tool in [
            "reddb_drop_collection",
            "reddb_delete",
            "reddb_update",
            "reddb_create_collection",
        ] {
            assert_eq!(tool_capability(tool), McpCapability::Write, "{tool}");
        }
        for tool in [
            "reddb_vault_unseal",
            "reddb_vault_get",
            "reddb_auth_bootstrap",
            "reddb_auth_create_api_key",
        ] {
            assert_eq!(tool_capability(tool), McpCapability::Admin, "{tool}");
        }
    }

    #[test]
    fn unknown_tools_default_to_admin() {
        // A tool added later must be refused by a read-only session until
        // someone classifies it, not exposed by omission.
        assert_eq!(tool_capability("reddb_future_tool"), McpCapability::Admin);
    }

    #[test]
    fn statement_classification_follows_the_statement_not_the_tool() {
        assert_eq!(
            statement_capability("SELECT * FROM users"),
            McpCapability::ReadOnly
        );
        assert_eq!(
            statement_capability("  \n  select 1"),
            McpCapability::ReadOnly
        );
        for sql in [
            "DROP TABLE users",
            "DELETE FROM users",
            "INSERT INTO users VALUES (1)",
            "UPDATE users SET a = 1",
        ] {
            assert_eq!(statement_capability(sql), McpCapability::Write, "{sql}");
        }
        for sql in [
            "SET SECRET red.secret.ai.openai = 'x'",
            "CREATE USER bob PASSWORD 'x'",
            "GRANT SELECT ON t TO bob",
            "SHOW SECRETS",
        ] {
            assert_eq!(statement_capability(sql), McpCapability::Admin, "{sql}");
        }
    }

    #[test]
    fn leading_comments_do_not_hide_the_verb() {
        // `-- SELECT\nDROP TABLE t` must classify as the DROP, not the
        // comment: the comment is exactly what an injected prompt controls.
        assert_eq!(
            statement_capability("-- SELECT harmless\nDROP TABLE t"),
            McpCapability::Write
        );
    }

    #[test]
    fn kv_reads_and_writes_are_separated() {
        assert_eq!(statement_capability("KV GET c.k"), McpCapability::ReadOnly);
        assert_eq!(
            statement_capability("KV PUT c.k = 'v'"),
            McpCapability::Write
        );
    }
}
