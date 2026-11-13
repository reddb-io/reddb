//! Relocate only physical addresses inside the authoritative catalog record.
use super::*;
use crate::storage::{unified::UnifiedStore, UnifiedEntity};

pub(super) struct InternalMetadata {
    pub root: u32,
    pub key: Vec<u8>,
    pub document: String,
    entity: UnifiedEntity,
    metadata: Option<crate::storage::unified::metadata::Metadata>,
    version: u32,
}

pub(super) fn read(source: &Arc<Source>, catalog: &Catalog) -> Result<Option<InternalMetadata>> {
    let Some(root) = catalog
        .roots
        .iter()
        .find(|root| root.collection == "red.storage.metadata")
    else {
        return Ok(None);
    };
    if root.root_page == 0 {
        return Ok(None);
    }
    let tree = source.tree(root.root_page)?;
    let mut cursor = tree.cursor_first()?;
    let mut latest = None;
    while let Some((key, value)) = cursor.next_with_budget(32 * 1024 * 1024)? {
        let (entity, metadata) =
            UnifiedStore::deserialize_entity_record(&value, catalog.header.format_version)?;
        let Some(row) = entity.data.as_row() else {
            continue;
        };
        if row.get_field("key") != Some(&reddb_types::Value::text("physical")) {
            continue;
        }
        let Some(reddb_types::Value::Text(document)) = row.get_field("value") else {
            return Err("invalid internal physical metadata record".into());
        };
        // Physical identities are monotonically ordered; latest record wins,
        // matching the runtime's interrupted upsert recovery semantics.
        latest = Some(InternalMetadata {
            root: root.root_page,
            key,
            document: document.to_string(),
            entity,
            metadata,
            version: catalog.header.format_version,
        });
    }
    Ok(latest)
}

impl InternalMetadata {
    pub fn rewrite(mut self, document: String) -> Result<Vec<u8>> {
        let crate::storage::EntityData::Row(row) = &mut self.entity.data else {
            return Err("physical metadata is not a row".into());
        };
        let value = reddb_types::Value::text(document);
        if let Some(named) = &mut row.named {
            named.insert("value".into(), value.clone());
        }
        if let Some(schema) = &row.schema {
            let index = schema
                .iter()
                .position(|name| name == "value")
                .ok_or("physical metadata has no value column")?;
            row.columns[index] = value;
        }
        Ok(UnifiedStore::serialize_entity_record(
            &self.entity,
            self.metadata.as_ref(),
            self.version,
        ))
    }
}
