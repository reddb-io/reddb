//! Internal collection names for the VCS ("Git for Data") layer.
//!
//! All VCS-owned collections share the `red.*` prefix — matching existing
//! internal stores like `red.config`, `red.query.stats_catalog`, and `red.queue.meta`.
//! Keeping every name in one file makes it obvious which collections are
//! internal and prevents accidental divergence between bootstrap code,
//! runtime access, and schema documentation.

/// Commit entities: hash, parent pointers, root snapshot xid, author, message,
/// timestamp, height, optional signature.
pub const COMMITS: &str = "red.vcs.commits";

/// Refs: branches (`refs/heads/*`), tags (`refs/tags/*`), and per-connection
/// `HEAD` pointers.
pub const REFS: &str = "red.vcs.refs";

/// Per-connection working set: working_xid, staged_xid, current branch,
/// base commit, optional in-progress merge state id.
pub const WORKSETS: &str = "red.vcs.worksets";

/// Commit ancestry index (height, commit_hash, ancestor_hash) used for
/// sub-linear LCA queries. Populated lazily at commit time.
pub const CLOSURE: &str = "red.vcs.closure";

/// Shadow documents representing unresolved merge conflicts. One row per
/// conflicting entity, scoped to a `merge_state_id`.
pub const CONFLICTS: &str = "red.vcs.conflicts";

/// In-progress merge / rebase / cherry-pick / revert metadata. Stores
/// pending commit series and current step for resumable operations.
pub const MERGE_STATE: &str = "red.vcs.merge_state";

/// Remote repository configuration: url, fetch_specs, auth key ref.
pub const REMOTES: &str = "red.vcs.remotes";

/// Per-user-collection opt-in flag for Git-for-Data. Each row:
/// `_id = <user_collection_name>`, `versioned = true`, `ts_ms`.
/// A collection is treated as versioned iff a row exists here with
/// `versioned = true`. Default (no row) is non-versioned — merge,
/// diff, conflict materialisation, and AS OF queries skip it.
pub const SETTINGS: &str = "red.vcs.settings";

/// All VCS collections, in bootstrap order. Consumed by the runtime
/// bootstrap to call `get_or_create_collection` once at startup.
pub const ALL: &[&str] = &[
    COMMITS,
    REFS,
    WORKSETS,
    CLOSURE,
    CONFLICTS,
    MERGE_STATE,
    REMOTES,
    SETTINGS,
];

/// Ref name for the default branch.
pub const DEFAULT_BRANCH_REF: &str = "refs/heads/main";

/// Ref prefix for branches.
pub const BRANCH_REF_PREFIX: &str = "refs/heads/";

/// Ref prefix for tags.
pub const TAG_REF_PREFIX: &str = "refs/tags/";

/// Ref id prefix for per-connection HEAD pointers (`HEAD:<connection_id>`).
pub const HEAD_ID_PREFIX: &str = "HEAD:";

/// `red.config` namespace for VCS configuration (retention, protected
/// branches, merge policy). Matches the `red.logging.*` / `red.ml.*`
/// namespacing convention.
pub const CONFIG_NAMESPACE: &str = "red.vcs";
