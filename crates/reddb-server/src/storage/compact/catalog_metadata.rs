//! Carry a proven catalog reconciliation into the complete in-file metadata.
use super::*;
use crate::serde_json::{self, Value};
use crate::storage::unified::UnifiedStore;

pub(super) fn persist_reconciled_metadata(
    store: &UnifiedStore,
    path: &Path,
    aliases: &[(&str, &str)],
) -> Result<()> {
    let binary = f::physical_metadata_binary_path(path);
    let json = f::physical_metadata_json_path(path);
    let text = match store.read_physical_metadata_document()? {
        Some(document) => document,
        None => {
            let source = if binary.exists() {
                binary
            } else if json.exists() {
                json
            } else {
                return Ok(());
            };
            ensure(
                std::fs::metadata(&source)?.len()
                    <= f::native_store::NATIVE_METADATA_DOCUMENT_MAX_BYTES,
                "physical metadata exceeds recovery budget",
            )?;
            f::read_physical_metadata_document(&source)?
        }
    };
    let mut document: Value = serde_json::from_str(&text)?;
    // get_collection canonicalizes legacy spellings, so it cannot prove that
    // the retired physical identity is absent. Inspect exact catalog names.
    let names = store.list_collections();
    for &(legacy, canonical) in aliases {
        ensure(
            !names.iter().any(|name| name == legacy) && names.iter().any(|name| name == canonical),
            "metadata reconciliation requires the proven surviving collection",
        )?;
        for keys in [
            ["catalog", "stats_by_collection"],
            ["superblock", "collection_roots"],
        ] {
            if let Value::Object(map) = &mut document[keys[0]][keys[1]] {
                if map.contains_key(canonical) {
                    map.remove(legacy);
                }
            }
        }
        if let Value::Object(map) = &mut document["collection_ttl_defaults_ms"] {
            if let (Some(a), Some(b)) = (map.get(legacy), map.get(canonical)) {
                ensure(a == b, "conflicting collection TTL metadata")?;
                map.remove(legacy);
            }
        }
        if let Value::Array(contracts) = &mut document["collection_contracts"] {
            if let Some(canonical_contract) = contracts
                .iter()
                .find(|c| c["name"].as_str() == Some(canonical))
                .cloned()
            {
                if legacy != "red_config" {
                    for contract in contracts
                        .iter()
                        .filter(|c| c["name"].as_str() == Some(legacy))
                    {
                        let mut normalized = contract.clone();
                        normalized["name"] = Value::String(canonical.into());
                        ensure(
                            normalized == canonical_contract,
                            "conflicting collection contract metadata",
                        )?;
                    }
                }
                contracts.retain(|c| c["name"].as_str() != Some(legacy));
            }
        }
        if let Value::Array(indexes) = &mut document["indexes"] {
            for index in indexes
                .iter()
                .filter(|index| index["collection"].as_str() == Some(legacy))
            {
                ensure(
                    index["artifact_root_page"] == Value::Null,
                    "retired collection owns an index artifact requiring separate recovery",
                )?;
            }
            // Runtime index declarations were reconciled by CanonicalCatalogPlan.
            // These descriptors only describe projections of the retired tree.
            indexes.retain(|index| index["collection"].as_str() != Some(legacy));
        }
    }
    let metadata = crate::physical::PhysicalMetadataFile::from_json_value(&crate::json::from_str(
        &serde_json::to_string(&document)?,
    )?)?;
    let mut header = store
        .physical_file_header()
        .ok_or("physical header missing")?;
    let summary = f::NativeMetadataStateSummary {
        protocol_version: metadata.protocol_version.clone(),
        generated_at_unix_ms: metadata.generated_at_unix_ms,
        last_loaded_from: metadata.last_loaded_from.clone(),
        last_healed_at_unix_ms: metadata.last_healed_at_unix_ms,
    };
    let (page, checksum) = store.write_physical_metadata_document(
        &summary,
        &metadata.to_json_value().to_string_compact(),
        (header.metadata_state_page != 0).then_some(header.metadata_state_page),
    )?;
    header.metadata_state_page = page;
    header.metadata_state_checksum = checksum;
    store.update_physical_file_header(header)?;
    Ok(())
}
