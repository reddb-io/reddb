//! Three-way merge over typed entity images. Presentation JSON never becomes
//! the storage representation of an automatically merged value.
use super::*;
use crate::storage::unified::store::RowPublication;
use crate::storage::unified::{Metadata, MetadataValue};
use reddb_engine::storage::entity::compute_entity_field_bloom;

#[derive(Clone)]
struct Image {
    entity: UnifiedEntity,
    metadata: HashMap<String, MetadataValue>,
}

impl PartialEq for Image {
    fn eq(&self, other: &Self) -> bool {
        self.entity.kind == other.entity.kind
            && self.entity.data == other.entity.data
            && self.entity.embeddings() == other.entity.embeddings()
            && self.entity.cross_refs() == other.entity.cross_refs()
            && self.metadata == other.metadata
    }
}

fn choose<T: Clone + PartialEq>(base: &T, ours: &T, theirs: &T, path: &str) -> Result<T, String> {
    if ours == theirs || theirs == base {
        Ok(ours.clone())
    } else if ours == base {
        Ok(theirs.clone())
    } else {
        Err(path.into())
    }
}

fn merge_map<T: Clone + PartialEq>(
    base: &HashMap<String, T>,
    ours: &HashMap<String, T>,
    theirs: &HashMap<String, T>,
    path: &str,
) -> Result<HashMap<String, T>, String> {
    let keys: BTreeSet<_> = base
        .keys()
        .chain(ours.keys())
        .chain(theirs.keys())
        .collect();
    let mut merged = HashMap::new();
    for key in keys {
        let value = choose(
            &base.get(key),
            &ours.get(key),
            &theirs.get(key),
            &format!("{path}/{key}"),
        )?;
        if let Some(value) = value {
            merged.insert(key.clone(), value.clone());
        }
    }
    Ok(merged)
}

fn merge_values(
    base: &HashMap<String, Value>,
    ours: &HashMap<String, Value>,
    theirs: &HashMap<String, Value>,
    path: &str,
    depth: usize,
) -> Result<HashMap<String, Value>, String> {
    if depth > 64 {
        return Err(path.into());
    }
    let keys: BTreeSet<_> = base
        .keys()
        .chain(ours.keys())
        .chain(theirs.keys())
        .collect();
    let mut merged = HashMap::new();
    for key in keys {
        let path = format!("{path}/{key}");
        let (b, o, t) = (base.get(key), ours.get(key), theirs.get(key));
        let chosen = choose(&b, &o, &t, &path).map(|value| value.cloned());
        let value = match (chosen, b, o, t) {
            (Err(_), Some(Value::Json(b)), Some(Value::Json(o)), Some(Value::Json(t))) => {
                let decode = |bytes: &[u8]| {
                    reddb_types::document_body_codec::decode(bytes)
                        .map(|fields| fields.into_iter().collect::<HashMap<_, _>>())
                };
                let bytes =
                    if b.starts_with(b"RDOC") && o.starts_with(b"RDOC") && t.starts_with(b"RDOC") {
                        let fields = merge_values(
                            &decode(b).map_err(|_| path.clone())?,
                            &decode(o).map_err(|_| path.clone())?,
                            &decode(t).map_err(|_| path.clone())?,
                            &path,
                            depth + 1,
                        )?;
                        let mut fields: Vec<_> = fields
                            .iter()
                            .map(|(key, value)| (key.as_str(), value))
                            .collect();
                        fields.sort_by_key(|(key, _)| *key);
                        let mut bytes = Vec::new();
                        reddb_types::document_body_codec::encode(&fields, &mut bytes)
                            .map_err(|_| path.clone())?;
                        bytes
                    } else {
                        let decode = |bytes: &[u8]| {
                            crate::json::from_slice::<JsonValue>(bytes).map_err(|_| path.clone())
                        };
                        let merged = crate::application::merge_json::three_way_merge(
                            &decode(b)?,
                            &decode(o)?,
                            &decode(t)?,
                        );
                        if !merged.is_clean() {
                            return Err(path);
                        }
                        crate::json::to_vec(&merged.merged).map_err(|_| path.clone())?
                    };
                Some(Value::Json(bytes))
            }
            (chosen, _, _, _) => chosen?,
        };
        if let Some(value) = value {
            merged.insert(key.clone(), value);
        }
    }
    Ok(merged)
}

fn merge_image(base: &Image, ours: &Image, theirs: &Image) -> Result<Image, String> {
    let mut merged = ours.clone();
    merged.entity.kind = choose(
        &base.entity.kind,
        &ours.entity.kind,
        &theirs.entity.kind,
        "/kind",
    )?;
    merged.metadata = merge_map(
        &base.metadata,
        &ours.metadata,
        &theirs.metadata,
        "/metadata",
    )?;
    *merged.entity.embeddings_mut() = choose(
        &base.entity.embeddings().to_vec(),
        &ours.entity.embeddings().to_vec(),
        &theirs.entity.embeddings().to_vec(),
        "/embeddings",
    )?;
    *merged.entity.cross_refs_mut() = choose(
        &base.entity.cross_refs().to_vec(),
        &ours.entity.cross_refs().to_vec(),
        &theirs.entity.cross_refs().to_vec(),
        "/cross_refs",
    )?;
    merged.entity.data = match (&base.entity.data, &ours.entity.data, &theirs.entity.data) {
        (EntityData::Node(b), EntityData::Node(o), EntityData::Node(t)) => {
            let mut value = o.clone();
            value.properties = merge_values(
                &b.properties,
                &o.properties,
                &t.properties,
                "/properties",
                0,
            )?;
            EntityData::Node(value)
        }
        (EntityData::Edge(b), EntityData::Edge(o), EntityData::Edge(t)) => {
            let mut value = o.clone();
            value.properties = merge_values(
                &b.properties,
                &o.properties,
                &t.properties,
                "/properties",
                0,
            )?;
            value.weight = choose(&b.weight, &o.weight, &t.weight, "/weight")?;
            EntityData::Edge(value)
        }
        (EntityData::Row(b), EntityData::Row(o), EntityData::Row(t))
            if b.named.is_some() && o.named.is_some() && t.named.is_some() =>
        {
            let mut value = o.clone();
            value.named = Some(merge_values(
                b.named.as_ref().unwrap(),
                o.named.as_ref().unwrap(),
                t.named.as_ref().unwrap(),
                "/row",
                0,
            )?);
            EntityData::Row(value)
        }
        (EntityData::Vector(b), EntityData::Vector(o), EntityData::Vector(t)) => {
            let mut value = o.clone();
            value.dense = choose(&b.dense, &o.dense, &t.dense, "/dense")?;
            value.sparse = choose(&b.sparse, &o.sparse, &t.sparse, "/sparse")?;
            value.content = choose(&b.content, &o.content, &t.content, "/content")?;
            EntityData::Vector(value)
        }
        (b, o, t) => choose(b, o, t, "/data")?,
    };
    Ok(merged)
}

struct Change {
    collection: String,
    before: Option<Image>,
    after: Option<Image>,
}

pub(super) struct Plan {
    changes: Vec<Change>,
    pub conflicts: Vec<Conflict>,
}

impl Plan {
    pub fn stage(
        self,
        store: &UnifiedStore,
        publication: &mut RowPublication,
        epoch: u64,
        xid: Xid,
    ) -> RedDBResult<()> {
        assert!(self.conflicts.is_empty());
        for change in self.changes {
            if let Some(before) = change.before {
                // The plan was built while holding the publication fence, so
                // its preimage includes deletion stamps from every branch.
                let mut entity = before.entity;
                entity.set_branch_xmax(epoch, xid);
                publication.replacements.push((
                    change.collection.clone(),
                    entity,
                    Some(Metadata::with_fields(before.metadata)),
                ));
            }
            if let Some(after) = change.after {
                let mut entity = after.entity;
                let logical_id = entity.logical_id();
                entity.id = store.next_entity_id();
                entity.set_logical_id(logical_id);
                entity.xmin = xid;
                entity.xmax = 0;
                entity.set_branch_origin(epoch);
                entity.field_bloom = compute_entity_field_bloom(&entity.data);
                publication.inserts.push((
                    change.collection,
                    entity,
                    Some(Metadata::with_fields(after.metadata)),
                ));
            }
        }
        Ok(())
    }
}

pub(super) fn plan(
    runtime: &RedDBRuntime,
    base: &str,
    ours: &str,
    theirs: &str,
    state: &str,
) -> RedDBResult<Plan> {
    let contexts = [
        runtime.vcs_commit_read_context(base)?,
        runtime.vcs_commit_read_context(ours)?,
        runtime.vcs_commit_read_context(theirs)?,
    ];
    plan_contexts(runtime, contexts, state, true)
}

pub(super) fn read_conflicts(runtime: &RedDBRuntime, state: &str) -> RedDBResult<Vec<Conflict>> {
    let Some(row) = metadata_row(&runtime.db().store(), vc::MERGE_STATE, state)? else {
        return Ok(Vec::new());
    };
    let hash = |name| {
        row_text(&row, name)
            .ok_or_else(|| RedDBError::Internal(format!("merge state missing {name}")))
    };
    let contexts = [
        runtime.vcs_commit_read_context(&hash("base")?)?,
        runtime.vcs_commit_read_context(&hash("ours")?)?,
        runtime.vcs_commit_read_context(&hash("theirs")?)?,
    ];
    Ok(plan_contexts(runtime, contexts, state, false)?.conflicts)
}

pub(super) fn validate_head_change(
    runtime: &RedDBRuntime,
    connection: u64,
    target: &str,
) -> RedDBResult<()> {
    let target = runtime.vcs_commit_read_context(target)?;
    let mut live = target.clone();
    live.snapshot = runtime.snapshot_manager().fresh_read_snapshot();
    live.branch = Some(Arc::new(branches::live_view(
        runtime,
        connection,
        &live.snapshot,
    )?));
    plan_contexts(runtime, [live.clone(), live, target], "head-change", true)?;
    Ok(())
}

fn plan_contexts(
    runtime: &RedDBRuntime,
    contexts: [reddb_engine::transaction::read::SnapshotContext; 3],
    state: &str,
    authorize_changes: bool,
) -> RedDBResult<Plan> {
    let store = runtime.db().store();
    let mut plan = Plan {
        changes: Vec::new(),
        conflicts: Vec::new(),
    };
    for collection in versioned_collections(&store)? {
        let Some(manager) = store.get_collection(&collection) else {
            continue;
        };
        let mut images: [HashMap<u64, Image>; 3] = Default::default();
        let mut visible = Vec::new();
        manager.try_for_each_entity(usize::MAX, |entity| {
            if contexts
                .iter()
                .any(|context| snapshot_sees_entity(context, entity))
            {
                visible.push(entity.clone());
            }
            true
        })?;
        for mut entity in visible {
            let metadata = store
                .try_get_metadata(&collection, entity.id, usize::MAX)?
                .unwrap_or_default()
                .fields;
            // Normalize schema-backed and named row representations before
            // comparison, retaining the original native Value variants.
            if let EntityData::Row(row) = &mut entity.data {
                if row.named.is_some() || row.schema.is_some() {
                    row.named = Some(
                        row.iter_fields()
                            .map(|(name, value)| (name.into(), value.clone()))
                            .collect(),
                    );
                    row.columns.clear();
                    row.schema = None;
                }
            }
            for (context, images) in contexts.iter().zip(&mut images) {
                if snapshot_sees_entity(context, &entity) {
                    if images
                        .insert(
                            entity.logical_id().raw(),
                            Image {
                                entity: entity.clone(),
                                metadata: metadata.clone(),
                            },
                        )
                        .is_some()
                    {
                        return Err(RedDBError::Internal(
                            "vcs: multiple visible physical versions".into(),
                        ));
                    }
                }
            }
        }
        if runtime
            .db()
            .collection_contract(&collection)
            .is_some_and(|contract| {
                matches!(
                    contract.declared_model,
                    crate::catalog::CollectionModel::Kv
                        | crate::catalog::CollectionModel::Config
                        | crate::catalog::CollectionModel::Vault
                )
            })
        {
            // Independently created keys have different physical/logical IDs.
            // They still address the same user key and must conflict together.
            let key = |image: &Image| {
                image.entity.data.as_row().and_then(|row| {
                    Some((
                        row_text(row, "_vault_tenant").unwrap_or_default(),
                        row_text(row, "key")?,
                    ))
                })
            };
            let mut identities = HashMap::new();
            for image in images.iter().flat_map(|images| images.values()) {
                if let Some(key) = key(image) {
                    identities
                        .entry(key)
                        .and_modify(|id: &mut u64| *id = (*id).min(image.entity.logical_id().raw()))
                        .or_insert(image.entity.logical_id().raw());
                }
            }
            for images in &mut images {
                let mut keyed: HashMap<u64, Image> = HashMap::new();
                for (id, image) in std::mem::take(images) {
                    let id = key(&image)
                        .and_then(|key| identities.get(&key).copied())
                        .unwrap_or(id);
                    let rank = |image: &Image| {
                        (
                            image.entity.xmin,
                            image
                                .entity
                                .data
                                .as_row()
                                .and_then(|row| row_i64(row, "version"))
                                .unwrap_or(0),
                            image.entity.id,
                        )
                    };
                    if keyed
                        .get(&id)
                        .is_none_or(|previous| rank(&image) > rank(previous))
                    {
                        keyed.insert(id, image);
                    }
                }
                *images = keyed;
            }
        }
        let [base, ours, theirs] = images;
        let ids: BTreeSet<_> = base
            .keys()
            .chain(ours.keys())
            .chain(theirs.keys())
            .copied()
            .collect();
        for id in ids {
            let b = base.get(&id).cloned();
            let o = ours.get(&id).cloned();
            let t = theirs.get(&id).cloned();
            let merged = choose(&b, &o, &t, "*").or_else(|path| match (&b, &o, &t) {
                (Some(b), Some(o), Some(t)) => merge_image(b, o, t).map(Some),
                _ => Err(path),
            });
            let conflict_id = format!("{state}:{collection}/{id}");
            let merged = if merged.is_err() {
                match metadata_row(&store, vc::CONFLICTS, &conflict_id)?
                    .as_ref()
                    .and_then(|row| row_text(row, "resolution"))
                    .as_deref()
                {
                    Some("ours") => Ok(o.clone()),
                    Some("theirs") => Ok(t.clone()),
                    Some("base") => Ok(b.clone()),
                    Some("delete") => Ok(None),
                    _ => merged,
                }
            } else {
                merged
            };
            match merged {
                Ok(after) if after != o => {
                    use reddb_rql::ast::PolicyAction;
                    let action = if !authorize_changes {
                        PolicyAction::Select
                    } else {
                        match (&o, &after) {
                            (None, _) => PolicyAction::Insert,
                            (_, None) => PolicyAction::Delete,
                            _ => PolicyAction::Update,
                        }
                    };
                    for image in o.iter().chain(after.iter()) {
                        ensure_vcs_entity_access(runtime, &collection, &image.entity, action)?;
                    }
                    plan.changes.push(Change {
                        collection: collection.clone(),
                        before: o,
                        after,
                    });
                }
                Ok(_) => {}
                Err(path) => {
                    for image in b.iter().chain(o.iter()).chain(t.iter()) {
                        ensure_vcs_entity_access(
                            runtime,
                            &collection,
                            &image.entity,
                            reddb_rql::ast::PolicyAction::Select,
                        )?;
                    }
                    let body = |image: &Option<Image>| {
                        image
                            .as_ref()
                            .map(|image| vcs_entity_body(runtime, &collection, &image.entity))
                            .unwrap_or(JsonValue::Null)
                    };
                    let (base_body, ours_body, theirs_body) = (body(&b), body(&o), body(&t));
                    let display = crate::application::merge_json::three_way_merge(
                        &base_body,
                        &ours_body,
                        &theirs_body,
                    );
                    let paths = if display.conflicts.is_empty() {
                        vec![path]
                    } else {
                        display
                            .conflicts
                            .into_iter()
                            .map(|conflict| {
                                if conflict.path.is_empty() {
                                    "*".into()
                                } else {
                                    conflict.path
                                }
                            })
                            .collect()
                    };
                    plan.conflicts.push(Conflict {
                        id: conflict_id,
                        collection: collection.clone(),
                        entity_id: id.to_string(),
                        base: base_body,
                        ours: ours_body,
                        theirs: theirs_body,
                        conflicting_paths: paths,
                        merge_state_id: state.into(),
                    });
                }
            }
        }
    }
    Ok(plan)
}

pub(super) fn publish_commit(
    runtime: &RedDBRuntime,
    connection: u64,
    branch: &str,
    commit: &Commit,
    snapshot: &Snapshot,
    plan: Option<Plan>,
) -> RedDBResult<()> {
    let store = runtime.db().store();
    let view = branches::live_view(runtime, connection, snapshot)?;
    let mut publication = RowPublication::default();
    if let Some(plan) = plan {
        plan.stage(&store, &mut publication, view.writer_epoch, snapshot.xid)?;
    }
    branches::stage_metadata(
        &store,
        &mut publication,
        vc::COMMITS,
        "id",
        &commit.hash,
        commit_fields(commit, snapshot, &view),
    )?;
    branches::stage_head(&store, &mut publication, connection, branch, commit, None)?;
    if let Some((_, _, Some(state))) = load_workset_state(&store, connection) {
        for collection in [vc::CONFLICTS, vc::MERGE_STATE] {
            if let Some(manager) = store.get_collection(collection) {
                manager.try_for_each_entity(usize::MAX, |entity| {
                    if entity.data.as_row().is_some_and(|row| {
                        row_text(
                            row,
                            if collection == vc::CONFLICTS {
                                "merge_state_id"
                            } else {
                                "id"
                            },
                        )
                        .as_deref()
                            == Some(&state)
                    }) {
                        publication.deletes.push((collection.into(), entity.id));
                    }
                    true
                })?;
            }
        }
    }
    store.publish_rows(publication)?;
    runtime.invalidate_result_cache();
    Ok(())
}

pub(super) fn metadata_row(
    store: &UnifiedStore,
    collection: &str,
    id: &str,
) -> RedDBResult<Option<RowData>> {
    let mut result = None;
    if let Some(manager) = store.get_collection(collection) {
        manager.try_for_each_entity(usize::MAX, |entity| {
            if let Some(row) = entity
                .data
                .as_row()
                .filter(|row| row_text(row, "id").as_deref() == Some(id))
            {
                result = Some(row.clone());
                return false;
            }
            true
        })?;
    }
    Ok(result)
}

pub(super) fn finish_pending(
    runtime: &RedDBRuntime,
    input: CreateCommitInput,
    state: &str,
) -> RedDBResult<Commit> {
    let store = runtime.db().store();
    let row = metadata_row(&store, vc::MERGE_STATE, state)?
        .ok_or_else(|| RedDBError::Internal("missing pending merge state".into()))?;
    let field = |name: &str| {
        row_text(&row, name).ok_or_else(|| RedDBError::Internal(format!("missing merge {name}")))
    };
    let (branch, base, ours, theirs) = (
        field("branch")?,
        field("base")?,
        field("ours")?,
        field("theirs")?,
    );
    if load_ref(&store, &branch)
        .map(|reference| reference.target)
        .as_deref()
        != Some(&ours)
    {
        return Err(RedDBError::InvalidOperation(
            "branch HEAD changed during pending merge".into(),
        ));
    }
    branches::ensure_clean(runtime, input.connection_id, &ours)?;
    let plan = plan(runtime, &base, &ours, &theirs, state)?;
    if !plan.conflicts.is_empty() {
        return Err(RedDBError::InvalidOperation(
            "resolve all pending merge conflicts before committing".into(),
        ));
    }
    let snapshot_manager = runtime.snapshot_manager();
    let mut snapshot = capture_commit_snapshot(&snapshot_manager);
    let parents = vec![ours, theirs];
    let timestamp_ms = now_ms();
    let commit = Commit {
        hash: compute_commit_hash(
            snapshot.xid,
            &parents,
            &input.author,
            &input.message,
            timestamp_ms,
        ),
        root_xid: snapshot.xid,
        height: parents
            .iter()
            .filter_map(|hash| load_commit(&store, hash).map(|commit| commit.height))
            .max()
            .unwrap_or(0)
            + 1,
        parents,
        committer: input.committer.unwrap_or_else(|| input.author.clone()),
        author: input.author,
        message: input.message,
        timestamp_ms,
        signature: None,
    };
    let result = publish_commit(
        runtime,
        input.connection_id,
        &branch,
        &commit,
        &snapshot,
        Some(plan),
    );
    result?;
    snapshot.retained = true;
    Ok(commit)
}

pub(super) fn apply_delta(
    runtime: &RedDBRuntime,
    connection: u64,
    base: &str,
    source: &str,
    author: Author,
    message: String,
) -> RedDBResult<Commit> {
    branches::ensure_entity_history_supported(runtime)?;
    let store = runtime.db().store();
    let (branch, workset_head) =
        load_workset(&store, connection).unwrap_or_else(|| (vc::DEFAULT_BRANCH_REF.into(), None));
    if !branch.starts_with(vc::BRANCH_REF_PREFIX) {
        return Err(RedDBError::InvalidOperation(
            "cannot apply history to a detached or tag checkout".into(),
        ));
    }
    let head = load_ref(&store, &branch)
        .map(|reference| reference.target)
        .or(workset_head)
        .filter(|hash| !hash.is_empty())
        .ok_or_else(|| {
            RedDBError::InvalidOperation("cannot apply history to an empty HEAD".into())
        })?;
    branches::ensure_clean(runtime, connection, &head)?;
    let plan = plan(runtime, base, &head, source, "delta")?;
    if !plan.conflicts.is_empty() {
        return Err(RedDBError::InvalidOperation(
            "history change conflicts with HEAD; data and HEAD are unchanged".into(),
        ));
    }
    let snapshot_manager = runtime.snapshot_manager();
    let mut snapshot = capture_commit_snapshot(&snapshot_manager);
    let parents = vec![head.clone()];
    let timestamp_ms = now_ms();
    let commit = Commit {
        hash: compute_commit_hash(snapshot.xid, &parents, &author, &message, timestamp_ms),
        root_xid: snapshot.xid,
        height: load_commit(&store, &head).expect("resolved head").height + 1,
        parents,
        author: author.clone(),
        committer: author,
        message,
        timestamp_ms,
        signature: None,
    };
    let result = publish_commit(runtime, connection, &branch, &commit, &snapshot, Some(plan));
    result?;
    snapshot.retained = true;
    Ok(commit)
}
