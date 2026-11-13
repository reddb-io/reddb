//! Branch epochs and frozen ancestor snapshots. Data versions carry epochs;
//! branch names and HEAD movement never change an existing epoch's identity.
use super::*;
use reddb_engine::transaction::read::BranchReadView;
use std::collections::BTreeMap;

const VIEWS: &str = "red_vcs_branch_views";

pub(super) fn encode_epochs(view: &BranchReadView) -> Value {
    Value::Array(
        view.epochs
            .iter()
            .map(|(&epoch, snapshot)| {
                let mut pending: Vec<_> = snapshot.in_progress.iter().copied().collect();
                pending.sort_unstable();
                Value::Array(vec![
                    Value::UnsignedInteger(epoch),
                    Value::UnsignedInteger(snapshot.xid),
                    Value::Array(pending.into_iter().map(Value::UnsignedInteger).collect()),
                ])
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn author() -> Author {
        Author {
            name: "test".into(),
            email: "test@reddb.io".into(),
        }
    }

    fn commit(runtime: &RedDBRuntime, connection: u64) -> Commit {
        runtime
            .vcs_commit(CreateCommitInput {
                connection_id: connection,
                message: "test".into(),
                author: author(),
                committer: None,
                amend: false,
                allow_empty: true,
            })
            .expect("commit")
    }

    #[test]
    fn failed_opt_in_restores_rows_and_setting() {
        for create in ["CREATE HLL items", "CREATE TABLE items (value INT)"] {
            let runtime = RedDBRuntime::in_memory().expect("runtime");
            runtime.execute_query(create).expect("collection");
            if create.contains("TABLE") {
                runtime
                    .execute_query("INSERT INTO items (value) VALUES (1)")
                    .expect("legacy row");
            }
            let store = runtime.db().store();
            store
                .test_fail_deferred_wal_append
                .store(true, std::sync::atomic::Ordering::Release);
            assert!(runtime.vcs_set_versioned("items", true).is_err());
            assert!(!runtime
                .vcs_is_versioned("items")
                .expect("unchanged setting"));
            let manager = store.get_collection("items").unwrap();
            manager
                .try_for_each_entity(usize::MAX, |entity| {
                    assert!(entity.branch_version().is_none());
                    true
                })
                .expect("restored provenance");
            runtime.vcs_set_versioned("items", true).expect("retry");
            assert_eq!(
                runtime
                    .scan_collection("items", None, 10)
                    .expect("single state")
                    .total,
                1
            );
        }
    }

    #[test]
    fn failed_state_write_preserves_visible_and_recovered_history() {
        for (create, seed, mutation, query, field) in [
            (
                "CREATE CONFIG items",
                "PUT CONFIG items key = 1",
                "PUT CONFIG items key = 2",
                "GET CONFIG items key",
                "value",
            ),
            (
                "CREATE HLL items",
                "HLL ADD items 'base'",
                "HLL ADD items 'failed'",
                "HLL COUNT items",
                "count",
            ),
            (
                "CREATE SKETCH items",
                "SKETCH ADD items 'base' 1",
                "SKETCH ADD items 'base' 2",
                "SKETCH COUNT items 'base'",
                "estimate",
            ),
            (
                "CREATE FILTER items",
                "FILTER ADD items 'base'",
                "FILTER DELETE items 'base'",
                "FILTER CHECK items 'base'",
                "exists",
            ),
        ] {
            let directory = tempfile::tempdir().expect("directory");
            let path = directory.path().join("state.rdb");
            let options = || crate::RedDBOptions::persistent(&path);
            let before;
            {
                let runtime = RedDBRuntime::with_options(options()).expect("runtime");
                runtime.execute_query(create).expect("create");
                runtime.vcs_set_versioned("items", true).expect("versioned");
                runtime.execute_query(seed).expect("seed");
                before = runtime.execute_query(query).unwrap().result.records[0]
                    .get(field)
                    .unwrap()
                    .clone();
                runtime
                    .db()
                    .store()
                    .test_fail_deferred_wal_append
                    .store(true, std::sync::atomic::Ordering::Release);
                assert!(runtime.execute_query(mutation).is_err(), "{mutation}");
                assert_eq!(
                    runtime.execute_query(query).unwrap().result.records[0].get(field),
                    Some(&before)
                );
                runtime.checkpoint().expect("checkpoint");
            }
            let runtime = RedDBRuntime::with_options(options()).expect("reopen");
            assert_eq!(
                runtime.execute_query(query).unwrap().result.records[0].get(field),
                Some(&before)
            );
        }
    }

    #[test]
    fn failed_head_publication_restores_branch_view_and_reference() {
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        let connection = 8401;
        crate::runtime::execution_context::set_current_connection_id(connection);
        runtime
            .execute_query("CREATE TABLE items (value INT)")
            .expect("table");
        runtime.vcs_set_versioned("items", true).expect("versioned");
        let base = commit(&runtime, connection);
        runtime
            .vcs_branch_create(CreateBranchInput {
                name: "agent".into(),
                from: None,
                connection_id: connection,
            })
            .expect("branch");
        let checkout = |name: &str| {
            runtime
                .vcs_checkout(CheckoutInput {
                    connection_id: connection,
                    target: CheckoutTarget::Branch(name.into()),
                    force: false,
                })
                .expect("checkout")
        };
        checkout("agent");
        runtime
            .execute_query("INSERT INTO items (value) VALUES (7)")
            .expect("source write");
        let source = commit(&runtime, connection);
        checkout("main");
        runtime
            .db()
            .store()
            .test_fail_deferred_wal_append
            .store(true, std::sync::atomic::Ordering::Release);
        let merge = || {
            runtime.vcs_merge(MergeInput {
                connection_id: connection,
                from: "agent".into(),
                opts: Default::default(),
                author: author(),
            })
        };
        assert!(merge().is_err());
        assert_eq!(
            runtime
                .vcs_status(StatusInput {
                    connection_id: connection
                })
                .expect("status")
                .head_commit,
            Some(base.hash)
        );
        assert_eq!(
            runtime
                .scan_collection("items", None, 10)
                .expect("unchanged rows")
                .total,
            0
        );
        assert!(merge().expect("retry").fast_forward);
        assert_eq!(
            runtime
                .vcs_status(StatusInput {
                    connection_id: connection
                })
                .expect("status")
                .head_commit,
            Some(source.hash)
        );
        assert_eq!(
            runtime
                .scan_collection("items", None, 10)
                .expect("merged rows")
                .total,
            1
        );
        crate::runtime::execution_context::clear_current_connection_id();
    }

    #[test]
    fn failed_divergent_merge_restores_versions_and_commit_metadata() {
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        let connection = 8402;
        crate::runtime::execution_context::set_current_connection_id(connection);
        runtime
            .execute_query("CREATE TABLE items (id INT, a INT, b INT)")
            .expect("table");
        runtime.vcs_set_versioned("items", true).expect("versioned");
        runtime
            .execute_query("INSERT INTO items (id,a,b) VALUES (1,1,1)")
            .expect("base");
        commit(&runtime, connection);
        runtime
            .vcs_branch_create(CreateBranchInput {
                connection_id: connection,
                name: "source".into(),
                from: None,
            })
            .expect("fork");
        let checkout = |branch: &str| {
            runtime
                .vcs_checkout(CheckoutInput {
                    connection_id: connection,
                    target: CheckoutTarget::Branch(branch.into()),
                    force: false,
                })
                .expect("checkout")
        };
        checkout("source");
        runtime
            .execute_query("UPDATE items SET a = 2 WHERE id = 1")
            .expect("source update");
        runtime
            .execute_query("INSERT INTO items (id,a,b) VALUES (2,2,2)")
            .expect("source insert");
        commit(&runtime, connection);
        checkout("main");
        runtime
            .execute_query("UPDATE items SET b = 3 WHERE id = 1")
            .expect("target update");
        let target = commit(&runtime, connection);
        let store = runtime.db().store();
        let commits_before = store.get_collection(vc::COMMITS).expect("commits").count();
        let rows_before = store.get_collection("items").expect("items").count();
        store
            .test_fail_deferred_wal_append
            .store(true, std::sync::atomic::Ordering::Release);
        let merge = || {
            runtime.vcs_merge(MergeInput {
                connection_id: connection,
                from: "source".into(),
                opts: Default::default(),
                author: author(),
            })
        };
        assert!(merge().is_err());
        assert_eq!(
            store.get_collection(vc::COMMITS).unwrap().count(),
            commits_before
        );
        assert_eq!(store.get_collection("items").unwrap().count(), rows_before);
        assert_eq!(
            runtime
                .vcs_status(StatusInput {
                    connection_id: connection
                })
                .unwrap()
                .head_commit,
            Some(target.hash)
        );
        let before = runtime
            .execute_query("SELECT a,b FROM items")
            .unwrap()
            .result
            .records;
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].get("a"), Some(&Value::Integer(1)));
        assert_eq!(before[0].get("b"), Some(&Value::Integer(3)));
        assert!(merge().expect("retry").conflicts.is_empty());
        let after = runtime
            .execute_query("SELECT a,b FROM items WHERE id = 1")
            .unwrap()
            .result
            .records;
        assert_eq!(after[0].get("a"), Some(&Value::Integer(2)));
        assert_eq!(after[0].get("b"), Some(&Value::Integer(3)));
        assert_eq!(runtime.scan_collection("items", None, 10).unwrap().total, 2);
        crate::runtime::execution_context::clear_current_connection_id();
    }

    #[test]
    fn failed_versioned_insert_callback_does_not_commit_partial_data() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("aborted-insert.rdb");
        crate::runtime::execution_context::set_current_connection_id(8403);
        {
            let runtime = RedDBRuntime::with_options(crate::RedDBOptions::persistent(&path))
                .expect("runtime");
            runtime.execute_query("CREATE GRAPH items").expect("graph");
            runtime.vcs_set_versioned("items", true).expect("versioned");
            let result: RedDBResult<()> = runtime.with_versioned_graph_insert(|| {
                runtime.execute_query(
                    "INSERT INTO items NODE (label,node_type) VALUES ('partial','Person')",
                )?;
                Err(RedDBError::Query("injected callback failure".into()))
            });
            assert!(result.is_err());
            assert_eq!(
                runtime
                    .scan_collection("items", None, 10)
                    .expect("live view")
                    .total,
                0
            );
            runtime.checkpoint().expect("checkpoint");
        }
        let runtime =
            RedDBRuntime::with_options(crate::RedDBOptions::persistent(&path)).expect("reopen");
        assert_eq!(
            runtime
                .scan_collection("items", None, 10)
                .expect("recovered view")
                .total,
            0
        );
        crate::runtime::execution_context::clear_current_connection_id();
    }
}

fn decode_epochs(value: &Value) -> RedDBResult<BTreeMap<u64, Snapshot>> {
    let invalid = || RedDBError::Internal("vcs: invalid branch lineage".into());
    let Value::Array(entries) = value else {
        return Err(invalid());
    };
    let mut epochs = BTreeMap::new();
    for entry in entries {
        let Value::Array(fields) = entry else {
            return Err(invalid());
        };
        let [Value::UnsignedInteger(epoch), Value::UnsignedInteger(xid), Value::Array(pending)] =
            fields.as_slice()
        else {
            return Err(invalid());
        };
        let mut in_progress = HashSet::new();
        for item in pending {
            let Value::UnsignedInteger(pending) = item else {
                return Err(invalid());
            };
            if *pending == 0 || *pending > *xid {
                return Err(invalid());
            }
            in_progress.insert(*pending);
        }
        if epochs
            .insert(
                *epoch,
                Snapshot {
                    xid: *xid,
                    in_progress,
                },
            )
            .is_some()
        {
            return Err(invalid());
        }
    }
    Ok(epochs)
}

pub(super) fn commit_view(store: &UnifiedStore, hash: &str) -> RedDBResult<BranchReadView> {
    let entity = load_commit_entity(store, hash)
        .ok_or_else(|| RedDBError::NotFound(format!("commit `{hash}` not found")))?;
    let row = entity
        .data
        .as_row()
        .ok_or_else(|| RedDBError::Internal("vcs: invalid commit".into()))?;
    let epochs = match row.get_field("branch_lineage") {
        Some(value) => decode_epochs(value)?,
        None => BTreeMap::from([(0, commit_snapshot_from_row(row)?)]),
    };
    Ok(BranchReadView {
        writer_epoch: row_u64(row, "writer_epoch").unwrap_or(0),
        epochs,
    })
}

pub(super) fn load_view(store: &UnifiedStore, branch: &str) -> RedDBResult<Option<BranchReadView>> {
    let Some(manager) = store.get_collection(VIEWS) else {
        return Ok(None);
    };
    let mut result = None;
    let mut failure = None;
    manager.try_for_each_entity(usize::MAX, |entity| {
        let Some(row) = entity.data.as_row() else {
            return true;
        };
        if row_text(row, "branch").as_deref() != Some(branch) {
            return true;
        }
        let decoded = row
            .get_field("ancestors")
            .ok_or_else(|| RedDBError::Internal("vcs: missing branch ancestors".into()))
            .and_then(decode_epochs);
        match decoded {
            Ok(epochs) => {
                result = Some(BranchReadView {
                    writer_epoch: row_u64(row, "writer_epoch").unwrap_or(0),
                    epochs,
                })
            }
            Err(error) => failure = Some(error),
        }
        false
    })?;
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(result)
}

pub(super) fn create_view(store: &UnifiedStore, branch: &str, from: &str) -> RedDBResult<()> {
    let mut view = if from.is_empty() {
        BranchReadView {
            writer_epoch: 0,
            epochs: BTreeMap::new(),
        }
    } else {
        commit_view(store, from)?
    };
    view.writer_epoch = store.next_entity_id().raw();
    let fields = HashMap::from([
        ("branch".into(), Value::text(branch)),
        (
            "writer_epoch".into(),
            Value::UnsignedInteger(view.writer_epoch),
        ),
        ("ancestors".into(), encode_epochs(&view)),
    ]);
    insert_meta_row(store, VIEWS, fields)?;
    Ok(())
}

pub(super) fn delete_view(store: &UnifiedStore, branch: &str) -> RedDBResult<()> {
    if let Some(manager) = store.get_collection(VIEWS) {
        let mut ids = Vec::new();
        manager.try_for_each_entity(usize::MAX, |entity| {
            if entity
                .data
                .as_row()
                .is_some_and(|row| row_text(row, "branch").as_deref() == Some(branch))
            {
                ids.push(entity.id);
            }
            true
        })?;
        store
            .delete_batch(VIEWS, &ids)
            .map_err(|error| RedDBError::Internal(error.to_string()))?;
    }
    Ok(())
}

pub(super) fn stage_metadata(
    store: &UnifiedStore,
    publication: &mut crate::storage::unified::store::RowPublication,
    collection: &str,
    key_field: &str,
    key: &str,
    fields: HashMap<String, Value>,
) -> RedDBResult<()> {
    let manager = store.get_or_create_collection(collection);
    manager.try_for_each_entity(usize::MAX, |entity| {
        if entity
            .data
            .as_row()
            .is_some_and(|row| row_text(row, key_field).as_deref() == Some(key))
        {
            publication.deletes.push((collection.into(), entity.id));
        }
        true
    })?;
    let entity = UnifiedEntity::new(
        store.next_entity_id(),
        EntityKind::TableRow {
            table: Arc::from(collection),
            row_id: 0,
        },
        EntityData::Row(RowData {
            columns: Vec::new(),
            named: Some(fields),
            schema: None,
        }),
    );
    publication.inserts.push((collection.into(), entity, None));
    Ok(())
}

/// Publish the data view and HEAD in one WAL batch. The caller holds the same
/// publication fence used by readers when capturing a branch view.
pub(super) fn publish_head(
    store: &UnifiedStore,
    connection: u64,
    branch: &str,
    commit: &Commit,
    mut view: BranchReadView,
) -> RedDBResult<()> {
    let mut publication = crate::storage::unified::store::RowPublication::default();
    // Later writes on the source cannot enter the adopted branch.
    view.writer_epoch = store.next_entity_id().raw();
    stage_head(
        store,
        &mut publication,
        connection,
        branch,
        commit,
        Some(&view),
    )?;
    store.publish_rows(publication)?;
    Ok(())
}

pub(super) fn stage_head(
    store: &UnifiedStore,
    publication: &mut crate::storage::unified::store::RowPublication,
    connection: u64,
    branch: &str,
    commit: &Commit,
    view: Option<&BranchReadView>,
) -> RedDBResult<()> {
    if let Some(view) = view {
        stage_metadata(
            store,
            publication,
            VIEWS,
            "branch",
            branch,
            HashMap::from([
                ("branch".into(), Value::text(branch)),
                (
                    "writer_epoch".into(),
                    Value::UnsignedInteger(view.writer_epoch),
                ),
                ("ancestors".into(), encode_epochs(&view)),
            ]),
        )?;
    }
    stage_metadata(
        store,
        publication,
        vc::REFS,
        "id",
        branch,
        HashMap::from([
            ("id".into(), Value::text(branch)),
            ("type".into(), Value::text("branch")),
            ("target".into(), Value::text(commit.hash.as_str())),
            (
                "protected".into(),
                Value::Boolean(
                    load_ref(store, branch).is_some_and(|reference| reference.protected),
                ),
            ),
        ]),
    )?;
    let connection = connection.to_string();
    stage_metadata(
        store,
        publication,
        vc::WORKSETS,
        "id",
        &connection,
        HashMap::from([
            ("id".into(), Value::text(connection.as_str())),
            ("branch".into(), Value::text(branch)),
            ("base_commit".into(), Value::text(commit.hash.as_str())),
            (
                "working_xid".into(),
                Value::UnsignedInteger(commit.root_xid),
            ),
        ]),
    )?;
    Ok(())
}

pub(super) fn ensure_clean(runtime: &RedDBRuntime, connection: u64, head: &str) -> RedDBResult<()> {
    let snapshot = runtime.snapshot_manager().fresh_read_snapshot();
    if !snapshot.in_progress.is_empty() {
        return Err(RedDBError::InvalidOperation(
            "finish active transactions before moving branch history".into(),
        ));
    }
    let mut live = runtime.vcs_commit_read_context(head)?;
    live.snapshot = snapshot.clone();
    live.branch = Some(Arc::new(live_view(runtime, connection, &snapshot)?));
    let committed = runtime.vcs_commit_read_context(head)?;
    let store = runtime.db().store();
    for collection in versioned_collections(&store)? {
        let Some(manager) = store.get_collection(&collection) else {
            continue;
        };
        let mut dirty = false;
        manager.try_for_each_entity(usize::MAX, |entity| {
            dirty = snapshot_sees_entity(&live, entity) != snapshot_sees_entity(&committed, entity);
            !dirty
        })?;
        if dirty {
            return Err(RedDBError::InvalidOperation(format!(
                "commit changes in `{collection}` before merging"
            )));
        }
    }
    Ok(())
}

pub(super) fn ensure_entity_history_supported(runtime: &RedDBRuntime) -> RedDBResult<()> {
    let store = runtime.db().store();
    for collection in versioned_collections(&store)? {
        if runtime
            .db()
            .collection_contract(&collection)
            .is_some_and(|contract| {
                !matches!(
                    contract.declared_model,
                    crate::catalog::CollectionModel::Table
                        | crate::catalog::CollectionModel::Document
                        | crate::catalog::CollectionModel::Graph
                        | crate::catalog::CollectionModel::Vector
                        | crate::catalog::CollectionModel::Kv
                        | crate::catalog::CollectionModel::Mixed
                        | crate::catalog::CollectionModel::TimeSeries
                        | crate::catalog::CollectionModel::Config
                        | crate::catalog::CollectionModel::Hll
                        | crate::catalog::CollectionModel::Sketch
                        | crate::catalog::CollectionModel::Filter
                        | crate::catalog::CollectionModel::Metrics
                )
            })
        {
            return Err(RedDBError::InvalidOperation(format!(
                "applying data for `{collection}` is not implemented"
            )));
        }
    }
    Ok(())
}

pub(super) fn live_view(
    runtime: &RedDBRuntime,
    connection: u64,
    snapshot: &Snapshot,
) -> RedDBResult<BranchReadView> {
    let store = runtime.db().store();
    let (branch, head) =
        load_workset(&store, connection).unwrap_or_else(|| (vc::DEFAULT_BRANCH_REF.into(), None));
    if !branch.starts_with(vc::BRANCH_REF_PREFIX) {
        return commit_view(&store, head.as_deref().unwrap_or(""));
    }
    let mut view = match load_view(&store, &branch)? {
        Some(view) => view,
        None if branch == vc::DEFAULT_BRANCH_REF => BranchReadView {
            writer_epoch: 0,
            epochs: BTreeMap::new(),
        },
        None => {
            return Err(RedDBError::InvalidOperation(
                "legacy branch must be checked out before access".into(),
            ))
        }
    };
    view.epochs.insert(view.writer_epoch, snapshot.clone());
    Ok(view)
}

impl RedDBRuntime {
    pub(super) fn vcs_commit_read_context(
        &self,
        hash: &str,
    ) -> RedDBResult<reddb_engine::transaction::read::SnapshotContext> {
        let snapshot = self.vcs_resolve_snapshot(AsOfSpec::Commit(hash.into()))?;
        Ok(reddb_engine::transaction::read::SnapshotContext {
            snapshot,
            branch: Some(Arc::new(commit_view(&self.db().store(), hash)?)),
            manager: self.snapshot_manager(),
            own_xids: HashSet::new(),
            requires_index_fallback: true,
            serializable_reader: None,
        })
    }

    pub(crate) fn vcs_branch_read_view(
        &self,
        snapshot: &Snapshot,
        as_of: Option<&AsOfSpec>,
    ) -> RedDBResult<Option<Arc<BranchReadView>>> {
        let store = self.db().store();
        let _publication = store.vault_publication_lock.lock();
        if versioned_collections(&store)?.is_empty() {
            return Ok(None);
        }
        let commit = match as_of {
            Some(AsOfSpec::Commit(hash)) => Some(self.vcs_resolve_commitish(hash)?),
            Some(AsOfSpec::Branch(branch)) => {
                load_ref(&store, &normalize_branch_name(branch)).map(|reference| reference.target)
            }
            Some(AsOfSpec::Tag(tag)) => {
                load_ref(&store, &normalize_tag_name(tag)).map(|reference| reference.target)
            }
            Some(AsOfSpec::TimestampMs(_)) => {
                let mut found = None;
                if let Some(commits) = store.get_collection(vc::COMMITS) {
                    commits.try_for_each_entity(usize::MAX, |entity| {
                        if let Some(row) = entity.data.as_row() {
                            if row_u64(row, "root_xid") == Some(snapshot.xid) {
                                found = row_text(row, "id");
                            }
                        }
                        true
                    })?;
                }
                found
            }
            _ => None,
        };
        let mut view = match commit {
            Some(hash) => commit_view(&store, &hash)?,
            None => live_view(
                self,
                super::super::execution_context::current_connection_id(),
                snapshot,
            )?,
        };
        if matches!(as_of, Some(AsOfSpec::Snapshot(_))) {
            for inherited in view.epochs.values_mut() {
                if inherited.xid > snapshot.xid {
                    *inherited = snapshot.clone();
                }
            }
        }
        Ok(Some(Arc::new(view)))
    }

    pub(crate) fn vcs_writer_epoch(&self, collection: &str) -> RedDBResult<Option<u64>> {
        let store = self.db().store();
        let _publication = store.vault_publication_lock.lock();
        if !is_versioned(&store, collection)? {
            return Ok(None);
        }
        let connection = super::super::execution_context::current_connection_id();
        ensure_no_pending_merge(&store, connection)?;
        let (branch, _) = load_workset(&store, connection)
            .unwrap_or_else(|| (vc::DEFAULT_BRANCH_REF.into(), None));
        if !branch.starts_with(vc::BRANCH_REF_PREFIX) {
            return Err(RedDBError::ReadOnly(
                "versioned writes require a branch checkout".into(),
            ));
        }
        let epoch = match load_view(&store, &branch)? {
            Some(view) => Ok(Some(view.writer_epoch)),
            None if branch == vc::DEFAULT_BRANCH_REF => Ok(Some(0)),
            None => Err(RedDBError::InvalidOperation(
                "branch has no writer epoch".into(),
            )),
        }?;
        if let Some(context) = super::super::execution_context::capture_current_snapshot() {
            if Arc::ptr_eq(&context.manager, &self.snapshot_manager())
                && context
                    .branch
                    .as_ref()
                    .is_some_and(|view| Some(view.writer_epoch) != epoch)
            {
                return Err(RedDBError::InvalidOperation(
                    "branch changed during statement; retry the statement".into(),
                ));
            }
        }
        Ok(epoch)
    }

    pub(crate) fn stamp_vcs_creation(
        &self,
        collection: &str,
        entity: &mut UnifiedEntity,
    ) -> RedDBResult<()> {
        if let Some(epoch) = self.vcs_writer_epoch(collection)? {
            entity.set_branch_origin(epoch);
        }
        Ok(())
    }
}
