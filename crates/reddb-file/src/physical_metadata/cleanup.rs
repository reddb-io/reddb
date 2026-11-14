use super::*;
use std::fs;
use std::path::Path;

/// Retire the recognized legacy physical metadata files after their contents
/// have been checkpointed inside the database. Never remove similarly named
/// operator backups or traverse directories.
pub fn remove_imported_physical_metadata_sidecars(data_path: &Path) -> RdbFileResult<()> {
    let prefix = crate::layout::physical_metadata_journal_prefix(data_path);
    let mut paths = list_physical_metadata_journal_paths(data_path)?;
    paths.retain(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix(&prefix))
            .is_some_and(|sequence| {
                sequence.len() == 20
                    && sequence.bytes().all(|byte| byte.is_ascii_digit())
                    && sequence.parse::<u64>().is_ok()
            })
    });
    paths.push(crate::layout::physical_metadata_binary_path(data_path));
    paths.push(crate::layout::physical_metadata_json_path(data_path));
    for path in paths {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
