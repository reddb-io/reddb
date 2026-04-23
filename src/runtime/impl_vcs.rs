//! Runtime implementation of the VCS ("Git for Data") surface.
//!
//! Phase 1 scaffold: method signatures wired into the port adapter with
//! stubbed bodies. Real persistence against the `red_*` collections
//! lands in subsequent phases (commit/branch/log first, then
//! checkout/status, diff, merge, cherry-pick/revert, AS OF resolution,
//! closure/LCA).
//!
//! Every method currently returns `RedDBError::Internal("vcs: … not yet
//! implemented")` so callers exercising the gRPC / REST / CLI surfaces
//! see a clear boundary while the internals are filled in.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::application::vcs::{
    AsOfSpec, Author, CheckoutInput, Commit, CommitHash, Conflict, CreateBranchInput,
    CreateCommitInput, CreateTagInput, Diff, DiffInput, LogInput, MergeInput, MergeOutcome, Ref,
    ResetInput, Status, StatusInput,
};
use crate::json::Value as JsonValue;
use crate::runtime::RedDBRuntime;
use crate::storage::transaction::snapshot::Xid;
use crate::{RedDBError, RedDBResult};

fn unimplemented(method: &str) -> RedDBError {
    RedDBError::Internal(format!("vcs: {method} not yet implemented"))
}

fn _now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl RedDBRuntime {
    pub fn vcs_commit(&self, input: CreateCommitInput) -> RedDBResult<Commit> {
        let _ = input;
        Err(unimplemented("commit"))
    }

    pub fn vcs_branch_create(&self, input: CreateBranchInput) -> RedDBResult<Ref> {
        let _ = input;
        Err(unimplemented("branch_create"))
    }

    pub fn vcs_branch_delete(&self, name: &str) -> RedDBResult<()> {
        let _ = name;
        Err(unimplemented("branch_delete"))
    }

    pub fn vcs_tag_create(&self, input: CreateTagInput) -> RedDBResult<Ref> {
        let _ = input;
        Err(unimplemented("tag_create"))
    }

    pub fn vcs_list_refs(&self, prefix: Option<&str>) -> RedDBResult<Vec<Ref>> {
        let _ = prefix;
        Ok(Vec::new())
    }

    pub fn vcs_checkout(&self, input: CheckoutInput) -> RedDBResult<Ref> {
        let _ = input;
        Err(unimplemented("checkout"))
    }

    pub fn vcs_merge(&self, input: MergeInput) -> RedDBResult<MergeOutcome> {
        let _ = input;
        Err(unimplemented("merge"))
    }

    pub fn vcs_cherry_pick(
        &self,
        connection_id: u64,
        commit: &str,
        author: Author,
    ) -> RedDBResult<MergeOutcome> {
        let _ = (connection_id, commit, author);
        Err(unimplemented("cherry_pick"))
    }

    pub fn vcs_revert(
        &self,
        connection_id: u64,
        commit: &str,
        author: Author,
    ) -> RedDBResult<Commit> {
        let _ = (connection_id, commit, author);
        Err(unimplemented("revert"))
    }

    pub fn vcs_reset(&self, input: ResetInput) -> RedDBResult<()> {
        let _ = input;
        Err(unimplemented("reset"))
    }

    pub fn vcs_log(&self, input: LogInput) -> RedDBResult<Vec<Commit>> {
        let _ = input;
        Ok(Vec::new())
    }

    pub fn vcs_diff(&self, input: DiffInput) -> RedDBResult<Diff> {
        let _ = input;
        Err(unimplemented("diff"))
    }

    pub fn vcs_status(&self, input: StatusInput) -> RedDBResult<Status> {
        Ok(Status {
            connection_id: input.connection_id,
            head_ref: None,
            head_commit: None,
            detached: false,
            staged_changes: 0,
            working_changes: 0,
            unresolved_conflicts: 0,
            merge_state_id: None,
        })
    }

    pub fn vcs_lca(&self, a: &str, b: &str) -> RedDBResult<Option<CommitHash>> {
        let _ = (a, b);
        Ok(None)
    }

    pub fn vcs_conflicts_list(
        &self,
        merge_state_id: &str,
    ) -> RedDBResult<Vec<Conflict>> {
        let _ = merge_state_id;
        Ok(Vec::new())
    }

    pub fn vcs_conflict_resolve(
        &self,
        conflict_id: &str,
        resolved: JsonValue,
    ) -> RedDBResult<()> {
        let _ = (conflict_id, resolved);
        Err(unimplemented("conflict_resolve"))
    }

    pub fn vcs_resolve_as_of(&self, spec: AsOfSpec) -> RedDBResult<Xid> {
        match spec {
            AsOfSpec::Snapshot(x) => Ok(x),
            _ => Err(unimplemented("resolve_as_of (non-snapshot)")),
        }
    }

    pub fn vcs_resolve_commitish(&self, spec: &str) -> RedDBResult<CommitHash> {
        let _ = spec;
        Err(unimplemented("resolve_commitish"))
    }
}
