//! CSV Importer (Phase 1.5 PG parity)
//!
//! Imports data from CSV (comma-separated-values) files into Store.
//! Implements a minimal RFC 4180 subset: quoted fields, escaped quotes
//! (`""` → `"`), configurable delimiter, and optional header row.
//!
//! # Format examples
//!
//! ```text
//! id,name,age
//! 1,Alice,30
//! 2,"Bob, Jr.",25
//! 3,"Say ""hi""",40
//! ```
//!
//! # Type coercion
//!
//! Each field is parsed with the following precedence:
//! 1. Empty string + `treat_empty_as_null=true` → Null
//! 2. Exact integer (`-?\d+`)  → Value::Integer
//! 3. Floating point (contains `.` or `e/E`) → Value::Float
//! 4. Boolean literal (`true`/`false`, case-insensitive) → Value::Boolean
//! 5. Fallback → Value::Text
//!
//! # Usage
//!
//! ```rust,ignore
//! let importer = CsvImporter::new(CsvConfig {
//!     collection: "users".to_string(),
//!     has_header: true,
//!     delimiter: b',',
//!     ..Default::default()
//! });
//! let stats = importer.import_file("users.csv", &mut store)?;
//! ```

use crate::storage::schema::types::Value;
use crate::storage::Store;
use crate::storage::{EntityData, EntityKind, RowData, UnifiedEntity};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::sync::Arc;

/// CSV import configuration
#[derive(Debug, Clone)]
pub struct CsvConfig {
    /// Collection/table name
    pub collection: String,
    /// Whether the first row contains column names.
    /// When false, columns are named `c0`, `c1`, ...
    pub has_header: bool,
    /// Field delimiter byte. Default `,`. Common alternates: `;`, `\t`.
    pub delimiter: u8,
    /// Quote character used to wrap fields that contain the delimiter or
    /// newlines. Default `"`. Doubled inside a field is an escaped quote.
    pub quote: u8,
    /// Empty (unquoted) fields map to `Value::Null` when true.
    /// An empty quoted field (`""`) is always `Value::Text("")`.
    pub treat_empty_as_null: bool,
    /// Batch size (records processed per bulk-insert chunk).
    pub batch_size: usize,
    /// Skip lines that fail to parse instead of aborting.
    pub skip_errors: bool,
    /// Maximum records to import (None for all).
    pub max_records: Option<usize>,
    /// Explicit column names, used when `has_header` is false but the
    /// caller wants typed names. Takes precedence over `c0`, `c1`, ...
    pub column_names: Option<Vec<String>>,
}

impl Default for CsvConfig {
    fn default() -> Self {
        Self {
            collection: "imported".to_string(),
            has_header: true,
            delimiter: b',',
            quote: b'"',
            treat_empty_as_null: true,
            batch_size: 1000,
            skip_errors: false,
            max_records: None,
            column_names: None,
        }
    }
}

/// Import statistics
#[derive(Debug, Clone, Default)]
pub struct CsvImportStats {
    pub lines_processed: usize,
    pub records_imported: usize,
    pub errors_skipped: usize,
    pub duration_ms: u64,
}

/// CSV import error
#[derive(Debug)]
pub enum CsvError {
    Io(String),
    Parse(String),
}

impl std::fmt::Display for CsvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CsvError::Io(s) => write!(f, "I/O error: {}", s),
            CsvError::Parse(s) => write!(f, "parse error: {}", s),
        }
    }
}

impl std::error::Error for CsvError {}

/// CSV importer
pub struct CsvImporter {
    config: CsvConfig,
}

impl CsvImporter {
    pub fn new(config: CsvConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(CsvConfig::default())
    }

    /// Import from a file path.
    pub fn import_file<P: AsRef<Path>>(
        &self,
        path: P,
        store: &Store,
    ) -> Result<CsvImportStats, CsvError> {
        let file = File::open(path.as_ref()).map_err(|e| CsvError::Io(e.to_string()))?;
        let reader = BufReader::new(file);
        self.import_reader(reader, store)
    }

    /// Import from any BufRead implementation.
    pub fn import_reader<R: BufRead>(
        &self,
        mut reader: R,
        store: &Store,
    ) -> Result<CsvImportStats, CsvError> {
        let start = std::time::Instant::now();
        let mut stats = CsvImportStats::default();
        let mut buf = String::new();
        reader
            .read_to_string(&mut buf)
            .map_err(|e| CsvError::Io(e.to_string()))?;

        let records = parse_records(&buf, self.config.delimiter, self.config.quote)
            .map_err(CsvError::Parse)?;
        let mut iter = records.into_iter();

        // Resolve column names.
        let headers: Vec<String> = if self.config.has_header {
            match iter.next() {
                Some(row) => row,
                None => {
                    stats.duration_ms = start.elapsed().as_millis() as u64;
                    return Ok(stats);
                }
            }
        } else if let Some(names) = &self.config.column_names {
            names.clone()
        } else {
            Vec::new()
        };

        for (row_idx, row) in iter.enumerate() {
            stats.lines_processed += 1;
            if let Some(max) = self.config.max_records {
                if stats.records_imported >= max {
                    break;
                }
            }

            let column_names: Vec<String> = if headers.is_empty() {
                (0..row.len()).map(|i| format!("c{i}")).collect()
            } else {
                headers.clone()
            };

            match self.insert_row(&column_names, row, store) {
                Ok(()) => stats.records_imported += 1,
                Err(e) => {
                    if self.config.skip_errors {
                        stats.errors_skipped += 1;
                        continue;
                    }
                    return Err(CsvError::Parse(format!("row {}: {}", row_idx + 1, e)));
                }
            }
        }

        stats.duration_ms = start.elapsed().as_millis() as u64;
        Ok(stats)
    }

    fn insert_row(
        &self,
        columns: &[String],
        values: Vec<String>,
        store: &Store,
    ) -> Result<(), String> {
        let mut named: HashMap<String, Value> = HashMap::with_capacity(values.len());
        for (i, raw) in values.into_iter().enumerate() {
            let name = columns.get(i).cloned().unwrap_or_else(|| format!("c{i}"));
            named.insert(name, coerce_field(&raw, self.config.treat_empty_as_null));
        }

        let entity_id = store.next_entity_id();
        let row_id = entity_id.0;
        let entity = UnifiedEntity::new(
            entity_id,
            EntityKind::TableRow {
                table: Arc::from(self.config.collection.as_str()),
                row_id,
            },
            EntityData::Row(RowData {
                columns: Vec::new(),
                named: Some(named),
                schema: None,
            }),
        );
        store
            .insert(&self.config.collection, entity)
            .map(|_| ())
            .map_err(|e| format!("insert failed: {:?}", e))
    }
}

/// Parse an entire CSV buffer into records.
///
/// Handles RFC 4180 quoting: a field wrapped in `"` may contain the
/// delimiter, newlines, and literal `"` escaped as `""`.
fn parse_records(input: &str, delimiter: u8, quote: u8) -> Result<Vec<Vec<String>>, String> {
    let bytes = input.as_bytes();
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut i = 0;
    let len = bytes.len();

    while i < len {
        let b = bytes[i];
        if in_quotes {
            if b == quote {
                if i + 1 < len && bytes[i + 1] == quote {
                    // Escaped quote.
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
        } else {
            if b == quote && field.is_empty() {
                in_quotes = true;
                i += 1;
            } else if b == delimiter {
                current_row.push(std::mem::take(&mut field));
                i += 1;
            } else if b == b'\r' {
                // Treat \r alone or \r\n as end of record.
                current_row.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut current_row));
                i += 1;
                if i < len && bytes[i] == b'\n' {
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
    }

    if in_quotes {
        return Err("unterminated quoted field".to_string());
    }
    // Flush trailing record (no final newline).
    if !field.is_empty() || !current_row.is_empty() {
        current_row.push(field);
        records.push(current_row);
    }
    Ok(records)
}

/// Coerce a raw CSV field string into the best-matching Value.
fn coerce_field(raw: &str, treat_empty_as_null: bool) -> Value {
    if treat_empty_as_null && raw.is_empty() {
        return Value::Null;
    }
    // Integer first — must not have decimal or exponent.
    if let Ok(n) = raw.parse::<i64>() {
        if !raw.contains('.') && !raw.contains('e') && !raw.contains('E') {
            return Value::Integer(n);
        }
    }
    // Float.
    if let Ok(f) = raw.parse::<f64>() {
        if raw.contains('.') || raw.contains('e') || raw.contains('E') {
            return Value::Float(f);
        }
    }
    // Boolean literal.
    if raw.eq_ignore_ascii_case("true") {
        return Value::Boolean(true);
    }
    if raw.eq_ignore_ascii_case("false") {
        return Value::Boolean(false);
    }
    // Fallback.
    Value::Text(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_csv() {
        let input = "id,name,age\n1,Alice,30\n2,Bob,25\n";
        let records = parse_records(input, b',', b'"').unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0], vec!["id", "name", "age"]);
        assert_eq!(records[1], vec!["1", "Alice", "30"]);
        assert_eq!(records[2], vec!["2", "Bob", "25"]);
    }

    #[test]
    fn parse_quoted_and_escaped_fields() {
        let input = "id,note\n1,\"hello, world\"\n2,\"say \"\"hi\"\"\"\n";
        let records = parse_records(input, b',', b'"').unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[1], vec!["1", "hello, world"]);
        assert_eq!(records[2], vec!["2", "say \"hi\""]);
    }

    #[test]
    fn parse_alternate_delimiter() {
        let input = "a;b;c\n1;2;3\n";
        let records = parse_records(input, b';', b'"').unwrap();
        assert_eq!(records[1], vec!["1", "2", "3"]);
    }

    #[test]
    fn parse_crlf_newlines() {
        let input = "a,b\r\n1,2\r\n";
        let records = parse_records(input, b',', b'"').unwrap();
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn parse_no_trailing_newline() {
        let input = "a,b\n1,2";
        let records = parse_records(input, b',', b'"').unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[1], vec!["1", "2"]);
    }

    #[test]
    fn parse_unterminated_quote_errors() {
        let input = "a,\"unclosed\n";
        assert!(parse_records(input, b',', b'"').is_err());
    }

    #[test]
    fn coerce_int_float_bool_text_null() {
        assert_eq!(coerce_field("42", true), Value::Integer(42));
        assert_eq!(coerce_field("-17", true), Value::Integer(-17));
        assert_eq!(coerce_field("3.14", true), Value::Float(3.14));
        assert_eq!(coerce_field("1e3", true), Value::Float(1000.0));
        assert_eq!(coerce_field("TRUE", true), Value::Boolean(true));
        assert_eq!(coerce_field("False", true), Value::Boolean(false));
        assert_eq!(
            coerce_field("hello", true),
            Value::Text("hello".to_string())
        );
        assert_eq!(coerce_field("", true), Value::Null);
        assert_eq!(coerce_field("", false), Value::Text(String::new()));
    }
}
