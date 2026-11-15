use std::fs;
use std::path::{Path, PathBuf};

use super::decode_physical_metadata_document;
#[cfg(feature = "full")]
use super::{encode_physical_metadata_binary_document, encode_physical_metadata_json_document};
use crate::RdbFileResult;

pub fn read_physical_metadata_document(path: &Path) -> RdbFileResult<String> {
    let bytes = fs::read(path)?;
    decode_physical_metadata_document(&bytes)
}

#[cfg(feature = "full")]
pub fn write_physical_metadata_json_document(path: &Path, pretty_json: &str) -> RdbFileResult<()> {
    let bytes = encode_physical_metadata_json_document(pretty_json)?;
    super::atomic::write_physical_metadata_bytes_atomically(path, &bytes)
}

#[cfg(feature = "full")]
pub fn write_physical_metadata_binary_document(
    path: &Path,
    compact_json: &str,
) -> RdbFileResult<()> {
    let bytes = encode_physical_metadata_binary_document(compact_json)?;
    super::atomic::write_physical_metadata_bytes_atomically(path, &bytes)
}

pub fn copy_physical_metadata_binary_to_journal(
    data_path: &Path,
    source_path: &Path,
    sequence: u64,
) -> RdbFileResult<PathBuf> {
    let journal_path = crate::layout::physical_metadata_journal_path(data_path, sequence);
    fs::copy(source_path, &journal_path)?;
    Ok(journal_path)
}

pub fn copy_physical_export_data_file(data_path: &Path, name: &str) -> RdbFileResult<PathBuf> {
    let export_data_path = crate::layout::physical_export_data_path(data_path, name);
    fs::copy(data_path, &export_data_path)?;
    Ok(export_data_path)
}

pub fn list_physical_metadata_journal_paths(data_path: &Path) -> RdbFileResult<Vec<PathBuf>> {
    let Some(parent) = data_path.parent() else {
        return Ok(Vec::new());
    };
    let prefix = crate::layout::physical_metadata_journal_prefix(data_path);

    let mut paths = Vec::new();
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().map(|name| name.to_string_lossy()) else {
            continue;
        };
        if name.starts_with(&prefix) {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

pub fn prune_physical_metadata_journal_paths(
    data_path: &Path,
    retention: usize,
) -> RdbFileResult<()> {
    let mut paths = list_physical_metadata_journal_paths(data_path)?;
    if paths.len() <= retention {
        return Ok(());
    }
    let delete_count = paths.len() - retention;
    for path in paths.drain(0..delete_count) {
        let _ = fs::remove_file(path);
    }
    Ok(())
}
