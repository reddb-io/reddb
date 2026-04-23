//! Version-control ("Git for Data") use cases.
//!
//! Sits on top of the MVCC snapshot manager and persists commit
//! metadata in the `red_*` collections declared in
//! [`crate::application::vcs_collections`]. Mirrors the git command
//! surface: commit, branch, checkout, merge, cherry-pick, revert,
//! reset, log, diff, status, tag.
//!
//! Designed to mirror the Graph/Query use-case pattern: thin struct
//! parameterised over a [`RuntimeVcsPort`] trait implemented by
//! `RedDBRuntime`. No storage logic lives here — only validation and
//! delegation.

use crate::application::ports::RuntimeVcsPort;
use crate::json::Value as JsonValue;
use crate::storage::transaction::snapshot::Xid;
use crate::RedDBResult;

// ---------------------------------------------------------------------------
// Domain types (shared between application and runtime)
// ---------------------------------------------------------------------------

/// A commit hash. 64-char lowercase hex (SHA-256 truncated or full).
pub type CommitHash = String;

/// A full ref name like `refs/heads/main` or `refs/tags/v1.0`.
pub type RefName = String;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone)]
pub struct Commit {
    pub hash: CommitHash,
    pub root_xid: Xid,
    pub parents: Vec<CommitHash>,
    pub height: u64,
    pub author: Author,
    pub committer: Author,
    pub message: String,
    /// Unix epoch milliseconds.
    pub timestamp_ms: i64,
    pub signature: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    Branch,
    Tag,
    Head,
}

#[derive(Debug, Clone)]
pub struct Ref {
    pub name: RefName,
    pub kind: RefKind,
    /// For `Branch`/`Tag`: the commit hash pointed to.
    /// For `Head`: the ref name it targets (resolved recursively).
    pub target: String,
    pub protected: bool,
}

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CreateCommitInput {
    /// Connection id whose working set is being committed.
    pub connection_id: u64,
    pub message: String,
    pub author: Author,
    /// Optional override for committer (falls back to author).
    pub committer: Option<Author>,
    /// When true, re-commit on top of HEAD's current parent instead of
    /// adding a new commit (git --amend semantics).
    pub amend: bool,
    /// When true and working set is empty, succeed silently instead of
    /// erroring. Mirrors `git commit --allow-empty`.
    pub allow_empty: bool,
}

#[derive(Debug, Clone)]
pub struct CreateBranchInput {
    pub name: String,
    /// Optional base: ref or commit hash. Defaults to current HEAD of
    /// the caller's connection.
    pub from: Option<String>,
    pub connection_id: u64,
}

#[derive(Debug, Clone)]
pub struct CreateTagInput {
    pub name: String,
    /// Ref or commit hash to tag.
    pub target: String,
    pub annotation: Option<String>,
}

#[derive(Debug, Clone)]
pub enum CheckoutTarget {
    /// Short branch name (`main`) or full ref (`refs/heads/main`).
    Branch(String),
    /// Commit hash — detached HEAD.
    Commit(CommitHash),
    /// Tag name or full ref.
    Tag(String),
}

#[derive(Debug, Clone)]
pub struct CheckoutInput {
    pub connection_id: u64,
    pub target: CheckoutTarget,
    /// Force checkout even if working set has uncommitted changes.
    pub force: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeStrategy {
    /// Fast-forward if possible, else create merge commit.
    Auto,
    /// Always create merge commit.
    NoFastForward,
    /// Fail if not fast-forward.
    FastForwardOnly,
}

#[derive(Debug, Clone)]
pub struct MergeOpts {
    pub strategy: MergeStrategy,
    /// Custom merge commit message; auto-generated if None.
    pub message: Option<String>,
    /// Abort merge and restore working set on conflict, instead of
    /// leaving shadow docs in `red_conflicts`.
    pub abort_on_conflict: bool,
}

impl Default for MergeOpts {
    fn default() -> Self {
        Self {
            strategy: MergeStrategy::Auto,
            message: None,
            abort_on_conflict: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MergeInput {
    pub connection_id: u64,
    /// Branch or commit being merged into the current HEAD.
    pub from: String,
    pub opts: MergeOpts,
    pub author: Author,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetMode {
    /// Move HEAD only. Working set preserved.
    Soft,
    /// Move HEAD and reset staged to target. Working set preserved.
    Mixed,
    /// Move HEAD, reset staged, discard working set.
    Hard,
}

#[derive(Debug, Clone)]
pub struct ResetInput {
    pub connection_id: u64,
    pub target: String,
    pub mode: ResetMode,
}

#[derive(Debug, Clone)]
pub struct LogRange {
    /// Upper bound ref / commit hash. Defaults to HEAD.
    pub to: Option<String>,
    /// Lower bound (exclusive). `from..to` semantics.
    pub from: Option<String>,
    pub limit: Option<usize>,
    pub skip: Option<usize>,
    /// When true, exclude merge commits.
    pub no_merges: bool,
}

impl Default for LogRange {
    fn default() -> Self {
        Self {
            to: None,
            from: None,
            limit: None,
            skip: None,
            no_merges: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LogInput {
    pub connection_id: u64,
    pub range: LogRange,
}

#[derive(Debug, Clone)]
pub struct DiffInput {
    /// Ref, commit hash, or AS OF spec.
    pub from: String,
    pub to: String,
    /// When set, restrict diff to this collection.
    pub collection: Option<String>,
    /// Omit entity bodies (metadata-only diff).
    pub summary_only: bool,
}

#[derive(Debug, Clone)]
pub struct StatusInput {
    pub connection_id: u64,
}

// ---------------------------------------------------------------------------
// AS OF time-travel spec
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum AsOfSpec {
    Commit(CommitHash),
    Branch(String),
    Tag(String),
    /// Unix epoch milliseconds.
    TimestampMs(i64),
    /// Raw transaction id (power users / diagnostics).
    Snapshot(Xid),
}

// ---------------------------------------------------------------------------
// Outputs
// ---------------------------------------------------------------------------

/// A single entity-level change between two commits.
#[derive(Debug, Clone)]
pub struct DiffEntry {
    pub collection: String,
    pub entity_id: String,
    pub change: DiffChange,
}

#[derive(Debug, Clone)]
pub enum DiffChange {
    Added { after: JsonValue },
    Removed { before: JsonValue },
    Modified { before: JsonValue, after: JsonValue },
}

#[derive(Debug, Clone, Default)]
pub struct Diff {
    pub from: CommitHash,
    pub to: CommitHash,
    pub entries: Vec<DiffEntry>,
    pub added: usize,
    pub removed: usize,
    pub modified: usize,
}

/// Result of a merge operation. Non-empty `conflicts` means the merge
/// is paused — user must resolve shadow docs in `red_conflicts` before
/// committing.
#[derive(Debug, Clone)]
pub struct MergeOutcome {
    pub merge_commit: Option<Commit>,
    pub fast_forward: bool,
    pub conflicts: Vec<Conflict>,
    pub merge_state_id: Option<String>,
}

impl MergeOutcome {
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct Conflict {
    pub id: String,
    pub collection: String,
    pub entity_id: String,
    pub base: JsonValue,
    pub ours: JsonValue,
    pub theirs: JsonValue,
    pub conflicting_paths: Vec<String>,
    pub merge_state_id: String,
}

#[derive(Debug, Clone)]
pub struct Status {
    pub connection_id: u64,
    pub head_ref: Option<RefName>,
    pub head_commit: Option<CommitHash>,
    pub detached: bool,
    pub staged_changes: usize,
    pub working_changes: usize,
    pub unresolved_conflicts: usize,
    pub merge_state_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Use-case surface
// ---------------------------------------------------------------------------

pub struct VcsUseCases<'a, P: ?Sized> {
    runtime: &'a P,
}

impl<'a, P: RuntimeVcsPort + ?Sized> VcsUseCases<'a, P> {
    pub fn new(runtime: &'a P) -> Self {
        Self { runtime }
    }

    pub fn commit(&self, input: CreateCommitInput) -> RedDBResult<Commit> {
        self.runtime.vcs_commit(input)
    }

    pub fn branch_create(&self, input: CreateBranchInput) -> RedDBResult<Ref> {
        self.runtime.vcs_branch_create(input)
    }

    pub fn branch_list(&self) -> RedDBResult<Vec<Ref>> {
        self.runtime.vcs_list_refs(Some("refs/heads/"))
    }

    pub fn branch_delete(&self, name: &str) -> RedDBResult<()> {
        self.runtime.vcs_branch_delete(name)
    }

    pub fn tag(&self, input: CreateTagInput) -> RedDBResult<Ref> {
        self.runtime.vcs_tag_create(input)
    }

    pub fn tag_list(&self) -> RedDBResult<Vec<Ref>> {
        self.runtime.vcs_list_refs(Some("refs/tags/"))
    }

    pub fn checkout(&self, input: CheckoutInput) -> RedDBResult<Ref> {
        self.runtime.vcs_checkout(input)
    }

    pub fn merge(&self, input: MergeInput) -> RedDBResult<MergeOutcome> {
        self.runtime.vcs_merge(input)
    }

    pub fn cherry_pick(
        &self,
        connection_id: u64,
        commit: &str,
        author: Author,
    ) -> RedDBResult<MergeOutcome> {
        self.runtime.vcs_cherry_pick(connection_id, commit, author)
    }

    pub fn revert(
        &self,
        connection_id: u64,
        commit: &str,
        author: Author,
    ) -> RedDBResult<Commit> {
        self.runtime.vcs_revert(connection_id, commit, author)
    }

    pub fn reset(&self, input: ResetInput) -> RedDBResult<()> {
        self.runtime.vcs_reset(input)
    }

    pub fn log(&self, input: LogInput) -> RedDBResult<Vec<Commit>> {
        self.runtime.vcs_log(input)
    }

    pub fn diff(&self, input: DiffInput) -> RedDBResult<Diff> {
        self.runtime.vcs_diff(input)
    }

    pub fn status(&self, input: StatusInput) -> RedDBResult<Status> {
        self.runtime.vcs_status(input)
    }

    pub fn lca(&self, a: &str, b: &str) -> RedDBResult<Option<CommitHash>> {
        self.runtime.vcs_lca(a, b)
    }

    pub fn conflicts_list(&self, merge_state_id: &str) -> RedDBResult<Vec<Conflict>> {
        self.runtime.vcs_conflicts_list(merge_state_id)
    }

    pub fn conflict_resolve(
        &self,
        conflict_id: &str,
        resolved: JsonValue,
    ) -> RedDBResult<()> {
        self.runtime.vcs_conflict_resolve(conflict_id, resolved)
    }

    pub fn resolve_as_of(&self, spec: AsOfSpec) -> RedDBResult<Xid> {
        self.runtime.vcs_resolve_as_of(spec)
    }

    /// Resolve a short ref / commit prefix / branch / tag to a full
    /// commit hash. Primary caller is the query parser's AS OF path.
    pub fn resolve_commitish(&self, spec: &str) -> RedDBResult<CommitHash> {
        self.runtime.vcs_resolve_commitish(spec)
    }
}
