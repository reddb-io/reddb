use super::*;

use crate::application::vcs::{
    AsOfSpec, CheckoutInput, Commit, CommitHash, Conflict, CreateBranchInput, CreateCommitInput,
    CreateTagInput, Diff, DiffInput, LogInput, MergeInput, MergeOutcome, Ref, ResetInput, Status,
    StatusInput,
};
use crate::json::Value as JsonValue;
use crate::storage::transaction::snapshot::Xid;

impl RuntimeVcsPort for RedDBRuntime {
    fn vcs_commit(&self, input: CreateCommitInput) -> RedDBResult<Commit> {
        RedDBRuntime::vcs_commit(self, input)
    }

    fn vcs_branch_create(&self, input: CreateBranchInput) -> RedDBResult<Ref> {
        RedDBRuntime::vcs_branch_create(self, input)
    }

    fn vcs_branch_delete(&self, name: &str) -> RedDBResult<()> {
        RedDBRuntime::vcs_branch_delete(self, name)
    }

    fn vcs_tag_create(&self, input: CreateTagInput) -> RedDBResult<Ref> {
        RedDBRuntime::vcs_tag_create(self, input)
    }

    fn vcs_list_refs(&self, prefix: Option<&str>) -> RedDBResult<Vec<Ref>> {
        RedDBRuntime::vcs_list_refs(self, prefix)
    }

    fn vcs_checkout(&self, input: CheckoutInput) -> RedDBResult<Ref> {
        RedDBRuntime::vcs_checkout(self, input)
    }

    fn vcs_merge(&self, input: MergeInput) -> RedDBResult<MergeOutcome> {
        RedDBRuntime::vcs_merge(self, input)
    }

    fn vcs_cherry_pick(
        &self,
        connection_id: u64,
        commit: &str,
        author: crate::application::vcs::Author,
    ) -> RedDBResult<MergeOutcome> {
        RedDBRuntime::vcs_cherry_pick(self, connection_id, commit, author)
    }

    fn vcs_revert(
        &self,
        connection_id: u64,
        commit: &str,
        author: crate::application::vcs::Author,
    ) -> RedDBResult<Commit> {
        RedDBRuntime::vcs_revert(self, connection_id, commit, author)
    }

    fn vcs_reset(&self, input: ResetInput) -> RedDBResult<()> {
        RedDBRuntime::vcs_reset(self, input)
    }

    fn vcs_log(&self, input: LogInput) -> RedDBResult<Vec<Commit>> {
        RedDBRuntime::vcs_log(self, input)
    }

    fn vcs_diff(&self, input: DiffInput) -> RedDBResult<Diff> {
        RedDBRuntime::vcs_diff(self, input)
    }

    fn vcs_status(&self, input: StatusInput) -> RedDBResult<Status> {
        RedDBRuntime::vcs_status(self, input)
    }

    fn vcs_lca(&self, a: &str, b: &str) -> RedDBResult<Option<CommitHash>> {
        RedDBRuntime::vcs_lca(self, a, b)
    }

    fn vcs_conflicts_list(&self, merge_state_id: &str) -> RedDBResult<Vec<Conflict>> {
        RedDBRuntime::vcs_conflicts_list(self, merge_state_id)
    }

    fn vcs_conflict_resolve(
        &self,
        conflict_id: &str,
        resolved: JsonValue,
    ) -> RedDBResult<()> {
        RedDBRuntime::vcs_conflict_resolve(self, conflict_id, resolved)
    }

    fn vcs_resolve_as_of(&self, spec: AsOfSpec) -> RedDBResult<Xid> {
        RedDBRuntime::vcs_resolve_as_of(self, spec)
    }

    fn vcs_resolve_commitish(&self, spec: &str) -> RedDBResult<CommitHash> {
        RedDBRuntime::vcs_resolve_commitish(self, spec)
    }

    fn vcs_set_versioned(&self, collection: &str, enabled: bool) -> RedDBResult<()> {
        RedDBRuntime::vcs_set_versioned(self, collection, enabled)
    }

    fn vcs_list_versioned(&self) -> RedDBResult<Vec<String>> {
        RedDBRuntime::vcs_list_versioned(self)
    }

    fn vcs_is_versioned(&self, collection: &str) -> RedDBResult<bool> {
        RedDBRuntime::vcs_is_versioned(self, collection)
    }
}
