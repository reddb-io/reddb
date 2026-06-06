//! Offline embedded single-file `.rdb` to operational-directory migration.
//!
//! This first contract is deliberately one-way and offline: the source file is
//! locked exclusively, checksum-validated, then copied into a directory layout
//! with a stable manifest. Opening the destination still goes through the
//! canonical paged store at `files/data.rdb`.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;

use crate::serde_json::{Map, Value as JsonValue};
use crate::storage::engine::page::{Page, DB_VERSION, HEADER_SIZE, MAGIC_BYTES, PAGE_SIZE};
use crate::storage::wal::sha256_file_hex;

pub const OPERATIONAL_MIGRATION_MANIFEST_FILE: &str = "MANIFEST.json";
pub const OPERATIONAL_MIGRATION_FORMAT: &str = "reddb.operational-directory.v1";
const DATA_FILE_ID: &str = "canonical-data";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationalMigrationFile {
    pub id: String,
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationalMigrationManifest {
    pub format: String,
    pub version: u32,
    pub engine_version: String,
    pub created_at_ms: u64,
    pub source_path: String,
    pub source_size_bytes: u64,
    pub source_sha256: String,
    pub checkpoint_lsn: u64,
    pub one_way: bool,
    pub offline_required: bool,
    pub files: Vec<OperationalMigrationFile>,
    pub manifest_sha256: String,
}

#[derive(Debug)]
pub enum OperationalMigrationError {
    Io(std::io::Error),
    Invalid(String),
    SourceOpen(String),
}

impl fmt::Display for OperationalMigrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::Invalid(msg) => write!(f, "{msg}"),
            Self::SourceOpen(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for OperationalMigrationError {}

impl From<std::io::Error> for OperationalMigrationError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

pub type OperationalMigrationResult<T> = Result<T, OperationalMigrationError>;

pub fn migrate_embedded_to_operational(
    source_rdb: impl AsRef<Path>,
    operational_dir: impl AsRef<Path>,
) -> OperationalMigrationResult<OperationalMigrationManifest> {
    let source_rdb = source_rdb.as_ref();
    let operational_dir = operational_dir.as_ref();
    if operational_dir.exists() {
        return Err(OperationalMigrationError::Invalid(format!(
            "operational destination already exists: {}",
            operational_dir.display()
        )));
    }

    let mut source = OpenOptions::new().read(true).write(true).open(source_rdb)?;
    source.try_lock_exclusive().map_err(|_| {
        OperationalMigrationError::SourceOpen(
            "source .rdb is open; close the embedded database before offline migration".to_string(),
        )
    })?;

    let validation = validate_checkpointed_source_file(&mut source, source_rdb)?;
    fs::create_dir_all(operational_dir)?;
    fs::create_dir_all(files_dir(operational_dir))?;

    let data_path = data_file_path(operational_dir);
    copy_locked_source(&mut source, &data_path)?;
    sync_file(&data_path)?;

    let mut files = vec![file_entry(
        DATA_FILE_ID,
        Path::new("files/data.rdb"),
        &data_path,
    )?];
    export_related_path(
        &appended_path(source_rdb, ".meta.rdbx"),
        Path::new("files/data.rdb.meta.rdbx"),
        operational_dir,
        &mut files,
    )?;
    export_related_path(
        &appended_path(source_rdb, ".red"),
        Path::new("files/data.rdb.red"),
        operational_dir,
        &mut files,
    )?;
    export_related_path(
        &source_rdb.with_extension("result-cache.l2"),
        Path::new("files/data.result-cache.l2"),
        operational_dir,
        &mut files,
    )?;
    export_related_path(
        &source_rdb.with_extension("result-cache.l2-dwb"),
        Path::new("files/data.result-cache.l2-dwb"),
        operational_dir,
        &mut files,
    )?;

    let created_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let manifest = OperationalMigrationManifest::new(
        created_at_ms,
        source_rdb.display().to_string(),
        validation.source_size_bytes,
        validation.source_sha256,
        validation.checkpoint_lsn,
        files,
    );
    write_manifest_atomic(operational_dir, &manifest)?;
    sync_dir(operational_dir);
    Ok(manifest)
}

pub fn operational_data_path(operational_dir: impl AsRef<Path>) -> PathBuf {
    data_file_path(operational_dir.as_ref())
}

pub fn load_operational_migration_manifest(
    operational_dir: impl AsRef<Path>,
) -> OperationalMigrationResult<OperationalMigrationManifest> {
    let path = operational_dir
        .as_ref()
        .join(OPERATIONAL_MIGRATION_MANIFEST_FILE);
    let value: JsonValue = crate::serde_json::from_slice(&fs::read(path)?)
        .map_err(|err| OperationalMigrationError::Invalid(format!("manifest JSON: {err}")))?;
    OperationalMigrationManifest::from_json_value(&value)
}

pub fn validate_operational_migration(
    operational_dir: impl AsRef<Path>,
    manifest: &OperationalMigrationManifest,
) -> OperationalMigrationResult<()> {
    if manifest.format != OPERATIONAL_MIGRATION_FORMAT {
        return Err(OperationalMigrationError::Invalid(format!(
            "unsupported operational migration format: {}",
            manifest.format
        )));
    }
    if manifest.version != 1 {
        return Err(OperationalMigrationError::Invalid(format!(
            "unsupported operational migration version: {}",
            manifest.version
        )));
    }
    if !manifest.one_way || !manifest.offline_required {
        return Err(OperationalMigrationError::Invalid(
            "operational migration manifest must declare offline one-way semantics".to_string(),
        ));
    }
    if manifest.manifest_sha256 != manifest_body_sha256(manifest) {
        return Err(OperationalMigrationError::Invalid(
            "operational manifest checksum mismatch".to_string(),
        ));
    }
    if !manifest.files.iter().any(|file| file.id == DATA_FILE_ID) {
        return Err(OperationalMigrationError::Invalid(
            "operational manifest missing canonical data file".to_string(),
        ));
    }
    for file in &manifest.files {
        validate_relative_path(&file.path)?;
        let path = operational_dir.as_ref().join(&file.path);
        if !path.is_file() {
            return Err(OperationalMigrationError::Invalid(format!(
                "operational file is missing: {}",
                file.path
            )));
        }
        let size = fs::metadata(&path)?.len();
        if size != file.size_bytes {
            return Err(OperationalMigrationError::Invalid(format!(
                "operational file size mismatch for {}: expected {}, got {size}",
                file.path, file.size_bytes
            )));
        }
        let sha = sha256_file_hex(&path).map_err(|err| {
            OperationalMigrationError::Invalid(format!("operational file checksum failed: {err}"))
        })?;
        if sha != file.sha256 {
            return Err(OperationalMigrationError::Invalid(format!(
                "operational file checksum mismatch for {}: expected {}, got {sha}",
                file.path, file.sha256
            )));
        }
    }
    Ok(())
}

struct SourceValidation {
    source_size_bytes: u64,
    source_sha256: String,
    checkpoint_lsn: u64,
}

fn validate_checkpointed_source_file(
    source: &mut File,
    source_rdb: &Path,
) -> OperationalMigrationResult<SourceValidation> {
    let source_size_bytes = source.metadata()?.len();
    if source_size_bytes == 0 || source_size_bytes % PAGE_SIZE as u64 != 0 {
        return Err(OperationalMigrationError::Invalid(format!(
            "source .rdb is not a complete paged database: {}",
            source_rdb.display()
        )));
    }

    source.seek(SeekFrom::Start(0))?;
    let header = read_page(source, 0)?;
    header.verify_checksum().map_err(|err| {
        OperationalMigrationError::Invalid(format!(
            "source page 0 checksum validation failed: {err}"
        ))
    })?;
    header.verify_header_page().map_err(|err| {
        OperationalMigrationError::Invalid(format!("source header validation failed: {err}"))
    })?;
    let bytes = header.as_bytes();
    if bytes[HEADER_SIZE..HEADER_SIZE + 4] != MAGIC_BYTES {
        return Err(OperationalMigrationError::Invalid(
            "source header magic is invalid".to_string(),
        ));
    }
    let version = u32::from_le_bytes(bytes[HEADER_SIZE + 4..HEADER_SIZE + 8].try_into().unwrap());
    if version > DB_VERSION {
        return Err(OperationalMigrationError::Invalid(format!(
            "source database version {version} is newer than supported {DB_VERSION}"
        )));
    }
    let page_count = u32::from_le_bytes(
        bytes[HEADER_SIZE + 12..HEADER_SIZE + 16]
            .try_into()
            .unwrap(),
    );
    if page_count == 0 {
        return Err(OperationalMigrationError::Invalid(
            "source checkpoint has no pages".to_string(),
        ));
    }
    let checkpoint_lsn = u64::from_le_bytes(
        bytes[HEADER_SIZE + 24..HEADER_SIZE + 32]
            .try_into()
            .unwrap(),
    );
    if bytes[HEADER_SIZE + 192] != 0 {
        return Err(OperationalMigrationError::Invalid(
            "source database has an incomplete checkpoint in progress".to_string(),
        ));
    }
    let expected_size = page_count as u64 * PAGE_SIZE as u64;
    if source_size_bytes < expected_size {
        return Err(OperationalMigrationError::Invalid(format!(
            "source .rdb is truncated: header expects {expected_size} bytes, file has {source_size_bytes}"
        )));
    }

    for page_id in 1..page_count {
        let page = read_page(source, page_id)?;
        page.verify_checksum().map_err(|err| {
            OperationalMigrationError::Invalid(format!(
                "source page {page_id} checksum validation failed: {err}"
            ))
        })?;
    }
    let source_sha256 = sha256_file_hex(source_rdb).map_err(|err| {
        OperationalMigrationError::Invalid(format!("source checksum failed: {err}"))
    })?;
    Ok(SourceValidation {
        source_size_bytes,
        source_sha256,
        checkpoint_lsn,
    })
}

fn read_page(source: &mut File, page_id: u32) -> OperationalMigrationResult<Page> {
    let offset = page_id as u64 * PAGE_SIZE as u64;
    source.seek(SeekFrom::Start(offset))?;
    let mut bytes = [0u8; PAGE_SIZE];
    source.read_exact(&mut bytes)?;
    Ok(Page::from_bytes(bytes))
}

fn copy_locked_source(source: &mut File, dest: &Path) -> OperationalMigrationResult<()> {
    source.seek(SeekFrom::Start(0))?;
    let mut out = OpenOptions::new().create_new(true).write(true).open(dest)?;
    std::io::copy(source, &mut out)?;
    out.sync_all()?;
    Ok(())
}

fn export_related_path(
    source: &Path,
    target: &Path,
    operational_dir: &Path,
    files: &mut Vec<OperationalMigrationFile>,
) -> OperationalMigrationResult<()> {
    if source.is_file() {
        let dest = operational_dir.join(target);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, &dest)?;
        sync_file(&dest)?;
        files.push(file_entry(&file_id_from_target(target)?, target, &dest)?);
    } else if source.is_dir() {
        let mut paths = Vec::new();
        collect_files(source, &mut paths)?;
        paths.sort();
        for path in paths {
            let relative = path.strip_prefix(source).map_err(|err| {
                OperationalMigrationError::Invalid(format!("sidecar path outside root: {err}"))
            })?;
            export_related_path(&path, &target.join(relative), operational_dir, files)?;
        }
    }
    Ok(())
}

fn file_entry(
    id: &str,
    target: &Path,
    physical_path: &Path,
) -> OperationalMigrationResult<OperationalMigrationFile> {
    let path = relative_path_to_string(target)?;
    Ok(OperationalMigrationFile {
        id: id.to_string(),
        path,
        size_bytes: fs::metadata(physical_path)?.len(),
        sha256: sha256_file_hex(physical_path).map_err(|err| {
            OperationalMigrationError::Invalid(format!("file checksum failed: {err}"))
        })?,
    })
}

fn file_id_from_target(target: &Path) -> OperationalMigrationResult<String> {
    let path = relative_path_to_string(target)?;
    Ok(format!(
        "sidecar:{}",
        path.strip_prefix("files/").unwrap_or(path.as_str())
    ))
}

impl OperationalMigrationManifest {
    fn new(
        created_at_ms: u64,
        source_path: String,
        source_size_bytes: u64,
        source_sha256: String,
        checkpoint_lsn: u64,
        files: Vec<OperationalMigrationFile>,
    ) -> Self {
        let mut manifest = Self {
            format: OPERATIONAL_MIGRATION_FORMAT.to_string(),
            version: 1,
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
            created_at_ms,
            source_path,
            source_size_bytes,
            source_sha256,
            checkpoint_lsn,
            one_way: true,
            offline_required: true,
            files,
            manifest_sha256: String::new(),
        };
        manifest.manifest_sha256 = manifest_body_sha256(&manifest);
        manifest
    }

    fn to_json_value(&self) -> JsonValue {
        let mut body = manifest_body_json(self);
        body.insert(
            "manifest_sha256".into(),
            JsonValue::String(self.manifest_sha256.clone()),
        );
        JsonValue::Object(body)
    }

    fn from_json_value(value: &JsonValue) -> OperationalMigrationResult<Self> {
        let object = value.as_object().ok_or_else(|| {
            OperationalMigrationError::Invalid("manifest root must be an object".into())
        })?;
        let file_values = object
            .get("files")
            .and_then(JsonValue::as_array)
            .ok_or_else(|| OperationalMigrationError::Invalid("manifest missing files".into()))?;
        let mut files = Vec::with_capacity(file_values.len());
        for value in file_values {
            let file = value.as_object().ok_or_else(|| {
                OperationalMigrationError::Invalid("file entry must be an object".into())
            })?;
            files.push(OperationalMigrationFile {
                id: required_str(file, "id")?.to_string(),
                path: required_str(file, "path")?.to_string(),
                size_bytes: required_u64(file, "size_bytes")?,
                sha256: required_str(file, "sha256")?.to_string(),
            });
        }
        Ok(Self {
            format: required_str(object, "format")?.to_string(),
            version: required_u64(object, "version")? as u32,
            engine_version: required_str(object, "engine_version")?.to_string(),
            created_at_ms: required_u64(object, "created_at_ms")?,
            source_path: required_str(object, "source_path")?.to_string(),
            source_size_bytes: required_u64(object, "source_size_bytes")?,
            source_sha256: required_str(object, "source_sha256")?.to_string(),
            checkpoint_lsn: required_u64(object, "checkpoint_lsn")?,
            one_way: required_bool(object, "one_way")?,
            offline_required: required_bool(object, "offline_required")?,
            files,
            manifest_sha256: required_str(object, "manifest_sha256")?.to_string(),
        })
    }
}

fn write_manifest_atomic(
    operational_dir: &Path,
    manifest: &OperationalMigrationManifest,
) -> OperationalMigrationResult<()> {
    let path = operational_dir.join(OPERATIONAL_MIGRATION_MANIFEST_FILE);
    let temp = operational_dir.join(format!(".{OPERATIONAL_MIGRATION_MANIFEST_FILE}.tmp"));
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

fn manifest_body_sha256(manifest: &OperationalMigrationManifest) -> String {
    let body = JsonValue::Object(manifest_body_json(manifest)).to_string_compact();
    crate::utils::to_hex(&crate::crypto::sha256::sha256(body.as_bytes()))
}

fn manifest_body_json(manifest: &OperationalMigrationManifest) -> Map<String, JsonValue> {
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
        "source_path".into(),
        JsonValue::String(manifest.source_path.clone()),
    );
    object.insert(
        "source_size_bytes".into(),
        JsonValue::Number(manifest.source_size_bytes as f64),
    );
    object.insert(
        "source_sha256".into(),
        JsonValue::String(manifest.source_sha256.clone()),
    );
    object.insert(
        "checkpoint_lsn".into(),
        JsonValue::Number(manifest.checkpoint_lsn as f64),
    );
    object.insert("one_way".into(), JsonValue::Bool(manifest.one_way));
    object.insert(
        "offline_required".into(),
        JsonValue::Bool(manifest.offline_required),
    );
    object.insert(
        "files".into(),
        JsonValue::Array(
            manifest
                .files
                .iter()
                .map(|file| {
                    let mut object = Map::new();
                    object.insert("id".into(), JsonValue::String(file.id.clone()));
                    object.insert("path".into(), JsonValue::String(file.path.clone()));
                    object.insert(
                        "size_bytes".into(),
                        JsonValue::Number(file.size_bytes as f64),
                    );
                    object.insert("sha256".into(), JsonValue::String(file.sha256.clone()));
                    JsonValue::Object(object)
                })
                .collect(),
        ),
    );
    object
}

fn required_str<'a>(
    object: &'a Map<String, JsonValue>,
    field: &str,
) -> OperationalMigrationResult<&'a str> {
    object
        .get(field)
        .and_then(JsonValue::as_str)
        .ok_or_else(|| OperationalMigrationError::Invalid(format!("manifest missing {field}")))
}

fn required_u64(object: &Map<String, JsonValue>, field: &str) -> OperationalMigrationResult<u64> {
    object
        .get(field)
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| OperationalMigrationError::Invalid(format!("manifest missing {field}")))
}

fn required_bool(object: &Map<String, JsonValue>, field: &str) -> OperationalMigrationResult<bool> {
    object
        .get(field)
        .and_then(JsonValue::as_bool)
        .ok_or_else(|| OperationalMigrationError::Invalid(format!("manifest missing {field}")))
}

fn files_dir(operational_dir: &Path) -> PathBuf {
    operational_dir.join("files")
}

fn data_file_path(operational_dir: &Path) -> PathBuf {
    files_dir(operational_dir).join("data.rdb")
}

fn appended_path(path: &Path, suffix: &str) -> PathBuf {
    let mut raw = path.as_os_str().to_os_string();
    raw.push(suffix);
    PathBuf::from(raw)
}

fn collect_files(root: &Path, files: &mut Vec<PathBuf>) -> OperationalMigrationResult<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, files)?;
        } else if path.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn relative_path_to_string(path: &Path) -> OperationalMigrationResult<String> {
    validate_relative_path(path)?;
    path.to_str()
        .map(|path| path.replace('\\', "/"))
        .ok_or_else(|| OperationalMigrationError::Invalid("path is not UTF-8".to_string()))
}

fn validate_relative_path(path: impl AsRef<Path>) -> OperationalMigrationResult<()> {
    let path = path.as_ref();
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(OperationalMigrationError::Invalid(format!(
            "invalid operational path: {}",
            path.display()
        )));
    }
    Ok(())
}

fn sync_file(path: &Path) -> OperationalMigrationResult<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn sync_dir(path: &Path) {
    if let Ok(dir) = File::open(path) {
        let _ = dir.sync_all();
    }
}
