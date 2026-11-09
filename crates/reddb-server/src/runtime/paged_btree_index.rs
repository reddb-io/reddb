//! Ordered secondary pages for scalar and fixed-arity composite BTREE indexes.

use super::index_store::{index_field_value, IndexMethodKind, IndexStore, RegisteredIndex};
use super::{RedDBRuntime, RuntimeInner};
use crate::storage::unified::hash_index::HashCandidates;
use crate::storage::unified::store::secondary_index::{
    SecondaryIndexIdentity, SecondaryIndexProjector, SECONDARY_KEY_BYTES_MAX,
};
use crate::storage::{EntityData, EntityId, EntityKind, StoreError, UnifiedEntity};
use reddb_engine::storage::ordered_secondary_key::encode_ordered_key;
use reddb_rql::ast::{CompareOp, FieldRef, Filter, OrderByClause};
use reddb_types::{value_to_canonical_key, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound::{self, Excluded, Included, Unbounded};
use std::sync::{Arc, Weak};

// Every valid family tag is below 255. An unrepresentable key retains a
// physical marker; the query then takes the existing authoritative scan.
// Never silently omit oversized or unsupported values from a complete index.
const SCAN_REQUIRED: u8 = 255;

struct BtreeProjection {
    columns: Vec<String>,
    schema: Option<Arc<Vec<String>>>,
}

impl BtreeProjection {
    fn project_fields(&self, fields: &[(String, Value)]) -> Result<Option<Vec<u8>>, StoreError> {
        let mut output = [0; SECONDARY_KEY_BYTES_MAX];
        let mut length = 0;
        for column in &self.columns {
            let Some(value) = index_field_value(fields, column) else {
                return Ok(None);
            };
            let Some(key) = value_to_canonical_key(value.as_ref()) else {
                return Ok(Some(vec![SCAN_REQUIRED]));
            };
            match encode_ordered_key(&key, &mut output[length..]) {
                Ok(written) => length += written,
                Err(StoreError::ReadLimitExceeded { .. }) => {
                    return Ok(Some(vec![SCAN_REQUIRED]));
                }
                Err(error) => return Err(error),
            }
        }
        Ok(Some(output[..length].to_vec()))
    }
}

impl SecondaryIndexProjector for BtreeProjection {
    fn project(
        &self,
        entity: &UnifiedEntity,
        output: &mut [u8; SECONDARY_KEY_BYTES_MAX],
    ) -> Result<Option<usize>, StoreError> {
        if !matches!(entity.kind, EntityKind::TableRow { .. }) {
            return Ok(None);
        }
        let EntityData::Row(row) = &entity.data else {
            return Ok(None);
        };
        let selected = |name: &str| {
            name == "body"
                || self
                    .columns
                    .iter()
                    .any(|column| name == column || column.split('.').next() == Some(name))
        };
        let fields: Vec<_> = if let Some(named) = &row.named {
            named
                .iter()
                .filter(|(name, _)| selected(name))
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect()
        } else if let Some(schema) = row.schema.as_ref().or(self.schema.as_ref()) {
            schema
                .iter()
                .zip(&row.columns)
                .filter(|(name, _)| selected(name))
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect()
        } else {
            Vec::new()
        };
        let mut length = 0;
        for column in &self.columns {
            let Some(value) = index_field_value(&fields, column) else {
                return Ok(None);
            };
            let Some(key) = value_to_canonical_key(value.as_ref()) else {
                output[0] = SCAN_REQUIRED;
                return Ok(Some(1));
            };
            match encode_ordered_key(&key, &mut output[length..]) {
                Ok(written) => length += written,
                Err(StoreError::ReadLimitExceeded { .. }) => {
                    output[0] = SCAN_REQUIRED;
                    return Ok(Some(1));
                }
                Err(error) => return Err(error),
            }
        }
        Ok(Some(length))
    }
}

pub(super) struct RuntimeBtreeIndex {
    runtime: Weak<RuntimeInner>,
    collection: String,
    name: String,
    identity: SecondaryIndexIdentity,
    projection: Arc<BtreeProjection>,
    overlay: parking_lot::RwLock<OverlayBtree>,
}

#[derive(Default)]
struct OverlayBtree {
    postings: BTreeMap<Vec<u8>, BTreeSet<EntityId>>,
    scan_required: BTreeSet<EntityId>,
    touched: std::collections::HashMap<EntityId, Option<Vec<u8>>>,
}

impl OverlayBtree {
    fn insert(&mut self, key: Vec<u8>, entity_id: EntityId) {
        self.remove_current(entity_id);
        if key.as_slice() == [SCAN_REQUIRED] {
            self.scan_required.insert(entity_id);
        } else {
            self.postings
                .entry(key.clone())
                .or_default()
                .insert(entity_id);
        }
        self.touched.insert(entity_id, Some(key));
    }

    fn remove_current(&mut self, entity_id: EntityId) {
        let Some(Some(key)) = self.touched.remove(&entity_id) else {
            return;
        };
        if key.as_slice() == [SCAN_REQUIRED] {
            self.scan_required.remove(&entity_id);
            return;
        }
        let remove_key = self.postings.get_mut(&key).is_some_and(|ids| {
            ids.remove(&entity_id);
            ids.is_empty()
        });
        if remove_key {
            self.postings.remove(&key);
        }
    }

    fn remove(&mut self, entity_id: EntityId) {
        self.remove_current(entity_id);
        self.touched.insert(entity_id, None);
    }

    fn memory_bytes(&self) -> u64 {
        let postings = self
            .postings
            .iter()
            .map(|(key, ids)| {
                key.capacity()
                    .saturating_add(ids.len().saturating_mul(std::mem::size_of::<EntityId>()))
                    .saturating_add(96)
            })
            .sum::<usize>();
        postings
            .saturating_add(
                self.scan_required
                    .len()
                    .saturating_mul(std::mem::size_of::<EntityId>() + 48),
            )
            .saturating_add(
                self.touched
                    .values()
                    .map(|key| key.as_ref().map_or(0, Vec::capacity) + 48)
                    .sum::<usize>(),
            )
            .min(u64::MAX as usize) as u64
    }
}

struct Range {
    lower: Bound<Vec<u8>>,
    upper: Bound<Vec<u8>>,
    families: Vec<ScopedFamily>,
    matched_columns: usize,
    order_start: usize,
    range_column: bool,
}

struct ScopedFamily {
    prefix: Vec<u8>,
    family: u8,
}

// The shortest byte prefix strictly following a complete ordered key (or an
// entire tuple-prefix block). None means the byte range has no finite upper end.
fn successor(mut key: Vec<u8>) -> Option<Vec<u8>> {
    let position = key.iter().rposition(|byte| *byte != u8::MAX)?;
    key[position] += 1;
    key.truncate(position + 1);
    Some(key)
}

// A bounded, iterative AND walk. Unsupported predicates remain residual SQL
// checks; never extract a candidate restriction from inside an OR.
fn conjuncts(filter: &Filter) -> Option<Vec<&Filter>> {
    let mut pending = vec![filter];
    let mut leaves = Vec::new();
    while let Some(filter) = pending.pop() {
        match filter {
            Filter::And(left, right) => {
                if pending.len() + leaves.len() + 2 > 64 {
                    return None;
                }
                pending.push(right);
                pending.push(left);
            }
            other => leaves.push(other),
        }
    }
    Some(leaves)
}

fn encode_bound(value: &reddb_types::Value) -> Option<Vec<u8>> {
    let key = value_to_canonical_key(value)?;
    let mut output = [0; SECONDARY_KEY_BYTES_MAX];
    let length = encode_ordered_key(&key, &mut output).ok()?;
    Some(output[..length].to_vec())
}

fn exact_prefix_bound(column: &str, filter: &Filter) -> Option<(Vec<u8>, bool)> {
    match filter {
        Filter::Compare {
            field: FieldRef::TableColumn {
                column: candidate, ..
            },
            op: CompareOp::Eq,
            value,
        } if candidate == column => {
            let key = encode_bound(value)?;
            // Equality can follow SQL coercion rules. Keep the existing
            // conservative families until the remaining coercions (notably
            // floating signed zero) have their own qualified ordered lookup.
            matches!(key[0], 2 | 3 | 4 | 6).then_some((key, true))
        }
        Filter::IsNull(FieldRef::TableColumn {
            column: candidate, ..
        }) if candidate == column => {
            // IS NULL is an exact family match: values from another family
            // cannot compare equal through SQL coercion, so a mixed column
            // does not force the authoritative scan fallback.
            Some((encode_bound(&Value::Null)?, false))
        }
        _ => None,
    }
}

impl Range {
    fn from_filter(filter: &Filter) -> Option<(&str, Self)> {
        let (field, lower, upper, family) = match filter {
            Filter::Between { field, low, high } => {
                let low = encode_bound(low)?;
                let high = encode_bound(high)?;
                if low[0] != high[0] {
                    return None;
                }
                let family = low[0];
                (field, Included(low), Included(high), family)
            }
            Filter::Compare { field, op, value } => {
                let key = encode_bound(value)?;
                let family = key[0];
                let (lower, upper) = match op {
                    CompareOp::Lt => (Unbounded, Excluded(key)),
                    CompareOp::Le => (Unbounded, Included(key)),
                    CompareOp::Gt => (Included(successor(key)?), Unbounded),
                    CompareOp::Ge => (Included(key), Unbounded),
                    _ => return None,
                };
                (field, lower, upper, family)
            }
            _ => return None,
        };
        // Start with integral and text ranges. Other families keep the SQL scan
        // until their coercions (including floating signed zero) are qualified.
        if !matches!(family, 2 | 3 | 4 | 6) {
            return None;
        }
        let FieldRef::TableColumn { column, .. } = field else {
            return None;
        };
        Some((
            column,
            Self {
                lower,
                upper,
                families: vec![ScopedFamily {
                    prefix: Vec::new(),
                    family,
                }],
                matched_columns: 1,
                order_start: 0,
                range_column: true,
            },
        ))
    }

    fn for_index(columns: &[String], filter: &Filter) -> Option<Self> {
        if columns.is_empty() {
            return None;
        }
        let leaves = conjuncts(filter)?;
        let mut prefix = Vec::new();
        let mut families = Vec::new();
        let mut append_exact = |column: &str| -> Option<()> {
            let (key, require_homogeneous_family) = leaves
                .iter()
                .find_map(|filter| exact_prefix_bound(column, filter))?;
            if prefix.len() + key.len() > SECONDARY_KEY_BYTES_MAX {
                return None;
            }
            if require_homogeneous_family {
                families.push(ScopedFamily {
                    prefix: prefix.clone(),
                    family: key[0],
                });
            }
            prefix.extend_from_slice(&key);
            Some(())
        };

        let mut exact_columns = 0;
        for column in columns {
            if append_exact(column).is_none() {
                break;
            }
            exact_columns += 1;
        }

        let trailing_range = columns.get(exact_columns).and_then(|range_column| {
            leaves.iter().find_map(|filter| {
                let (column, range) = Self::from_filter(filter)?;
                (column == range_column).then_some(range)
            })
        });
        if let Some(mut range) = trailing_range {
            let family = range.families[0].family;
            families.push(ScopedFamily {
                prefix: prefix.clone(),
                family,
            });
            let with_prefix = |key: Vec<u8>| -> Option<Vec<u8>> {
                if prefix.len() + key.len() > SECONDARY_KEY_BYTES_MAX {
                    return None;
                }
                let mut joined = prefix.clone();
                joined.extend_from_slice(&key);
                Some(joined)
            };
            let has_suffix = exact_columns + 1 < columns.len();
            range.lower = match range.lower {
                Included(key) => Included(with_prefix(key)?),
                Excluded(key) => Excluded(with_prefix(key)?),
                Unbounded if !prefix.is_empty() => Included(prefix.clone()),
                Unbounded => Unbounded,
            };
            range.upper = match range.upper {
                Included(key) if has_suffix => successor(with_prefix(key)?)
                    .map(Excluded)
                    .unwrap_or(Unbounded),
                Included(key) => Included(with_prefix(key)?),
                Excluded(key) => Excluded(with_prefix(key)?),
                Unbounded => successor(prefix).map(Excluded).unwrap_or(Unbounded),
            };
            range.families = families;
            range.matched_columns = exact_columns + 1;
            range.order_start = exact_columns;
            range.range_column = true;
            return Some(range);
        }

        // A leading exact prefix is enough to seek a composite tree even when
        // the remaining columns are used only for ordering. SQL execution
        // still reapplies the complete filter to every returned candidate.
        (exact_columns > 0).then(|| Self {
            lower: Included(prefix.clone()),
            upper: successor(prefix).map(Excluded).unwrap_or(Unbounded),
            families,
            matched_columns: exact_columns,
            order_start: exact_columns,
            range_column: false,
        })
    }

    fn supports_order(
        &self,
        columns: &[String],
        order_by: &[OrderByClause],
        not_null: impl Fn(&str) -> bool,
    ) -> bool {
        if order_by.is_empty() || self.order_start + order_by.len() > columns.len() {
            return false;
        }
        order_by.iter().enumerate().all(|(offset, clause)| {
            let expected = &columns[self.order_start + offset];
            let field_matches = matches!(
                &clause.field,
                FieldRef::TableColumn { column, .. } if column == expected
            );
            let null_order_matches =
                clause.nulls_first || not_null(expected) || (offset == 0 && self.range_column);
            clause.expr.is_none() && clause.ascending && field_matches && null_order_matches
        })
    }

    fn contains(&self, key: &[u8]) -> bool {
        let above = match &self.lower {
            Included(low) => key >= low.as_slice(),
            Excluded(low) => key > low.as_slice(),
            Unbounded => true,
        };
        let below = match &self.upper {
            Included(high) => key <= high.as_slice(),
            Excluded(high) => key < high.as_slice(),
            Unbounded => true,
        };
        above && below
    }
}

impl RuntimeBtreeIndex {
    fn supports_order(&self, range: &Range, order_by: &[OrderByClause]) -> bool {
        let contract = self
            .runtime
            .upgrade()
            .and_then(|inner| inner.db.collection_contract(&self.collection));
        range.supports_order(&self.projection.columns, order_by, |column| {
            contract.as_ref().is_some_and(|contract| {
                contract.declared_columns.iter().any(|declared| {
                    declared.name == column && (declared.not_null || declared.primary_key)
                })
            })
        })
    }

    pub(super) fn memory_bytes(&self) -> u64 {
        (std::mem::size_of::<Self>()
            + std::mem::size_of::<BtreeProjection>()
            + 64
            + self.collection.len()
            + self.name.len()
            + self
                .projection
                .columns
                .iter()
                .map(|column| column.len() + 24)
                .sum::<usize>()
            + self
                .projection
                .schema
                .as_ref()
                .map_or(0, |schema| schema.iter().map(|name| name.len() + 24).sum())) as u64
            + self.overlay.read().memory_bytes()
    }

    fn overlay_insert(
        &self,
        entity_id: EntityId,
        fields: &[(String, Value)],
    ) -> Result<(), String> {
        let Some(key) = self
            .projection
            .project_fields(fields)
            .map_err(|error| error.to_string())?
        else {
            return Ok(());
        };
        self.overlay.write().insert(key, entity_id);
        Ok(())
    }

    fn overlay_delete(
        &self,
        entity_id: EntityId,
        fields: &[(String, Value)],
    ) -> Result<(), String> {
        let Some(_key) = self
            .projection
            .project_fields(fields)
            .map_err(|error| error.to_string())?
        else {
            // Update hooks also publish one changed column at a time. A
            // partial composite key must not erase the full-row projection
            // installed by `overlay_update` above those hooks.
            return Ok(());
        };
        self.overlay.write().remove(entity_id);
        Ok(())
    }

    fn overlay_update(
        &self,
        entity_id: EntityId,
        old_fields: &[(String, Value)],
        new_fields: &[(String, Value)],
    ) -> Result<(), String> {
        let old_key = self
            .projection
            .project_fields(old_fields)
            .map_err(|error| error.to_string())?;
        let new_key = self
            .projection
            .project_fields(new_fields)
            .map_err(|error| error.to_string())?;
        if old_key == new_key {
            return Ok(());
        }
        let mut overlay = self.overlay.write();
        let _ = old_key;
        if let Some(key) = new_key {
            overlay.insert(key, entity_id);
        } else {
            overlay.remove(entity_id);
        }
        Ok(())
    }

    fn lookup(
        &self,
        range: &Range,
        preserve_index_order: bool,
    ) -> crate::RedDBResult<Option<HashCandidates>> {
        let inner = self
            .runtime
            .upgrade()
            .ok_or_else(|| crate::RedDBError::Internal("BTREE runtime closed".into()))?;
        let runtime = RedDBRuntime {
            inner,
            background_tasks: None,
        };
        let ceiling = usize::try_from(runtime.memory_accounting().budget().resolved_bytes / 16)
            .unwrap_or(usize::MAX)
            .max(1);
        let mut limit = ceiling.min(64 * 1024);
        loop {
            match self.lookup_with_limit(&runtime, range, limit, preserve_index_order) {
                Err(StoreError::ReadLimitExceeded { required_bytes, .. })
                    if limit < ceiling && required_bytes <= ceiling =>
                {
                    limit = limit.saturating_mul(2).max(required_bytes).min(ceiling);
                }
                result => return result.map_err(crate::RedDBError::from),
            }
        }
    }

    fn lookup_with_limit(
        &self,
        runtime: &RedDBRuntime,
        range: &Range,
        limit: usize,
        preserve_index_order: bool,
    ) -> Result<Option<HashCandidates>, StoreError> {
        let store = runtime.db().store();
        let manager = store
            .get_collection(&self.collection)
            .ok_or_else(|| StoreError::CollectionNotFound(self.collection.clone()))?;
        let (_scratch, read_bytes_max) = runtime
            .reserve_index_scan_with_limit(&manager, limit)
            .map_err(|error| StoreError::Serialization(error.to_string()))?;
        let output = runtime
            .try_reserve_query_buffer(read_bytes_max)
            .ok_or_else(|| {
                StoreError::Serialization("BTREE candidates exceed shared memory budget".into())
            })?;
        let ids_max = read_bytes_max / std::mem::size_of::<EntityId>();
        let mut ids = Vec::new();
        ids.try_reserve_exact(ids_max)
            .map_err(|error| StoreError::Serialization(error.to_string()))?;
        // Admission precedes publication/source locks; the returned owner keeps
        // candidate memory charged through result assembly and error paths.
        let _publication = store.vault_publication_lock.lock();
        let Some(index) = store.secondary_index(&self.collection, &self.name, &self.identity)
        else {
            return Ok(None);
        };
        let overlay = self.overlay.read();
        let mut key = [0; SECONDARY_KEY_BYTES_MAX];
        // Existing SQL range semantics require a homogeneous canonical family.
        // Two seeks per bound component detect mixed families and overflow
        // markers within the matching prefix, without hydrating posting maps.
        for scope in &range.families {
            let mut family_start = scope.prefix.clone();
            family_start.push(scope.family);
            let mut family_end = scope.prefix.clone();
            family_end.push(scope.family + 1);
            let prefix_end = successor(scope.prefix.clone());
            let upper = prefix_end.as_deref().map(Excluded).unwrap_or(Unbounded);
            for (lower, upper) in [
                (
                    Included(scope.prefix.as_slice()),
                    Excluded(family_start.as_slice()),
                ),
                (Included(family_end.as_slice()), upper),
            ] {
                if index
                    .ordered_range(lower, upper)?
                    .next_posting(&mut key)?
                    .is_some()
                {
                    return Ok(None);
                }
            }
        }
        let mut needs_scan = false;
        let mut accept = |entity: &UnifiedEntity| -> Result<(), StoreError> {
            let required_bytes = crate::storage::unified::memory_size::entity_bytes(entity);
            if required_bytes > read_bytes_max {
                return Err(StoreError::ReadLimitExceeded {
                    required_bytes,
                    limit_bytes: read_bytes_max,
                });
            }
            let Some(length) = self.projection.project(entity, &mut key)? else {
                return Ok(());
            };
            for scope in &range.families {
                if !key[..length].starts_with(&scope.prefix) {
                    return Ok(());
                }
                if key.get(scope.prefix.len()) != Some(&scope.family) {
                    needs_scan = true;
                    return Ok(());
                }
            }
            if range.contains(&key[..length]) {
                if ids.len() == ids_max {
                    return Err(StoreError::ReadLimitExceeded {
                        required_bytes: read_bytes_max
                            .saturating_add(std::mem::size_of::<EntityId>()),
                        limit_bytes: read_bytes_max,
                    });
                }
                ids.push(entity.id);
            }
            Ok(())
        };
        if !overlay.scan_required.is_empty() {
            return Ok(None);
        }
        for scope in &range.families {
            let mut family_start = scope.prefix.clone();
            family_start.push(scope.family);
            let mut family_end = scope.prefix.clone();
            family_end.push(scope.family + 1);
            if overlay
                .postings
                .range((Included(scope.prefix.clone()), Excluded(family_start)))
                .next()
                .is_some()
            {
                return Ok(None);
            }
            let outside_family = match successor(scope.prefix.clone()) {
                Some(prefix_end) => overlay
                    .postings
                    .range((Included(family_end), Excluded(prefix_end)))
                    .next()
                    .is_some(),
                None => overlay
                    .postings
                    .range((Included(family_end), Unbounded))
                    .next()
                    .is_some(),
            };
            if outside_family {
                return Ok(None);
            }
        }

        let mut cursor = index.ordered_range(
            range.lower.as_ref().map(Vec::as_slice),
            range.upper.as_ref().map(Vec::as_slice),
        )?;
        let mut posting = [0; SECONDARY_KEY_BYTES_MAX];
        let mut physical = cursor.next_posting(&mut posting)?;
        let mut live = overlay
            .postings
            .range((range.lower.clone(), range.upper.clone()))
            .flat_map(|(key, entity_ids)| {
                entity_ids
                    .iter()
                    .copied()
                    .map(move |entity_id| (key.as_slice(), entity_id))
            })
            .peekable();
        loop {
            let take_physical = match (physical, live.peek()) {
                (Some((length, _)), Some((live_key, _))) => posting[..length] <= **live_key,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            let entity_id = if take_physical {
                let (_, entity_id) = physical.expect("physical candidate is present");
                physical = cursor.next_posting(&mut posting)?;
                if overlay.touched.contains_key(&entity_id) {
                    continue;
                }
                entity_id
            } else {
                live.next().expect("live candidate is present").1
            };
            if let Some(entity) = manager.try_get(entity_id, read_bytes_max)? {
                accept(&entity)?;
            }
        }
        if needs_scan {
            return Ok(None);
        }
        if !preserve_index_order {
            ids.sort_unstable();
            ids.dedup();
        }
        Ok(Some(HashCandidates::owned(ids, Box::new(output))))
    }
}

impl IndexStore {
    fn select_paged_btree(
        &self,
        collection: &str,
        filter: &Filter,
    ) -> Option<(Arc<RuntimeBtreeIndex>, Range)> {
        self.paged_btrees
            .read()
            .values()
            .filter(|provider| provider.collection == collection)
            .filter_map(|provider| {
                Range::for_index(&provider.projection.columns, filter)
                    .map(|range| (Arc::clone(provider), range))
            })
            .max_by_key(|(provider, range)| {
                (range.matched_columns, provider.projection.columns.len())
            })
    }

    pub(crate) fn clear_paged_btree_overlays(&self) {
        let providers: Vec<_> = self.paged_btrees.read().values().cloned().collect();
        for provider in providers {
            *provider.overlay.write() = OverlayBtree::default();
        }
    }

    pub(crate) fn paged_btree_overlay_insert(
        &self,
        collection: &str,
        entity_id: EntityId,
        fields: &[(String, Value)],
    ) -> Result<(), String> {
        let providers: Vec<_> = self
            .paged_btrees
            .read()
            .values()
            .filter(|provider| provider.collection == collection)
            .cloned()
            .collect();
        for provider in providers {
            provider.overlay_insert(entity_id, fields)?;
        }
        Ok(())
    }

    pub(crate) fn paged_btree_overlay_delete(
        &self,
        collection: &str,
        entity_id: EntityId,
        fields: &[(String, Value)],
    ) -> Result<(), String> {
        let providers: Vec<_> = self
            .paged_btrees
            .read()
            .values()
            .filter(|provider| provider.collection == collection)
            .cloned()
            .collect();
        for provider in providers {
            provider.overlay_delete(entity_id, fields)?;
        }
        Ok(())
    }

    pub(crate) fn paged_btree_overlay_update(
        &self,
        collection: &str,
        entity_id: EntityId,
        old_fields: &[(String, Value)],
        new_fields: &[(String, Value)],
    ) -> Result<(), String> {
        let providers: Vec<_> = self
            .paged_btrees
            .read()
            .values()
            .filter(|provider| provider.collection == collection)
            .cloned()
            .collect();
        for provider in providers {
            provider.overlay_update(entity_id, old_fields, new_fields)?;
        }
        Ok(())
    }

    pub(crate) fn paged_btree_lookup(
        &self,
        collection: &str,
        filter: &Filter,
    ) -> crate::RedDBResult<Option<HashCandidates>> {
        if self.paged_btrees.read().is_empty() {
            return Ok(None);
        }
        // Release descriptor guards before admission samples memory pools.
        match self.select_paged_btree(collection, filter) {
            Some((provider, range)) => provider.lookup(&range, false),
            None => Ok(None),
        }
    }

    pub(crate) fn paged_btree_lookup_for_query(
        &self,
        collection: &str,
        filter: &Filter,
        order_by: &[OrderByClause],
    ) -> crate::RedDBResult<(Option<HashCandidates>, bool)> {
        if self.paged_btrees.read().is_empty() {
            return Ok((None, false));
        }
        let Some((provider, range)) = self.select_paged_btree(collection, filter) else {
            return Ok((None, false));
        };
        let ordered = provider.supports_order(&range, order_by);
        provider.lookup(&range, ordered).map(|candidates| {
            let order_satisfied = ordered && candidates.is_some();
            (candidates, order_satisfied)
        })
    }
}

impl RedDBRuntime {
    pub(super) fn prepare_paged_btree(
        &self,
        index: &RegisteredIndex,
        prepared: &IndexStore,
    ) -> crate::RedDBResult<()> {
        assert_eq!(index.method, IndexMethodKind::BTree);
        assert!(!index.columns.is_empty());
        let schema = self
            .db()
            .store()
            .get_collection(&index.collection)
            .and_then(|manager| manager.column_schema());
        let projection = Arc::new(BtreeProjection {
            columns: index.columns.clone(),
            schema,
        });
        let mut digest = Sha256::new();
        digest.update(b"runtime-btree-ordered-v1");
        digest.update([
            u8::from(index.unique),
            u8::from(projection.schema.is_some()),
        ]);
        for names in [
            index.columns.as_slice(),
            projection
                .schema
                .as_deref()
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        ] {
            digest.update((names.len() as u64).to_le_bytes());
            for name in names {
                digest.update((name.len() as u64).to_le_bytes());
                digest.update(name.as_bytes());
            }
        }
        let identity = SecondaryIndexIdentity {
            codec_version: 1,
            definition: digest.finalize().into(),
        };
        self.db().store().register_secondary_index(
            &index.collection,
            &index.name,
            identity.clone(),
            projection.clone(),
        )?;
        if index.columns.len() == 1 || index.unique {
            let mut equality = index.clone();
            equality.name = index.hash_lookup_name().into_owned();
            self.prepare_paged_hash_named(
                &equality,
                prepared,
                &equality_storage_name(&index.name),
            )?;
        }
        prepared.paged_btrees.write().insert(
            (index.collection.clone(), index.name.clone()),
            Arc::new(RuntimeBtreeIndex {
                runtime: Arc::downgrade(&self.inner),
                collection: index.collection.clone(),
                name: index.name.clone(),
                identity,
                projection,
                overlay: parking_lot::RwLock::new(OverlayBtree::default()),
            }),
        );
        Ok(())
    }
}

pub(super) fn equality_storage_name(name: &str) -> String {
    format!(
        "\0reddb/btree-equality-v1/{}",
        hex::encode(Sha256::digest(name.as_bytes()))
    )
}

#[cfg(test)]
mod tests {
    use super::{encode_bound, Range};
    use crate::storage::{EntityId, StoreError};
    use crate::{RedDBOptions, RedDBRuntime};
    use reddb_engine::storage::collection_read::{
        CollectionCursor, CollectionRead, EncodedRecord, PagedCollection,
    };
    use reddb_types::Value;
    use std::sync::Arc;

    struct NoScan(Arc<dyn CollectionRead + Send + Sync>);
    impl CollectionRead for NoScan {
        fn get_record(
            &self,
            id: EntityId,
            limit: usize,
        ) -> Result<Option<EncodedRecord>, StoreError> {
            self.0.get_record(id, limit)
        }
        fn cursor(&self) -> Result<Box<dyn CollectionCursor + '_>, StoreError> {
            Err(StoreError::Serialization(
                "unexpected BTREE source scan".into(),
            ))
        }
        fn owned_cursor(&self) -> Result<Box<dyn CollectionCursor>, StoreError> {
            Err(StoreError::Serialization(
                "unexpected BTREE source scan".into(),
            ))
        }
    }

    fn fixture() -> (tempfile::TempDir, RedDBRuntime, RedDBOptions) {
        let directory = tempfile::tempdir().expect("BTREE fixture");
        let options = RedDBOptions::persistent(directory.path().join("btree.rdb"))
            .with_memory_budget(64 * 1024 * 1024)
            .with_storage_profile(
                crate::storage::profile::StorageDeployPreset::PrimaryReplicaDev.selection(),
            )
            .expect("profile");
        let seed = RedDBRuntime::with_options(options.clone()).expect("seed");
        seed.execute_query("CREATE TABLE ordered_rows (id INTEGER, label TEXT, payload TEXT)")
            .expect("table");
        seed.execute_query("CREATE UNIQUE INDEX ordered_id ON ordered_rows (id) USING BTREE")
            .expect("numeric index");
        seed.execute_query("CREATE INDEX ordered_label ON ordered_rows (label) USING BTREE")
            .expect("text index");
        for id in 0..32 {
            seed.execute_query_with_params(
                "INSERT INTO ordered_rows (id, label, payload) VALUES ($1, $2, $3)",
                &[
                    Value::Integer(id),
                    Value::text(format!("key-{id:02}")),
                    Value::text("payload".repeat(2048)),
                ],
            )
            .expect("insert");
        }
        seed.flush().expect("seed checkpoint");
        drop(seed);
        let runtime = open(&options);
        (directory, runtime, options)
    }

    fn open(options: &RedDBOptions) -> RedDBRuntime {
        crate::storage::RedDB::with_disk_residency_for_qualification(|| {
            RedDBRuntime::with_options(options.clone())
        })
        .expect("cold runtime")
    }

    fn deny_scan(runtime: &RedDBRuntime) {
        deny_scan_table(runtime, "ordered_rows");
    }

    fn deny_scan_table(runtime: &RedDBRuntime, table: &str) {
        let store = runtime.db().store();
        let manager = store.get_collection(table).expect("collection");
        let root = manager.backing_root_page().expect("root");
        let count = manager.backing_count_at_root(root).expect("count");
        manager.install_backing(
            Arc::new(NoScan(Arc::new(PagedCollection::new(Arc::new(
                crate::storage::engine::BTree::with_root(
                    Arc::clone(store.pager().expect("pager")),
                    root,
                ),
            ))))),
            root,
            store.format_version(),
            1024 * 1024,
            count,
        );
    }

    fn ids(runtime: &RedDBRuntime, sql: &str) -> Vec<i64> {
        runtime
            .execute_query(sql)
            .expect(sql)
            .result
            .records
            .iter()
            .map(|record| match record.get("id").expect("id") {
                Value::Integer(id) => *id,
                other => panic!("unexpected id {other:?}"),
            })
            .collect()
    }

    #[test]
    fn paged_btree_prepared_restore_and_ranges_never_scan_source() {
        let (_directory, runtime, options) = fixture();
        drop(runtime);
        let runtime = open(&options);
        deny_scan(&runtime);
        runtime
            .rebuild_runtime_indexes_for_table("ordered_rows")
            .expect("prepared metadata-only BTREE restore");
        assert_eq!(
            runtime.index_store_ref().sorted.memory_bytes(),
            0,
            "no resident sorted postings"
        );
        for (predicate, expected) in [
            ("id BETWEEN 9 AND 12", vec![9, 10, 11, 12]),
            ("id > 28", vec![29, 30, 31]),
            ("id >= 30", vec![30, 31]),
            ("id < 2", vec![0, 1]),
            ("id <= 1", vec![0, 1]),
            ("label BETWEEN 'key-09' AND 'key-12'", vec![9, 10, 11, 12]),
        ] {
            assert_eq!(
                ids(
                    &runtime,
                    &format!("SELECT id FROM ordered_rows WHERE {predicate} ORDER BY id")
                ),
                expected
            );
        }
    }
    #[test]
    fn paged_btree_overlay_unique_rollback_and_checkpoint_preserve_results() {
        let (_directory, runtime, options) = fixture();
        for sql in [
            "UPDATE ordered_rows SET id = 40 WHERE id = 1",
            "DELETE FROM ordered_rows WHERE id = 2",
            "INSERT INTO ordered_rows (id, label, payload) VALUES (50, 'new', 'new')",
        ] {
            runtime.execute_query(sql).expect(sql);
        }
        let expected: Vec<_> = std::iter::once(0).chain(3..32).chain([40, 50]).collect();
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE id >= 0 ORDER BY id"
            ),
            expected
        );
        assert!(runtime
            .execute_query("INSERT INTO ordered_rows (id, label) VALUES (40, 'duplicate')")
            .is_err());
        runtime.execute_query("BEGIN").expect("begin");
        runtime
            .execute_query("UPDATE ordered_rows SET id = 60 WHERE id = 3")
            .expect("own update");
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE id >= 40 ORDER BY id"
            ),
            [40, 50, 60]
        );
        runtime
            .execute_query("SAVEPOINT nested")
            .expect("savepoint");
        runtime
            .execute_query("DELETE FROM ordered_rows WHERE id = 40")
            .expect("own delete");
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE id >= 40 ORDER BY id"
            ),
            [50, 60]
        );
        runtime
            .execute_query("ROLLBACK TO SAVEPOINT nested")
            .expect("rollback savepoint");
        runtime.execute_query("ROLLBACK").expect("rollback");
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE id >= 0 ORDER BY id"
            ),
            expected
        );
        runtime.flush().expect("rebase");
        drop(runtime);
        let runtime = open(&options);
        deny_scan(&runtime);
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE id >= 0 ORDER BY id"
            ),
            expected
        );
        assert_eq!(ids(&runtime, "SELECT id FROM ordered_rows WHERE id BETWEEN 9 AND 12 ORDER BY id DESC LIMIT 2 OFFSET 1"), [11, 10]);
        assert!(runtime
            .execute_query("INSERT INTO ordered_rows (id, label) VALUES (40, 'duplicate')")
            .is_err());
    }

    #[test]
    fn paged_btree_large_keys_fall_back_without_losing_rows() {
        use super::*;
        let (_directory, runtime, options) = fixture();
        runtime
            .execute_query_with_params(
                "INSERT INTO ordered_rows (id, label) VALUES (80, $1)",
                &[Value::text("z".repeat(2048))],
            )
            .expect("large label");
        let filter = Filter::Compare {
            field: FieldRef::column("ordered_rows", "label"),
            op: CompareOp::Ge,
            value: Value::text("key-30"),
        };
        assert!(runtime
            .index_store_ref()
            .paged_btree_lookup("ordered_rows", &filter)
            .expect("overlay overflow")
            .is_none());
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE label >= 'key-30' ORDER BY id"
            ),
            [30, 31, 80]
        );
        runtime.flush().expect("publish overflow marker");
        drop(runtime);
        let runtime = open(&options);
        assert!(runtime
            .index_store_ref()
            .paged_btree_lookup("ordered_rows", &filter)
            .expect("base overflow")
            .is_none());
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE label >= 'key-30' ORDER BY id"
            ),
            [30, 31, 80]
        );
    }

    #[test]
    fn paged_btree_page_errors_are_authoritative() {
        use crate::storage::engine::{Page, PageType};
        let (_directory, runtime, _options) = fixture();
        let store = runtime.db().store();
        let provider = runtime
            .index_store_ref()
            .paged_btrees
            .read()
            .get(&("ordered_rows".into(), "ordered_id".into()))
            .cloned()
            .expect("paged BTREE");
        let index = store
            .secondary_index("ordered_rows", "ordered_id", &provider.identity)
            .expect("root");
        let root = index.root_page_id();
        let pager = store.pager().expect("pager");
        let original = pager.read_page(root).expect("original");
        pager
            .write_page(root, Page::new(PageType::Overflow, root))
            .expect("corrupt fixture");
        runtime
            .execute_query("SELECT id FROM ordered_rows WHERE id BETWEEN 9 AND 12")
            .expect_err("an unreadable range must not become an empty result or fallback scan");
        pager.write_page(root, original).expect("restore");
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE id BETWEEN 9 AND 12 ORDER BY id"
            ),
            [9, 10, 11, 12]
        );
    }

    #[test]
    fn paged_btree_candidate_owner_retains_and_releases_admission() {
        use super::*;
        use crate::storage::memory_pools::MemoryPool;
        let (_directory, runtime, _options) = fixture();
        let filter = Filter::Between {
            field: FieldRef::column("ordered_rows", "id"),
            low: Value::Integer(9),
            high: Value::Integer(12),
        };
        let accounting = runtime.memory_accounting();
        let before = accounting.used_bytes(MemoryPool::QueryBuffers);
        let candidates = runtime
            .index_store_ref()
            .paged_btree_lookup("ordered_rows", &filter)
            .expect("range")
            .expect("provider");
        assert_eq!(candidates.len(), 4);
        let retained = accounting.used_bytes(MemoryPool::QueryBuffers);
        assert!(retained > before);
        assert!(retained - before <= 64 * 1024);
        let mut iterator = candidates.into_iter();
        assert!(iterator.next().is_some());
        assert_eq!(accounting.used_bytes(MemoryPool::QueryBuffers), retained);
        drop(iterator);
        assert_eq!(accounting.used_bytes(MemoryPool::QueryBuffers), before);
        drop(runtime.try_reserve_query_buffer(0).expect("sample"));
        let available = accounting.budget().resolved_bytes - accounting.total_used_bytes();
        let held = runtime
            .try_reserve_query_buffer((available - 16 * 1024) as usize)
            .expect("pressure");
        runtime
            .index_store_ref()
            .paged_btree_lookup("ordered_rows", &filter)
            .expect_err("deny before allocating");
        drop(held);
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE id BETWEEN 9 AND 12 ORDER BY id"
            ),
            [9, 10, 11, 12]
        );
    }

    #[test]
    fn paged_btree_sixteen_clients_retain_bounded_candidates() {
        use super::*;
        use crate::storage::memory_pools::MemoryPool;
        let (_directory, runtime, _options) = fixture();
        let accounting = runtime.memory_accounting();
        let before = accounting.used_bytes(MemoryPool::QueryBuffers);
        let barrier = Arc::new(std::sync::Barrier::new(17));
        let (send, receive) = std::sync::mpsc::channel();
        let mut workers = Vec::new();
        for _ in 0..16 {
            let runtime = runtime.clone();
            let barrier = Arc::clone(&barrier);
            let send = send.clone();
            workers.push(std::thread::spawn(move || {
                let filter = Filter::Between {
                    field: FieldRef::column("ordered_rows", "id"),
                    low: Value::Integer(9),
                    high: Value::Integer(12),
                };
                let candidates = runtime
                    .index_store_ref()
                    .paged_btree_lookup("ordered_rows", &filter);
                send.send(
                    candidates
                        .as_ref()
                        .map(|ids| ids.as_ref().map(|ids| ids.len()))
                        .map_err(ToString::to_string),
                )
                .expect("result");
                barrier.wait();
                drop(candidates);
            }));
        }
        let results: Vec<_> = (0..16)
            .map(|_| {
                receive
                    .recv_timeout(std::time::Duration::from_secs(30))
                    .expect("all clients finish admission")
            })
            .collect();
        let retained = accounting.used_bytes(MemoryPool::QueryBuffers) - before;
        barrier.wait();
        for worker in workers {
            worker.join().expect("client");
        }
        for result in results {
            assert_eq!(result.expect("admitted"), Some(4));
        }
        assert!(retained <= 16 * 64 * 1024, "{retained}");
        assert_eq!(accounting.used_bytes(MemoryPool::QueryBuffers), before);
    }

    #[test]
    fn paged_btree_drop_recreate_preserves_new_definition() {
        let (_directory, runtime, options) = fixture();
        runtime
            .execute_query("DROP INDEX ordered_id ON ordered_rows")
            .expect("drop");
        assert!(!runtime
            .index_store_ref()
            .paged_btrees
            .read()
            .contains_key(&("ordered_rows".into(), "ordered_id".into())));
        // Reuse the name with a different key; an old persisted numeric root
        // must never qualify this new definition.
        runtime
            .execute_query("CREATE INDEX ordered_id ON ordered_rows (label) USING BTREE")
            .expect("recreate");
        runtime
            .execute_query("INSERT INTO ordered_rows (id, label) VALUES (80, 'key-10')")
            .expect("write after recreate");
        runtime.flush().expect("checkpoint");
        drop(runtime);
        let runtime = open(&options);
        deny_scan(&runtime);
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE label BETWEEN 'key-09' AND 'key-11' ORDER BY id"
            ),
            [9, 10, 11, 80]
        );
    }

    #[test]
    fn paged_btree_null_family_and_numeric_coercion_keep_sql_fallback() {
        use super::*;
        let (_directory, runtime, options) = fixture();
        runtime
            .execute_query("INSERT INTO ordered_rows (id, label) VALUES (80, NULL)")
            .expect("null");
        let filter = Filter::Compare {
            field: FieldRef::column("ordered_rows", "label"),
            op: CompareOp::Ge,
            value: Value::text("key-30"),
        };
        assert!(runtime
            .index_store_ref()
            .paged_btree_lookup("ordered_rows", &filter)
            .expect("overlay mixed family")
            .is_none());
        runtime.flush().expect("publish mixed family");
        drop(runtime);
        let runtime = open(&options);
        assert!(runtime
            .index_store_ref()
            .paged_btree_lookup("ordered_rows", &filter)
            .expect("base mixed family")
            .is_none());
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE label >= 'key-30' ORDER BY id"
            ),
            [30, 31]
        );
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM ordered_rows WHERE id >= 29.0 ORDER BY id"
            ),
            [29, 30, 31, 80]
        );
    }

    fn composite_fixture() -> (tempfile::TempDir, RedDBRuntime, RedDBOptions) {
        let (directory, runtime, options) = fixture();
        for (arity, columns) in [
            (2, "account, id"),
            (3, "account, bucket, id"),
            (5, "account, bucket, category, shard, id"),
        ] {
            runtime.execute_query(&format!("CREATE TABLE compound_{arity} (account TEXT, bucket INTEGER, category TEXT, shard TEXT, id INTEGER)")).expect("compound table");
            let uniqueness = if arity == 2 { "UNIQUE " } else { "" };
            runtime
                .execute_query(&format!(
                    "CREATE {uniqueness}INDEX compound_key ON compound_{arity} ({columns}) USING BTREE"
                ))
                .expect("compound index");
            for account in ["alpha", "alpha-prefix", "beta"] {
                for id in 0..16 {
                    runtime.execute_query_with_params(&format!("INSERT INTO compound_{arity} (account, bucket, category, shard, id) VALUES ($1, 1, 'events', 'west', $2)"), &[Value::text(account), Value::Integer(id)]).expect("compound row");
                }
            }
        }
        runtime
            .execute_query("CREATE INDEX range_only ON compound_3 (id) USING BTREE")
            .expect("competing scalar range");
        runtime.flush().expect("persist");
        drop(runtime);
        let runtime = open(&options);
        runtime.flush().expect("prepared roots");
        drop(runtime);
        let runtime = open(&options);
        (directory, runtime, options)
    }

    #[test]
    fn paged_composite_btree_restore_and_prefix_ranges_do_not_scan_source() {
        let (_directory, runtime, _options) = composite_fixture();
        for (arity, prefix) in [
            (2, "account = 'alpha'"),
            (3, "account = 'alpha' AND bucket = 1"),
            (
                5,
                "account = 'alpha' AND bucket = 1 AND category = 'events' AND shard = 'west'",
            ),
        ] {
            let table = format!("compound_{arity}");
            deny_scan_table(&runtime, &table);
            runtime
                .rebuild_runtime_indexes_for_table(&table)
                .expect("prepared compound metadata-only restore");
            for (predicate, expected) in [
                ("id BETWEEN 9 AND 12", vec![9, 10, 11, 12]),
                ("id > 13", vec![14, 15]),
                ("id >= 15", vec![15]),
                ("id < 2", vec![0, 1]),
                ("id <= 1", vec![0, 1]),
                ("id > 15", vec![]),
            ] {
                assert_eq!(
                    ids(
                        &runtime,
                        &format!(
                            "SELECT id FROM {table} WHERE {prefix} AND {predicate} ORDER BY id"
                        )
                    ),
                    expected,
                    "arity={arity} {predicate}"
                );
                assert_eq!(
                    ids(
                        &runtime,
                        &format!(
                            "SELECT id FROM {table} WHERE {predicate} AND {prefix} ORDER BY id"
                        )
                    ),
                    expected,
                    "reversed arity={arity}"
                );
            }
        }
        assert_eq!(runtime.index_store_ref().sorted.memory_bytes(), 0);
    }

    fn compound_filter(bucket: Value) -> reddb_rql::ast::Filter {
        use reddb_rql::ast::{CompareOp, FieldRef, Filter};
        Filter::And(
            Box::new(Filter::Compare {
                field: FieldRef::column("compound_3", "account"),
                op: CompareOp::Eq,
                value: Value::text("alpha"),
            }),
            Box::new(Filter::And(
                Box::new(Filter::Compare {
                    field: FieldRef::column("compound_3", "bucket"),
                    op: CompareOp::Eq,
                    value: bucket,
                }),
                Box::new(Filter::Between {
                    field: FieldRef::column("compound_3", "id"),
                    low: Value::Integer(9),
                    high: Value::Integer(12),
                }),
            )),
        )
    }

    #[test]
    fn composite_prefix_accepts_is_null_without_a_trailing_range() {
        use reddb_rql::ast::{CompareOp, FieldRef, Filter};

        let filter = Filter::And(
            Box::new(Filter::Compare {
                field: FieldRef::column("monitoring_exports", "status"),
                op: CompareOp::Eq,
                value: Value::text("accepted"),
            }),
            Box::new(Filter::IsNull(FieldRef::column(
                "monitoring_exports",
                "estimated_at",
            ))),
        );
        let columns = [
            "status".to_string(),
            "estimated_at".to_string(),
            "received_at".to_string(),
        ];
        let range = Range::for_index(&columns, &filter).expect("exact composite prefix");
        let tuple = |values: &[Value]| {
            values
                .iter()
                .flat_map(|value| encode_bound(value).expect("ordered value"))
                .collect::<Vec<_>>()
        };

        assert!(range.contains(&tuple(&[
            Value::text("accepted"),
            Value::Null,
            Value::text("2026-11-06T00:00:00Z"),
        ])));
        assert!(!range.contains(&tuple(&[
            Value::text("accepted"),
            Value::text("2026-11-06T00:01:00Z"),
            Value::text("2026-11-06T00:00:00Z"),
        ])));
        assert!(!range.contains(&tuple(&[
            Value::text("rejected"),
            Value::Null,
            Value::text("2026-11-06T00:00:00Z"),
        ])));
    }

    #[test]
    fn composite_range_bounds_the_first_column_after_equal_prefix() {
        use reddb_rql::ast::{CompareOp, FieldRef, Filter};

        let equality = |column: &str, value: Value| Filter::Compare {
            field: FieldRef::column("jobs", column),
            op: CompareOp::Eq,
            value,
        };
        let filter = Filter::And(
            Box::new(equality("park_id", Value::Integer(7))),
            Box::new(Filter::And(
                Box::new(equality("environment", Value::text("prod"))),
                Box::new(Filter::Between {
                    field: FieldRef::column("jobs", "completed_at"),
                    low: Value::Integer(100),
                    high: Value::Integer(200),
                }),
            )),
        );
        let columns = [
            "park_id".to_string(),
            "environment".to_string(),
            "completed_at".to_string(),
            "id".to_string(),
            "window_end".to_string(),
        ];
        let range = Range::for_index(&columns, &filter).expect("prefix plus middle range");
        assert_eq!(range.matched_columns, 3);
        let tuple = |values: &[Value]| {
            values
                .iter()
                .flat_map(|value| encode_bound(value).expect("ordered value"))
                .collect::<Vec<_>>()
        };

        for completed_at in [100, 150, 200] {
            assert!(range.contains(&tuple(&[
                Value::Integer(7),
                Value::text("prod"),
                Value::Integer(completed_at),
                Value::Integer(99),
                Value::Integer(999),
            ])));
        }
        assert!(!range.contains(&tuple(&[
            Value::Integer(7),
            Value::text("prod"),
            Value::Integer(201),
            Value::Integer(1),
            Value::Integer(1),
        ])));
        assert!(!range.contains(&tuple(&[
            Value::Integer(8),
            Value::text("prod"),
            Value::Integer(150),
            Value::Integer(1),
            Value::Integer(1),
        ])));
    }

    #[test]
    fn composite_middle_range_merges_overlay_in_order_before_limit() {
        use reddb_rql::ast::{CompareOp, FieldRef, Filter};

        let (directory, runtime, options) = fixture();
        runtime
            .execute_query(
                "CREATE TABLE jobs (park_id INTEGER NOT NULL, environment TEXT NOT NULL, \
                 completed_at INTEGER NOT NULL, id INTEGER NOT NULL, window_end INTEGER NOT NULL)",
            )
            .expect("jobs table");
        runtime
            .execute_query(
                "CREATE INDEX jobs_window ON jobs \
                 (park_id, environment, completed_at, id, window_end) USING BTREE",
            )
            .expect("jobs index");
        for id in 0..256 {
            runtime
                .execute_query(&format!(
                    "INSERT INTO jobs (park_id, environment, completed_at, id, window_end) \
                     VALUES (7, 'prod', {id}, {id}, {})",
                    id + 1_000
                ))
                .expect("job row");
        }
        runtime.flush().expect("jobs checkpoint");
        drop(runtime);

        let runtime = open(&options);
        for statement in [
            "DELETE FROM jobs WHERE id = 103".to_string(),
            "UPDATE jobs SET completed_at = 300 WHERE id = 104".to_string(),
            "INSERT INTO jobs (park_id, environment, completed_at, id, window_end) \
             VALUES (7, 'prod', 105, 1050, 2050)"
                .to_string(),
        ] {
            runtime.execute_query(&statement).expect(&statement);
        }
        let filter = Filter::And(
            Box::new(Filter::Compare {
                field: FieldRef::column("jobs", "park_id"),
                op: CompareOp::Eq,
                value: Value::Integer(7),
            }),
            Box::new(Filter::And(
                Box::new(Filter::Compare {
                    field: FieldRef::column("jobs", "environment"),
                    op: CompareOp::Eq,
                    value: Value::text("prod"),
                }),
                Box::new(Filter::Between {
                    field: FieldRef::column("jobs", "completed_at"),
                    low: Value::Integer(100),
                    high: Value::Integer(110),
                }),
            )),
        );
        let (provider, range) = runtime
            .index_store_ref()
            .select_paged_btree("jobs", &filter)
            .expect("jobs provider");
        let inserted_key = provider
            .projection
            .project_fields(&[
                ("park_id".into(), Value::Integer(7)),
                ("environment".into(), Value::text("prod")),
                ("completed_at".into(), Value::Integer(105)),
                ("id".into(), Value::Integer(1050)),
                ("window_end".into(), Value::Integer(2050)),
            ])
            .expect("insert projection")
            .expect("insert key");
        {
            let overlay = provider.overlay.read();
            assert_eq!(
                overlay.touched.len(),
                4,
                "delete, MVCC update pair and insert must leave authoritative overlay markers"
            );
            assert!(
                overlay.postings.contains_key(&inserted_key),
                "the inserted composite key must be present in the live overlay"
            );
            assert!(range.contains(&inserted_key));
        }
        let candidates = runtime
            .index_store_ref()
            .paged_btree_lookup("jobs", &filter)
            .expect("jobs lookup")
            .expect("jobs index");
        let manager = runtime
            .db()
            .store()
            .get_collection("jobs")
            .expect("jobs manager");
        let mut candidate_ids = candidates
            .iter()
            .map(|entity_id| {
                let entity = manager
                    .try_get(*entity_id, usize::MAX)
                    .expect("candidate read")
                    .expect("candidate entity");
                crate::application::ports::entity_row_fields_snapshot(&entity)
                    .into_iter()
                    .find_map(|(name, value)| (name == "id").then_some(value))
                    .and_then(|value| match value {
                        Value::Integer(id) => Some(id),
                        _ => None,
                    })
                    .expect("candidate id")
            })
            .collect::<Vec<_>>();
        candidate_ids.sort_unstable();
        assert_eq!(
            candidate_ids,
            [100, 101, 102, 105, 106, 107, 108, 109, 110, 1050],
            "the seek must return only the bounded physical and live-overlay entries"
        );
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM jobs WHERE park_id = 7 AND environment = 'prod' \
                 AND completed_at BETWEEN 100 AND 110 \
                 ORDER BY completed_at, id, window_end LIMIT 4 OFFSET 2"
            ),
            [102, 105, 1050, 106]
        );
        drop(runtime);
        drop(directory);
    }

    #[test]
    fn paged_composite_exact_null_prefix_without_trailing_range_never_scans_source() {
        let (_directory, runtime, options) = fixture();
        runtime
            .execute_query("CREATE TABLE monitoring_exports (status TEXT, estimated_at TEXT, received_at TEXT, payload TEXT)")
            .expect("monitoring table");
        runtime
            .execute_query("CREATE INDEX monitoring_pending ON monitoring_exports (status, estimated_at, received_at) USING BTREE")
            .expect("monitoring index");
        for values in [
            "('accepted', NULL, '2026-11-06T00:00:02Z', 'second')",
            "('accepted', NULL, '2026-11-06T00:00:01Z', 'first')",
            "('accepted', '2026-11-06T00:01:00Z', '2026-11-06T00:00:00Z', 'estimated')",
            "('rejected', NULL, '2026-11-06T00:00:00Z', 'rejected')",
        ] {
            runtime
                .execute_query(&format!(
                    "INSERT INTO monitoring_exports (status, estimated_at, received_at, payload) VALUES {values}"
                ))
                .expect("monitoring row");
        }
        runtime.flush().expect("monitoring checkpoint");
        drop(runtime);

        let runtime = open(&options);
        deny_scan_table(&runtime, "monitoring_exports");
        runtime
            .rebuild_runtime_indexes_for_table("monitoring_exports")
            .expect("prepared monitoring metadata-only restore");
        let result = runtime
            .execute_query("SELECT payload FROM monitoring_exports WHERE status = 'accepted' AND estimated_at IS NULL ORDER BY received_at LIMIT 100")
            .expect("pending monitoring exports");
        let payloads: Vec<_> = result
            .result
            .records
            .iter()
            .map(|record| record.get("payload").expect("payload").clone())
            .collect();
        assert_eq!(payloads, [Value::text("first"), Value::text("second")]);
    }

    #[test]
    fn paged_composite_btree_prefix_families_and_candidate_scope_are_exact() {
        let (_directory, runtime, options) = composite_fixture();
        let filter = compound_filter(Value::Integer(1));
        // Prefer the complete compound prefix over the competing id-only
        // range, which would return twelve candidates from three accounts.
        assert_eq!(
            runtime
                .index_store_ref()
                .paged_btree_lookup("compound_3", &filter)
                .expect("lookup")
                .expect("compound provider")
                .len(),
            4
        );
        runtime
            .execute_query("INSERT INTO compound_3 (account, bucket, id) VALUES ('beta', NULL, 80)")
            .expect("other prefix");
        runtime.flush().expect("publish other prefix family");
        assert_eq!(
            runtime
                .index_store_ref()
                .paged_btree_lookup("compound_3", &filter)
                .expect("scoped family")
                .expect("unrelated NULL does not disable alpha prefix")
                .len(),
            4
        );
        runtime
            .execute_query(
                "INSERT INTO compound_3 (account, bucket, id) VALUES ('alpha', NULL, 80)",
            )
            .expect("mixed target prefix");
        // The mixed prefix requires SQL comparison; it must not silently lose
        // a coerced candidate or treat NULL as an ordinary ordered value.
        assert!(runtime
            .index_store_ref()
            .paged_btree_lookup("compound_3", &filter)
            .expect("mixed overlay")
            .is_none());
        runtime.flush().expect("publish mixed target prefix");
        drop(runtime);
        let runtime = open(&options);
        assert!(runtime
            .index_store_ref()
            .paged_btree_lookup("compound_3", &filter)
            .expect("mixed base")
            .is_none());
        assert_eq!(ids(&runtime, "SELECT id FROM compound_3 WHERE account = 'alpha' AND bucket = 1 AND id BETWEEN 9 AND 12 ORDER BY id"), [9,10,11,12]);
        assert_eq!(ids(&runtime, "SELECT id FROM compound_3 WHERE account = 'alpha' AND bucket = 1.0 AND id BETWEEN 9 AND 12 ORDER BY id"), [9,10,11,12]);
    }

    #[test]
    fn paged_composite_btree_unique_overlay_and_rollback_survive_reopen() {
        let (_directory, runtime, options) = composite_fixture();
        runtime
            .execute_query(
                "UPDATE compound_2 SET account = 'moved' WHERE account = 'alpha' AND id = 1",
            )
            .expect("move prefix");
        assert_eq!(ids(&runtime, "SELECT id FROM compound_2 WHERE account = 'alpha' AND id BETWEEN 0 AND 2 ORDER BY id"), [0,2]);
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM compound_2 WHERE account = 'moved' AND id >= 0 ORDER BY id"
            ),
            [1]
        );
        assert!(runtime
            .execute_query("INSERT INTO compound_2 (account, id) VALUES ('moved', 1)")
            .is_err());
        runtime
            .execute_query("INSERT INTO compound_2 (account, id) VALUES ('different', 1)")
            .expect("same suffix, different prefix");
        runtime.execute_query("BEGIN").expect("begin");
        runtime
            .execute_query("UPDATE compound_2 SET id = 9 WHERE account = 'moved' AND id = 1")
            .expect("own write");
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM compound_2 WHERE account = 'moved' AND id >= 0 ORDER BY id"
            ),
            [9]
        );
        runtime.execute_query("ROLLBACK").expect("rollback");
        runtime.flush().expect("publish");
        drop(runtime);
        let runtime = open(&options);
        deny_scan_table(&runtime, "compound_2");
        assert_eq!(
            ids(
                &runtime,
                "SELECT id FROM compound_2 WHERE account = 'moved' AND id >= 0 ORDER BY id"
            ),
            [1]
        );
        assert_eq!(ids(&runtime, "SELECT id FROM compound_2 WHERE account = 'alpha' AND id BETWEEN 0 AND 2 ORDER BY id"), [0,2]);
        assert!(runtime
            .execute_query("INSERT INTO compound_2 (account, id) VALUES ('moved', 1)")
            .is_err());
    }

    #[test]
    fn paged_composite_btree_oversized_tuple_keeps_scan_fallback() {
        let (_directory, runtime, options) = composite_fixture();
        let text = Value::text("z".repeat(600));
        runtime.execute_query_with_params("INSERT INTO compound_5 (account, bucket, category, shard, id) VALUES ('alpha', 1, $1, $2, 80)", &[text.clone(),text.clone()]).expect("combined key larger than native slot");
        runtime.flush().expect("overflow marker");
        drop(runtime);
        let runtime = open(&options);
        let result = runtime.execute_query_with_params("SELECT id FROM compound_5 WHERE account = 'alpha' AND bucket = 1 AND category = $1 AND shard = $2 AND id >= 0", &[text.clone(), text]).expect("overflow fallback");
        assert_eq!(result.result.records.len(), 1);
        assert_eq!(
            result.result.records[0].get("id"),
            Some(&Value::Integer(80))
        );
    }
}
