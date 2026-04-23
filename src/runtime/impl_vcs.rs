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
    CreateBranchInput, CreateCommitInput, CreateTagInput, Diff, DiffChange, DiffEntry, DiffInput,
    LogInput, LogRange, MergeInput, MergeOutcome, MergeStrategy, Ref, RefKind, RefName, ResetInput,
    ResetMode, Status, StatusInput,
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

fn row_json(row: &RowData, field: &str) -> JsonValue {
    match row.get_field(field) {
        Some(Value::Json(bytes)) => crate::json::from_slice::<JsonValue>(bytes)
            .unwrap_or(JsonValue::Null),
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
                .is_some_and(|row| row_text(row, "id").as_deref() == Some(hash))
        })
        .into_iter()
        .next()
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

fn load_commit(store: &UnifiedStore, hash: &str) -> Option<Commit> {
    let entity = load_commit_entity(store, hash)?;
    let row = entity.data.as_row()?;
    commit_from_row(row)
}

fn save_commit(store: &UnifiedStore, commit: &Commit) -> RedDBResult<()> {
    let mut fields: HashMap<String, Value> = HashMap::new();
    fields.insert("id".to_string(), Value::text(commit.hash.as_str()));
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
        name: row_text(row, "id")?,
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
                .is_some_and(|row| row_text(row, "id").as_deref() == Some(name))
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
    fields.insert("id".to_string(), Value::text(r.name.as_str()));
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

// ---------------------------------------------------------------------------
// Opt-in per-collection versioning (Phase 7 — disk-cost isolation)
// ---------------------------------------------------------------------------

/// Is this user collection opted in to Git-for-Data?
///
/// Default is `false`: a fresh collection stays outside the VCS so
/// transactional churn (sessions, caches, queues) doesn't pin
/// extra row versions. Internal `red_*` collections are never
/// versioned — they store VCS metadata itself.
fn is_versioned(store: &UnifiedStore, name: &str) -> bool {
    if name.starts_with("red_") {
        return false;
    }
    let Some(manager) = store.get_collection(vc::SETTINGS) else {
        return false;
    };
    let target = name.to_string();
    manager
        .query_all(|entity| {
            entity
                .data
                .as_row()
                .is_some_and(|row| row_text(row, "id").as_deref() == Some(&target))
        })
        .into_iter()
        .any(|entity| {
            entity
                .data
                .as_row()
                .and_then(|row| row.get_field("versioned"))
                .map(|v| matches!(v, Value::Boolean(true)))
                .unwrap_or(false)
        })
}

/// Enumerate every user collection currently opted in. Order
/// undefined — callers that need a deterministic iteration should
/// sort the returned list.
fn versioned_collections(store: &UnifiedStore) -> Vec<String> {
    let Some(manager) = store.get_collection(vc::SETTINGS) else {
        return Vec::new();
    };
    manager
        .query_all(|entity| {
            entity
                .data
                .as_row()
                .and_then(|row| row.get_field("versioned"))
                .map(|v| matches!(v, Value::Boolean(true)))
                .unwrap_or(false)
        })
        .into_iter()
        .filter_map(|entity| row_text(entity.data.as_row()?, "id"))
        .collect()
}

/// Upsert / delete the `red_vcs_settings` row for `name`. `true`
/// opts the collection into VCS; `false` opts it out (subsequent
/// merges / diffs / AS OF queries stop seeing it). Does NOT touch
/// existing row versions — you control whether history is retained
/// by deciding *when* to opt out.
fn set_versioned_flag(
    store: &UnifiedStore,
    name: &str,
    enabled: bool,
) -> RedDBResult<()> {
    if name.starts_with("red_") {
        return Err(RedDBError::InvalidConfig(format!(
            "cannot version internal collection `{name}`"
        )));
    }
    let target = name.to_string();
    if let Some(manager) = store.get_collection(vc::SETTINGS) {
        let rows = manager.query_all(|entity| {
            entity
                .data
                .as_row()
                .is_some_and(|row| row_text(row, "id").as_deref() == Some(&target))
        });
        for row in rows {
            let _ = store.delete(vc::SETTINGS, row.id);
        }
    }
    if !enabled {
        return Ok(());
    }
    let mut fields: HashMap<String, Value> = HashMap::new();
    fields.insert("id".to_string(), Value::text(name));
    fields.insert("versioned".to_string(), Value::Boolean(true));
    fields.insert("ts_ms".to_string(), Value::TimestampMs(now_ms()));
    insert_meta_row(store, vc::SETTINGS, fields)?;
    Ok(())
}

fn list_refs_by_prefix(store: &UnifiedStore, prefix: Option<&str>) -> Vec<Ref> {
    let Some(manager) = store.get_collection(vc::REFS) else {
        return Vec::new();
    };
    let prefix_owned = prefix.map(|s| s.to_string());
    manager
        .query_all(|entity| {
            entity.data.as_row().is_some_and(|row| {
                let id = row_text(row, "id").unwrap_or_default();
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
                row_text(row, "id")
                    .map(|id| id.starts_with(prefix))
                    .unwrap_or(false)
            })
        })
        .into_iter()
        .filter_map(|entity| row_text(entity.data.as_row()?, "id"))
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
                .is_some_and(|row| row_text(row, "id").as_deref() == Some(&conn_id))
        });
        for row in rows {
            let _ = store.delete(vc::WORKSETS, row.id);
        }
    }
    let mut fields: HashMap<String, Value> = HashMap::new();
    fields.insert("id".to_string(), Value::text(conn_id.as_str()));
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
                .is_some_and(|row| row_text(row, "id").as_deref() == Some(&conn_id))
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

        // Allocate a fresh xid for this commit and immediately mark
        // it committed so all future snapshots see the commit record.
        // Each commit therefore has a unique, monotonic root_xid — a
        // prerequisite for `AS OF BRANCH` to map to distinct
        // snapshots across divergent branches.
        let root_xid = self.inner.snapshot_manager.begin();
        self.inner.snapshot_manager.commit(root_xid);
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
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;

        // Resolve source commit (the one being merged in).
        let from_hash = RedDBRuntime::vcs_resolve_commitish(self, &input.from)?;
        let from_commit = load_commit(store, &from_hash).ok_or_else(|| {
            RedDBError::NotFound(format!("source commit `{from_hash}` not found"))
        })?;

        // Resolve current HEAD for the connection.
        let workset = load_workset(store, input.connection_id);
        let (head_branch, head_hash) = match workset {
            Some((branch, Some(head))) => (branch, head),
            Some((branch, None)) => {
                let head = load_ref(store, &branch).map(|r| r.target).unwrap_or_default();
                (branch, head)
            }
            None => {
                let head = load_ref(store, vc::DEFAULT_BRANCH_REF)
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
        let from_ancestors = ancestor_set(store, &from_hash, 100_000);
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
            save_ref(
                store,
                &Ref {
                    name: head_branch.clone(),
                    kind: RefKind::Branch,
                    target: from_hash.clone(),
                    protected: false,
                },
            )?;
            upsert_workset(
                store,
                input.connection_id,
                &head_branch,
                Some(&from_hash),
                from_commit.root_xid,
            )?;
            return Ok(MergeOutcome {
                merge_commit: Some(from_commit),
                fast_forward: true,
                conflicts: Vec::new(),
                merge_state_id: None,
            });
        }

        // Non-fast-forward: compute LCA, create a merge commit.
        // Data-level 3-way merge (with conflict materialisation into
        // red_conflicts) is deferred to Phase 4 — for now we produce
        // the merge commit only when both sides have no content
        // overlap to resolve, i.e. when LCA == head_hash (reverse FF)
        // we already handled above. Otherwise surface a merge_state
        // placeholder so callers know data reconciliation is required.
        let lca = RedDBRuntime::vcs_lca(self, &head_hash, &from_hash)?;

        let message = input.opts.message.unwrap_or_else(|| {
            format!("Merge {} into {}", input.from, head_branch)
        });
        let author = input.author.clone();
        let timestamp_ms = now_ms();
        let parents = vec![head_hash.clone(), from_hash.clone()];
        let parent_height = parents
            .iter()
            .filter_map(|p| load_commit(store, p).map(|c| c.height))
            .max()
            .unwrap_or(0);
        let height = parent_height + 1;
        let root_xid = self.inner.snapshot_manager.begin();
        self.inner.snapshot_manager.commit(root_xid);

        let hash = compute_commit_hash(
            root_xid,
            &parents,
            &author,
            &message,
            timestamp_ms,
        );

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

        // Create a merge_state row so Phase 4 can pick up where we
        // stopped and actually reconcile data. We still open the
        // commit / ref so log() shows the history; worksets gets the
        // merge_state_id so status() surfaces the in-progress state.
        save_commit(store, &merge_commit)?;
        if root_xid != XID_NONE {
            self.inner.snapshot_manager.pin(root_xid);
        }
        if !head_branch.is_empty() {
            save_ref(
                store,
                &Ref {
                    name: head_branch.clone(),
                    kind: RefKind::Branch,
                    target: hash.clone(),
                    protected: false,
                },
            )?;
        }
        upsert_workset(
            store,
            input.connection_id,
            &head_branch,
            Some(&hash),
            root_xid,
        )?;

        let merge_state_id = format!("ms:{}", &hash[..16]);

        // Materialise conflicts: any entity whose visibility changed
        // in BOTH sides relative to the LCA snapshot is a candidate
        // conflict. Phase 5 records identifiers + metadata; Phase 6
        // will stage the merged body and apply it to the user data.
        let conflicts = if let Some(lca_hash) = &lca {
            materialize_merge_conflicts(
                self,
                store,
                lca_hash,
                &head_hash,
                &from_hash,
                &merge_state_id,
            )?
        } else {
            Vec::new()
        };

        let mut ms_fields: HashMap<String, Value> = HashMap::new();
        ms_fields.insert("id".to_string(), Value::text(merge_state_id.as_str()));
        ms_fields.insert("kind".to_string(), Value::text("merge"));
        ms_fields.insert("branch".to_string(), Value::text(head_branch.as_str()));
        if let Some(base_hash) = &lca {
            ms_fields.insert("base".to_string(), Value::text(base_hash.as_str()));
        }
        ms_fields.insert("ours".to_string(), Value::text(head_hash.as_str()));
        ms_fields.insert("theirs".to_string(), Value::text(from_hash.as_str()));
        ms_fields.insert(
            "conflicts_count".to_string(),
            Value::UnsignedInteger(conflicts.len() as u64),
        );
        insert_meta_row(store, vc::MERGE_STATE, ms_fields)?;

        Ok(MergeOutcome {
            merge_commit: Some(merge_commit),
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

        // Resolve the commit being cherry-picked and its parent.
        let src_hash = RedDBRuntime::vcs_resolve_commitish(self, commit)?;
        let src_commit = load_commit(store, &src_hash).ok_or_else(|| {
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

        // Resolve HEAD for this connection.
        let workset = load_workset(store, connection_id);
        let (head_branch, head_hash) = match workset {
            Some((branch, Some(head))) => (branch, head),
            Some((branch, None)) => {
                let head = load_ref(store, &branch).map(|r| r.target).unwrap_or_default();
                (branch, head)
            }
            None => {
                let head = load_ref(store, vc::DEFAULT_BRANCH_REF)
                    .map(|r| r.target)
                    .unwrap_or_default();
                (vc::DEFAULT_BRANCH_REF.to_string(), head)
            }
        };
        if head_hash.is_empty() {
            return Err(RedDBError::InvalidConfig(
                "cannot cherry-pick onto empty HEAD".to_string(),
            ));
        }

        // Cherry-pick = 3-way merge with base = parent(src), ours = HEAD,
        // theirs = src. The new commit records the pick so log() sees
        // it; data application is staged in merge_state.pending so
        // Phase 6 can apply it transactionally.
        let message = format!("cherry-pick: {}", src_commit.message);
        let parents = vec![head_hash.clone()];
        let parent_height = load_commit(store, &head_hash)
            .map(|c| c.height)
            .unwrap_or(0);
        let height = parent_height + 1;
        let root_xid = self.inner.snapshot_manager.begin();
        self.inner.snapshot_manager.commit(root_xid);
        let timestamp_ms = now_ms();

        let hash = compute_commit_hash(
            root_xid,
            &parents,
            &author,
            &message,
            timestamp_ms,
        );
        let pick_commit = Commit {
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
        save_commit(store, &pick_commit)?;
        if root_xid != XID_NONE {
            self.inner.snapshot_manager.pin(root_xid);
        }
        if !head_branch.is_empty() {
            save_ref(
                store,
                &Ref {
                    name: head_branch.clone(),
                    kind: RefKind::Branch,
                    target: hash.clone(),
                    protected: false,
                },
            )?;
        }
        upsert_workset(
            store,
            connection_id,
            &head_branch,
            Some(&hash),
            root_xid,
        )?;

        let merge_state_id = format!("cp:{}", &hash[..16]);
        let conflicts = materialize_merge_conflicts(
            self,
            store,
            &parent_hash,
            &head_hash,
            &src_hash,
            &merge_state_id,
        )?;

        let mut ms_fields: HashMap<String, Value> = HashMap::new();
        ms_fields.insert("id".to_string(), Value::text(merge_state_id.as_str()));
        ms_fields.insert("kind".to_string(), Value::text("cherry_pick"));
        ms_fields.insert("branch".to_string(), Value::text(head_branch.as_str()));
        ms_fields.insert("base".to_string(), Value::text(parent_hash.as_str()));
        ms_fields.insert("ours".to_string(), Value::text(head_hash.as_str()));
        ms_fields.insert("theirs".to_string(), Value::text(src_hash.as_str()));
        ms_fields.insert(
            "conflicts_count".to_string(),
            Value::UnsignedInteger(conflicts.len() as u64),
        );
        insert_meta_row(store, vc::MERGE_STATE, ms_fields)?;

        Ok(MergeOutcome {
            merge_commit: Some(pick_commit),
            fast_forward: false,
            conflicts,
            merge_state_id: Some(merge_state_id),
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

        let src_hash = RedDBRuntime::vcs_resolve_commitish(self, commit)?;
        let src_commit = load_commit(store, &src_hash).ok_or_else(|| {
            RedDBError::NotFound(format!("revert source `{src_hash}` not found"))
        })?;
        if src_commit.parents.is_empty() {
            return Err(RedDBError::InvalidConfig(
                "cannot revert a root commit".to_string(),
            ));
        }
        let parent_hash = src_commit.parents[0].clone();

        let workset = load_workset(store, connection_id);
        let (head_branch, head_hash) = match workset {
            Some((branch, Some(head))) => (branch, head),
            Some((branch, None)) => {
                let head = load_ref(store, &branch).map(|r| r.target).unwrap_or_default();
                (branch, head)
            }
            None => {
                let head = load_ref(store, vc::DEFAULT_BRANCH_REF)
                    .map(|r| r.target)
                    .unwrap_or_default();
                (vc::DEFAULT_BRANCH_REF.to_string(), head)
            }
        };
        if head_hash.is_empty() {
            return Err(RedDBError::InvalidConfig(
                "cannot revert onto empty HEAD".to_string(),
            ));
        }

        // Revert = 3-way merge with base = src, ours = HEAD,
        // theirs = parent(src). The asymmetry vs cherry-pick flips
        // which side provides the "forward" delta so the effect is
        // the inverse of the original commit.
        let message = format!("Revert \"{}\"", src_commit.message);
        let parents = vec![head_hash.clone()];
        let parent_height = load_commit(store, &head_hash)
            .map(|c| c.height)
            .unwrap_or(0);
        let height = parent_height + 1;
        let root_xid = self.inner.snapshot_manager.begin();
        self.inner.snapshot_manager.commit(root_xid);
        let timestamp_ms = now_ms();

        let hash = compute_commit_hash(
            root_xid,
            &parents,
            &author,
            &message,
            timestamp_ms,
        );
        let rv_commit = Commit {
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
        save_commit(store, &rv_commit)?;
        if root_xid != XID_NONE {
            self.inner.snapshot_manager.pin(root_xid);
        }
        if !head_branch.is_empty() {
            save_ref(
                store,
                &Ref {
                    name: head_branch.clone(),
                    kind: RefKind::Branch,
                    target: hash.clone(),
                    protected: false,
                },
            )?;
        }
        upsert_workset(
            store,
            connection_id,
            &head_branch,
            Some(&hash),
            root_xid,
        )?;

        let merge_state_id = format!("rv:{}", &hash[..16]);
        // Record merge_state for later data apply. No conflict
        // materialisation here — revert conflicts are rare when the
        // reverted commit touches disjoint entities.
        let mut ms_fields: HashMap<String, Value> = HashMap::new();
        ms_fields.insert("id".to_string(), Value::text(merge_state_id.as_str()));
        ms_fields.insert("kind".to_string(), Value::text("revert"));
        ms_fields.insert("branch".to_string(), Value::text(head_branch.as_str()));
        ms_fields.insert("base".to_string(), Value::text(src_hash.as_str()));
        ms_fields.insert("ours".to_string(), Value::text(head_hash.as_str()));
        ms_fields.insert("theirs".to_string(), Value::text(parent_hash.as_str()));
        ms_fields.insert("conflicts_count".to_string(), Value::UnsignedInteger(0));
        insert_meta_row(store, vc::MERGE_STATE, ms_fields)?;

        Ok(rv_commit)
    }

    pub fn vcs_reset(&self, input: ResetInput) -> RedDBResult<()> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let target_hash = RedDBRuntime::vcs_resolve_commitish(self, &input.target)?;
        let target_commit = load_commit(store, &target_hash).ok_or_else(|| {
            RedDBError::NotFound(format!("target commit `{target_hash}` not found"))
        })?;

        // Find the current branch for this connection.
        let workset = load_workset(store, input.connection_id);
        let branch = workset
            .as_ref()
            .map(|(b, _)| b.clone())
            .unwrap_or_else(|| vc::DEFAULT_BRANCH_REF.to_string());

        // Soft and Mixed both move the branch ref + workset base.
        // Hard would additionally revert entity data — deferred to
        // Phase 4 because it requires selective MVCC rewind.
        match input.mode {
            ResetMode::Soft | ResetMode::Mixed => {
                if !branch.is_empty() {
                    save_ref(
                        store,
                        &Ref {
                            name: branch.clone(),
                            kind: RefKind::Branch,
                            target: target_hash.clone(),
                            protected: false,
                        },
                    )?;
                }
                upsert_workset(
                    store,
                    input.connection_id,
                    &branch,
                    Some(&target_hash),
                    target_commit.root_xid,
                )?;
                Ok(())
            }
            ResetMode::Hard => Err(unimplemented("reset --hard (Phase 4)")),
        }
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
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let from_hash = RedDBRuntime::vcs_resolve_commitish(self, &input.from)?;
        let to_hash = RedDBRuntime::vcs_resolve_commitish(self, &input.to)?;
        let from_xid = RedDBRuntime::vcs_resolve_as_of(
            self,
            AsOfSpec::Commit(from_hash.clone()),
        )?;
        let to_xid = RedDBRuntime::vcs_resolve_as_of(
            self,
            AsOfSpec::Commit(to_hash.clone()),
        )?;

        let sm = &self.inner.snapshot_manager;
        let from_snap = sm.snapshot(from_xid);
        let to_snap = sm.snapshot(to_xid);

        // Iterate every *user* collection (skip internal red_*).
        let mut entries: Vec<DiffEntry> = Vec::new();
        let mut added = 0usize;
        let mut removed = 0usize;
        let mut modified = 0usize;
        let collections = store.list_collections();
        for coll in collections {
            if coll.starts_with("red_") {
                continue;
            }
            if !is_versioned(store, &coll) {
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
            let entities = manager.query_all(|_| true);
            // Group by entity id so we can compare before/after state.
            for entity in &entities {
                let xmin = entity.xmin;
                let xmax = entity.xmax;
                let in_from = from_snap.sees(xmin, xmax)
                    && !sm.is_aborted(xmin);
                let in_to = to_snap.sees(xmin, xmax)
                    && !sm.is_aborted(xmin);
                if in_from == in_to {
                    continue;
                }
                let entity_id = entity.id.raw().to_string();
                let payload = if input.summary_only {
                    JsonValue::Null
                } else {
                    JsonValue::String(format!(
                        "entity#{} xmin={} xmax={}",
                        entity_id, xmin, xmax
                    ))
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
                    id: row_text(row, "id")?,
                    collection: row_text(row, "collection")?,
                    entity_id: row_text(row, "entity_id").unwrap_or_default(),
                    base: row_json(row, "base_json"),
                    ours: row_json(row, "ours_json"),
                    theirs: row_json(row, "theirs_json"),
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
        _resolved: JsonValue,
    ) -> RedDBResult<()> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let Some(manager) = store.get_collection(vc::CONFLICTS) else {
            return Err(RedDBError::NotFound(format!(
                "conflict `{conflict_id}` not found"
            )));
        };
        let cid = conflict_id.to_string();
        let mut deleted = 0usize;
        let matches = manager.query_all(|entity| {
            entity
                .data
                .as_row()
                .is_some_and(|row| row_text(row, "id").as_deref() == Some(&cid))
        });
        for entity in matches {
            store
                .delete(vc::CONFLICTS, entity.id)
                .map_err(|e| RedDBError::Internal(e.to_string()))?;
            deleted += 1;
        }
        // NOTE: Phase 4 will also apply `_resolved` to the user
        // collection under the current working set before deleting
        // the conflict row. Here we only remove the marker.
        if deleted == 0 {
            return Err(RedDBError::NotFound(format!(
                "conflict `{conflict_id}` not found"
            )));
        }
        Ok(())
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

    pub fn vcs_set_versioned(&self, collection: &str, enabled: bool) -> RedDBResult<()> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        set_versioned_flag(store, collection, enabled)
    }

    pub fn vcs_list_versioned(&self) -> RedDBResult<Vec<String>> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        let mut list = versioned_collections(store);
        list.sort();
        Ok(list)
    }

    pub fn vcs_is_versioned(&self, collection: &str) -> RedDBResult<bool> {
        let store_arc = self.inner.db.store();
        let store: &UnifiedStore = &store_arc;
        Ok(is_versioned(store, collection))
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

/// Detect entities changed on both sides of a 3-way merge and write
/// a `red_conflicts` row for each. Returns the in-memory `Conflict`
/// list in the same shape `vcs_conflicts_list` would later read back.
///
/// Phase 5: metadata-level only — identifiers + which sides moved.
/// Phase 6 will stage the merged body (base/ours/theirs payloads +
/// JSON 3-way merge result) so `vcs_conflict_resolve` can apply the
/// resolved value back to the user collection.
fn materialize_merge_conflicts(
    rt: &RedDBRuntime,
    store: &UnifiedStore,
    base_hash: &str,
    ours_hash: &str,
    theirs_hash: &str,
    merge_state_id: &str,
) -> RedDBResult<Vec<Conflict>> {
    use crate::application::merge_json::three_way_merge;
    use crate::application::vcs::AsOfSpec;

    let base_xid = rt.vcs_resolve_as_of(AsOfSpec::Commit(base_hash.to_string()))?;
    let ours_xid = rt.vcs_resolve_as_of(AsOfSpec::Commit(ours_hash.to_string()))?;
    let theirs_xid = rt.vcs_resolve_as_of(AsOfSpec::Commit(theirs_hash.to_string()))?;

    let sm = &rt.inner.snapshot_manager;
    let base_snap = sm.snapshot(base_xid);
    let ours_snap = sm.snapshot(ours_xid);
    let theirs_snap = sm.snapshot(theirs_xid);

    // Walk every VCS-opted-in user collection, materialising
    // per-(entity_id) visible JSON bodies at each of the three
    // snapshots. Collections that never opted in are skipped — by
    // definition they have no history semantics worth conflicting
    // over.
    let mut conflicts: Vec<Conflict> = Vec::new();
    for coll in store.list_collections() {
        if coll.starts_with("red_") {
            continue;
        }
        if !is_versioned(store, &coll) {
            continue;
        }
        let Some(manager) = store.get_collection(&coll) else {
            continue;
        };
        let mut at_base: HashMap<u64, JsonValue> = HashMap::new();
        let mut at_ours: HashMap<u64, JsonValue> = HashMap::new();
        let mut at_theirs: HashMap<u64, JsonValue> = HashMap::new();
        for entity in manager.query_all(|_| true) {
            let xmin = entity.xmin;
            let xmax = entity.xmax;
            if sm.is_aborted(xmin) {
                continue;
            }
            let eid = entity.id.raw();
            let body = crate::presentation::entity_json::compact_entity_json(&entity);
            if base_snap.sees(xmin, xmax) {
                at_base.insert(eid, body.clone());
            }
            if ours_snap.sees(xmin, xmax) {
                at_ours.insert(eid, body.clone());
            }
            if theirs_snap.sees(xmin, xmax) {
                at_theirs.insert(eid, body);
            }
        }

        let mut all_ids: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
        all_ids.extend(at_base.keys().copied());
        all_ids.extend(at_ours.keys().copied());
        all_ids.extend(at_theirs.keys().copied());

        for eid in all_ids {
            let b = at_base.get(&eid).cloned().unwrap_or(JsonValue::Null);
            let o = at_ours.get(&eid).cloned().unwrap_or(JsonValue::Null);
            let t = at_theirs.get(&eid).cloned().unwrap_or(JsonValue::Null);
            let ours_changed = b != o;
            let theirs_changed = b != t;
            if !(ours_changed && theirs_changed) {
                continue;
            }
            if o == t {
                continue;
            }
            let merge = three_way_merge(&b, &o, &t);
            if merge.is_clean() {
                // Both sides touched different paths — no conflict
                // to record; Phase 6.2 will stage the merged body
                // into the workset for automatic apply.
                continue;
            }
            let conflict_id = format!("{}:{}/{}", merge_state_id, coll, eid);
            let paths: Vec<String> = merge
                .conflicts
                .iter()
                .map(|c| {
                    if c.path.is_empty() {
                        "*".to_string()
                    } else {
                        c.path.clone()
                    }
                })
                .collect();

            let mut fields: HashMap<String, Value> = HashMap::new();
            fields.insert("id".to_string(), Value::text(conflict_id.as_str()));
            fields.insert("collection".to_string(), Value::text(coll.as_str()));
            fields.insert("entity_id".to_string(), Value::text(eid.to_string().as_str()));
            fields.insert(
                "merge_state_id".to_string(),
                Value::text(merge_state_id),
            );
            fields.insert(
                "conflicting_paths".to_string(),
                Value::Array(paths.iter().map(|p| Value::text(p.as_str())).collect()),
            );
            // Persist the three bodies as Value::Json blobs so
            // vcs_conflicts_list can hydrate them back into the
            // Conflict struct for presentation.
            if let Ok(bytes) = crate::json::to_vec(&b) {
                fields.insert("base_json".to_string(), Value::Json(bytes));
            }
            if let Ok(bytes) = crate::json::to_vec(&o) {
                fields.insert("ours_json".to_string(), Value::Json(bytes));
            }
            if let Ok(bytes) = crate::json::to_vec(&t) {
                fields.insert("theirs_json".to_string(), Value::Json(bytes));
            }
            insert_meta_row(store, vc::CONFLICTS, fields)?;
            conflicts.push(Conflict {
                id: conflict_id,
                collection: coll.clone(),
                entity_id: eid.to_string(),
                base: b,
                ours: o,
                theirs: t,
                conflicting_paths: paths,
                merge_state_id: merge_state_id.to_string(),
            });
        }
    }
    Ok(conflicts)
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
    out.sort_by(|a, b| a.collection.cmp(&b.collection).then(a.entity_id.cmp(&b.entity_id)));
    out
}

// Unused-import guards for stubs that reference types via `_`.
#[allow(dead_code)]
fn _touch_types(_: BTreeSet<()>) {}
