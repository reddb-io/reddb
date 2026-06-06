//! Serverless segment pack export/hydrate for checkpointed `.rdb` files.
//!
//! A segment pack is a derived artifact: the canonical database remains the
//! `.rdb` file. The pack stores immutable byte parts plus a manifest with
//! checksums and a recovery boundary that says hydration needs no WAL replay
//! beyond the checkpointed file bytes.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::json::{Map, Value as JsonValue};
use crate::storage::wal::sha256_file_hex;

pub const SEGMENT_PACK_MANIFEST_FILE: &str = "manifest.json";
pub const SEGMENT_PACK_FORMAT: &str = "reddb.serverless.segment-pack";
pub const SEGMENT_PACK_VERSION: u32 = 1;
pub const DEFAULT_SEGMENT_PART_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentPackPart {
    pub name: String,
    pub target_path: String,
    pub offset: u64,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentPackRecoveryBoundary {
    pub kind: String,
    pub base_lsn: u64,
    pub wal_segments_required: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentPackManifest {
    pub format: String,
    pub version: u32,
    pub engine_version: String,
    pub created_at_ms: u64,
    pub source_size_bytes: u64,
    pub source_sha256: String,
    pub recovery_boundary: SegmentPackRecoveryBoundary,
    pub parts: Vec<SegmentPackPart>,
    pub manifest_sha256: String,
}

#[derive(Debug)]
pub enum SegmentPackError {
    Io(std::io::Error),
    Invalid(String),
}

impl fmt::Display for SegmentPackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::Invalid(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for SegmentPackError {}

impl From<std::io::Error> for SegmentPackError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

pub type SegmentPackResult<T> = Result<T, SegmentPackError>;

/// Export a checkpointed canonical `.rdb` into immutable segment parts plus a manifest.
pub fn export_segment_pack(
    source_rdb: impl AsRef<Path>,
    pack_dir: impl AsRef<Path>,
) -> SegmentPackResult<SegmentPackManifest> {
    export_segment_pack_with_part_size(source_rdb, pack_dir, DEFAULT_SEGMENT_PART_BYTES)
}

pub fn export_segment_pack_with_part_size(
    source_rdb: impl AsRef<Path>,
    pack_dir: impl AsRef<Path>,
    part_size_bytes: u64,
) -> SegmentPackResult<SegmentPackManifest> {
    let source_rdb = source_rdb.as_ref();
    let pack_dir = pack_dir.as_ref();
    if part_size_bytes == 0 {
        return Err(SegmentPackError::Invalid(
            "segment pack part size must be non-zero".to_string(),
        ));
    }
    if !source_rdb.is_file() {
        return Err(SegmentPackError::Invalid(format!(
            "source .rdb does not exist: {}",
            source_rdb.display()
        )));
    }

    fs::create_dir_all(pack_dir)?;
    let parts_dir = pack_dir.join("parts");
    if parts_dir.exists() {
        fs::remove_dir_all(&parts_dir)?;
    }
    fs::create_dir_all(&parts_dir)?;

    let source_size_bytes = fs::metadata(source_rdb)?.len();
    let source_sha256 = sha256_file_hex(source_rdb)
        .map_err(|err| SegmentPackError::Invalid(format!("source checksum failed: {err}")))?;

    let mut parts = Vec::new();
    let mut index = export_file_parts(
        source_rdb,
        "data.rdb",
        &parts_dir,
        part_size_bytes,
        0,
        &mut parts,
    )?;
    let ops_dir = ops_dir_for_db_path(source_rdb);
    if ops_dir.is_dir() {
        let mut files = Vec::new();
        collect_files(&ops_dir, &mut files)?;
        files.sort();
        for file in files {
            let relative = file.strip_prefix(&ops_dir).map_err(|err| {
                SegmentPackError::Invalid(format!("sidecar path is outside ops dir: {err}"))
            })?;
            let target_path = target_path_to_string(&Path::new("data.rdb.ops").join(relative))?;
            index = export_file_parts(
                &file,
                &target_path,
                &parts_dir,
                part_size_bytes,
                index,
                &mut parts,
            )?;
        }
    }
    index = export_related_path(
        &appended_path(source_rdb, ".meta.rdbx"),
        Path::new("data.rdb.meta.rdbx"),
        &parts_dir,
        part_size_bytes,
        index,
        &mut parts,
    )?;
    index = export_related_path(
        &appended_path(source_rdb, ".red"),
        Path::new("data.rdb.red"),
        &parts_dir,
        part_size_bytes,
        index,
        &mut parts,
    )?;
    index = export_related_path(
        &source_rdb.with_extension("result-cache.l2"),
        Path::new("data.result-cache.l2"),
        &parts_dir,
        part_size_bytes,
        index,
        &mut parts,
    )?;
    let _ = export_related_path(
        &source_rdb.with_extension("result-cache.l2-dwb"),
        Path::new("data.result-cache.l2-dwb"),
        &parts_dir,
        part_size_bytes,
        index,
        &mut parts,
    )?;

    let created_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let manifest = SegmentPackManifest::new(
        created_at_ms,
        source_size_bytes,
        source_sha256,
        SegmentPackRecoveryBoundary {
            kind: "checkpointed-rdb".to_string(),
            base_lsn: 0,
            wal_segments_required: 0,
        },
        parts,
    );
    write_manifest_atomic(pack_dir, &manifest)?;
    sync_dir(pack_dir);
    Ok(manifest)
}

fn export_related_path(
    source: &Path,
    target: &Path,
    parts_dir: &Path,
    part_size_bytes: u64,
    mut index: u64,
    parts: &mut Vec<SegmentPackPart>,
) -> SegmentPackResult<u64> {
    if source.is_file() {
        let target_path = target_path_to_string(target)?;
        return export_file_parts(
            source,
            &target_path,
            parts_dir,
            part_size_bytes,
            index,
            parts,
        );
    }
    if source.is_dir() {
        let mut files = Vec::new();
        collect_files(source, &mut files)?;
        files.sort();
        for file in files {
            let relative = file.strip_prefix(source).map_err(|err| {
                SegmentPackError::Invalid(format!("related path is outside root: {err}"))
            })?;
            let target_path = target_path_to_string(&target.join(relative))?;
            index = export_file_parts(
                &file,
                &target_path,
                parts_dir,
                part_size_bytes,
                index,
                parts,
            )?;
        }
    }
    Ok(index)
}

fn export_file_parts(
    source_path: &Path,
    target_path: &str,
    parts_dir: &Path,
    part_size_bytes: u64,
    mut index: u64,
    parts: &mut Vec<SegmentPackPart>,
) -> SegmentPackResult<u64> {
    let mut source = File::open(source_path)?;
    let mut offset = 0u64;
    let mut buf = vec![0u8; part_size_bytes.min(1024 * 1024) as usize];
    let mut current_part: Option<(File, PathBuf, crate::crypto::sha256::Sha256, u64)> = None;
    let mut emitted_part = false;

    loop {
        let n = source.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let mut consumed = 0usize;
        while consumed < n {
            if current_part.is_none() {
                let name = format!("part-{index:08}.rdbseg");
                let path = parts_dir.join(&name);
                let file = OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&path)?;
                current_part = Some((file, path, crate::crypto::sha256::Sha256::new(), 0));
            }
            let (file, path, hasher, written) = current_part.as_mut().unwrap();
            let remaining = (part_size_bytes - *written) as usize;
            let take = remaining.min(n - consumed);
            file.write_all(&buf[consumed..consumed + take])?;
            hasher.update(&buf[consumed..consumed + take]);
            *written += take as u64;
            consumed += take;

            if *written == part_size_bytes {
                file.sync_all()?;
                let (_, path, hasher, size_bytes) = current_part.take().unwrap();
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| SegmentPackError::Invalid("part name is not UTF-8".to_string()))?
                    .to_string();
                parts.push(SegmentPackPart {
                    name,
                    target_path: target_path.to_string(),
                    offset,
                    size_bytes,
                    sha256: crate::utils::to_hex(&hasher.finalize()),
                });
                offset += size_bytes;
                emitted_part = true;
                index += 1;
            }
        }
    }

    if let Some((file, path, hasher, size_bytes)) = current_part.take() {
        file.sync_all()?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| SegmentPackError::Invalid("part name is not UTF-8".to_string()))?
            .to_string();
        parts.push(SegmentPackPart {
            name,
            target_path: target_path.to_string(),
            offset,
            size_bytes,
            sha256: crate::utils::to_hex(&hasher.finalize()),
        });
        emitted_part = true;
        index += 1;
    }

    if !emitted_part {
        let name = format!("part-{index:08}.rdbseg");
        let path = parts_dir.join(&name);
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?
            .sync_all()?;
        parts.push(SegmentPackPart {
            name,
            target_path: target_path.to_string(),
            offset: 0,
            size_bytes: 0,
            sha256: crate::utils::to_hex(&crate::crypto::sha256::sha256(&[])),
        });
        index += 1;
    }
    Ok(index)
}

/// Validate a segment pack and hydrate it back into a canonical `.rdb` file.
pub fn hydrate_segment_pack(
    pack_dir: impl AsRef<Path>,
    dest_rdb: impl AsRef<Path>,
) -> SegmentPackResult<SegmentPackManifest> {
    let pack_dir = pack_dir.as_ref();
    let dest_rdb = dest_rdb.as_ref();
    if dest_rdb.exists() {
        return Err(SegmentPackError::Invalid(format!(
            "destination already exists: {}",
            dest_rdb.display()
        )));
    }
    let manifest = load_segment_pack_manifest(pack_dir)?;
    validate_segment_pack(pack_dir, &manifest)?;
    if let Some(parent) = dest_rdb.parent() {
        fs::create_dir_all(parent)?;
    }
    let file_name = dest_rdb
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| SegmentPackError::Invalid("destination file name is not UTF-8".into()))?;
    let temp = dest_rdb.with_file_name(format!(".{file_name}.hydrate.tmp"));
    if temp.exists() {
        fs::remove_file(&temp)?;
    }

    let hydrate_result = (|| -> SegmentPackResult<()> {
        let parts_dir = pack_dir.join("parts");
        let mut open_target: Option<(String, File)> = None;
        for part in &manifest.parts {
            let output_path = if part.target_path == "data.rdb" {
                temp.clone()
            } else {
                hydrate_target_path(dest_rdb, &part.target_path)?
            };
            if open_target
                .as_ref()
                .is_none_or(|(target, _)| target != &part.target_path)
            {
                if let Some((_, file)) = open_target.take() {
                    file.sync_all()?;
                }
                if let Some(parent) = output_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                open_target = Some((
                    part.target_path.clone(),
                    OpenOptions::new()
                        .create_new(true)
                        .write(true)
                        .open(&output_path)?,
                ));
            }
            let mut input = File::open(parts_dir.join(&part.name))?;
            let (_, out) = open_target.as_mut().unwrap();
            std::io::copy(&mut input, out)?;
        }
        if let Some((_, file)) = open_target.take() {
            file.sync_all()?;
        }
        let hydrated_sha = sha256_file_hex(&temp)
            .map_err(|err| SegmentPackError::Invalid(format!("hydrated checksum failed: {err}")))?;
        if hydrated_sha != manifest.source_sha256 {
            return Err(SegmentPackError::Invalid(format!(
                "hydrated checksum mismatch: expected {}, got {hydrated_sha}",
                manifest.source_sha256
            )));
        }
        fs::rename(&temp, dest_rdb)?;
        Ok(())
    })();

    if hydrate_result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    hydrate_result?;
    if let Some(parent) = dest_rdb.parent() {
        sync_dir(parent);
    }
    Ok(manifest)
}

pub fn load_segment_pack_manifest(
    pack_dir: impl AsRef<Path>,
) -> SegmentPackResult<SegmentPackManifest> {
    let path = pack_dir.as_ref().join(SEGMENT_PACK_MANIFEST_FILE);
    let text = fs::read_to_string(&path)?;
    let value = crate::json::parse_json(&text)
        .map(JsonValue::from)
        .map_err(|err| SegmentPackError::Invalid(format!("segment pack manifest JSON: {err}")))?;
    SegmentPackManifest::from_json_value(&value)
}

pub fn validate_segment_pack(
    pack_dir: impl AsRef<Path>,
    manifest: &SegmentPackManifest,
) -> SegmentPackResult<()> {
    if manifest.format != SEGMENT_PACK_FORMAT {
        return Err(SegmentPackError::Invalid(format!(
            "unsupported segment pack format: {}",
            manifest.format
        )));
    }
    if manifest.version != SEGMENT_PACK_VERSION {
        return Err(SegmentPackError::Invalid(format!(
            "unsupported segment pack version: {}",
            manifest.version
        )));
    }
    if manifest.recovery_boundary.kind != "checkpointed-rdb" {
        return Err(SegmentPackError::Invalid(format!(
            "unsupported recovery boundary: {}",
            manifest.recovery_boundary.kind
        )));
    }
    if manifest.recovery_boundary.wal_segments_required != 0 {
        return Err(SegmentPackError::Invalid(
            "segment pack requires WAL segments but none are supported in v1".to_string(),
        ));
    }
    if manifest.parts.is_empty() {
        return Err(SegmentPackError::Invalid(
            "segment pack manifest has no parts".to_string(),
        ));
    }

    let expected_manifest_sha = manifest_body_sha256(manifest);
    if manifest.manifest_sha256 != expected_manifest_sha {
        return Err(SegmentPackError::Invalid(format!(
            "manifest checksum mismatch: expected {expected_manifest_sha}, got {}",
            manifest.manifest_sha256
        )));
    }

    let mut expected_offset = 0u64;
    let mut current_target = String::new();
    let parts_dir = pack_dir.as_ref().join("parts");
    let mut combined = crate::crypto::sha256::Sha256::new();
    for part in &manifest.parts {
        validate_part_name(&part.name)?;
        validate_target_path(&part.target_path)?;
        if part.target_path != current_target {
            current_target = part.target_path.clone();
            expected_offset = 0;
        }
        if part.offset != expected_offset {
            return Err(SegmentPackError::Invalid(format!(
                "segment part offset mismatch for {}: expected {expected_offset}, got {}",
                part.name, part.offset
            )));
        }
        let path = parts_dir.join(&part.name);
        if !path.is_file() {
            return Err(SegmentPackError::Invalid(format!(
                "segment part is missing: {}",
                part.name
            )));
        }
        let size = fs::metadata(&path)?.len();
        if size != part.size_bytes {
            return Err(SegmentPackError::Invalid(format!(
                "segment part size mismatch for {}: expected {}, got {size}",
                part.name, part.size_bytes
            )));
        }
        let sha = sha256_file_hex(&path)
            .map_err(|err| SegmentPackError::Invalid(format!("part checksum failed: {err}")))?;
        if sha != part.sha256 {
            return Err(SegmentPackError::Invalid(format!(
                "segment part checksum mismatch for {}: expected {}, got {sha}",
                part.name, part.sha256
            )));
        }
        if part.target_path == "data.rdb" {
            let mut file = File::open(&path)?;
            let mut buf = vec![0u8; 8 * 1024];
            loop {
                let n = file.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                combined.update(&buf[..n]);
            }
        }
        expected_offset += part.size_bytes;
    }
    let data_size: u64 = manifest
        .parts
        .iter()
        .filter(|part| part.target_path == "data.rdb")
        .map(|part| part.size_bytes)
        .sum();
    if data_size != manifest.source_size_bytes {
        return Err(SegmentPackError::Invalid(format!(
            "segment pack size mismatch: expected {}, got {data_size}",
            manifest.source_size_bytes
        )));
    }
    let combined_sha = crate::utils::to_hex(&combined.finalize());
    if combined_sha != manifest.source_sha256 {
        return Err(SegmentPackError::Invalid(format!(
            "segment pack checksum mismatch: expected {}, got {combined_sha}",
            manifest.source_sha256
        )));
    }
    Ok(())
}

impl SegmentPackManifest {
    fn new(
        created_at_ms: u64,
        source_size_bytes: u64,
        source_sha256: String,
        recovery_boundary: SegmentPackRecoveryBoundary,
        parts: Vec<SegmentPackPart>,
    ) -> Self {
        let mut manifest = Self {
            format: SEGMENT_PACK_FORMAT.to_string(),
            version: SEGMENT_PACK_VERSION,
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
            created_at_ms,
            source_size_bytes,
            source_sha256,
            recovery_boundary,
            parts,
            manifest_sha256: String::new(),
        };
        manifest.manifest_sha256 = manifest_body_sha256(&manifest);
        manifest
    }

    fn to_json_value(&self) -> JsonValue {
        let mut object = manifest_body_json(self);
        object.insert(
            "manifest_sha256".to_string(),
            JsonValue::String(self.manifest_sha256.clone()),
        );
        JsonValue::Object(object)
    }

    fn from_json_value(value: &JsonValue) -> SegmentPackResult<Self> {
        let object = value
            .as_object()
            .ok_or_else(|| SegmentPackError::Invalid("manifest root must be an object".into()))?;
        let recovery = object
            .get("recovery_boundary")
            .and_then(JsonValue::as_object)
            .ok_or_else(|| {
                SegmentPackError::Invalid("manifest missing recovery_boundary".into())
            })?;
        let parts_value = object
            .get("parts")
            .and_then(JsonValue::as_array)
            .ok_or_else(|| SegmentPackError::Invalid("manifest missing parts".into()))?;
        let mut parts = Vec::with_capacity(parts_value.len());
        for value in parts_value {
            let part = value
                .as_object()
                .ok_or_else(|| SegmentPackError::Invalid("part entry must be an object".into()))?;
            parts.push(SegmentPackPart {
                name: required_str(part, "name")?.to_string(),
                target_path: required_str(part, "target_path")?.to_string(),
                offset: required_u64(part, "offset")?,
                size_bytes: required_u64(part, "size_bytes")?,
                sha256: required_str(part, "sha256")?.to_string(),
            });
        }
        Ok(Self {
            format: required_str(object, "format")?.to_string(),
            version: required_u64(object, "version")? as u32,
            engine_version: required_str(object, "engine_version")?.to_string(),
            created_at_ms: required_u64(object, "created_at_ms")?,
            source_size_bytes: required_u64(object, "source_size_bytes")?,
            source_sha256: required_str(object, "source_sha256")?.to_string(),
            recovery_boundary: SegmentPackRecoveryBoundary {
                kind: required_str(recovery, "kind")?.to_string(),
                base_lsn: required_u64(recovery, "base_lsn")?,
                wal_segments_required: required_u64(recovery, "wal_segments_required")?,
            },
            parts,
            manifest_sha256: required_str(object, "manifest_sha256")?.to_string(),
        })
    }
}

fn write_manifest_atomic(pack_dir: &Path, manifest: &SegmentPackManifest) -> SegmentPackResult<()> {
    let path = pack_dir.join(SEGMENT_PACK_MANIFEST_FILE);
    let temp = pack_dir.join(format!(".{SEGMENT_PACK_MANIFEST_FILE}.tmp"));
    let bytes = manifest.to_json_value().to_string_pretty().into_bytes();
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(temp, path)?;
    Ok(())
}

fn manifest_body_sha256(manifest: &SegmentPackManifest) -> String {
    let body = JsonValue::Object(manifest_body_json(manifest)).to_string_compact();
    crate::utils::to_hex(&crate::crypto::sha256::sha256(body.as_bytes()))
}

fn manifest_body_json(manifest: &SegmentPackManifest) -> Map<String, JsonValue> {
    let mut object = Map::new();
    object.insert("format".into(), JsonValue::String(manifest.format.clone()));
    object.insert("version".into(), JsonValue::Number(manifest.version as f64));
    object.insert(
        "engine_version".into(),
        JsonValue::String(manifest.engine_version.clone()),
    );
    object.insert(
        "created_at_ms".into(),
        JsonValue::Number(manifest.created_at_ms as f64),
    );
    object.insert(
        "source_size_bytes".into(),
        JsonValue::Number(manifest.source_size_bytes as f64),
    );
    object.insert(
        "source_sha256".into(),
        JsonValue::String(manifest.source_sha256.clone()),
    );

    let mut recovery = Map::new();
    recovery.insert(
        "kind".into(),
        JsonValue::String(manifest.recovery_boundary.kind.clone()),
    );
    recovery.insert(
        "base_lsn".into(),
        JsonValue::Number(manifest.recovery_boundary.base_lsn as f64),
    );
    recovery.insert(
        "wal_segments_required".into(),
        JsonValue::Number(manifest.recovery_boundary.wal_segments_required as f64),
    );
    object.insert("recovery_boundary".into(), JsonValue::Object(recovery));

    object.insert(
        "parts".into(),
        JsonValue::Array(
            manifest
                .parts
                .iter()
                .map(|part| {
                    let mut object = Map::new();
                    object.insert("name".into(), JsonValue::String(part.name.clone()));
                    object.insert(
                        "target_path".into(),
                        JsonValue::String(part.target_path.clone()),
                    );
                    object.insert("offset".into(), JsonValue::Number(part.offset as f64));
                    object.insert(
                        "size_bytes".into(),
                        JsonValue::Number(part.size_bytes as f64),
                    );
                    object.insert("sha256".into(), JsonValue::String(part.sha256.clone()));
                    JsonValue::Object(object)
                })
                .collect(),
        ),
    );
    object
}

fn required_str<'a>(object: &'a Map<String, JsonValue>, field: &str) -> SegmentPackResult<&'a str> {
    object
        .get(field)
        .and_then(JsonValue::as_str)
        .ok_or_else(|| SegmentPackError::Invalid(format!("manifest missing {field}")))
}

fn required_u64(object: &Map<String, JsonValue>, field: &str) -> SegmentPackResult<u64> {
    object
        .get(field)
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| SegmentPackError::Invalid(format!("manifest missing {field}")))
}

fn validate_part_name(name: &str) -> SegmentPackResult<()> {
    let path = Path::new(name);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(SegmentPackError::Invalid(format!(
            "invalid segment part name: {name}"
        )));
    }
    Ok(())
}

fn validate_target_path(path: &str) -> SegmentPackResult<()> {
    let path = Path::new(path);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(SegmentPackError::Invalid(format!(
            "invalid segment target path: {}",
            path.display()
        )));
    }
    let mut components = path.components();
    let Some(Component::Normal(first)) = components.next() else {
        return Err(SegmentPackError::Invalid(
            "segment target path is empty".to_string(),
        ));
    };
    let first = first.to_str().ok_or_else(|| {
        SegmentPackError::Invalid(format!("target path is not UTF-8: {}", path.display()))
    })?;
    let allowed = [
        "data.rdb",
        "data.rdb.ops",
        "data.rdb.meta.rdbx",
        "data.rdb.red",
        "data.result-cache.l2",
        "data.result-cache.l2-dwb",
    ];
    if !allowed.contains(&first) {
        return Err(SegmentPackError::Invalid(format!(
            "unsupported segment target path: {}",
            path.display()
        )));
    }
    if first == "data.rdb" && components.next().is_some() {
        return Err(SegmentPackError::Invalid(format!(
            "canonical data target must be data.rdb: {}",
            path.display()
        )));
    }
    Ok(())
}

fn hydrate_target_path(dest_rdb: &Path, target_path: &str) -> SegmentPackResult<PathBuf> {
    validate_target_path(target_path)?;
    if target_path == "data.rdb" {
        return Ok(dest_rdb.to_path_buf());
    }
    let file_name = dest_rdb
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| SegmentPackError::Invalid("destination file name is not UTF-8".into()))?;
    let parent = dest_rdb.parent().unwrap_or_else(|| Path::new("."));
    let result_cache = dest_rdb
        .with_extension("result-cache.l2")
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("data.result-cache.l2")
        .to_string();
    let result_cache_dwb = dest_rdb
        .with_extension("result-cache.l2-dwb")
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("data.result-cache.l2-dwb")
        .to_string();
    for (prefix, dest_name) in [
        ("data.rdb.ops", format!("{file_name}.ops")),
        ("data.rdb.meta.rdbx", format!("{file_name}.meta.rdbx")),
        ("data.rdb.red", format!("{file_name}.red")),
        ("data.result-cache.l2-dwb", result_cache_dwb),
        ("data.result-cache.l2", result_cache),
    ] {
        if target_path == prefix {
            return Ok(parent.join(dest_name));
        }
        if let Ok(relative) = Path::new(target_path).strip_prefix(prefix) {
            return Ok(parent.join(dest_name).join(relative));
        }
    }
    Err(SegmentPackError::Invalid(format!(
        "unsupported segment target path: {target_path}"
    )))
}

fn ops_dir_for_db_path(path: &Path) -> PathBuf {
    appended_path(path, ".ops")
}

fn appended_path(path: &Path, suffix: &str) -> PathBuf {
    path.with_file_name(format!(
        "{}{suffix}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("data.rdb")
    ))
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> SegmentPackResult<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out)?;
        } else if path.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

fn target_path_to_string(path: &Path) -> SegmentPackResult<String> {
    let mut out = Vec::new();
    for component in path.components() {
        let Component::Normal(value) = component else {
            return Err(SegmentPackError::Invalid(format!(
                "invalid segment target path: {}",
                path.display()
            )));
        };
        out.push(value.to_str().ok_or_else(|| {
            SegmentPackError::Invalid(format!("target path is not UTF-8: {}", path.display()))
        })?);
    }
    Ok(out.join("/"))
}

fn sync_dir(path: &Path) {
    let _ = File::open(path).and_then(|dir| dir.sync_all());
}
