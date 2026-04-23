//! Runtime implementation of the VCS ("Git for Data") surface.
//!
//! Phase 2: real persistence for commit / branch / tag / refs / log /
//! status / checkout / lca / resolve_commitish / resolve_as_of.
//! Merge / cherry-pick / revert / reset / diff / conflict handling
//! remain stubbed and land in Phase 3.
//!
//! Every VCS entity is stored as a plain TableRow in a `red_*`
//! collection (same pattern as `red_queue_meta` / `red_stats`).
//! Commits reuse the MVCC snapshot xid as their immutable root, and
//! pin that xid so VACUUM cannot reclaim historical row versions.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::application::vcs::{
    AsOfSpec, Author, CheckoutInput, CheckoutTarget, Commit, CommitHash, Conflict,
    CreateBranchInput, CreateCommitInput, CreateTagInput, Diff, DiffInput, LogInput, LogRange,
    MergeInput, MergeOutcome, Ref, RefKind, RefName, ResetInput, Status, StatusInput,
};
use crate::application::vcs_collections as vc;
use crate::json::Value as JsonValue;
use crate::runtime::RedDBRuntime;
use crate::storage::schema::Value;
use crate::storage::transaction::snapshot::{Xid, XID_NONE};
use crate::storage::unified::entity::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};
use crate::storage::unified::UnifiedStore;
use crate::{RedDBError, RedDBResult};

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

fn unimplemented(method: &str) -> RedDBError {
    RedDBError::Internal(format!("vcs: {method} not yet implemented"))
}

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

fn insert_meta_row(
    store: &UnifiedStore,
    collection: &str,
    fields: HashMap<String, Value>,
) -> RedDBResult<EntityId> {
    let _ = store.get_or_create_collection(collection);
    store
        .insert_auto(
            collection,
            UnifiedEntity::new(
                EntityId::new(0),
                EntityKind::TableRow {
                    table: Arc::from(collection),
                    row_id: 0,
                },
                EntityData::Row(RowData {
                    columns: Vec::new(),
                    named: Some(fields),
                    schema: None,
                }),
            ),
        )
        .map_err(|e| RedDBError::Internal(e.to_string()))
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

fn load_commit_entity(store: &UnifiedStore, hash: &str) -> Option<UnifiedEntity> {
    let manager = store.get_collection(vc::COMMITS)?;
    manager
        .query_all(|entity| {
            entity
                .data
                .as_row()
                .is_some_and(|row| row_text(row, "_id").as_deref() == Some(hash))
        })
        .into_iter()
        .next()
}

fn commit_from_row(row: &RowData) -> Option<Commit> {
    Some(Commit {
        hash: row_text(row, "_id")?,
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

fn load_commit(store: &UnifiedStore, hash: &str) -> Option<Commit> {
    let entity = load_commit_entity(store, hash)?;
    let row = entity.data.as_row()?;
    commit_from_row(row)
}

fn save_commit(store: &UnifiedStore, commit: &Commit) -> RedDBResult<()> {
    let mut fields: HashMap<String, Value> = HashMap::new();
    fields.insert("_id".to_string(), Value::text(commit.hash.as_str()));
    fields.insert(
        "root_xid".to_string(),
        Value::UnsignedInteger(commit.root_xid),
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
    fields.insert(
        "height".to_string(),
        Value::UnsignedInteger(commit.height),
    );
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
    fields.insert(
        "message".to_string(),
        Value::text(commit.message.as_str()),
    );
    fields.insert(
        "timestamp_ms".to_string(),
        Value::TimestampMs(commit.timestamp_ms),
    );
    if let Some(sig) = &commit.signature {
        fields.insert("signature".to_string(), Value::text(sig.as_str()));
    }
    insert_meta_row(store, vc::COMMITS, fields)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Ref load / save / delete
// ---------------------------------------------------------------------------

fn ref_kind_from_str(s: &str) -> RefKind {
    match s {
        "tag" => RefKind::Tag,
        "head" => RefKind::Head,
        _ => RefKind::Branch,
    }
}

fn ref_from_row(row: &RowData) -> Option<Ref> {
    Some(Ref {
        name: row_text(row, "_id")?,
        kind: ref_kind_from_str(&row_text(row, "type").unwrap_or_default()),
        target: row_text(row, "target").unwrap_or_default(),
        protected: row.get_field("protected")
            .and_then(|v| match v {
                Value::Boolean(b) => Some(*b),
                _ => None,
            })
            .unwrap_or(false),
    })
}

fn load_ref_entity(store: &UnifiedStore, name: &str) -> Option<(EntityId, UnifiedEntity)> {
    let manager = store.get_collection(vc::REFS)?;
    manager
        .query_all(|entity| {
            entity
                .data
                .as_row()
                .is_some_and(|row| row_text(row, "_id").as_deref() == Some(name))
        })
        .into_iter()
        .next()
        .map(|entity| (entity.id, entity))
}

fn load_ref(store: &UnifiedStore, name: &str) -> Option<Ref> {
    let (_, entity) = load_ref_entity(store, name)?;
    ref_from_row(entity.data.as_row()?)
}

fn save_ref(store: &UnifiedStore, r: &Ref) -> RedDBResult<()> {
    // Delete-then-insert gives us upsert semantics over the TableRow
    // primary-key `_id` used by every red_* collection.
    if let Some((id, _)) = load_ref_entity(store, &r.name) {
        let _ = store.delete(vc::REFS, id);
    }
    let mut fields: HashMap<String, Value> = HashMap::new();
    fields.insert("_id".to_string(), Value::text(r.name.as_str()));
    let kind_str = match r.kind {
        RefKind::Branch => "branch",
        RefKind::Tag => "tag",
        RefKind::Head => "head",
    };
    fields.insert("type".to_string(), Value::text(kind_str));
    fields.insert("target".to_string(), Value::text(r.target.as_str()));
    fields.insert("protected".to_string(), Value::Boolean(r.protected));
    insert_meta_row(store, vc::REFS, fields)?;
    Ok(())
}

fn delete_ref(store: &UnifiedStore, name: &str) -> RedDBResult<bool> {
    let Some((id, _)) = load_ref_entity(store, name) else {
        return Ok(false);
    };
    store
        .delete(vc::REFS, id)
        .map_err(|e| RedDBError::Internal(e.to_string()))?;
    Ok(true)
}

fn list_refs_by_prefix(store: &UnifiedStore, prefix: Option<&str>) -> Vec<Ref> {
    let Some(manager) = store.get_collection(vc::REFS) else {
        return Vec::new();
    };
    let prefix_owned = prefix.map(|s| s.to_string());
    manager
        .query_all(|entity| {
            entity.data.as_row().is_some_and(|row| {
                let id = row_text(row, "_id").unwrap_or_default();
                match &prefix_owned {
                    Some(p) => id.starts_with(p),
                    None => true,
                }
            })
        })
        .into_iter()
        .filter_map(|entity| ref_from_row(entity.data.as_row()?))
        .collect()
}

// ---------------------------------------------------------------------------
// Ancestry helpers
// ---------------------------------------------------------------------------

fn ancestor_set(store: &UnifiedStore, start: &str, max_steps: usize) -> HashSet<CommitHash> {
    let mut visited: HashSet<CommitHash> = HashSet::new();
    let mut stack: Vec<CommitHash> = vec![start.to_string()];
    let mut steps = 0usize;
    while let Some(hash) = stack.pop() {
        if !visited.insert(hash.clone()) {
            continue;
        }
        if let Some(c) = load_commit(store, &hash) {
            for p in c.parents {
                if !visited.contains(&p) {
                    stack.push(p);
                }
            }
        }
        steps += 1;
        if steps >= max_steps {
            break;
        }
    }
    visited
}

fn topo_walk(store: &UnifiedStore, start: &str, range: &LogRange) -> Vec<Commit> {
    let limit = range.limit.unwrap_or(usize::MAX);
    let skip = range.skip.unwrap_or(0);
    let exclude = range
        .from
        .as_ref()
        .map(|h| ancestor_set(store, h, 100_000))
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
        let Some(commit) = load_commit(store, &hash) else {
            continue;
        };
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
    out
}

// ---------------------------------------------------------------------------
// Commitish resolution
// ---------------------------------------------------------------------------

fn resolve_ref_chain(store: &UnifiedStore, name: &str) -> Option<CommitHash> {
    // Follow up to 4 levels of ref indirection.
    let mut current = name.to_string();
    for _ in 0..4 {
        let r = load_ref(store, &current)?;
        match r.kind {
            RefKind::Branch | RefKind::Tag => return Some(r.target),
            RefKind::Head => {
                current = r.target;
                continue;
            }
        }
    }
    None
}

fn resolve_short_commit(store: &UnifiedStore, prefix: &str) -> Option<CommitHash> {
    if prefix.len() < 4 {
        return None;
    }
    let manager = store.get_collection(vc::COMMITS)?;
    let matches: Vec<String> = manager
        .query_all(|entity| {
            entity.data.as_row().is_some_and(|row| {
                row_text(row, "_id")
                    .map(|id| id.starts_with(prefix))
                    .unwrap_or(false)
            })
        })
        .into_iter()
        .filter_map(|entity| row_text(entity.data.as_row()?, "_id"))
        .collect();
    if matches.len() == 1 {
        matches.into_iter().next()
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Workset helpers (lightweight — full WIP tracking lands in Phase 3)
// ---------------------------------------------------------------------------

fn upsert_workset(
    store: &UnifiedStore,
    connection_id: u64,
    branch: &str,
    base_commit: Option<&str>,
    working_xid: Xid,
) -> RedDBResult<()> {
    let conn_id = connection_id.to_string();
    // Delete old workset for this connection
    if let Some(manager) = store.get_collection(vc::WORKSETS) {
        let rows = manager.query_all(|entity| {
            entity
                .data
                .as_row()
                .is_some_and(|row| row_text(row, "_id").as_deref() == Some(&conn_id))
        });
        for row in rows {
            let _ = store.delete(vc::WORKSETS, row.id);
        }
    }
    let mut fields: HashMap<String, Value> = HashMap::new();
    fields.insert("_id".to_string(), Value::text(conn_id.as_str()));
    fields.insert("branch".to_string(), Value::text(branch));
    if let Some(base) = base_commit {
        fields.insert("base_commit".to_string(), Value::text(base));
    }
    fields.insert(
        "working_xid".to_string(),
        Value::UnsignedInteger(working_xid),
    );
    insert_meta_row(store, vc::WORKSETS, fields)?;
    Ok(())
}

fn load_workset(store: &UnifiedStore, connection_id: u64) -> Option<(RefName, Option<CommitHash>)> {
    let manager = store.get_collection(vc::WORKSETS)?;
    let conn_id = connection_id.to_string();
    manager
        .query_all(|entity| {
            entity
                .data
                .as_row()
                .is_some_and(|row| row_text(row, "_id").as_deref() == Some(&conn_id))
        })
        .into_iter()
        .find_map(|entity| {
            let row = entity.data.as_row()?;
            Some((
                row_text(row, "branch").unwrap_or_else(|| vc::DEFAULT_BRANCH_REF.to_string()),
                row_text(row, "base_commit"),
            ))
        })
}

// ---------------------------------------------------------------------------
// Runtime impl
// ---------------------------------------------------------------------------

impl RedDBRuntime {
    pub fn vcs_commit(&self, input: CreateCommitInput) -> RedDBResult<Commit> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;

        // Resolve current HEAD for this connection: workset -> HEAD:<conn>
        // -> default branch.
        let workset = load_workset(store, input.connection_id);
        let branch_ref = workset
            .as_ref()
            .map(|(b, _)| b.clone())
            .or_else(|| resolve_ref_chain(store, &head_ref_id(input.connection_id)).and(None))
            .unwrap_or_else(|| vc::DEFAULT_BRANCH_REF.to_string());

        let parent_hash = workset
            .as_ref()
            .and_then(|(_, base)| base.clone())
            .or_else(|| load_ref(store, &branch_ref).map(|r| r.target));

        let parents: Vec<CommitHash> = match parent_hash.clone() {
            Some(h) if !h.is_empty() => vec![h],
            _ => Vec::new(),
        };

        let parent_height = parents
            .iter()
            .filter_map(|p| load_commit(store, p).map(|c| c.height))
            .max()
            .unwrap_or(0);
        let height = if parents.is_empty() {
            0
        } else {
            parent_height + 1
        };

        let root_xid = self.inner.snapshot_manager.peek_next_xid();
        let timestamp_ms = now_ms();
        let committer = input.committer.unwrap_or_else(|| input.author.clone());

        let hash = compute_commit_hash(
            root_xid,
            &parents,
            &input.author,
            &input.message,
            timestamp_ms,
        );

        if load_commit(store, &hash).is_some() {
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

        save_commit(store, &commit)?;

        // Pin the root xid so VACUUM cannot reclaim history while the
        // commit is reachable.
        if root_xid != XID_NONE {
            self.inner.snapshot_manager.pin(root_xid);
        }

        // Advance (or create) the branch ref.
        let branch_name = if branch_ref.is_empty() {
            vc::DEFAULT_BRANCH_REF.to_string()
        } else {
            branch_ref
        };
        save_ref(
            store,
            &Ref {
                name: branch_name.clone(),
                kind: RefKind::Branch,
                target: hash.clone(),
                protected: false,
            },
        )?;

        upsert_workset(
            store,
            input.connection_id,
            &branch_name,
            Some(&hash),
            root_xid,
        )?;

        Ok(commit)
    }

    pub fn vcs_branch_create(&self, input: CreateBranchInput) -> RedDBResult<Ref> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let full_name = normalize_branch_name(&input.name);
        if load_ref(store, &full_name).is_some() {
            return Err(RedDBError::InvalidConfig(format!(
                "branch `{full_name}` already exists"
            )));
        }
        let target_hash = match &input.from {
            Some(spec) => RedDBRuntime::vcs_resolve_commitish(self, spec)?,
            None => {
                // Fall back to the connection's current HEAD branch, then default.
                let workset = load_workset(store, input.connection_id);
                let base = workset
                    .and_then(|(_, base)| base)
                    .or_else(|| {
                        load_ref(store, vc::DEFAULT_BRANCH_REF).map(|r| r.target)
                    })
                    .unwrap_or_default();
                base
            }
        };
        let r = Ref {
            name: full_name,
            kind: RefKind::Branch,
            target: target_hash,
            protected: false,
        };
        save_ref(store, &r)?;
        Ok(r)
    }

    pub fn vcs_branch_delete(&self, name: &str) -> RedDBResult<()> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let full = normalize_branch_name(name);
        let existing = load_ref(store, &full).ok_or_else(|| {
            RedDBError::NotFound(format!("branch `{full}` does not exist"))
        })?;
        if existing.protected {
            return Err(RedDBError::ReadOnly(format!(
                "branch `{full}` is protected"
            )));
        }
        delete_ref(store, &full)?;
        Ok(())
    }

    pub fn vcs_tag_create(&self, input: CreateTagInput) -> RedDBResult<Ref> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let full_name = normalize_tag_name(&input.name);
        if load_ref(store, &full_name).is_some() {
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

    pub fn vcs_list_refs(&self, prefix: Option<&str>) -> RedDBResult<Vec<Ref>> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        Ok(list_refs_by_prefix(store, prefix))
    }

    pub fn vcs_checkout(&self, input: CheckoutInput) -> RedDBResult<Ref> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let (branch_ref, target_hash) = match &input.target {
            CheckoutTarget::Branch(name) => {
                let full = normalize_branch_name(name);
                let r = load_ref(store, &full).ok_or_else(|| {
                    RedDBError::NotFound(format!("branch `{full}` does not exist"))
                })?;
                (full, r.target)
            }
            CheckoutTarget::Tag(name) => {
                let full = normalize_tag_name(name);
                let r = load_ref(store, &full).ok_or_else(|| {
                    RedDBError::NotFound(format!("tag `{full}` does not exist"))
                })?;
                (full, r.target)
            }
            CheckoutTarget::Commit(hash) => {
                let resolved = RedDBRuntime::vcs_resolve_commitish(self, hash)?;
                // Detached HEAD — we point HEAD directly at a commit via
                // the workset base; no branch ref is updated.
                (String::new(), resolved)
            }
        };

        upsert_workset(
            store,
            input.connection_id,
            &branch_ref,
            Some(&target_hash),
            self.inner.snapshot_manager.peek_next_xid(),
        )?;

        // Update per-connection HEAD pointer for status/introspection.
        save_ref(
            store,
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
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let start = match &input.range.to {
            Some(spec) => RedDBRuntime::vcs_resolve_commitish(self, spec)?,
            None => {
                let workset = load_workset(store, input.connection_id);
                workset
                    .and_then(|(_, base)| base)
                    .or_else(|| {
                        load_ref(store, vc::DEFAULT_BRANCH_REF).map(|r| r.target)
                    })
                    .unwrap_or_default()
            }
        };
        if start.is_empty() {
            return Ok(Vec::new());
        }
        Ok(topo_walk(store, &start, &input.range))
    }

    pub fn vcs_diff(&self, input: DiffInput) -> RedDBResult<Diff> {
        let _ = input;
        Err(unimplemented("diff"))
    }

    pub fn vcs_status(&self, input: StatusInput) -> RedDBResult<Status> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let workset = load_workset(store, input.connection_id);
        let (head_ref, head_commit) = match workset {
            Some((branch, base)) => {
                let base = base.or_else(|| load_ref(store, &branch).map(|r| r.target));
                (Some(branch), base)
            }
            None => {
                let base = load_ref(store, vc::DEFAULT_BRANCH_REF).map(|r| r.target);
                (Some(vc::DEFAULT_BRANCH_REF.to_string()), base)
            }
        };
        let detached = matches!(&head_ref, Some(s) if s.is_empty());
        Ok(Status {
            connection_id: input.connection_id,
            head_ref: head_ref.filter(|s| !s.is_empty()),
            head_commit,
            detached,
            staged_changes: 0,
            working_changes: 0,
            unresolved_conflicts: 0,
            merge_state_id: None,
        })
    }

    pub fn vcs_lca(&self, a: &str, b: &str) -> RedDBResult<Option<CommitHash>> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let a_hash = RedDBRuntime::vcs_resolve_commitish(self, a)?;
        let b_hash = RedDBRuntime::vcs_resolve_commitish(self, b)?;
        let a_ancestors = ancestor_set(store, &a_hash, 100_000);

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
                let height = load_commit(store, &hash).map(|c| c.height).unwrap_or(0);
                match &best {
                    Some((h, _)) if *h >= height => {}
                    _ => best = Some((height, hash.clone())),
                }
                // Don't descend below an ancestor; higher-height hits
                // further from root are already captured.
                continue;
            }
            if let Some(commit) = load_commit(store, &hash) {
                for p in commit.parents {
                    if !visited.contains(&p) {
                        stack.push(p);
                    }
                }
            }
        }
        Ok(best.map(|(_, h)| h))
    }

    pub fn vcs_conflicts_list(
        &self,
        merge_state_id: &str,
    ) -> RedDBResult<Vec<Conflict>> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let Some(manager) = store.get_collection(vc::CONFLICTS) else {
            return Ok(Vec::new());
        };
        let msid = merge_state_id.to_string();
        let out = manager
            .query_all(|entity| {
                entity
                    .data
                    .as_row()
                    .is_some_and(|row| row_text(row, "merge_state_id").as_deref() == Some(&msid))
            })
            .into_iter()
            .filter_map(|entity| {
                let row = entity.data.as_row()?;
                Some(Conflict {
                    id: row_text(row, "_id")?,
                    collection: row_text(row, "collection")?,
                    entity_id: row_text(row, "entity_id").unwrap_or_default(),
                    base: JsonValue::Null,
                    ours: JsonValue::Null,
                    theirs: JsonValue::Null,
                    conflicting_paths: row_string_list(row, "conflicting_paths"),
                    merge_state_id: row_text(row, "merge_state_id").unwrap_or_default(),
                })
            })
            .collect();
        Ok(out)
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
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        match spec {
            AsOfSpec::Snapshot(x) => Ok(x),
            AsOfSpec::Commit(h) => {
                let c = load_commit(store, &h).ok_or_else(|| {
                    RedDBError::NotFound(format!("commit `{h}` not found"))
                })?;
                Ok(c.root_xid)
            }
            AsOfSpec::Branch(name) => {
                let full = normalize_branch_name(&name);
                let r = load_ref(store, &full).ok_or_else(|| {
                    RedDBError::NotFound(format!("branch `{full}` does not exist"))
                })?;
                let c = load_commit(store, &r.target).ok_or_else(|| {
                    RedDBError::NotFound(format!("branch `{full}` points to missing commit"))
                })?;
                Ok(c.root_xid)
            }
            AsOfSpec::Tag(name) => {
                let full = normalize_tag_name(&name);
                let r = load_ref(store, &full).ok_or_else(|| {
                    RedDBError::NotFound(format!("tag `{full}` does not exist"))
                })?;
                let c = load_commit(store, &r.target).ok_or_else(|| {
                    RedDBError::NotFound(format!("tag `{full}` points to missing commit"))
                })?;
                Ok(c.root_xid)
            }
            AsOfSpec::TimestampMs(ts) => {
                let manager = store.get_collection(vc::COMMITS).ok_or_else(|| {
                    RedDBError::NotFound("no commits exist yet".to_string())
                })?;
                // Find the commit with the greatest timestamp_ms <= ts.
                let mut best: Option<(i64, Xid)> = None;
                let entities = manager.query_all(|_| true);
                for entity in entities {
                    if let Some(row) = entity.data.as_row() {
                        let t = row_i64(row, "timestamp_ms").unwrap_or(0);
                        if t <= ts {
                            let xid = row_u64(row, "root_xid").unwrap_or(0);
                            match &best {
                                Some((bt, _)) if *bt >= t => {}
                                _ => best = Some((t, xid)),
                            }
                        }
                    }
                }
                best.map(|(_, x)| x).ok_or_else(|| {
                    RedDBError::NotFound(format!("no commit at or before ts={ts}"))
                })
            }
        }
    }

    pub fn vcs_resolve_commitish(&self, spec: &str) -> RedDBResult<CommitHash> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        if spec.is_empty() {
            return Err(RedDBError::InvalidConfig(
                "empty commitish".to_string(),
            ));
        }

        // 1. Exact commit hash.
        if spec.len() == 64 && spec.chars().all(|c| c.is_ascii_hexdigit()) {
            if load_commit(store, spec).is_some() {
                return Ok(spec.to_string());
            }
        }

        // 2. Full ref name (refs/heads/..., refs/tags/...).
        if spec.starts_with("refs/") {
            if let Some(hash) = resolve_ref_chain(store, spec) {
                return Ok(hash);
            }
        }

        // 3. Short branch / tag name.
        let normalized_branch = normalize_branch_name(spec);
        if let Some(hash) = resolve_ref_chain(store, &normalized_branch) {
            return Ok(hash);
        }
        let normalized_tag = normalize_tag_name(spec);
        if let Some(hash) = resolve_ref_chain(store, &normalized_tag) {
            return Ok(hash);
        }

        // 4. Short commit hash prefix (≥ 4 chars, unique match).
        if let Some(hash) = resolve_short_commit(store, spec) {
            return Ok(hash);
        }

        Err(RedDBError::NotFound(format!(
            "commitish `{spec}` did not resolve to any ref or commit"
        )))
    }
}

// Unused-import guards for stubs that reference types via `_`.
#[allow(dead_code)]
fn _touch_types(_: BTreeSet<()>) {}
