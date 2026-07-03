//! VCS / versioning `red.*` snapshot builders.
//!
//! Extracted from the `red_schema` dispatcher (issue #1638). Serves
//! `red.commits`, `red.branches`, `red.tags`, `red.status`,
//! `red.conflicts`, and `red.versioned`.

use super::helpers::*;
use super::*;

fn isolation_level_name(level: crate::storage::transaction::IsolationLevel) -> &'static str {
    match level {
        crate::storage::transaction::IsolationLevel::ReadUncommitted => "read_uncommitted",
        crate::storage::transaction::IsolationLevel::ReadCommitted => "read_committed",
        crate::storage::transaction::IsolationLevel::SnapshotIsolation => "snapshot_isolation",
        crate::storage::transaction::IsolationLevel::Serializable => "serializable",
    }
}

pub(super) fn commits_snapshot(
    runtime: &RedDBRuntime,
    query: &TableQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let schema = Arc::new(
        COMMIT_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let hash = hash_filter(query);
    let range = if let Some(hash) = hash.clone() {
        crate::application::vcs::LogRange {
            to: Some(hash),
            limit: Some(1),
            ..Default::default()
        }
    } else {
        crate::application::vcs::LogRange::default()
    };
    Ok(runtime
        .vcs_log(crate::application::vcs::LogInput {
            connection_id: if hash.is_some() {
                0
            } else {
                current_connection_id()
            },
            range,
        })?
        .into_iter()
        .map(|commit| {
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(commit.hash),
                    Value::UnsignedInteger(commit.root_xid),
                    Value::Array(commit.parents.into_iter().map(Value::text).collect()),
                    Value::UnsignedInteger(commit.height),
                    Value::text(commit.author.name),
                    Value::text(commit.author.email),
                    Value::text(commit.committer.name),
                    Value::text(commit.committer.email),
                    Value::text(commit.message),
                    Value::TimestampMs(commit.timestamp_ms),
                    commit.signature.map(Value::text).unwrap_or(Value::Null),
                ],
            )
        })
        .collect())
}

pub(super) fn hash_filter(query: &TableQuery) -> Option<String> {
    fn visit(filter: &Filter) -> Option<String> {
        match filter {
            Filter::Compare {
                field: FieldRef::TableColumn { column, .. },
                op: CompareOp::Eq,
                value: Value::Text(hash),
            } if column == "hash" => Some(hash.to_string()),
            Filter::And(left, right) => visit(left).or_else(|| visit(right)),
            _ => None,
        }
    }
    effective_table_filter(query).and_then(|filter| visit(&filter))
}

pub(super) fn refs_snapshot(
    runtime: &RedDBRuntime,
    prefix: Option<&str>,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let schema = Arc::new(
        REF_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    Ok(runtime
        .vcs_list_refs(prefix)?
        .into_iter()
        .map(|reference| {
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(reference.name),
                    Value::text(ref_kind_name(reference.kind)),
                    Value::text(reference.target),
                    Value::Boolean(reference.protected),
                ],
            )
        })
        .collect())
}

pub(super) fn status_snapshot(runtime: &RedDBRuntime) -> RedDBResult<Vec<UnifiedRecord>> {
    let schema = Arc::new(
        STATUS_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let connection_id = current_connection_id();
    let status = runtime.vcs_status(crate::application::vcs::StatusInput { connection_id })?;
    let isolation = runtime
        .inner
        .tx_contexts
        .read()
        .get(&connection_id)
        .map(|ctx| ctx.isolation);
    Ok(vec![UnifiedRecord::with_schema(
        schema,
        vec![
            Value::UnsignedInteger(status.connection_id),
            status.head_ref.map(Value::text).unwrap_or(Value::Null),
            status.head_commit.map(Value::text).unwrap_or(Value::Null),
            Value::Boolean(status.detached),
            Value::UnsignedInteger(status.staged_changes as u64),
            Value::UnsignedInteger(status.working_changes as u64),
            Value::UnsignedInteger(status.unresolved_conflicts as u64),
            status
                .merge_state_id
                .map(Value::text)
                .unwrap_or(Value::Null),
            isolation
                .map(isolation_level_name)
                .map(Value::text)
                .unwrap_or(Value::Null),
            isolation
                .map(isolation_level_name)
                .map(Value::text)
                .unwrap_or(Value::Null),
        ],
    )])
}

pub(super) fn conflicts_snapshot(runtime: &RedDBRuntime) -> RedDBResult<Vec<UnifiedRecord>> {
    let schema = Arc::new(
        CONFLICT_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let status = runtime.vcs_status(crate::application::vcs::StatusInput {
        connection_id: current_connection_id(),
    })?;
    let Some(merge_state_id) = status.merge_state_id else {
        return Ok(Vec::new());
    };
    Ok(runtime
        .vcs_conflicts_list(&merge_state_id)?
        .into_iter()
        .map(|conflict| {
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(conflict.id),
                    Value::text(conflict.collection),
                    Value::text(conflict.entity_id),
                    json_value(conflict.base),
                    json_value(conflict.ours),
                    json_value(conflict.theirs),
                    Value::Array(
                        conflict
                            .conflicting_paths
                            .into_iter()
                            .map(Value::text)
                            .collect(),
                    ),
                    Value::text(conflict.merge_state_id),
                ],
            )
        })
        .collect())
}

pub(super) fn versioned_snapshot(
    runtime: &RedDBRuntime,
    visible_collections: Option<&HashSet<String>>,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let schema = Arc::new(
        VERSIONED_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    Ok(runtime
        .vcs_list_versioned()?
        .into_iter()
        .filter(|collection| collection_is_visible(collection, visible_collections))
        .map(|collection| {
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![Value::text(collection), Value::Boolean(true)],
            )
        })
        .collect())
}
