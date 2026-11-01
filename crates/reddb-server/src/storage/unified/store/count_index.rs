//! Exact visibility projection for payload-free scalar counts.
//!
//! Preparation shares the secondary-index source pass and publication fences.
//! Cost: 17 visibility bytes + 8 physical-ID bytes per version (plus tree slots),
//! O(N) compact reads per count, O(one leaf) scratch, no resident posting map.
use super::secondary_index::{
    SecondaryIndexIdentity, SecondaryIndexProjector, SECONDARY_KEY_BYTES_MAX,
};
use super::*;
use reddb_engine::storage::visibility::entity_moderation_hidden;
use reddb_engine::transaction::read::{visibility_check, SnapshotContext};
use sha2::{Digest, Sha256};

// Separate from SQL-visible registry names; a different definition can never
// accidentally qualify because reads also verify the full fingerprint.
const NAME: &str = "\0reddb/visibility-count-v1";

fn identity(schema: Option<&[String]>) -> SecondaryIndexIdentity {
    let mut digest = Sha256::new();
    digest.update(b"reddb:count:xmin-be64:xmax-be64:moderation-u8:v1");
    digest.update([u8::from(schema.is_some())]);
    for name in schema.unwrap_or_default() {
        digest.update((name.len() as u64).to_be_bytes());
        digest.update(name.as_bytes());
    }
    SecondaryIndexIdentity {
        codec_version: 1,
        definition: digest.finalize().into(),
    }
}

struct VisibilityProjection {
    schema: Option<Arc<Vec<String>>>,
}

fn moderation_hidden(entity: &UnifiedEntity, schema: Option<&[String]>) -> bool {
    use reddb_engine::storage::visibility::{
        MODERATION_STATUS_FIELD, MODERATION_STATUS_PENDING, MODERATION_STATUS_REJECTED,
    };
    if entity_moderation_hidden(entity) {
        return true;
    }
    let EntityData::Row(row) = &entity.data else {
        return false;
    };
    if row.named.is_some() || row.schema.is_some() {
        return false;
    }
    let marker = schema
        .and_then(|schema| {
            schema
                .iter()
                .position(|name| name == MODERATION_STATUS_FIELD)
        })
        .and_then(|position| row.columns.get(position));
    matches!(marker, Some(Value::Text(status)) if status.as_ref() == MODERATION_STATUS_PENDING || status.as_ref() == MODERATION_STATUS_REJECTED)
}
impl SecondaryIndexProjector for VisibilityProjection {
    fn project(
        &self,
        entity: &UnifiedEntity,
        key: &mut [u8; SECONDARY_KEY_BYTES_MAX],
    ) -> Result<Option<usize>, StoreError> {
        key[..8].copy_from_slice(&entity.xmin.to_be_bytes());
        key[8..16].copy_from_slice(&entity.xmax.to_be_bytes());
        key[16] = u8::from(moderation_hidden(
            entity,
            self.schema.as_deref().map(Vec::as_slice),
        ));
        Ok(Some(17))
    }
}

fn visible(snapshot: Option<&SnapshotContext>, xmin: u64, xmax: u64, hidden: bool) -> bool {
    !hidden && snapshot.map_or(xmax == 0, |context| visibility_check(context, xmin, xmax))
}

impl UnifiedStore {
    /// Only the opt-in cold runtime prepares these indexes. Ordinary stores
    /// retain their current formats and scans. Missing projections are safe
    /// scan fallbacks until the next preparation checkpoint.
    pub(crate) fn register_count_index(&self, collection: &str) -> Result<(), StoreError> {
        let schema = self
            .get_collection(collection)
            .and_then(|manager| manager.column_schema());
        self.register_secondary_index(
            collection,
            NAME,
            identity(schema.as_deref().map(Vec::as_slice)),
            Arc::new(VisibilityProjection { schema }),
        )
    }

    /// None means the ordinary entity scan is required. In particular SSI
    /// readers retain per-entity logical read tracking on that existing path.
    pub(crate) fn try_count_from_index(
        &self,
        collection: &str,
        snapshot: Option<&SnapshotContext>,
    ) -> Result<Option<u64>, StoreError> {
        if snapshot.is_some_and(|context| context.serializable_reader.is_some()) {
            return Ok(None);
        }
        let _publication = self.vault_publication_lock.lock();
        let Some(manager) = self.get_collection(collection) else {
            return Ok(None);
        };
        let Some(root) = manager.backing_root_page() else {
            return Ok(None);
        };
        if self.collection_root_page(collection) != Some(root) {
            return Ok(None);
        }
        let schema = manager.column_schema();
        let Some(index) = self.secondary_index(
            collection,
            NAME,
            &identity(schema.as_deref().map(Vec::as_slice)),
        ) else {
            return Ok(None);
        };
        let mut count = 0u64;
        index.try_for_each(|key, id| {
            if key.len() != 17 || key[16] > 1 {
                return Err(StoreError::Serialization(
                    "invalid COUNT visibility projection".into(),
                ));
            }
            if !manager.backing_row_is_shadowed(id) {
                let xmin = u64::from_be_bytes(key[..8].try_into().expect("eight-byte xmin"));
                let xmax = u64::from_be_bytes(key[8..16].try_into().expect("eight-byte xmax"));
                count += u64::from(visible(snapshot, xmin, xmax, key[16] != 0));
            }
            Ok(())
        })?;
        // Resident iteration contains only WAL changes. Replacements were
        // excluded above, so no dedup set or payload clone is necessary.
        manager.for_each_entity(|entity| {
            count += u64::from(visible(
                snapshot,
                entity.xmin,
                entity.xmax,
                moderation_hidden(entity, schema.as_deref().map(Vec::as_slice)),
            ));
            true
        });
        Ok(Some(count))
    }
}

#[cfg(test)]
mod tests;
