//! Proof for retiring obsolete, boot-derived configuration on an isolated copy.
//! Positions refer to the original WAL: no frame, transaction or ownership
//! record is rewritten. Ordinary opens never receive this proof.
use super::*;
use crate::storage::wal::{WalReader, WalRecord};
use crate::storage::{unified::UnifiedStore, EntityId, EntityKind, UnifiedEntity};
use std::collections::{BTreeSet, HashMap};

const MAX_CONFIG_RECORDS: usize = 100_000;
const LEGACY_WAL_ERROR: &str = "WAL references divergent legacy red_config";

#[derive(Default)]
pub(crate) struct BootSystemWalPlan {
    actions: BTreeSet<(u64, usize)>,
    entity_next: u64,
    row_next: u64,
    sequence_next: u64,
    xid_high: u64,
}

fn boot_key(entity: &UnifiedEntity) -> Option<&'static str> {
    let EntityKind::TableRow { table, .. } = &entity.kind else {
        return None;
    };
    if !matches!(table.as_ref(), "red_config" | "red.config")
        || !entity.cross_refs().is_empty()
        || !entity.embeddings().is_empty()
    {
        return None;
    }
    let row = entity.data.as_row()?;
    let reddb_types::Value::Text(key) = row.get_field("key")? else {
        return None;
    };
    row.get_field("value")?;
    // Exactly the process information recreated by runtime/impl_lifecycle.rs.
    match key.as_ref() {
        "system.pid" => Some("system.pid"),
        "system.cpu_cores" => Some("system.cpu_cores"),
        "system.total_memory_bytes" => Some("system.total_memory_bytes"),
        "system.available_memory_bytes" => Some("system.available_memory_bytes"),
        "system.os" => Some("system.os"),
        "system.arch" => Some("system.arch"),
        "system.hostname" => Some("system.hostname"),
        "system.started_at" => Some("system.started_at"),
        _ => None,
    }
}

fn wal_shape(wal: &Path) -> Result<(bool, bool)> {
    use f::StoreWalActionFrame as Action;
    let mut has_legacy_actions = false;
    let mut has_page_images = false;
    for item in WalReader::open(wal)?.iter() {
        match item?.1 {
            WalRecord::TxCommitBatch { actions, .. } => {
                for bytes in actions {
                    has_legacy_actions |= matches!(
                        f::decode_store_wal_action_frame(&bytes)?,
                        Action::UpsertEntityRecord { collection, .. }
                            | Action::DeleteEntityRecord { collection, .. }
                            if collection == "red_config"
                    );
                }
            }
            WalRecord::FullPageImage { .. } => has_page_images = true,
            _ => {}
        }
    }
    Ok((has_legacy_actions, has_page_images))
}

impl BootSystemWalPlan {
    pub(super) fn prepare(source: &Arc<Source>, wal: &Path, pending_engine: bool) -> Result<Self> {
        use f::StoreWalActionFrame as Action;
        let (has_legacy_actions, has_page_images) = wal_shape(wal)?;
        let mut plan = Self::default();
        if !has_legacy_actions {
            return Ok(plan);
        }
        ensure(
            !pending_engine
                && !source.pending_double_write
                && !source.header.checkpoint_in_progress
                && !has_page_images,
            "legacy boot WAL proof requires a stable checkpoint image",
        )?;
        let (version, mut identities) = plan.checkpoint_identities(source)?;
        for item in WalReader::open(wal)?.iter() {
            let (lsn, record) = item?;
            let WalRecord::TxCommitBatch { tx_id, actions } = record else {
                continue;
            };
            for (index, bytes) in actions.into_iter().enumerate() {
                match f::decode_store_wal_action_frame(&bytes)? {
                    Action::UpsertEntityRecord { collection, record }
                        if collection == "red_config" =>
                    {
                        let (entity, _) =
                            reddb_engine::storage::metadata_codec::deserialize_entity_record(
                                &record, version,
                            )?;
                        let key = boot_key(&entity).ok_or(LEGACY_WAL_ERROR)?;
                        if let Some(existing) = identities.get(&entity.id.raw()) {
                            ensure(*existing == Some(key), LEGACY_WAL_ERROR)?;
                        }
                        ensure(
                            identities.len() < MAX_CONFIG_RECORDS,
                            "legacy config proof exceeds budget",
                        )?;
                        identities.insert(entity.id.raw(), Some(key));
                        plan.remember(&entity)?;
                    }
                    Action::DeleteEntityRecord {
                        collection,
                        entity_id,
                    } if collection == "red_config" => {
                        ensure(
                            identities.remove(&entity_id).flatten().is_some(),
                            LEGACY_WAL_ERROR,
                        )?;
                    }
                    _ => continue,
                }
                ensure(
                    plan.actions.len() < MAX_CONFIG_RECORDS,
                    "legacy WAL proof exceeds budget",
                )?;
                plan.actions.insert((lsn, index));
                plan.xid_high = plan.xid_high.max(tx_id);
            }
        }
        ensure(
            plan.xid_high < u64::MAX,
            "recovered transaction IDs are exhausted",
        )?;
        Ok(plan)
    }

    fn checkpoint_identities(
        &mut self,
        source: &Arc<Source>,
    ) -> Result<(u32, HashMap<u64, Option<&'static str>>)> {
        let mut catalog = Catalog::read(source)?;
        let legacy_root = catalog
            .roots
            .iter()
            .find(|root| root.collection == "red_config")
            .ok_or(LEGACY_WAL_ERROR)?
            .root_page;
        let version = catalog.header.format_version;
        if let Some(resume) = catalog
            .resume
            .as_ref()
            .and_then(|r| r.collections.get("red_config"))
        {
            self.entity_next = resume.entity_id_next;
            self.row_next = resume.row_id_next;
            self.sequence_next = resume.sequence_id_next;
        }
        catalog.retain_canonical_system_config()?;
        let mut identities = HashMap::new();
        if legacy_root != 0 {
            let tree = source.tree(legacy_root)?;
            let mut cursor = tree.cursor_first()?;
            while let Some((key, bytes)) = cursor.next_with_budget(32 * 1024 * 1024)? {
                ensure(
                    identities.len() < MAX_CONFIG_RECORDS,
                    "legacy config proof exceeds budget",
                )?;
                let (entity, _) = reddb_engine::storage::metadata_codec::deserialize_entity_record(
                    &bytes, version,
                )?;
                ensure(
                    key.as_slice() == entity.id.raw().to_be_bytes(),
                    "legacy config record identity mismatch",
                )?;
                self.remember(&entity)?;
                ensure(
                    identities
                        .insert(entity.id.raw(), boot_key(&entity))
                        .is_none(),
                    "duplicate legacy config identity",
                )?;
            }
        }
        Ok((version, identities))
    }

    fn remember(&mut self, entity: &UnifiedEntity) -> Result<()> {
        self.entity_next = self.entity_next.max(
            entity
                .id
                .raw()
                .max(entity.logical_id().raw())
                .checked_add(1)
                .ok_or("recovered entity IDs are exhausted")?,
        );
        if let EntityKind::TableRow { row_id, .. } = &entity.kind {
            self.row_next = self.row_next.max(
                row_id
                    .checked_add(1)
                    .ok_or("recovered row IDs are exhausted")?,
            );
        }
        self.sequence_next = self.sequence_next.max(
            entity
                .sequence_id
                .checked_add(1)
                .ok_or("recovered sequence IDs are exhausted")?,
        );
        self.xid_high = self.xid_high.max(entity.max_transaction_id());
        Ok(())
    }

    pub(super) fn action_count(&self) -> usize {
        self.actions.len()
    }

    pub(crate) fn skips_action(&self, lsn: u64, index: usize) -> bool {
        self.actions.contains(&(lsn, index))
    }

    pub(super) fn preserve_identity_floors(&self, store: &UnifiedStore) -> Result<()> {
        if self.actions.is_empty() {
            return Ok(());
        }
        let manager = store
            .get_collection("red.config")
            .ok_or("canonical config disappeared")?;
        manager.restore_identity_counters(self.entity_next.max(1), self.row_next.max(1));
        manager.restore_sequence_id_next(self.sequence_next.max(1));
        store.register_entity_id(EntityId::new(self.entity_next.saturating_sub(1)));
        store.remember_startup_xid_high_water_mark(
            store
                .startup_xid_high_water_mark()
                .unwrap_or(0)
                .max(self.xid_high),
        );
        Ok(())
    }
}
