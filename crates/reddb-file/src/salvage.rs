use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{
    decode_native_store_header, verify_native_store_crc32_footer, EmbeddedRdbArtifact,
    RdbFileError, RdbFileResult,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageSalvageMode {
    Manifest,
    Carving,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageSalvageCollection {
    pub collection: String,
    pub recovered_entities: u64,
    pub skipped_entities: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageSalvageSkippedRegion {
    pub zone_kind: String,
    pub physical_identity: String,
    pub reason: String,
    pub collection: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageSalvageReport {
    pub schema_version: u32,
    pub mode: StorageSalvageMode,
    pub collections: Vec<StorageSalvageCollection>,
    pub skipped_regions: Vec<StorageSalvageSkippedRegion>,
}

impl StorageSalvageReport {
    pub fn machine_json(&self) -> RdbFileResult<String> {
        serde_json::to_string(self).map_err(|err| {
            RdbFileError::InvalidOperation(format!("serialize salvage report: {err}"))
        })
    }

    pub fn human_summary(&self) -> String {
        let mode = match self.mode {
            StorageSalvageMode::Manifest => "manifest",
            StorageSalvageMode::Carving => "carving",
        };
        let mut lines = vec![format!(
            "Salvage completed in {mode} mode: {} skipped region(s).",
            self.skipped_regions.len()
        )];
        for collection in &self.collections {
            lines.push(format!(
                "{}: recovered {} entity artifact(s), skipped {}.",
                collection.collection, collection.recovered_entities, collection.skipped_entities
            ));
        }
        if self.skipped_regions.is_empty() {
            lines.push("Next steps: open the recovered store and run the query corpus before replacing any production copy.".to_string());
        } else {
            lines.push("Next steps: inspect the skipped-region list, validate the recovered store, and restore missing entities from backup or upstream systems.".to_string());
        }
        lines.join("\n")
    }
}

pub fn salvage_embedded_store(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
) -> RdbFileResult<StorageSalvageReport> {
    let source = source.as_ref();
    let destination = destination.as_ref();
    if destination.exists() {
        return Err(RdbFileError::InvalidOperation(format!(
            "salvage destination already exists: {}",
            destination.display()
        )));
    }

    let open = match EmbeddedRdbArtifact::open_strict_manifest(source) {
        Ok(open) => open,
        Err(_) => return salvage_by_carving(source, destination),
    };
    let mut skipped_regions = Vec::new();
    let mut skipped_entities = 0;
    let snapshot = match EmbeddedRdbArtifact::read_snapshot(&open) {
        Ok(snapshot) => snapshot.unwrap_or_default(),
        Err(_) => {
            skipped_entities = 1;
            skipped_regions.push(StorageSalvageSkippedRegion {
                zone_kind: "page".to_string(),
                physical_identity: "snapshot".to_string(),
                reason: "checksum mismatch".to_string(),
                collection: Some("embedded_snapshot".to_string()),
            });
            Vec::new()
        }
    };
    let wal_payloads = if skipped_entities == 0 {
        match EmbeddedRdbArtifact::read_wal_payloads(&open) {
            Ok(payloads) => payloads,
            Err(_) => {
                skipped_regions.push(StorageSalvageSkippedRegion {
                    zone_kind: "wal".to_string(),
                    physical_identity: "embedded-wal".to_string(),
                    reason: "checksum mismatch".to_string(),
                    collection: Some("embedded_snapshot".to_string()),
                });
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    EmbeddedRdbArtifact::create_with_snapshot(destination, &snapshot)?;
    if !wal_payloads.is_empty() {
        EmbeddedRdbArtifact::append_wal_payloads(destination, &wal_payloads)?;
    }

    Ok(StorageSalvageReport {
        schema_version: 1,
        mode: StorageSalvageMode::Manifest,
        collections: vec![StorageSalvageCollection {
            collection: "embedded_snapshot".to_string(),
            recovered_entities: recovered_entity_artifacts(
                !snapshot.is_empty(),
                wal_payloads.len(),
            ),
            skipped_entities,
        }],
        skipped_regions,
    })
}

fn recovered_entity_artifacts(has_snapshot: bool, wal_payload_count: usize) -> u64 {
    u64::from(has_snapshot) + u64::try_from(wal_payload_count).unwrap_or(u64::MAX)
}

fn salvage_by_carving(source: &Path, destination: &Path) -> RdbFileResult<StorageSalvageReport> {
    let source_bytes = fs::read(source)?;
    let snapshot = carve_verified_native_snapshot(&source_bytes).ok_or_else(|| {
        RdbFileError::InvalidOperation(
            "salvage carving found no checksum-valid embedded snapshot".to_string(),
        )
    })?;

    EmbeddedRdbArtifact::create_with_snapshot(destination, &snapshot)?;
    Ok(StorageSalvageReport {
        schema_version: 1,
        mode: StorageSalvageMode::Carving,
        collections: vec![StorageSalvageCollection {
            collection: "embedded_snapshot".to_string(),
            recovered_entities: 1,
            skipped_entities: 0,
        }],
        skipped_regions: vec![
            StorageSalvageSkippedRegion {
                zone_kind: "superblock".to_string(),
                physical_identity: "superblock:all".to_string(),
                reason: "checksum mismatch".to_string(),
                collection: None,
            },
            StorageSalvageSkippedRegion {
                zone_kind: "manifest".to_string(),
                physical_identity: "manifest:all".to_string(),
                reason: "unreachable from manifest".to_string(),
                collection: None,
            },
            StorageSalvageSkippedRegion {
                zone_kind: "wal".to_string(),
                physical_identity: "embedded-wal".to_string(),
                reason: "unreachable from manifest".to_string(),
                collection: None,
            },
        ],
    })
}

fn carve_verified_native_snapshot(source_bytes: &[u8]) -> Option<Vec<u8>> {
    if source_bytes.len() < crate::native_store::STORE_MAGIC.len() {
        return None;
    }
    let mut carved = None;
    let last_offset = source_bytes
        .len()
        .saturating_sub(crate::native_store::STORE_MAGIC.len());
    for offset in 0..=last_offset {
        if &source_bytes[offset..offset + crate::native_store::STORE_MAGIC.len()]
            != crate::native_store::STORE_MAGIC
        {
            continue;
        }
        let candidate = source_bytes[offset..].to_vec();
        if native_snapshot_verifies(&candidate) {
            carved = Some(candidate);
        }
    }
    carved
}

fn native_snapshot_verifies(candidate: &[u8]) -> bool {
    let Ok(version) = decode_native_store_header(candidate) else {
        return false;
    };
    let mut check = candidate.to_vec();
    verify_native_store_crc32_footer(&mut check, version).is_ok()
}
