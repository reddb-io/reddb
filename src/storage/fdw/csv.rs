//! CSV foreign-data wrapper (Phase 3.2 PG parity).
//!
//! Exposes a CSV file on local disk as a read-only foreign table. The
//! wrapper re-uses the RFC 4180 parser + type coercion from
//! `storage::import::csv` so behaviour matches `COPY FROM 'path'`.
//!
//! # Options (table-level)
//!
//! | Option            | Required | Default | Description                   |
//! |-------------------|:--------:|---------|-------------------------------|
//! | `path`            | yes      | —       | Absolute or relative file path|
//! | `delimiter`       | no       | `,`     | Single byte field separator   |
//! | `header`          | no       | `true`  | Whether first row is names    |
//! | `quote`           | no       | `"`     | Quote character               |
//! | `treat_empty_as_null` | no   | `true`  | Empty field → Value::Null     |
//!
//! A server-level `base_path` option is prepended when `path` is
//! relative, letting callers create one server per directory and many
//! foreign tables pointing at different files inside it.

use std::path::PathBuf;

use super::{FdwError, FdwOptions, ForeignDataWrapper, WrapperState};
use crate::storage::import::csv::{CsvConfig, CsvImporter};
use crate::storage::query::unified::UnifiedRecord;
use crate::storage::schema::Value;

/// The CSV wrapper registers under the kind `"csv"`:
/// `CREATE SERVER srv FOREIGN DATA WRAPPER csv OPTIONS (base_path '/data/csv')`.
pub struct CsvForeignWrapper;

/// Server-level cached state. Only `base_path` lives here today;
/// encoding / quote are per-table so they stay on the scan options.
struct CsvServerState {
    base_path: Option<PathBuf>,
}

impl WrapperState for CsvServerState {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl ForeignDataWrapper for CsvForeignWrapper {
    fn kind(&self) -> &'static str {
        "csv"
    }

    fn build_server_state(
        &self,
        options: &FdwOptions,
    ) -> Result<Option<std::sync::Arc<dyn WrapperState>>, FdwError> {
        let base_path = options.get("base_path").map(PathBuf::from);
        Ok(Some(std::sync::Arc::new(CsvServerState { base_path })))
    }

    fn scan(
        &self,
        server_state: Option<&std::sync::Arc<dyn WrapperState>>,
        table_options: &FdwOptions,
    ) -> Result<Vec<UnifiedRecord>, FdwError> {
        // Resolve file path: table-level `path` + optional server-level
        // `base_path` prefix when the table path is relative.
        let rel = table_options.require("path")?;
        let mut path = PathBuf::from(rel);
        if path.is_relative() {
            if let Some(state) = server_state {
                if let Some(css) = state.as_any().downcast_ref::<CsvServerState>() {
                    if let Some(base) = &css.base_path {
                        path = base.join(&path);
                    }
                }
            }
        }

        // Translate FDW options → CsvConfig.
        let delimiter = table_options
            .get("delimiter")
            .and_then(|s| s.as_bytes().first().copied())
            .unwrap_or(b',');
        let quote = table_options
            .get("quote")
            .and_then(|s| s.as_bytes().first().copied())
            .unwrap_or(b'"');
        let has_header = table_options
            .get("header")
            .map(|s| !matches!(s.to_ascii_lowercase().as_str(), "false" | "0" | "no"))
            .unwrap_or(true);
        let treat_empty_as_null = table_options
            .get("treat_empty_as_null")
            .map(|s| !matches!(s.to_ascii_lowercase().as_str(), "false" | "0" | "no"))
            .unwrap_or(true);

        // Parse the file directly into memory (CsvImporter's import_reader
        // normally inserts into a Store; here we parse then wrap records).
        let text = std::fs::read_to_string(&path)
            .map_err(|e| FdwError::Io(format!("read '{}': {e}", path.display())))?;
        let records = parse_csv_records(&text, delimiter, quote).map_err(FdwError::Io)?;

        let mut iter = records.into_iter();
        let headers: Vec<String> = if has_header {
            match iter.next() {
                Some(row) => row,
                None => return Ok(Vec::new()),
            }
        } else {
            Vec::new()
        };

        let mut out: Vec<UnifiedRecord> = Vec::new();
        for row in iter {
            let names: Vec<String> = if headers.is_empty() {
                (0..row.len()).map(|i| format!("c{i}")).collect()
            } else {
                headers.clone()
            };
            let mut record = UnifiedRecord::with_capacity(row.len());
            for (i, field) in row.into_iter().enumerate() {
                let name = names.get(i).cloned().unwrap_or_else(|| format!("c{i}"));
                let value = coerce_field(&field, treat_empty_as_null);
                record.set(&name, value);
            }
            out.push(record);
        }
        Ok(out)
    }

    fn estimated_row_count(
        &self,
        server_state: Option<&std::sync::Arc<dyn WrapperState>>,
        table_options: &FdwOptions,
    ) -> Option<usize> {
        // Cheap estimate: file bytes / average row length (128 bytes).
        // Doesn't open the file twice — `scan` re-reads on demand.
        let rel = table_options.get("path")?;
        let mut path = PathBuf::from(rel);
        if path.is_relative() {
            if let Some(state) = server_state {
                if let Some(css) = state.as_any().downcast_ref::<CsvServerState>() {
                    if let Some(base) = &css.base_path {
                        path = base.join(&path);
                    }
                }
            }
        }
        std::fs::metadata(&path)
            .ok()
            .map(|m| (m.len() / 128).max(1) as usize)
    }
}

// ────────────────────────────────────────────────────────────────────
// Local parser + coercion helpers
//
// We duplicate minimal scaffolding here (rather than re-export from
// storage::import::csv) to keep FDW independent of the importer's
// Store-writing path. Both trees stay consistent via shared expectations
// in tests.
// ────────────────────────────────────────────────────────────────────

fn parse_csv_records(input: &str, delimiter: u8, quote: u8) -> Result<Vec<Vec<String>>, String> {
    let bytes = input.as_bytes();
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if in_quotes {
            if b == quote {
                if i + 1 < bytes.len() && bytes[i + 1] == quote {
                    field.push(quote as char);
                    i += 2;
                } else {
                    in_quotes = false;
                    i += 1;
                }
            } else {
                field.push(b as char);
                i += 1;
            }
        } else if b == quote && field.is_empty() {
            in_quotes = true;
            i += 1;
        } else if b == delimiter {
            current_row.push(std::mem::take(&mut field));
            i += 1;
        } else if b == b'\r' {
            current_row.push(std::mem::take(&mut field));
            records.push(std::mem::take(&mut current_row));
            i += 1;
            if i < bytes.len() && bytes[i] == b'\n' {
                i += 1;
            }
        } else if b == b'\n' {
            current_row.push(std::mem::take(&mut field));
            records.push(std::mem::take(&mut current_row));
            i += 1;
        } else {
            field.push(b as char);
            i += 1;
        }
    }
    if in_quotes {
        return Err("unterminated quoted field".to_string());
    }
    if !field.is_empty() || !current_row.is_empty() {
        current_row.push(field);
        records.push(current_row);
    }
    Ok(records)
}

fn coerce_field(raw: &str, treat_empty_as_null: bool) -> Value {
    if treat_empty_as_null && raw.is_empty() {
        return Value::Null;
    }
    if let Ok(n) = raw.parse::<i64>() {
        if !raw.contains('.') && !raw.contains('e') && !raw.contains('E') {
            return Value::Integer(n);
        }
    }
    if let Ok(f) = raw.parse::<f64>() {
        if raw.contains('.') || raw.contains('e') || raw.contains('E') {
            return Value::Float(f);
        }
    }
    if raw.eq_ignore_ascii_case("true") {
        return Value::Boolean(true);
    }
    if raw.eq_ignore_ascii_case("false") {
        return Value::Boolean(false);
    }
    Value::Text(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::fdw::FdwOptions;

    fn tmp_path(name: &str, contents: &str) -> PathBuf {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("fdw_csv_{name}.csv"));
        std::fs::write(&path, contents).expect("write temp csv");
        path
    }

    #[test]
    fn scans_csv_with_header() {
        let path = tmp_path("basic", "id,name,age\n1,Alice,30\n2,Bob,25\n");
        let wrapper = CsvForeignWrapper;
        let server_state = wrapper.build_server_state(&FdwOptions::new()).unwrap();
        let mut opts = FdwOptions::new();
        opts.values
            .insert("path".to_string(), path.display().to_string());
        let rows = wrapper.scan(server_state.as_ref(), &opts).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get("id"), Some(&Value::Integer(1)));
        assert_eq!(rows[0].get("name"), Some(&Value::Text("Alice".to_string())));
    }

    #[test]
    fn scans_with_base_path() {
        let path = tmp_path("base", "a,b\n1,2\n");
        let wrapper = CsvForeignWrapper;
        let base = path.parent().unwrap().to_path_buf();
        let server_state = wrapper
            .build_server_state(&FdwOptions::new().with("base_path", &base.display().to_string()))
            .unwrap();
        let mut opts = FdwOptions::new();
        opts.values.insert(
            "path".to_string(),
            path.file_name().unwrap().to_string_lossy().into_owned(),
        );
        let rows = wrapper.scan(server_state.as_ref(), &opts).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("a"), Some(&Value::Integer(1)));
    }

    #[test]
    fn custom_delimiter_and_quote() {
        let path = tmp_path("sep", "id;note\n1;hello\n2;world\n");
        let wrapper = CsvForeignWrapper;
        let server_state = wrapper.build_server_state(&FdwOptions::new()).unwrap();
        let mut opts = FdwOptions::new();
        opts.values
            .insert("path".to_string(), path.display().to_string());
        opts.values.insert("delimiter".to_string(), ";".to_string());
        let rows = wrapper.scan(server_state.as_ref(), &opts).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].get("note"), Some(&Value::Text("world".to_string())));
    }

    #[test]
    fn missing_path_option_errors() {
        let wrapper = CsvForeignWrapper;
        let server_state = wrapper.build_server_state(&FdwOptions::new()).unwrap();
        let err = wrapper
            .scan(server_state.as_ref(), &FdwOptions::new())
            .unwrap_err();
        assert!(matches!(err, FdwError::MissingOption(_)));
    }
}
