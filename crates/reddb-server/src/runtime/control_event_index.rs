//! Generation-bound newest-first ledger postings. Source rows remain authoritative.
use super::control_events::CONTROL_EVENTS_COLLECTION;
use super::RedDBRuntime;
use crate::storage::unified::store::secondary_index::{
    SecondaryIndexIdentity, SecondaryIndexProjector, SECONDARY_KEY_BYTES_MAX,
};
use crate::storage::{SegmentManager, StoreError, UnifiedEntity};
use crate::{RedDBError, RedDBResult};
use reddb_rql::ast::{FieldRef, TableQuery};
use reddb_types::Value;
use sha2::{Digest, Sha256};
use std::{ops::Bound, sync::Arc};

const NAME: &str = "\0reddb/control-events-desc-v1";

fn identity() -> SecondaryIndexIdentity {
    SecondaryIndexIdentity {
        codec_version: 1,
        definition: Sha256::digest(b"control-events-v1:integer-ts-desc:text-id-36-bytes-desc")
            .into(),
    }
}

struct Projection;
impl SecondaryIndexProjector for Projection {
    fn project(
        &self,
        entity: &UnifiedEntity,
        key: &mut [u8; SECONDARY_KEY_BYTES_MAX],
    ) -> Result<Option<usize>, StoreError> {
        let Some(row) = entity.data.as_row() else {
            return Ok(None);
        };
        let (Some(Value::Integer(ts)), Some(Value::Text(id))) =
            (row.get_field("ts"), row.get_field("id"))
        else {
            key[0] = 0;
            return Ok(Some(1));
        };
        if id.len() != 36 {
            // Keep an explicit fallback marker: an incomplete index must never
            // omit legacy/unsupported rows from an otherwise successful query.
            key[0] = 0;
            return Ok(Some(1));
        }
        key[0] = 1;
        let time = (!((*ts as u64) ^ (1u64 << 63))).to_be_bytes();
        key[1..9].copy_from_slice(&time);
        for (target, byte) in key[9..45].iter_mut().zip(id.as_bytes()) {
            *target = !byte;
        }
        Ok(Some(45))
    }
}

pub(super) fn prepare(runtime: &RedDBRuntime) -> RedDBResult<()> {
    let store = runtime.db().store();
    if store.pager().is_some() && store.get_collection(CONTROL_EVENTS_COLLECTION).is_some() {
        store.register_secondary_index(
            CONTROL_EVENTS_COLLECTION,
            NAME,
            identity(),
            Arc::new(Projection),
        )?;
    }
    Ok(())
}

fn eligible(query: &TableQuery) -> bool {
    if query.limit.is_none() || query.order_by.len() != 2 {
        return false;
    }
    query
        .order_by
        .iter()
        .zip(["ts", "id"])
        .all(|(clause, name)| {
            if clause.ascending || clause.expr.is_some() {
                return false;
            }
            match &clause.field {
                FieldRef::TableColumn { table, column } => {
                    column == name
                        && (table.is_empty()
                            || table == &query.table
                            || query.alias.as_deref() == Some(table.as_str()))
                }
                _ => false,
            }
        })
}

/// Visit all visible overlay rows, then only the best qualifying base window.
/// The caller applies scope/filter and returns whether the row matched. Its
/// final bounded sort merges the two sets and applies OFFSET/LIMIT once.
/// Returns false before visiting anything when a complete index is unavailable.
pub(super) fn try_visit_ordered(
    runtime: &RedDBRuntime,
    query: &TableQuery,
    read_bytes: usize,
    mut visit: impl FnMut(&UnifiedEntity) -> RedDBResult<bool>,
) -> RedDBResult<bool> {
    if !eligible(query) {
        return Ok(false);
    }
    let store = runtime.db().store();
    // The caller has reserved scratch/output before taking publication locks.
    // Pin paired source/postings and fence overlay changes for the whole merge.
    let _publication = store.vault_publication_lock.lock();
    let Some(index) = store.secondary_index(CONTROL_EVENTS_COLLECTION, NAME, &identity()) else {
        return Ok(false);
    };
    let manager = store
        .get_collection(CONTROL_EVENTS_COLLECTION)
        .ok_or_else(|| RedDBError::Internal("audit index without source collection".into()))?;
    let mut key = [0; SECONDARY_KEY_BYTES_MAX];
    if index
        .ordered_range(Bound::Included(&[0]), Bound::Excluded(&[1]))?
        .next_posting(&mut key)?
        .is_some()
    {
        return Ok(false);
    }
    let mut supported = true;
    let mut failure = None;
    manager.scan_for_each(None, |entity| {
        let result = Projection.project(entity, &mut key);
        match result {
            Ok(Some(1)) => {
                supported = false;
                false
            }
            Err(error) => {
                failure = Some(error);
                false
            }
            _ => true,
        }
    });
    if let Some(error) = failure {
        return Err(error.into());
    }
    if !supported {
        return Ok(false);
    }
    super::function_budget::try_scan_result(
        |callback| {
            manager.scan_for_each(None, callback);
            Ok(())
        },
        |entity| {
            let required_bytes = crate::storage::unified::memory_size::entity_bytes(entity);
            if required_bytes > read_bytes {
                return Err(StoreError::ReadLimitExceeded {
                    required_bytes,
                    limit_bytes: read_bytes,
                }
                .into());
            }
            visit(entity)?;
            Ok(true)
        },
    )?;
    let keep = query
        .limit
        .unwrap_or(0)
        .saturating_add(query.offset.unwrap_or(0));
    let mut accepted = 0u64;
    let mut cursor = index.ordered_range(Bound::Included(&[1]), Bound::Excluded(&[2]))?;
    while accepted < keep {
        let Some((length, id)) = cursor.next_posting(&mut key)? else {
            break;
        };
        if manager.backing_row_is_shadowed(id) {
            continue;
        }
        let entity = manager
            .try_get(id, read_bytes)?
            .ok_or_else(|| RedDBError::Internal("audit posting has no source row".into()))?;
        if !SegmentManager::scan_entity_visible(None, &entity) {
            continue;
        }
        let mut current_key = [0; SECONDARY_KEY_BYTES_MAX];
        if Projection.project(&entity, &mut current_key)? != Some(length)
            || current_key[..length] != key[..length]
        {
            return Err(RedDBError::Internal(
                "audit posting does not match its source row".into(),
            ));
        }
        if visit(&entity)? {
            accepted += 1;
        }
    }
    Ok(true)
}
