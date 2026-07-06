//! Ephemeral store materialization (PRD #1785, issue #1786).
//!
//! The `red` binary can take a local CSV/TSV file plus an RQL query,
//! materialize the file as a row table inside a throwaway in-memory
//! embedded store, run the query, and discard the store — no server, no
//! pre-existing store, nothing durable created.
//!
//! This module is the CSV/TSV tracer: the skeleton every other ephemeral
//! slice (JSON/documents, multi-file, writes/`--save`) extends. It rides
//! the existing CSV import path (the shared [`CsvImporter`]) so the file
//! becomes a real row table with header-derived columns and inferred
//! types.
//!
//! The loaded collection is named by its sanitized file stem and — as the
//! single loaded file — is also materialized under the positional file
//! alias [`POSITIONAL_ALIAS`] (`t`), so `SELECT … FROM t` and
//! `SELECT … FROM <stem>` resolve identically for every query shape
//! (projections, filters, and aggregates alike).

use std::path::Path;

use crate::runtime::RedDBRuntime;
use crate::storage::import::{CsvConfig, CsvImporter};

/// Positional alias for the single loaded file: `SELECT … FROM t`.
pub const POSITIONAL_ALIAS: &str = "t";

/// Outcome of materializing a data file into the ephemeral store.
#[derive(Debug, Clone)]
pub struct EphemeralTable {
    /// Collection name derived from the sanitized file stem.
    pub collection: String,
    /// Positional alias (`t`) also addressing the collection.
    pub alias: String,
    /// Number of data rows imported (header excluded).
    pub rows_imported: usize,
}

/// A didactic error explaining why a file could not be materialized.
///
/// Every variant renders to a human-readable, non-panicking message: a
/// missing, unreadable, unsupported, or malformed file never aborts the
/// process abnormally.
#[derive(Debug)]
pub enum EphemeralError {
    /// The path does not point at a readable regular file.
    NotAFile { path: String },
    /// The extension is neither `.csv` nor `.tsv`/`.tab`.
    UnsupportedExtension { path: String, ext: String },
    /// The file stem sanitized to an empty identifier.
    EmptyStem { path: String },
    /// The CSV import path rejected the file (I/O or parse failure).
    Import { path: String, source: String },
    /// Registering the positional alias view failed.
    Alias { source: String },
}

impl std::fmt::Display for EphemeralError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EphemeralError::NotAFile { path } => {
                write!(f, "cannot read data file '{path}': no such file")
            }
            EphemeralError::UnsupportedExtension { path, ext } => write!(
                f,
                "unsupported data file '{path}': '.{ext}' is not a CSV or TSV file \
                 (expected .csv, .tsv, or .tab)"
            ),
            EphemeralError::EmptyStem { path } => write!(
                f,
                "cannot derive a table name from '{path}': the file stem is empty"
            ),
            EphemeralError::Import { path, source } => {
                write!(f, "failed to load '{path}': {source}")
            }
            EphemeralError::Alias { source } => {
                write!(f, "failed to register positional alias '{POSITIONAL_ALIAS}': {source}")
            }
        }
    }
}

impl std::error::Error for EphemeralError {}

/// Sanitize a file stem into a safe collection identifier.
///
/// Non-alphanumeric characters collapse to a single `_`; leading/trailing
/// underscores are trimmed; a leading digit is prefixed with `_` so the
/// result is always a valid identifier. Returns `None` when nothing
/// usable survives (e.g. a stem of only punctuation).
#[must_use]
pub fn sanitize_stem(stem: &str) -> Option<String> {
    let mut out = String::with_capacity(stem.len());
    let mut prev_underscore = false;
    for ch in stem.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_underscore = false;
        } else if !prev_underscore {
            out.push('_');
            prev_underscore = true;
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        return None;
    }
    // Identifiers cannot start with a digit.
    if trimmed.starts_with(|c: char| c.is_ascii_digit()) {
        Some(format!("_{trimmed}"))
    } else {
        Some(trimmed.to_string())
    }
}

/// Field delimiter inferred from a data file's extension.
fn delimiter_for_extension(ext: &str) -> Option<u8> {
    match ext {
        "csv" => Some(b','),
        "tsv" | "tab" => Some(b'\t'),
        _ => None,
    }
}

impl RedDBRuntime {
    /// Materialize a local CSV/TSV file as a row table in this runtime.
    ///
    /// The collection is auto-created from the sanitized file stem, and —
    /// as the single loaded file — is also materialized under the
    /// positional alias `t` ([`POSITIONAL_ALIAS`]) so it is addressable
    /// both ways. Intended for the in-memory ephemeral store — nothing
    /// durable is written beyond what this runtime already persists.
    pub fn materialize_data_file(&self, path: &Path) -> Result<EphemeralTable, EphemeralError> {
        let display = path.display().to_string();

        if !path.is_file() {
            return Err(EphemeralError::NotAFile { path: display });
        }

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let delimiter = delimiter_for_extension(&ext).ok_or_else(|| {
            EphemeralError::UnsupportedExtension {
                path: display.clone(),
                ext: ext.clone(),
            }
        })?;

        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let collection = sanitize_stem(stem).ok_or_else(|| EphemeralError::EmptyStem {
            path: display.clone(),
        })?;

        let rows_imported = self.import_csv_into(path, &collection, delimiter, &display)?;

        // The single loaded file is also addressable by the positional
        // alias `t`. Rather than a rewrite view — which leaks on
        // aggregates and other non-trivial shapes — `t` is materialized
        // as its own real collection so every query resolves identically
        // through either name. Skipped when the stem already sanitized to
        // `t` (e.g. `t.csv`), which would collide.
        if collection != POSITIONAL_ALIAS {
            self.import_csv_into(path, POSITIONAL_ALIAS, delimiter, &display)?;
        }

        Ok(EphemeralTable {
            collection,
            alias: POSITIONAL_ALIAS.to_string(),
            rows_imported,
        })
    }

    /// Import `path` into `collection` via the shared [`CsvImporter`],
    /// returning the number of data rows written.
    fn import_csv_into(
        &self,
        path: &Path,
        collection: &str,
        delimiter: u8,
        display: &str,
    ) -> Result<usize, EphemeralError> {
        let importer = CsvImporter::new(CsvConfig {
            collection: collection.to_string(),
            has_header: true,
            delimiter,
            skip_errors: false,
            ..CsvConfig::default()
        });

        let store = self.inner.db.store();
        // The shared CsvImporter writes straight through `store.insert`,
        // which does not auto-create the collection — provision it up
        // front the same way the runtime's INSERT path does on first
        // write.
        let _ = store.get_or_create_collection(collection);
        let stats = importer
            .import_file(path, store.as_ref())
            .map_err(|e| EphemeralError::Import {
                path: display.to_string(),
                source: e.to_string(),
            })?;

        // The rows were written straight through the store, so nudge the
        // planner/result cache exactly as the COPY path does.
        self.note_table_write(collection);

        Ok(stats.records_imported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_stem_basic() {
        assert_eq!(sanitize_stem("data").as_deref(), Some("data"));
        assert_eq!(sanitize_stem("Users").as_deref(), Some("users"));
    }

    #[test]
    fn sanitize_stem_collapses_and_trims() {
        assert_eq!(
            sanitize_stem("vendas-2026 (v2)").as_deref(),
            Some("vendas_2026_v2")
        );
        assert_eq!(sanitize_stem("__weird__name__").as_deref(), Some("weird_name"));
    }

    #[test]
    fn sanitize_stem_leading_digit_prefixed() {
        assert_eq!(sanitize_stem("2026sales").as_deref(), Some("_2026sales"));
    }

    #[test]
    fn sanitize_stem_all_punctuation_is_none() {
        assert_eq!(sanitize_stem("---"), None);
        assert_eq!(sanitize_stem(""), None);
    }

    #[test]
    fn delimiter_inference() {
        assert_eq!(delimiter_for_extension("csv"), Some(b','));
        assert_eq!(delimiter_for_extension("tsv"), Some(b'\t'));
        assert_eq!(delimiter_for_extension("tab"), Some(b'\t'));
        assert_eq!(delimiter_for_extension("json"), None);
    }
}
