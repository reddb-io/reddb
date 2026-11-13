//! Runtime implementation of the VCS ("Git for Data") surface.
//!
//! Branches retain immutable ancestor snapshots and independently version
//! subsequent writes. Native merge plans publish data and history together.
//!
//! Every VCS entity is stored as a plain TableRow in a `red_*`
//! collection (same pattern as `red.queue.meta` / `red.query.stats_catalog`).
//! Commits reuse the MVCC snapshot xid as their immutable root, and
//! pin that xid so VACUUM cannot reclaim historical row versions.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::application::vcs::{
    AsOfSpec, Author, CheckoutInput, CheckoutTarget, Commit, CommitHash, Conflict,
    CreateBranchInput, CreateCommitInput, CreateTagInput, Diff, DiffChange, DiffEntry, DiffInput,
    LogInput, LogRange, MergeInput, MergeOutcome, MergeStrategy, Ref, RefKind, RefName, ResetInput,
    ResetMode, Status, StatusInput,
};
use crate::application::vcs_collections as vc;
use crate::json::{Map, Value as JsonValue};
use crate::runtime::RedDBRuntime;
use crate::storage::transaction::snapshot::{Snapshot, SnapshotManager, Xid, XID_NONE};
use crate::storage::unified::entity::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};
use crate::storage::unified::UnifiedStore;
use crate::{RedDBError, RedDBResult};
use reddb_types::Value;

#[path = "vcs_branches.rs"]
mod branches;
#[path = "vcs_merge_data.rs"]
mod merge_data;
#[cfg(test)]
#[path = "vcs_metadata_tests.rs"]
mod metadata_tests;

impl RedDBRuntime {
    pub(crate) fn vcs_live_graph_validation_contexts(
        &self,
        collection: &str,
    ) -> RedDBResult<Vec<reddb_engine::transaction::read::SnapshotContext>> {
        let (mut context, _retention) = self.native_read_snapshot()?;
        context.snapshot = self.snapshot_manager().fresh_read_snapshot();
        context.own_xids.clear();
        if !self.vcs_is_versioned(collection)? {
            return Ok(vec![context]);
        }
        let store = self.db().store();
        let mut names = BTreeSet::from([vc::DEFAULT_BRANCH_REF.to_string()]);
        if let Some(manager) = store.get_collection(vc::REFS) {
            manager.try_for_each_entity(usize::MAX, |entity| {
                if let Some(name) = entity.data.as_row().and_then(|row| row_text(row, "id")) {
                    if name.starts_with(vc::BRANCH_REF_PREFIX) {
                        names.insert(name);
                    }
                }
                true
            })?;
        }
        names
            .into_iter()
            .map(|name| {
                let mut branch = match branches::load_view(&store, &name)? {
                    Some(branch) => branch,
                    None if name == vc::DEFAULT_BRANCH_REF => {
                        reddb_engine::transaction::read::BranchReadView {
                            writer_epoch: 0,
                            epochs: Default::default(),
                        }
                    }
                    None => {
                        return Err(RedDBError::InvalidOperation(format!(
                            "checkout legacy branch '{name}' before enabling a graph contract"
                        )))
                    }
                };
                branch
                    .epochs
                    .insert(branch.writer_epoch, context.snapshot.clone());
                let mut snapshot = context.clone();
                snapshot.branch = Some(Arc::new(branch));
                Ok(snapshot)
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn row_text(row: &RowData, field: &str) -> Option<String> {
    match row.get_field(field)?.clone() {
        Value::Text(v) => Some(v.to_string()),
        _ => None,
    }
}

fn row_u64(row: &RowData, field: &str) -> Option<u64> {
    match row.get_field(field)?.clone() {
        Value::UnsignedInteger(v) => Some(v),
        Value::Integer(v) if v >= 0 => Some(v as u64),
        _ => None,
    }
}

fn row_i64(row: &RowData, field: &str) -> Option<i64> {
    match row.get_field(field)?.clone() {
        Value::Integer(v) => Some(v),
        Value::UnsignedInteger(v) => Some(v as i64),
        Value::TimestampMs(v) | Value::Timestamp(v) => Some(v),
        _ => None,
    }
}

fn row_json(row: &RowData, field: &str) -> JsonValue {
    match row.get_field(field) {
        Some(Value::Json(bytes)) => {
            crate::json::from_slice::<JsonValue>(bytes).unwrap_or(JsonValue::Null)
        }
        Some(Value::Text(s)) => crate::json::from_str::<JsonValue>(s)
            .unwrap_or_else(|_| JsonValue::String(s.to_string())),
        _ => JsonValue::Null,
    }
}

fn row_string_list(row: &RowData, field: &str) -> Vec<String> {
    match row.get_field(field) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| match v {
                Value::Text(s) => Some(s.to_string()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn compute_commit_hash(
    root_xid: Xid,
    parents: &[CommitHash],
    author: &Author,
    message: &str,
    timestamp_ms: i64,
) -> CommitHash {
    let mut h = Sha256::new();
    h.update(b"reddb-commit-v1\n");
    h.update(root_xid.to_be_bytes());
    let mut sorted = parents.to_vec();
    sorted.sort();
    for p in &sorted {
        h.update(b"\np=");
        h.update(p.as_bytes());
    }
    h.update(b"\na=");
    h.update(author.name.as_bytes());
    h.update(b"\n");
    h.update(author.email.as_bytes());
    h.update(b"\nm=");
    h.update(message.as_bytes());
    h.update(b"\nt=");
    h.update(timestamp_ms.to_be_bytes());
    let digest = h.finalize();
    hex::encode(digest)
}

fn normalize_branch_name(raw: &str) -> String {
    if raw.starts_with(vc::BRANCH_REF_PREFIX) {
        raw.to_string()
    } else {
        format!("{}{}", vc::BRANCH_REF_PREFIX, raw)
    }
}

fn normalize_tag_name(raw: &str) -> String {
    if raw.starts_with(vc::TAG_REF_PREFIX) {
        raw.to_string()
    } else {
        format!("{}{}", vc::TAG_REF_PREFIX, raw)
    }
}

fn head_ref_id(connection_id: u64) -> String {
    format!("{}{}", vc::HEAD_ID_PREFIX, connection_id)
}

// ---------------------------------------------------------------------------
// Commit load / save
// ---------------------------------------------------------------------------

fn load_metadata_entity(
    store: &UnifiedStore,
    collection: &str,
    key: &str,
) -> RedDBResult<Option<UnifiedEntity>> {
    let mut result = None;
    if let Some(manager) = store.get_collection(collection) {
        super::function_budget::try_scan(
            |visit| manager.try_for_each_entity(usize::MAX, visit),
            |entity| {
                if entity
                    .data
                    .as_row()
                    .is_some_and(|row| row_text(row, "id").as_deref() == Some(key))
                {
                    result = Some(entity.clone());
                    return false;
                }
                true
            },
        )?;
    }
    Ok(result)
}

fn load_commit_entity(store: &UnifiedStore, hash: &str) -> RedDBResult<Option<UnifiedEntity>> {
    load_metadata_entity(store, vc::COMMITS, hash)
}

fn commit_from_row(row: &RowData) -> Option<Commit> {
    Some(Commit {
        hash: row_text(row, "id")?,
        root_xid: row_u64(row, "root_xid")?,
        parents: row_string_list(row, "parents"),
        height: row_u64(row, "height").unwrap_or(0),
        author: Author {
            name: row_text(row, "author_name").unwrap_or_default(),
            email: row_text(row, "author_email").unwrap_or_default(),
        },
        committer: Author {
            name: row_text(row, "committer_name").unwrap_or_default(),
            email: row_text(row, "committer_email").unwrap_or_default(),
        },
        message: row_text(row, "message").unwrap_or_default(),
        timestamp_ms: row_i64(row, "timestamp_ms").unwrap_or(0),
        signature: row_text(row, "signature"),
    })
}

fn load_commit(store: &UnifiedStore, hash: &str) -> RedDBResult<Option<Commit>> {
    load_commit_entity(store, hash)?
        .map(|entity| {
            entity
                .data
                .as_row()
                .and_then(commit_from_row)
                .ok_or_else(|| RedDBError::Internal(format!("vcs: invalid commit '{hash}'")))
        })
        .transpose()
}

struct CommitSnapshot<'a> {
    manager: &'a SnapshotManager,
    snapshot: Snapshot,
    retained: bool,
}

impl std::ops::Deref for CommitSnapshot<'_> {
    type Target = Snapshot;
    fn deref(&self) -> &Snapshot {
        &self.snapshot
    }
}

impl Drop for CommitSnapshot<'_> {
    fn drop(&mut self) {
        if !self.retained {
            self.manager.unpin(
                self.snapshot
                    .in_progress
                    .iter()
                    .copied()
                    .fold(self.snapshot.xid, Xid::min),
            );
        }
    }
}

fn capture_commit_snapshot(manager: &SnapshotManager) -> CommitSnapshot<'_> {
    let xid = manager.begin();
    let mut snapshot = manager.snapshot(xid);
    snapshot.in_progress.retain(|pending| *pending < xid);
    // An excluded writer may later delete a version visible to this commit.
    // Pin its horizon before committing the temporary xid releases protection.
    let horizon = snapshot.in_progress.iter().copied().fold(xid, Xid::min);
    manager.pin(horizon);
    manager.commit(xid);
    CommitSnapshot {
        manager,
        snapshot,
        retained: false,
    }
}

fn commit_snapshot_from_row(row: &RowData) -> RedDBResult<Snapshot> {
    let xid = row_u64(row, "root_xid")
        .ok_or_else(|| RedDBError::Internal("vcs: commit has no root snapshot".to_string()))?;
    let mut in_progress = HashSet::new();
    // Legacy commits recorded only the root xid; their exclusions are unknown.
    if let Some(value) = row.get_field("snapshot_in_progress") {
        let Value::Array(values) = value else {
            return Err(RedDBError::Internal(
                "vcs: invalid commit snapshot exclusions".to_string(),
            ));
        };
        for value in values {
            let pending = match value {
                Value::UnsignedInteger(value) => *value,
                Value::Integer(value) if *value > 0 => *value as u64,
                _ => {
                    return Err(RedDBError::Internal(
                        "vcs: invalid commit snapshot exclusion".to_string(),
                    ));
                }
            };
            if pending == XID_NONE || pending >= xid {
                return Err(RedDBError::Internal(
                    "vcs: commit snapshot exclusion outside its horizon".to_string(),
                ));
            }
            in_progress.insert(pending);
        }
    }
    Ok(Snapshot { xid, in_progress })
}

fn snapshot_sees_entity(
    context: &reddb_engine::transaction::read::SnapshotContext,
    entity: &UnifiedEntity,
) -> bool {
    reddb_engine::transaction::read::entity_visibility_check(context, entity)
}

fn commit_fields(
    commit: &Commit,
    snapshot: &Snapshot,
    view: &reddb_engine::transaction::read::BranchReadView,
) -> HashMap<String, Value> {
    let mut fields: HashMap<String, Value> = HashMap::new();
    fields.insert("branch_lineage".into(), branches::encode_epochs(view));
    fields.insert(
        "writer_epoch".into(),
        Value::UnsignedInteger(view.writer_epoch),
    );
    fields.insert("id".to_string(), Value::text(commit.hash.as_str()));
    fields.insert(
        "root_xid".to_string(),
        Value::UnsignedInteger(commit.root_xid),
    );
    let mut pending: Vec<Xid> = snapshot.in_progress.iter().copied().collect();
    pending.sort_unstable();
    fields.insert(
        "snapshot_in_progress".to_string(),
        Value::Array(pending.into_iter().map(Value::UnsignedInteger).collect()),
    );
    fields.insert(
        "parents".to_string(),
        Value::Array(
            commit
                .parents
                .iter()
                .map(|p| Value::text(p.as_str()))
                .collect(),
        ),
    );
    fields.insert("height".to_string(), Value::UnsignedInteger(commit.height));
    fields.insert(
        "author_name".to_string(),
        Value::text(commit.author.name.as_str()),
    );
    fields.insert(
        "author_email".to_string(),
        Value::text(commit.author.email.as_str()),
    );
    fields.insert(
        "committer_name".to_string(),
        Value::text(commit.committer.name.as_str()),
    );
    fields.insert(
        "committer_email".to_string(),
        Value::text(commit.committer.email.as_str()),
    );
    fields.insert("message".to_string(), Value::text(commit.message.as_str()));
    fields.insert(
        "timestamp_ms".to_string(),
        Value::TimestampMs(commit.timestamp_ms),
    );
    if let Some(sig) = &commit.signature {
        fields.insert("signature".to_string(), Value::text(sig.as_str()));
    }
    fields
}

// ---------------------------------------------------------------------------
// Ref load / save / delete
// ---------------------------------------------------------------------------

fn ref_kind_from_str(s: &str) -> Option<RefKind> {
    match s {
        "branch" => Some(RefKind::Branch),
        "tag" => Some(RefKind::Tag),
        "head" => Some(RefKind::Head),
        _ => None,
    }
}

fn ref_from_row(row: &RowData) -> Option<Ref> {
    Some(Ref {
        name: row_text(row, "id")?,
        kind: ref_kind_from_str(&row_text(row, "type")?)?,
        target: row_text(row, "target")?,
        protected: row
            .get_field("protected")
            .and_then(|v| match v {
                Value::Boolean(b) => Some(*b),
                _ => None,
            })
            .unwrap_or(false),
    })
}

fn load_ref_entity(
    store: &UnifiedStore,
    name: &str,
) -> RedDBResult<Option<(EntityId, UnifiedEntity)>> {
    Ok(load_metadata_entity(store, vc::REFS, name)?.map(|entity| (entity.id, entity)))
}

fn load_ref(store: &UnifiedStore, name: &str) -> RedDBResult<Option<Ref>> {
    load_ref_entity(store, name)?
        .map(|(_, entity)| {
            entity
                .data
                .as_row()
                .and_then(ref_from_row)
                .ok_or_else(|| RedDBError::Internal(format!("vcs: invalid reference '{name}'")))
        })
        .transpose()
}

fn stage_ref(
    store: &UnifiedStore,
    publication: &mut crate::storage::unified::store::RowPublication,
    reference: &Ref,
) -> RedDBResult<()> {
    let kind = match reference.kind {
        RefKind::Branch => "branch",
        RefKind::Tag => "tag",
        RefKind::Head => "head",
    };
    branches::stage_metadata(
        store,
        publication,
        vc::REFS,
        "id",
        &reference.name,
        HashMap::from([
            ("id".into(), Value::text(reference.name.as_str())),
            ("type".into(), Value::text(kind)),
            ("target".into(), Value::text(reference.target.as_str())),
            ("protected".into(), Value::Boolean(reference.protected)),
        ]),
    )
}

fn save_ref(store: &UnifiedStore, reference: &Ref) -> RedDBResult<()> {
    let mut publication = crate::storage::unified::store::RowPublication::default();
    stage_ref(store, &mut publication, reference)?;
    store.publish_rows(publication)?;
    Ok(())
}

fn delete_ref(store: &UnifiedStore, name: &str) -> RedDBResult<bool> {
    let Some((id, _)) = load_ref_entity(store, name)? else {
        return Ok(false);
    };
    let mut publication = crate::storage::unified::store::RowPublication::default();
    publication.deletes.push((vc::REFS.into(), id));
    store.publish_rows(publication)?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Opt-in per-collection versioning (Phase 7 — disk-cost isolation)
// ---------------------------------------------------------------------------

/// Is this user collection opted in to Git-for-Data?
///
/// Default is `false`: a fresh collection stays outside the VCS so
/// transactional churn (sessions, caches, queues) doesn't pin
/// extra row versions. Internal `red_*` collections are never
/// versioned — they store VCS metadata itself.
fn is_versioned(store: &UnifiedStore, name: &str) -> RedDBResult<bool> {
    if name.starts_with("red_") || name.starts_with("red.") {
        return Ok(false);
    }
    let Some(manager) = store.get_collection(vc::SETTINGS) else {
        return Ok(false);
    };
    let _publication = manager
        .has_backing()
        .then(|| store.vault_publication_lock.lock());
    let mut versioned = false;
    super::function_budget::try_scan(
        |visit| manager.try_for_each_entity(usize::MAX, visit),
        |entity| {
            versioned = entity.data.as_row().is_some_and(|row| {
                row.get_field("id")
                    .is_some_and(|id| matches!(id, Value::Text(id) if id.as_ref() == name))
                    && matches!(row.get_field("versioned"), Some(Value::Boolean(true)))
            });
            !versioned
        },
    )?;
    Ok(versioned)
}

/// Enumerate every user collection currently opted in. Order
/// undefined — callers that need a deterministic iteration should
/// sort the returned list.
fn versioned_collections(store: &UnifiedStore) -> RedDBResult<Vec<String>> {
    let Some(manager) = store.get_collection(vc::SETTINGS) else {
        return Ok(Vec::new());
    };
    let _publication = manager
        .has_backing()
        .then(|| store.vault_publication_lock.lock());
    let mut collections = Vec::new();
    super::function_budget::try_scan(
        |visit| manager.try_for_each_entity(usize::MAX, visit),
        |entity| {
            if let Some(row) = entity.data.as_row() {
                if matches!(row.get_field("versioned"), Some(Value::Boolean(true))) {
                    if let Some(name) = row_text(row, "id") {
                        collections.push(name);
                    }
                }
            }
            true
        },
    )?;
    Ok(collections)
}

fn list_refs_by_prefix(store: &UnifiedStore, prefix: Option<&str>) -> RedDBResult<Vec<Ref>> {
    let mut refs = Vec::new();
    if let Some(manager) = store.get_collection(vc::REFS) {
        super::function_budget::try_scan_result(
            |visit| manager.try_for_each_entity(usize::MAX, visit),
            |entity| {
                let row = entity
                    .data
                    .as_row()
                    .ok_or_else(|| RedDBError::Internal("vcs: invalid reference row".into()))?;
                let reference = ref_from_row(row)
                    .ok_or_else(|| RedDBError::Internal("vcs: invalid reference".into()))?;
                if prefix.is_none_or(|prefix| reference.name.starts_with(prefix)) {
                    refs.push(reference);
                }
                Ok(true)
            },
        )?;
    }
    Ok(refs)
}

// ---------------------------------------------------------------------------
// Ancestry helpers
// ---------------------------------------------------------------------------

fn ancestor_set(
    store: &UnifiedStore,
    start: &str,
    max_steps: usize,
) -> RedDBResult<HashSet<CommitHash>> {
    let mut visited: HashSet<CommitHash> = HashSet::new();
    let mut stack: Vec<CommitHash> = vec![start.to_string()];
    let mut steps = 0usize;
    while let Some(hash) = stack.pop() {
        if !visited.insert(hash.clone()) {
            continue;
        }
        {
            let c = load_commit(store, &hash)?
                .ok_or_else(|| RedDBError::Internal(format!("vcs: missing ancestor '{hash}'")))?;
            for p in c.parents {
                if !visited.contains(&p) {
                    stack.push(p);
                }
            }
        }
        steps += 1;
        if steps >= max_steps && !stack.is_empty() {
            return Err(RedDBError::InvalidOperation(
                "vcs: ancestry exceeds traversal budget".into(),
            ));
        }
    }
    Ok(visited)
}

fn topo_walk(store: &UnifiedStore, start: &str, range: &LogRange) -> RedDBResult<Vec<Commit>> {
    let limit = range.limit.unwrap_or(usize::MAX);
    let skip = range.skip.unwrap_or(0);
    let exclude = range
        .from
        .as_ref()
        .map(|h| ancestor_set(store, h, 100_000))
        .transpose()?
        .unwrap_or_default();

    let mut visited: HashSet<CommitHash> = HashSet::new();
    let mut stack: Vec<CommitHash> = vec![start.to_string()];
    let mut out: Vec<Commit> = Vec::new();
    let mut skipped = 0usize;
    while let Some(hash) = stack.pop() {
        if !visited.insert(hash.clone()) {
            continue;
        }
        if exclude.contains(&hash) {
            continue;
        }
        let commit = load_commit(store, &hash)?
            .ok_or_else(|| RedDBError::Internal(format!("vcs: missing ancestor '{hash}'")))?;
        if range.no_merges && commit.parents.len() > 1 {
            for p in &commit.parents {
                if !visited.contains(p) {
                    stack.push(p.clone());
                }
            }
            continue;
        }
        for p in &commit.parents {
            if !visited.contains(p) {
                stack.push(p.clone());
            }
        }
        if skipped < skip {
            skipped += 1;
            continue;
        }
        if out.len() >= limit {
            break;
        }
        out.push(commit);
    }
    // Highest height first (newest commits on top).
    out.sort_by(|a, b| {
        b.height
            .cmp(&a.height)
            .then(b.timestamp_ms.cmp(&a.timestamp_ms))
    });
    Ok(out)
}

// ---------------------------------------------------------------------------
// Commitish resolution
// ---------------------------------------------------------------------------

fn resolve_ref_chain(store: &UnifiedStore, name: &str) -> RedDBResult<Option<CommitHash>> {
    let mut current = name.to_string();
    for _ in 0..4 {
        let Some(reference) = load_ref(store, &current)? else {
            return Ok(None);
        };
        match reference.kind {
            RefKind::Branch | RefKind::Tag => return Ok(Some(reference.target)),
            RefKind::Head => current = reference.target,
        }
    }
    Err(RedDBError::InvalidOperation(
        "vcs: reference chain exceeds four levels".into(),
    ))
}

fn resolve_short_commit(store: &UnifiedStore, prefix: &str) -> RedDBResult<Option<CommitHash>> {
    if prefix.len() < 4 {
        return Ok(None);
    }
    let mut found = None;
    let mut ambiguous = false;
    if let Some(manager) = store.get_collection(vc::COMMITS) {
        super::function_budget::try_scan(
            |visit| manager.try_for_each_entity(usize::MAX, visit),
            |entity| {
                if let Some(hash) = entity.data.as_row().and_then(|row| row_text(row, "id")) {
                    if hash.starts_with(prefix) {
                        ambiguous = found.is_some();
                        found = Some(hash);
                    }
                }
                !ambiguous
            },
        )?;
    }
    Ok(if ambiguous { None } else { found })
}

// ---------------------------------------------------------------------------
// Workset helpers (lightweight — full WIP tracking lands in Phase 3)
// ---------------------------------------------------------------------------

fn stage_workset(
    store: &UnifiedStore,
    publication: &mut crate::storage::unified::store::RowPublication,
    connection_id: u64,
    branch: &str,
    base_commit: Option<&str>,
    working_xid: Xid,
) -> RedDBResult<()> {
    let connection = connection_id.to_string();
    let mut fields = HashMap::from([
        ("id".into(), Value::text(connection.as_str())),
        ("branch".into(), Value::text(branch)),
        ("working_xid".into(), Value::UnsignedInteger(working_xid)),
    ]);
    if let Some(base) = base_commit {
        fields.insert("base_commit".into(), Value::text(base));
    }
    branches::stage_metadata(store, publication, vc::WORKSETS, "id", &connection, fields)
}

fn load_workset(
    store: &UnifiedStore,
    connection_id: u64,
) -> RedDBResult<Option<(RefName, Option<CommitHash>)>> {
    Ok(load_workset_state(store, connection_id)?.map(|(branch, base, _)| (branch, base)))
}

fn load_workset_state(
    store: &UnifiedStore,
    connection_id: u64,
) -> RedDBResult<Option<(RefName, Option<CommitHash>, Option<String>)>> {
    let Some(entity) = load_metadata_entity(store, vc::WORKSETS, &connection_id.to_string())?
    else {
        return Ok(None);
    };
    let row = entity
        .data
        .as_row()
        .ok_or_else(|| RedDBError::Internal("vcs: invalid workset".into()))?;
    let branch = row_text(row, "branch")
        .ok_or_else(|| RedDBError::Internal("vcs: workset has no branch".into()))?;
    let base = row_text(row, "base_commit");
    let pending_merge = row_text(row, "merge_state_id");
    let head = if branch.starts_with(vc::BRANCH_REF_PREFIX) && pending_merge.is_none() {
        load_ref(store, &branch)?
            .map(|reference| reference.target)
            .or(base)
    } else {
        base
    };
    Ok(Some((branch, head, pending_merge)))
}

fn ensure_no_pending_merge(store: &UnifiedStore, connection_id: u64) -> RedDBResult<()> {
    let branch = load_workset(store, connection_id)?
        .map(|(branch, _)| branch)
        .unwrap_or_else(|| vc::DEFAULT_BRANCH_REF.into());
    let mut pending = None;
    if let Some(manager) = store.get_collection(vc::WORKSETS) {
        manager.try_for_each_entity(usize::MAX, |entity| {
            if let Some(row) = entity
                .data
                .as_row()
                .filter(|row| row_text(row, "branch").as_deref() == Some(&branch))
            {
                pending = row_text(row, "merge_state_id");
            }
            pending.is_none()
        })?;
    }
    if let Some(state) = pending {
        return Err(RedDBError::InvalidOperation(format!(
            "cannot change branch data or HEAD while merge `{state}` is pending"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Runtime impl
// ---------------------------------------------------------------------------

impl RedDBRuntime {
    pub(crate) fn vcs_reject_destructive_ddl(&self, collection: &str) -> RedDBResult<()> {
        if self.vcs_is_versioned(collection)? {
            return Err(RedDBError::InvalidOperation(format!(
                "cannot physically drop or truncate versioned collection '{collection}'; use versioned deletes or QUEUE PURGE"
            )));
        }
        Ok(())
    }

    pub(super) fn rehydrate_vcs_snapshot_pins(&self) -> RedDBResult<()> {
        let store = self.inner.db.store();
        let Some(commits) = store.get_collection(vc::COMMITS) else {
            return Ok(());
        };
        let manager = self.inner.transaction_state.snapshot_manager();
        let mut failure = None;
        commits.try_for_each_entity(usize::MAX, |entity| {
            let Some(row) = entity.data.as_row() else {
                failure = Some(RedDBError::Internal(
                    "vcs: invalid commit record".to_string(),
                ));
                return false;
            };
            match commit_snapshot_from_row(row) {
                Ok(snapshot) => {
                    manager.observe_committed_xid(snapshot.xid);
                    let horizon = snapshot
                        .in_progress
                        .iter()
                        .copied()
                        .fold(snapshot.xid, Xid::min);
                    manager.pin(horizon);
                    true
                }
                Err(error) => {
                    failure = Some(error);
                    false
                }
            }
        })?;
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub fn vcs_commit(&self, input: CreateCommitInput) -> RedDBResult<Commit> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let _publication = store.vault_publication_lock.lock();

        if let Some((_, _, Some(state))) = load_workset_state(store, input.connection_id)? {
            return merge_data::finish_pending(self, input, &state);
        }

        ensure_no_pending_merge(store, input.connection_id)?;

        // Resolve current HEAD for this connection: workset -> HEAD:<conn>
        // -> default branch.
        let workset = load_workset(store, input.connection_id)?;
        let branch_ref = workset
            .as_ref()
            .map(|(b, _)| b.clone())
            .unwrap_or_else(|| vc::DEFAULT_BRANCH_REF.to_string());

        if !branch_ref.starts_with(vc::BRANCH_REF_PREFIX) {
            return Err(RedDBError::InvalidOperation(
                "cannot commit a detached or tag checkout".into(),
            ));
        }
        let parent_hash = load_ref(store, &branch_ref)?
            .map(|reference| reference.target)
            .or_else(|| workset.as_ref().and_then(|(_, base)| base.clone()));

        let parents: Vec<CommitHash> = match parent_hash.clone() {
            Some(h) if !h.is_empty() => vec![h],
            _ => Vec::new(),
        };

        let mut parent_height = 0;
        for parent in &parents {
            let commit = load_commit(store, parent)?
                .ok_or_else(|| RedDBError::Internal("vcs: missing parent commit".into()))?;
            parent_height = parent_height.max(commit.height);
        }
        let height = if parents.is_empty() {
            0
        } else {
            parent_height + 1
        };

        // Allocate a fresh xid for this commit and immediately mark
        // it committed so all future snapshots see the commit record.
        // Each commit therefore has a unique, monotonic root_xid — a
        // prerequisite for `AS OF BRANCH` to map to distinct
        // snapshots across divergent branches.
        let snapshot_manager = self.inner.transaction_state.snapshot_manager();
        let mut snapshot = capture_commit_snapshot(&snapshot_manager);
        let root_xid = snapshot.xid;
        let timestamp_ms = now_ms();
        let committer = input.committer.unwrap_or_else(|| input.author.clone());

        let hash = compute_commit_hash(
            root_xid,
            &parents,
            &input.author,
            &input.message,
            timestamp_ms,
        );

        if load_commit(store, &hash)?.is_some() {
            return Err(RedDBError::InvalidConfig(format!(
                "commit {hash} already exists (duplicate timestamp+content)"
            )));
        }

        let commit = Commit {
            hash: hash.clone(),
            root_xid,
            parents,
            height,
            author: input.author,
            committer,
            message: input.message,
            timestamp_ms,
            signature: None,
        };

        if !branch_ref.starts_with(vc::BRANCH_REF_PREFIX) {
            return Err(RedDBError::InvalidOperation(
                "cannot commit a detached or tag checkout".into(),
            ));
        }
        merge_data::publish_commit(
            self,
            input.connection_id,
            &branch_ref,
            &commit,
            &snapshot,
            None,
        )?;

        snapshot.retained = true;
        Ok(commit)
    }

    pub fn vcs_branch_create(&self, input: CreateBranchInput) -> RedDBResult<Ref> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let _publication = store.vault_publication_lock.lock();
        let full_name = normalize_branch_name(&input.name);
        if load_ref(store, &full_name)?.is_some() {
            return Err(RedDBError::InvalidConfig(format!(
                "branch `{full_name}` already exists"
            )));
        }
        let target_hash = match &input.from {
            Some(spec) => RedDBRuntime::vcs_resolve_commitish(self, spec)?,
            None => {
                // Fall back to the connection's current HEAD branch, then default.
                let workset = load_workset(store, input.connection_id)?;

                workset
                    .and_then(|(_, base)| base)
                    .or(load_ref(store, vc::DEFAULT_BRANCH_REF)?.map(|r| r.target))
                    .unwrap_or_default()
            }
        };
        let r = Ref {
            name: full_name,
            kind: RefKind::Branch,
            target: target_hash,
            protected: false,
        };
        let mut publication = crate::storage::unified::store::RowPublication::default();
        branches::stage_new_view(store, &mut publication, &r.name, &r.target)?;
        stage_ref(store, &mut publication, &r)?;
        store.publish_rows(publication)?;
        Ok(r)
    }

    pub fn vcs_branch_delete(&self, name: &str) -> RedDBResult<()> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let _publication = store.vault_publication_lock.lock();
        let full = normalize_branch_name(name);
        let existing = load_ref(store, &full)?
            .ok_or_else(|| RedDBError::NotFound(format!("branch `{full}` does not exist")))?;
        if existing.protected {
            return Err(RedDBError::ReadOnly(format!(
                "branch `{full}` is protected"
            )));
        }
        if full == vc::DEFAULT_BRANCH_REF {
            return Err(RedDBError::InvalidOperation(
                "cannot delete the default branch".into(),
            ));
        }
        let mut publication = crate::storage::unified::store::RowPublication::default();
        if let Some((id, _)) = load_ref_entity(store, &full)? {
            publication.deletes.push((vc::REFS.into(), id));
        }
        branches::stage_delete_view(store, &mut publication, &full)?;
        store.publish_rows(publication)?;
        Ok(())
    }

    pub fn vcs_tag_create(&self, input: CreateTagInput) -> RedDBResult<Ref> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let _publication = store.vault_publication_lock.lock();
        let full_name = normalize_tag_name(&input.name);
        if load_ref(store, &full_name)?.is_some() {
            return Err(RedDBError::InvalidConfig(format!(
                "tag `{full_name}` already exists"
            )));
        }
        let target_hash = RedDBRuntime::vcs_resolve_commitish(self, &input.target)?;
        let r = Ref {
            name: full_name,
            kind: RefKind::Tag,
            target: target_hash,
            protected: false,
        };
        save_ref(store, &r)?;
        Ok(r)
    }

    pub fn vcs_tag_delete(&self, name: &str) -> RedDBResult<()> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let _publication = store.vault_publication_lock.lock();
        let full = normalize_tag_name(name);
        let existing = load_ref(store, &full)?
            .ok_or_else(|| RedDBError::NotFound(format!("tag `{full}` does not exist")))?;
        if existing.protected {
            return Err(RedDBError::ReadOnly(format!("tag `{full}` is protected")));
        }
        delete_ref(store, &full)?;
        Ok(())
    }

    pub fn vcs_list_refs(&self, prefix: Option<&str>) -> RedDBResult<Vec<Ref>> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        Ok(list_refs_by_prefix(store, prefix)?)
    }

    pub fn vcs_checkout(&self, input: CheckoutInput) -> RedDBResult<Ref> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let _publication = store.vault_publication_lock.lock();
        if self
            .inner
            .transaction_state
            .context(input.connection_id)
            .is_some()
        {
            return Err(RedDBError::InvalidOperation(
                "cannot checkout during an active transaction".into(),
            ));
        }
        if !input.force {
            ensure_no_pending_merge(store, input.connection_id)?;
        }
        let mut publication = crate::storage::unified::store::RowPublication::default();
        let (branch_ref, target_hash) = match &input.target {
            CheckoutTarget::Branch(name) => {
                let full = normalize_branch_name(name);
                let r = load_ref(store, &full)?.ok_or_else(|| {
                    RedDBError::NotFound(format!("branch `{full}` does not exist"))
                })?;
                if full != vc::DEFAULT_BRANCH_REF && branches::load_view(store, &full)?.is_none() {
                    branches::stage_new_view(store, &mut publication, &full, &r.target)?;
                }
                (full, r.target)
            }
            CheckoutTarget::Tag(name) => {
                let full = normalize_tag_name(name);
                let r = load_ref(store, &full)?
                    .ok_or_else(|| RedDBError::NotFound(format!("tag `{full}` does not exist")))?;
                (full, r.target)
            }
            CheckoutTarget::Commit(hash) => {
                let resolved = RedDBRuntime::vcs_resolve_commitish(self, hash)?;
                // Detached HEAD — we point HEAD directly at a commit via
                // the workset base; no branch ref is updated.
                (String::new(), resolved)
            }
        };

        stage_workset(
            store,
            &mut publication,
            input.connection_id,
            &branch_ref,
            Some(&target_hash),
            self.inner
                .transaction_state
                .snapshot_manager()
                .peek_next_xid(),
        )?;

        // Update per-connection HEAD pointer for status/introspection.
        stage_ref(
            store,
            &mut publication,
            &Ref {
                name: head_ref_id(input.connection_id),
                kind: RefKind::Head,
                target: if branch_ref.is_empty() {
                    target_hash.clone()
                } else {
                    branch_ref.clone()
                },
                protected: false,
            },
        )?;

        store.publish_rows(publication)?;
        Ok(Ref {
            name: if branch_ref.is_empty() {
                format!("detached:{target_hash}")
            } else {
                branch_ref
            },
            kind: if target_hash.is_empty() {
                RefKind::Head
            } else {
                RefKind::Branch
            },
            target: target_hash,
            protected: false,
        })
    }

    pub fn vcs_merge(&self, input: MergeInput) -> RedDBResult<MergeOutcome> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let _publication = store.vault_publication_lock.lock();
        ensure_no_pending_merge(store, input.connection_id)?;

        // Resolve source commit (the one being merged in).
        let from_hash = RedDBRuntime::vcs_resolve_commitish(self, &input.from)?;
        let from_commit = load_commit(store, &from_hash)?.ok_or_else(|| {
            RedDBError::NotFound(format!("source commit `{from_hash}` not found"))
        })?;

        // Resolve current HEAD for the connection.
        let workset = load_workset(store, input.connection_id)?;
        let (head_branch, head_hash) = match workset {
            Some((branch, Some(head))) => (branch, head),
            Some((branch, None)) => {
                let head = load_ref(store, &branch)?
                    .map(|r| r.target)
                    .unwrap_or_default();
                (branch, head)
            }
            None => {
                let head = load_ref(store, vc::DEFAULT_BRANCH_REF)?
                    .map(|r| r.target)
                    .unwrap_or_default();
                (vc::DEFAULT_BRANCH_REF.to_string(), head)
            }
        };
        if head_hash.is_empty() {
            return Err(RedDBError::InvalidConfig(
                "cannot merge: HEAD has no commits".to_string(),
            ));
        }

        // Fast-forward check: is HEAD an ancestor of `from`?
        let from_ancestors = ancestor_set(store, &from_hash, 100_000)?;
        let can_fast_forward = from_ancestors.contains(&head_hash);

        match input.opts.strategy {
            MergeStrategy::FastForwardOnly if !can_fast_forward => {
                return Err(RedDBError::InvalidConfig(
                    "not a fast-forward — use --strategy auto or no-ff".to_string(),
                ));
            }
            _ => {}
        }

        if can_fast_forward && input.opts.strategy != MergeStrategy::NoFastForward {
            if head_branch.is_empty() {
                return Err(RedDBError::InvalidConfig(
                    "cannot fast-forward a detached HEAD".to_string(),
                ));
            }
            branches::ensure_entity_history_supported(self)?;
            branches::ensure_clean(self, input.connection_id, &head_hash)?;
            merge_data::plan(self, &head_hash, &head_hash, &from_hash, "head-change")?;
            branches::publish_head(
                store,
                input.connection_id,
                &head_branch,
                &from_commit,
                branches::commit_view(store, &from_hash)?,
            )?;
            return Ok(MergeOutcome {
                merge_commit: Some(from_commit),
                fast_forward: true,
                conflicts: Vec::new(),
                merge_state_id: None,
            });
        }

        if !head_branch.starts_with(vc::BRANCH_REF_PREFIX) {
            return Err(RedDBError::InvalidOperation(
                "cannot merge into a detached or tag checkout".into(),
            ));
        }
        branches::ensure_entity_history_supported(self)?;
        branches::ensure_clean(self, input.connection_id, &head_hash)?;
        let lca = RedDBRuntime::vcs_lca(self, &head_hash, &from_hash)?.ok_or_else(|| {
            RedDBError::InvalidOperation("cannot merge unrelated histories".into())
        })?;

        let message = input
            .opts
            .message
            .unwrap_or_else(|| format!("Merge {} into {}", input.from, head_branch));
        let author = input.author.clone();
        let timestamp_ms = now_ms();
        let parents = vec![head_hash.clone(), from_hash.clone()];
        let mut parent_height = 0;
        for parent in &parents {
            let commit = load_commit(store, parent)?
                .ok_or_else(|| RedDBError::Internal("vcs: missing parent commit".into()))?;
            parent_height = parent_height.max(commit.height);
        }
        let height = parent_height + 1;
        let snapshot_manager = self.inner.transaction_state.snapshot_manager();
        let mut snapshot = capture_commit_snapshot(&snapshot_manager);
        let root_xid = snapshot.xid;

        let hash = compute_commit_hash(root_xid, &parents, &author, &message, timestamp_ms);

        let merge_commit = Commit {
            hash: hash.clone(),
            root_xid,
            parents,
            height,
            author: author.clone(),
            committer: author,
            message,
            timestamp_ms,
            signature: None,
        };

        let merge_state_id = format!("ms:{}", &hash[..16]);

        let plan = merge_data::plan(self, &lca, &head_hash, &from_hash, &merge_state_id)?;
        let conflicts = plan.conflicts.clone();
        if conflicts.is_empty() {
            let result = merge_data::publish_commit(
                self,
                input.connection_id,
                &head_branch,
                &merge_commit,
                &snapshot,
                Some(plan),
            );
            result?;
            snapshot.retained = true;
            return Ok(MergeOutcome {
                merge_commit: Some(merge_commit),
                fast_forward: false,
                conflicts,
                merge_state_id: None,
            });
        }
        let mut publication = crate::storage::unified::store::RowPublication::default();
        stage_merge_conflicts(store, &mut publication, &conflicts)?;

        let mut ms_fields: HashMap<String, Value> = HashMap::new();
        ms_fields.insert("id".to_string(), Value::text(merge_state_id.as_str()));
        ms_fields.insert("kind".to_string(), Value::text("merge"));
        ms_fields.insert("branch".to_string(), Value::text(head_branch.as_str()));
        ms_fields.insert("base".to_string(), Value::text(lca.as_str()));
        ms_fields.insert("ours".to_string(), Value::text(head_hash.as_str()));
        ms_fields.insert("theirs".to_string(), Value::text(from_hash.as_str()));
        ms_fields.insert(
            "conflicts_count".to_string(),
            Value::UnsignedInteger(conflicts.len() as u64),
        );
        branches::stage_metadata(
            store,
            &mut publication,
            vc::MERGE_STATE,
            "id",
            &merge_state_id,
            ms_fields,
        )?;

        let head_xid = self.vcs_resolve_as_of(AsOfSpec::Commit(head_hash.clone()))?;
        let conn_id = input.connection_id.to_string();
        branches::stage_metadata(
            store,
            &mut publication,
            vc::WORKSETS,
            "id",
            &conn_id,
            HashMap::from([
                ("id".into(), Value::text(conn_id.as_str())),
                ("branch".into(), Value::text(head_branch.as_str())),
                ("base_commit".into(), Value::text(head_hash.as_str())),
                ("working_xid".into(), Value::UnsignedInteger(head_xid)),
                (
                    "merge_state_id".into(),
                    Value::text(merge_state_id.as_str()),
                ),
            ]),
        )?;
        store.publish_rows(publication)?;
        Ok(MergeOutcome {
            merge_commit: None,
            fast_forward: false,
            conflicts,
            merge_state_id: Some(merge_state_id),
        })
    }

    pub fn vcs_cherry_pick(
        &self,
        connection_id: u64,
        commit: &str,
        author: Author,
    ) -> RedDBResult<MergeOutcome> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let _publication = store.vault_publication_lock.lock();
        ensure_no_pending_merge(store, connection_id)?;

        // Resolve the commit being cherry-picked and its parent.
        let src_hash = RedDBRuntime::vcs_resolve_commitish(self, commit)?;
        let src_commit = load_commit(store, &src_hash)?.ok_or_else(|| {
            RedDBError::NotFound(format!("cherry-pick source `{src_hash}` not found"))
        })?;
        if src_commit.parents.is_empty() {
            return Err(RedDBError::InvalidConfig(
                "cannot cherry-pick a root commit".to_string(),
            ));
        }
        if src_commit.parents.len() > 1 {
            return Err(RedDBError::InvalidConfig(
                "cannot cherry-pick a merge commit; resolve manually".to_string(),
            ));
        }
        let parent_hash = src_commit.parents[0].clone();

        let picked = merge_data::apply_delta(
            self,
            connection_id,
            &parent_hash,
            &src_hash,
            author,
            format!("cherry-pick: {}", src_commit.message),
        )?;
        Ok(MergeOutcome {
            merge_commit: Some(picked),
            fast_forward: false,
            conflicts: Vec::new(),
            merge_state_id: None,
        })
    }

    pub fn vcs_revert(
        &self,
        connection_id: u64,
        commit: &str,
        author: Author,
    ) -> RedDBResult<Commit> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let _publication = store.vault_publication_lock.lock();
        ensure_no_pending_merge(store, connection_id)?;

        let src_hash = RedDBRuntime::vcs_resolve_commitish(self, commit)?;
        let src_commit = load_commit(store, &src_hash)?
            .ok_or_else(|| RedDBError::NotFound(format!("revert source `{src_hash}` not found")))?;
        if src_commit.parents.is_empty() {
            return Err(RedDBError::InvalidConfig(
                "cannot revert a root commit".to_string(),
            ));
        }
        let parent_hash = src_commit.parents[0].clone();

        if src_commit.parents.len() != 1 {
            return Err(RedDBError::InvalidOperation(
                "cannot revert a merge commit without selecting a parent".into(),
            ));
        }
        merge_data::apply_delta(
            self,
            connection_id,
            &src_hash,
            &parent_hash,
            author,
            format!("Revert \"{}\"", src_commit.message),
        )
    }

    pub fn vcs_reset(&self, input: ResetInput) -> RedDBResult<()> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let _publication = store.vault_publication_lock.lock();
        ensure_no_pending_merge(store, input.connection_id)?;
        let target_hash = RedDBRuntime::vcs_resolve_commitish(self, &input.target)?;
        let target_commit = load_commit(store, &target_hash)?.ok_or_else(|| {
            RedDBError::NotFound(format!("target commit `{target_hash}` not found"))
        })?;

        // Find the current branch for this connection.
        let workset = load_workset(store, input.connection_id)?;
        let branch = workset
            .as_ref()
            .map(|(b, _)| b.clone())
            .unwrap_or_else(|| vc::DEFAULT_BRANCH_REF.to_string());

        if !branch.starts_with(vc::BRANCH_REF_PREFIX) {
            return Err(RedDBError::InvalidOperation(
                "reset requires a branch".into(),
            ));
        }
        if self.snapshot_manager().oldest_active_xid().is_some() {
            return Err(RedDBError::InvalidOperation(
                "finish active transactions before reset".into(),
            ));
        }
        // Soft and Mixed retain working data; Hard installs the target view.
        match input.mode {
            ResetMode::Soft | ResetMode::Mixed => {
                let mut publication = crate::storage::unified::store::RowPublication::default();
                branches::stage_head(
                    store,
                    &mut publication,
                    input.connection_id,
                    &branch,
                    &target_commit,
                    None,
                )?;
                store.publish_rows(publication)?;
                Ok(())
            }
            ResetMode::Hard => {
                branches::ensure_entity_history_supported(self)?;
                merge_data::validate_head_change(self, input.connection_id, &target_hash)?;
                branches::publish_head(
                    store,
                    input.connection_id,
                    &branch,
                    &target_commit,
                    branches::commit_view(store, &target_hash)?,
                )
            }
        }
    }

    pub fn vcs_log(&self, input: LogInput) -> RedDBResult<Vec<Commit>> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let start = match &input.range.to {
            Some(spec) => RedDBRuntime::vcs_resolve_commitish(self, spec)?,
            None => {
                let workset = load_workset(store, input.connection_id)?;
                workset
                    .and_then(|(_, base)| base)
                    .or(load_ref(store, vc::DEFAULT_BRANCH_REF)?.map(|r| r.target))
                    .unwrap_or_default()
            }
        };
        if start.is_empty() {
            return Ok(Vec::new());
        }
        Ok(topo_walk(store, &start, &input.range)?)
    }

    pub fn vcs_diff(&self, input: DiffInput) -> RedDBResult<Diff> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let from_hash = RedDBRuntime::vcs_resolve_commitish(self, &input.from)?;
        let to_hash = RedDBRuntime::vcs_resolve_commitish(self, &input.to)?;
        let from_snap = self.vcs_commit_read_context(&from_hash)?;
        let to_snap = self.vcs_commit_read_context(&to_hash)?;

        // Iterate every *user* collection (skip internal red_*).
        let mut entries: Vec<DiffEntry> = Vec::new();
        let mut added = 0usize;
        let mut removed = 0usize;
        let mut modified = 0usize;
        let collections = store.list_collections();
        for coll in collections {
            if coll.starts_with("red_") || coll.starts_with("red.") {
                continue;
            }
            if !is_versioned(store, &coll)? {
                continue;
            }
            if let Some(filter) = &input.collection {
                if filter != &coll {
                    continue;
                }
            }
            let Some(manager) = store.get_collection(&coll) else {
                continue;
            };
            self.check_collection_privilege(reddb_rql::ast::PolicyAction::Select, &coll)?;
            let mut entities = Vec::new();
            manager.try_for_each_entity(usize::MAX, |entity| {
                entities.push(entity.clone());
                true
            })?;
            // Compare physical versions under the stable logical identity.
            for entity in &entities {
                let in_from = snapshot_sees_entity(&from_snap, entity);
                let in_to = snapshot_sees_entity(&to_snap, entity);
                if in_from == in_to {
                    continue;
                }
                ensure_vcs_entity_access(
                    self,
                    &coll,
                    entity,
                    reddb_rql::ast::PolicyAction::Select,
                )?;
                let entity_id = entity.logical_id().raw().to_string();
                let payload = if input.summary_only {
                    JsonValue::Null
                } else {
                    vcs_entity_body(self, &coll, entity)
                };
                let change = match (in_from, in_to) {
                    (false, true) => {
                        added += 1;
                        DiffChange::Added { after: payload }
                    }
                    (true, false) => {
                        removed += 1;
                        DiffChange::Removed { before: payload }
                    }
                    _ => unreachable!(),
                };
                entries.push(DiffEntry {
                    collection: coll.clone(),
                    entity_id,
                    change,
                });
            }
        }

        // Modified rows in an append-only MVCC are expressed as a pair
        // (remove old version + add new version sharing entity_id);
        // collapse them into DiffChange::Modified so the wire format
        // matches user intuition.
        entries = coalesce_modifications(entries, &mut added, &mut removed, &mut modified);

        Ok(Diff {
            from: from_hash,
            to: to_hash,
            entries,
            added,
            removed,
            modified,
        })
    }

    pub fn vcs_status(&self, input: StatusInput) -> RedDBResult<Status> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let workset = load_workset_state(store, input.connection_id)?;
        let (head_ref, head_commit, merge_state_id) = match workset {
            Some((branch, base, merge_state_id)) => {
                let base = base.or(load_ref(store, &branch)?.map(|r| r.target));
                (Some(branch), base, merge_state_id)
            }
            None => {
                let base = load_ref(store, vc::DEFAULT_BRANCH_REF)?.map(|r| r.target);
                (Some(vc::DEFAULT_BRANCH_REF.to_string()), base, None)
            }
        };
        let detached = matches!(&head_ref, Some(s) if s.is_empty());
        let unresolved_conflicts = match &merge_state_id {
            Some(id) => self.vcs_conflicts_list(id)?.len(),
            None => 0,
        };
        Ok(Status {
            connection_id: input.connection_id,
            head_ref: head_ref.filter(|s| !s.is_empty()),
            head_commit,
            detached,
            staged_changes: 0,
            working_changes: 0,
            unresolved_conflicts,
            merge_state_id,
        })
    }

    pub fn vcs_lca(&self, a: &str, b: &str) -> RedDBResult<Option<CommitHash>> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let a_hash = RedDBRuntime::vcs_resolve_commitish(self, a)?;
        let b_hash = RedDBRuntime::vcs_resolve_commitish(self, b)?;
        let a_ancestors = ancestor_set(store, &a_hash, 100_000)?;

        // BFS from b, return first hit in a_ancestors with the greatest
        // height (closest common ancestor to b).
        let mut visited: HashSet<CommitHash> = HashSet::new();
        let mut stack: Vec<CommitHash> = vec![b_hash];
        let mut best: Option<(u64, CommitHash)> = None;
        while let Some(hash) = stack.pop() {
            if !visited.insert(hash.clone()) {
                continue;
            }
            if a_ancestors.contains(&hash) {
                let height = load_commit(store, &hash)?.map(|c| c.height).unwrap_or(0);
                match &best {
                    Some((h, _)) if *h >= height => {}
                    _ => best = Some((height, hash.clone())),
                }
                // Don't descend below an ancestor; higher-height hits
                // further from root are already captured.
                continue;
            }
            {
                let commit = load_commit(store, &hash)?.ok_or_else(|| {
                    RedDBError::Internal(format!("vcs: missing ancestor '{hash}'"))
                })?;
                for p in commit.parents {
                    if !visited.contains(&p) {
                        stack.push(p);
                    }
                }
            }
        }
        Ok(best.map(|(_, h)| h))
    }

    pub fn vcs_conflicts_list(&self, merge_state_id: &str) -> RedDBResult<Vec<Conflict>> {
        let store = self.db().store();
        let _publication = store.vault_publication_lock.lock();
        merge_data::read_conflicts(self, merge_state_id)
    }

    pub fn vcs_conflict_resolve(&self, conflict_id: &str, resolved: JsonValue) -> RedDBResult<()> {
        let store = self.db().store();
        let _publication = store.vault_publication_lock.lock();
        let row = merge_data::metadata_row(&store, vc::CONFLICTS, conflict_id)?
            .ok_or_else(|| RedDBError::NotFound(format!("conflict `{conflict_id}` not found")))?;
        let choice = match &resolved {
            JsonValue::String(choice)
                if matches!(choice.as_str(), "ours" | "theirs" | "base" | "delete") =>
            {
                choice
            }
            _ => {
                return Err(RedDBError::InvalidOperation(
                    "resolve with ours, theirs, base or delete; conflict retained".into(),
                ))
            }
        };
        let connection = crate::runtime::impl_core::current_connection_id();
        let active_state = load_workset_state(&store, connection)?.and_then(|(_, _, state)| state);
        if active_state != row_text(&row, "merge_state_id") {
            return Err(RedDBError::InvalidOperation(
                "conflict belongs to another pending merge".into(),
            ));
        }
        let mut fields: HashMap<_, _> = row
            .iter_fields()
            .map(|(name, value)| (name.into(), value.clone()))
            .collect();
        fields.insert("resolution".into(), Value::text(choice.as_str()));
        let mut publication = crate::storage::unified::store::RowPublication::default();
        branches::stage_metadata(
            &store,
            &mut publication,
            vc::CONFLICTS,
            "id",
            conflict_id,
            fields,
        )?;
        store.publish_rows(publication)?;
        Ok(())
    }

    pub fn vcs_resolve_as_of(&self, spec: AsOfSpec) -> RedDBResult<Xid> {
        Ok(self.vcs_resolve_snapshot(spec)?.xid)
    }

    pub(super) fn vcs_resolve_snapshot(&self, spec: AsOfSpec) -> RedDBResult<Snapshot> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        match spec {
            AsOfSpec::Snapshot(xid) => Ok(Snapshot {
                xid,
                in_progress: HashSet::new(),
            }),
            AsOfSpec::Commit(h) => {
                let entity = load_commit_entity(store, &h)?
                    .ok_or_else(|| RedDBError::NotFound(format!("commit `{h}` not found")))?;
                let row = entity.data.as_row().ok_or_else(|| {
                    RedDBError::Internal("vcs: invalid commit record".to_string())
                })?;
                commit_snapshot_from_row(row)
            }
            AsOfSpec::Branch(name) => {
                let full = normalize_branch_name(&name);
                let r = load_ref(store, &full)?.ok_or_else(|| {
                    RedDBError::NotFound(format!("branch `{full}` does not exist"))
                })?;
                let entity = load_commit_entity(store, &r.target)?.ok_or_else(|| {
                    RedDBError::NotFound(format!("branch `{full}` points to missing commit"))
                })?;
                let row = entity.data.as_row().ok_or_else(|| {
                    RedDBError::Internal("vcs: invalid commit record".to_string())
                })?;
                commit_snapshot_from_row(row)
            }
            AsOfSpec::Tag(name) => {
                let full = normalize_tag_name(&name);
                let r = load_ref(store, &full)?
                    .ok_or_else(|| RedDBError::NotFound(format!("tag `{full}` does not exist")))?;
                let entity = load_commit_entity(store, &r.target)?.ok_or_else(|| {
                    RedDBError::NotFound(format!("tag `{full}` points to missing commit"))
                })?;
                let row = entity.data.as_row().ok_or_else(|| {
                    RedDBError::Internal("vcs: invalid commit record".to_string())
                })?;
                commit_snapshot_from_row(row)
            }
            AsOfSpec::TimestampMs(ts) => {
                let manager = store
                    .get_collection(vc::COMMITS)
                    .ok_or_else(|| RedDBError::NotFound("no commits exist yet".to_string()))?;
                // Find the commit with the greatest timestamp_ms <= ts.
                let mut best: Option<(i64, UnifiedEntity)> = None;
                let mut entities = Vec::new();
                manager.try_for_each_entity(usize::MAX, |entity| {
                    entities.push(entity.clone());
                    true
                })?;
                for entity in entities {
                    if let Some(row) = entity.data.as_row() {
                        let t = row_i64(row, "timestamp_ms").unwrap_or(0);
                        if t <= ts {
                            match &best {
                                Some((bt, _)) if *bt >= t => {}
                                _ => best = Some((t, entity)),
                            }
                        }
                    }
                }
                let (_, entity) = best.ok_or_else(|| {
                    RedDBError::NotFound(format!("no commit at or before ts={ts}"))
                })?;
                let row = entity.data.as_row().ok_or_else(|| {
                    RedDBError::Internal("vcs: invalid commit record".to_string())
                })?;
                commit_snapshot_from_row(row)
            }
        }
    }

    pub fn vcs_set_versioned(&self, collection: &str, enabled: bool) -> RedDBResult<()> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let _publication = store.vault_publication_lock.lock();
        if collection.starts_with("red_") || collection.starts_with("red.") {
            return Err(RedDBError::InvalidConfig(format!(
                "cannot version internal collection `{collection}`"
            )));
        }
        if is_versioned(store, collection)? == enabled {
            return Ok(());
        }
        if self.snapshot_manager().oldest_active_xid().is_some() {
            return Err(RedDBError::InvalidOperation(
                "finish active transactions before changing version history".into(),
            ));
        }
        if !enabled
            && self
                .db()
                .collection_contract(collection)
                .is_some_and(|contract| {
                    matches!(
                        contract.declared_model,
                        crate::catalog::CollectionModel::Hll
                            | crate::catalog::CollectionModel::Sketch
                            | crate::catalog::CollectionModel::Filter
                    )
                })
        {
            return Err(RedDBError::InvalidOperation(
                "export probabilistic state before discarding its version history".into(),
            ));
        }
        if !enabled && list_refs_by_prefix(store, Some(vc::BRANCH_REF_PREFIX))?.len() > 1 {
            return Err(RedDBError::InvalidOperation(
                "cannot disable history while branches share versions".into(),
            ));
        }
        if enabled && store.format_version() < reddb_file::native_store::STORE_VERSION_V14 {
            let previous_format = store.format_version();
            store.set_format_version(reddb_file::native_store::STORE_VERSION_V14);
            if store.pager().is_some() {
                if let Err(error) = store.persist() {
                    store.set_format_version(previous_format);
                    return Err(RedDBError::Internal(error.to_string()));
                }
            }
        }
        let mut publication = crate::storage::unified::store::RowPublication::default();
        if enabled {
            if let Some(mut entity) = self.vcs_initial_probabilistic_state(collection)? {
                entity.id = store.next_entity_id();
                publication.inserts.push((collection.into(), entity, None));
            }
        }
        if self
            .db()
            .collection_contract(collection)
            .is_some_and(|contract| {
                contract.declared_model == crate::catalog::CollectionModel::Queue
            })
        {
            super::vcs_queue::stage_metadata_scope(store, &mut publication, collection, enabled)?;
        }
        if let Some(manager) = store.get_collection(collection) {
            if !enabled {
                let mut shared = false;
                manager.try_for_each_entity(usize::MAX, |entity| {
                    shared = entity.branch_version().is_some_and(|version| {
                        version.origin != 0 || !version.deletions.is_empty()
                    });
                    !shared
                })?;
                if shared {
                    return Err(RedDBError::InvalidOperation("cannot discard branch provenance; export the selected branch to an unversioned collection".into()));
                }
            }
            let mut ids = Vec::new();
            manager.try_for_each_entity(usize::MAX, |entity| {
                if entity.branch_version().is_some() != enabled {
                    ids.push(entity.id);
                }
                true
            })?;
            for id in ids {
                let mut entity = manager
                    .try_get(id, usize::MAX)?
                    .ok_or_else(|| RedDBError::NotFound(id.to_string()))?;
                if enabled {
                    entity.set_branch_origin(0);
                } else {
                    entity.clear_branch_version();
                }
                let metadata = store.try_get_metadata(collection, id, usize::MAX)?;
                publication
                    .replacements
                    .push((collection.into(), entity, metadata));
            }
        }
        branches::stage_metadata(
            store,
            &mut publication,
            vc::SETTINGS,
            "id",
            collection,
            HashMap::from([
                ("id".into(), Value::text(collection)),
                ("versioned".into(), Value::Boolean(enabled)),
                ("ts_ms".into(), Value::TimestampMs(now_ms())),
            ]),
        )?;
        store.publish_rows(publication)?;
        self.invalidate_result_cache();
        Ok(())
    }

    pub fn vcs_list_versioned(&self) -> RedDBResult<Vec<String>> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let mut list = versioned_collections(store)?;
        list.sort();
        Ok(list)
    }

    pub fn vcs_is_versioned(&self, collection: &str) -> RedDBResult<bool> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        is_versioned(store, collection)
    }

    pub fn vcs_resolve_commitish(&self, spec: &str) -> RedDBResult<CommitHash> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        if spec.is_empty() {
            return Err(RedDBError::InvalidConfig("empty commitish".to_string()));
        }

        // 1. Exact commit hash.
        if spec.len() == 64
            && spec.chars().all(|c| c.is_ascii_hexdigit())
            && load_commit(store, spec)?.is_some()
        {
            return Ok(spec.to_string());
        }

        // 2. Full ref name (refs/heads/..., refs/tags/...).
        if spec.starts_with("refs/") {
            if let Some(hash) = resolve_ref_chain(store, spec)? {
                return Ok(hash);
            }
        }

        // 3. Short branch / tag name.
        let normalized_branch = normalize_branch_name(spec);
        if let Some(hash) = resolve_ref_chain(store, &normalized_branch)? {
            return Ok(hash);
        }
        let normalized_tag = normalize_tag_name(spec);
        if let Some(hash) = resolve_ref_chain(store, &normalized_tag)? {
            return Ok(hash);
        }

        // 4. Short commit hash prefix (≥ 4 chars, unique match).
        if let Some(hash) = resolve_short_commit(store, spec)? {
            return Ok(hash);
        }

        Err(RedDBError::NotFound(format!(
            "commitish `{spec}` did not resolve to any ref or commit"
        )))
    }
}

/// Stage a paused merge's conflicts in the same publication as its workset.
fn stage_merge_conflicts(
    store: &UnifiedStore,
    publication: &mut crate::storage::unified::store::RowPublication,
    conflicts: &[Conflict],
) -> RedDBResult<()> {
    for conflict in conflicts {
        let fields = HashMap::from([
            ("id".into(), Value::text(conflict.id.as_str())),
            (
                "collection".into(),
                Value::text(conflict.collection.as_str()),
            ),
            ("entity_id".into(), Value::text(conflict.entity_id.as_str())),
            (
                "merge_state_id".into(),
                Value::text(conflict.merge_state_id.as_str()),
            ),
            (
                "conflicting_paths".into(),
                Value::Array(
                    conflict
                        .conflicting_paths
                        .iter()
                        .map(|path| Value::text(path.as_str()))
                        .collect(),
                ),
            ),
            (
                "base_json".into(),
                Value::Json(
                    crate::json::to_vec(&conflict.base)
                        .map_err(|e| RedDBError::Internal(e.to_string()))?,
                ),
            ),
            (
                "ours_json".into(),
                Value::Json(
                    crate::json::to_vec(&conflict.ours)
                        .map_err(|e| RedDBError::Internal(e.to_string()))?,
                ),
            ),
            (
                "theirs_json".into(),
                Value::Json(
                    crate::json::to_vec(&conflict.theirs)
                        .map_err(|e| RedDBError::Internal(e.to_string()))?,
                ),
            ),
        ]);
        branches::stage_metadata(
            store,
            publication,
            vc::CONFLICTS,
            "id",
            &conflict.id,
            fields,
        )?;
    }
    Ok(())
}

fn ensure_vcs_entity_access(
    runtime: &RedDBRuntime,
    collection: &str,
    entity: &UnifiedEntity,
    action: reddb_rql::ast::PolicyAction,
) -> RedDBResult<()> {
    use reddb_rql::ast::PolicyAction;
    runtime.check_collection_privilege(action, collection)?;
    if !crate::runtime::rls_injection::entity_passes_rls_action(runtime, collection, action, entity)
    {
        return Err(RedDBError::Query(format!(
            "VCS access to collection '{collection}' denied by row-level security"
        )));
    }
    if let Some(row) = entity.data.as_row() {
        if let Some(key) = row_text(row, "key") {
            match runtime
                .db()
                .collection_contract(collection)
                .map(|c| c.declared_model)
            {
                Some(crate::catalog::CollectionModel::Vault) => {
                    let capability = if action == PolicyAction::Select {
                        "vault:read_metadata"
                    } else {
                        "vault:write"
                    };
                    runtime
                        .check_vault_capability_in_tenant(
                            capability,
                            collection,
                            &key,
                            row_text(row, "_vault_tenant").as_deref(),
                        )
                        .map_err(RedDBError::Query)?;
                }
                Some(crate::catalog::CollectionModel::Config) => {
                    let capability = if action == PolicyAction::Select {
                        "config:read"
                    } else {
                        "config:write"
                    };
                    runtime
                        .check_config_capability(capability, collection, &key)
                        .map_err(RedDBError::Query)?;
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn vcs_entity_body(runtime: &RedDBRuntime, collection: &str, entity: &UnifiedEntity) -> JsonValue {
    if runtime
        .db()
        .collection_contract(collection)
        .is_some_and(|contract| contract.declared_model == crate::catalog::CollectionModel::Vault)
    {
        // VCS exposes secret metadata, never the encrypted envelope. Native
        // merge images retain the original sealed bytes without rendering them.
        let mut fields = Map::new();
        fields.insert(
            "id".into(),
            JsonValue::String(entity.logical_id().raw().to_string()),
        );
        if let Some(row) = entity.data.as_row() {
            for name in ["key", "version", "tombstone"] {
                if let Some(value) = row.get_field(name) {
                    fields.insert(name.into(), value.to_json());
                }
            }
        }
        return JsonValue::Object(fields);
    }
    let mut body = crate::entity_render::compact_entity_json(entity);
    if let JsonValue::Object(fields) = &mut body {
        fields.insert(
            "id".to_string(),
            JsonValue::Number(entity.logical_id().raw() as f64),
        );
        match &entity.kind {
            EntityKind::GraphNode(node) => {
                fields.insert("label".to_string(), JsonValue::String(node.label.clone()));
                fields.insert(
                    "node_type".to_string(),
                    JsonValue::String(node.node_type.clone()),
                );
            }
            EntityKind::GraphEdge(edge) => {
                fields.insert("label".to_string(), JsonValue::String(edge.label.clone()));
                fields.insert(
                    "from_node".to_string(),
                    JsonValue::String(edge.from_node.clone()),
                );
                fields.insert(
                    "to_node".to_string(),
                    JsonValue::String(edge.to_node.clone()),
                );
            }
            _ => {}
        }
        if let EntityData::Row(row) = &entity.data {
            if row.named.is_none() {
                fields.insert(
                    "row".to_string(),
                    JsonValue::Object(
                        row.iter_fields()
                            .map(|(key, value)| (key.to_string(), value.to_json()))
                            .collect(),
                    ),
                );
            }
        }
    }
    body
}

// Collapse Add+Remove pairs sharing the same (collection, entity_id)
// into a single `Modified` entry. Called by `vcs_diff` after the
// naive pass so the caller sees one row per change, not two.
fn coalesce_modifications(
    entries: Vec<DiffEntry>,
    added: &mut usize,
    removed: &mut usize,
    modified: &mut usize,
) -> Vec<DiffEntry> {
    let mut by_key: HashMap<(String, String), Vec<DiffEntry>> = HashMap::new();
    for e in entries {
        by_key
            .entry((e.collection.clone(), e.entity_id.clone()))
            .or_default()
            .push(e);
    }
    let mut out: Vec<DiffEntry> = Vec::new();
    for ((coll, eid), group) in by_key {
        if group.len() >= 2 {
            let mut before = JsonValue::Null;
            let mut after = JsonValue::Null;
            for item in group {
                match item.change {
                    DiffChange::Removed { before: b } => before = b,
                    DiffChange::Added { after: a } => after = a,
                    DiffChange::Modified { .. } => {}
                }
            }
            *added = added.saturating_sub(1);
            *removed = removed.saturating_sub(1);
            *modified += 1;
            out.push(DiffEntry {
                collection: coll,
                entity_id: eid,
                change: DiffChange::Modified { before, after },
            });
        } else {
            out.extend(group);
        }
    }
    out.sort_by(|a, b| {
        a.collection
            .cmp(&b.collection)
            .then(a.entity_id.cmp(&b.entity_id))
    });
    out
}

// Unused-import guards for stubs that reference types via `_`.
#[allow(dead_code)]
fn _touch_types(_: BTreeSet<()>) {}
