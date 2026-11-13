//! Snapshot-local probabilistic states. The shared caches remain authoritative
//! only for collections that have not opted into history.
use super::*;
use crate::catalog::CollectionModel;
use crate::runtime::probabilistic_store::ProbabilisticStore;
use crate::storage::primitives::{
    count_min_sketch::CountMinSketch, cuckoo_filter::CuckooFilter, hyperloglog::HyperLogLog,
};
use crate::storage::RowData;

fn state_bytes(state: &ProbabilisticStore, name: &str) -> Option<(u8, Vec<u8>)> {
    if let Some(value) = state.hlls.read().get(name) {
        return Some((PROB_WAL_KIND_HLL, value.as_bytes().to_vec()));
    }
    if let Some(value) = state.sketches.read().get(name) {
        return Some((PROB_WAL_KIND_SKETCH, value.as_bytes()));
    }
    state
        .filters
        .read()
        .get(name)
        .map(|value| (PROB_WAL_KIND_FILTER, value.as_bytes()))
}

fn load_bytes(state: &ProbabilisticStore, name: &str, kind: u8, bytes: Vec<u8>) -> RedDBResult<()> {
    let invalid = || RedDBError::Internal(format!("invalid probabilistic state for `{name}`"));
    match kind {
        PROB_WAL_KIND_HLL => {
            state.hlls.write().insert(
                name.into(),
                HyperLogLog::from_bytes(bytes).ok_or_else(invalid)?,
            );
        }
        PROB_WAL_KIND_SKETCH => {
            state.sketches.write().insert(
                name.into(),
                CountMinSketch::from_bytes(&bytes).ok_or_else(invalid)?,
            );
        }
        PROB_WAL_KIND_FILTER => {
            state.filters.write().insert(
                name.into(),
                CuckooFilter::from_bytes(&bytes).ok_or_else(invalid)?,
            );
        }
        _ => return Err(invalid()),
    }
    Ok(())
}

fn state_entity(name: &str, kind: u8, bytes: Vec<u8>) -> UnifiedEntity {
    let mut row = RowData::new(Vec::new());
    row.named = Some(HashMap::from([
        ("state".into(), Value::Blob(bytes)),
        ("state_kind".into(), Value::UnsignedInteger(kind.into())),
    ]));
    UnifiedEntity::new(
        EntityId::new(0),
        EntityKind::TableRow {
            table: Arc::from(name),
            row_id: 0,
        },
        EntityData::Row(row),
    )
}

impl RedDBRuntime {
    pub(crate) fn vcs_initial_probabilistic_state(
        &self,
        name: &str,
    ) -> RedDBResult<Option<UnifiedEntity>> {
        if !self.db().collection_contract(name).is_some_and(|contract| {
            matches!(
                contract.declared_model,
                CollectionModel::Hll | CollectionModel::Sketch | CollectionModel::Filter
            )
        }) {
            return Ok(None);
        }
        let (kind, bytes) = state_bytes(&self.inner.probabilistic, name)
            .ok_or_else(|| RedDBError::NotFound(format!("probabilistic state `{name}`")))?;
        let mut entity = state_entity(name, kind, bytes);
        entity.set_branch_origin(0);
        Ok(Some(entity))
    }

    fn vcs_probabilistic_entity(&self, name: &str) -> RedDBResult<UnifiedEntity> {
        let (context, _retention) = self.native_read_snapshot()?;
        let store = self.db().store();
        let manager = store
            .get_collection(name)
            .ok_or_else(|| RedDBError::NotFound(name.into()))?;
        let mut found = None;
        let mut duplicate = false;
        super::super::function_budget::try_scan(
            |visit| manager.try_for_each_entity(usize::MAX, visit),
            |entity| {
                if super::super::impl_core::entity_visible_with_context(Some(&context), entity) {
                    duplicate = found.replace(entity.clone()).is_some();
                }
                !duplicate
            },
        )?;
        if duplicate {
            return Err(RedDBError::Internal(format!(
                "multiple visible probabilistic states in `{name}`"
            )));
        }
        found.ok_or_else(|| RedDBError::NotFound(format!("probabilistic state `{name}`")))
    }

    pub(super) fn probabilistic_view(&self, names: &[&str]) -> RedDBResult<ProbabilisticStore> {
        let state = ProbabilisticStore::new();
        for &name in names {
            if self.vcs_is_versioned(name)? {
                let entity = self.vcs_probabilistic_entity(name)?;
                let row = entity
                    .data
                    .as_row()
                    .ok_or_else(|| RedDBError::Internal("invalid state entity".into()))?;
                let Some(Value::Blob(bytes)) = row.get_field("state") else {
                    return Err(RedDBError::Internal(
                        "missing probabilistic state bytes".into(),
                    ));
                };
                let Some(Value::UnsignedInteger(kind)) = row.get_field("state_kind") else {
                    return Err(RedDBError::Internal(
                        "missing probabilistic state kind".into(),
                    ));
                };
                load_bytes(
                    &state,
                    name,
                    u8::try_from(*kind)
                        .map_err(|_| RedDBError::Internal("invalid state kind".into()))?,
                    bytes.clone(),
                )?;
            } else if let Some((kind, bytes)) = state_bytes(&self.inner.probabilistic, name) {
                load_bytes(&state, name, kind, bytes)?;
            }
        }
        Ok(state)
    }

    pub(super) fn execute_versioned_probabilistic(
        &self,
        raw_query: &str,
        cmd: &ProbabilisticCommand,
    ) -> RedDBResult<Option<RuntimeQueryResult>> {
        use ProbabilisticCommand::*;
        let (names, target, ddl): (Vec<&str>, Option<&str>, bool) = match cmd {
            HllCount { names } => (names.iter().map(String::as_str).collect(), None, false),
            HllMerge { dest, sources } | SketchMerge { dest, sources } => (
                std::iter::once(dest.as_str())
                    .chain(sources.iter().map(String::as_str))
                    .collect(),
                Some(dest),
                false,
            ),
            HllAdd { name, .. }
            | SketchAdd { name, .. }
            | FilterAdd { name, .. }
            | FilterDelete { name, .. } => (vec![name], Some(name), false),
            HllInfo { name }
            | SketchCount { name, .. }
            | SketchInfo { name }
            | FilterCheck { name, .. }
            | FilterCount { name }
            | FilterInfo { name } => (vec![name], None, false),
            CreateHll { name, .. }
            | CreateSketch { name, .. }
            | CreateFilter { name, .. }
            | DropHll { name, .. }
            | DropSketch { name, .. }
            | DropFilter { name, .. } => (vec![name], Some(name), true),
        };
        let mut versioned = false;
        for name in &names {
            versioned |= self.vcs_is_versioned(name)?;
        }
        if !versioned {
            return Ok(None);
        }
        if ddl {
            return Err(RedDBError::InvalidOperation(
                "cannot replace or drop a versioned probabilistic collection".into(),
            ));
        }
        let (snapshot, _retention) = match target {
            Some(target) => self.vcs_state_write_snapshot(target)?,
            None => self.native_read_snapshot()?,
        };
        let _snapshot = super::super::impl_core::CurrentSnapshotGuard::install(snapshot);
        let state = self.probabilistic_view(&names)?;
        let target_versioned = target
            .map(|name| self.vcs_is_versioned(name))
            .transpose()?
            .unwrap_or(false);
        if !target_versioned {
            let result =
                self.execute_probabilistic_command_in_store(raw_query, cmd, &state, true)?;
            if let Some(target) = target {
                let (kind, bytes) =
                    state_bytes(&state, target).expect("successful mutation has a state");
                load_bytes(&self.inner.probabilistic, target, kind, bytes)?;
                self.invalidate_result_cache();
            }
            return Ok(Some(result));
        }
        let target = target.expect("versioned mutation target");
        let previous = self.vcs_probabilistic_entity(target)?;
        self.with_versioned_graph_insert(|| {
            let result =
                self.execute_probabilistic_command_in_store(raw_query, cmd, &state, false)?;
            let (kind, bytes) =
                state_bytes(&state, target).expect("successful mutation has a state");
            self.insert_vcs_state_version(
                target,
                state_entity(target, kind, bytes),
                Some(previous.id),
            )?;
            self.invalidate_result_cache();
            Ok(Some(result))
        })
    }
}
