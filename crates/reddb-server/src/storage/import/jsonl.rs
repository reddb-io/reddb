//! JSONL (JSON Lines) Importer
//!
//! Imports data from JSONL (newline-delimited JSON) files into Store.
//! Supports streaming import for large files.
//!
//! # Format
//!
//! Each line is a valid JSON object:
//! ```text
//! {"id": "1", "name": "Alice", "embedding": [0.1, 0.2, 0.3]}
//! {"id": "2", "name": "Bob", "embedding": [0.4, 0.5, 0.6]}
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! let importer = JsonlImporter::new(JsonlConfig {
//!     id_field: Some("id".to_string()),
//!     embedding_field: Some("embedding".to_string()),
//!     batch_size: 1000,
//! });
//! let stats = importer.import_file("data.jsonl", &mut store)?;
//! ```

use crate::storage::schema::types::Value;
use crate::storage::Store;
use crate::storage::{EntityData, EntityKind, RowData, UnifiedEntity, VectorData};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::Arc;

/// JSONL import configuration
#[derive(Debug, Clone)]
pub struct JsonlConfig {
    /// Field to use as entity ID (auto-generated if None)
    pub id_field: Option<String>,
    /// Field containing vector embedding (if any)
    pub embedding_field: Option<String>,
    /// Collection/table name
    pub collection: String,
    /// Number of records to process in each batch
    pub batch_size: usize,
    /// Skip lines that fail to parse
    pub skip_errors: bool,
    /// Maximum lines to import (None for all)
    pub max_lines: Option<usize>,
}

impl Default for JsonlConfig {
    fn default() -> Self {
        Self {
            id_field: None,
            embedding_field: None,
            collection: "imported".to_string(),
            batch_size: 1000,
            skip_errors: false,
            max_lines: None,
        }
    }
}

/// Import statistics
#[derive(Debug, Clone, Default)]
pub struct ImportStats {
    /// Total lines processed
    pub lines_processed: usize,
    /// Records successfully imported
    pub records_imported: usize,
    /// Lines skipped due to errors
    pub errors_skipped: usize,
    /// Import duration in milliseconds
    pub duration_ms: u64,
}

/// JSONL importer
pub struct JsonlImporter {
    config: JsonlConfig,
}

impl JsonlImporter {
    /// Create a new JSONL importer with configuration
    pub fn new(config: JsonlConfig) -> Self {
        Self { config }
    }

    /// Create a default importer
    pub fn with_defaults() -> Self {
        Self::new(JsonlConfig::default())
    }

    /// Import from a file path
    pub fn import_file<P: AsRef<Path>>(
        &self,
        path: P,
        store: &mut Store,
    ) -> Result<ImportStats, JsonlError> {
        let file = File::open(path.as_ref()).map_err(|e| JsonlError::Io(e.to_string()))?;
        let reader = BufReader::new(file);
        self.import_reader(reader, store)
    }

    /// Import from any reader (for flexibility)
    pub fn import_reader<R: BufRead>(
        &self,
        reader: R,
        store: &mut Store,
    ) -> Result<ImportStats, JsonlError> {
        let start = std::time::Instant::now();
        let mut stats = ImportStats::default();

        for (line_num, line_result) in reader.lines().enumerate() {
            // Check max lines limit
            if let Some(max) = self.config.max_lines {
                if stats.lines_processed >= max {
                    break;
                }
            }

            stats.lines_processed += 1;

            let line = match line_result {
                Ok(l) => l,
                Err(e) => {
                    if self.config.skip_errors {
                        stats.errors_skipped += 1;
                        continue;
                    }
                    return Err(JsonlError::Io(format!("Line {}: {}", line_num + 1, e)));
                }
            };

            // Skip empty lines
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Parse JSON
            match self.parse_and_insert(&line, store) {
                Ok(()) => {
                    stats.records_imported += 1;
                }
                Err(e) => {
                    if self.config.skip_errors {
                        stats.errors_skipped += 1;
                        continue;
                    }
                    return Err(JsonlError::Parse(format!("Line {}: {}", line_num + 1, e)));
                }
            }
        }

        stats.duration_ms = start.elapsed().as_millis() as u64;
        Ok(stats)
    }

    /// Parse a single JSON line and insert into store
    fn parse_and_insert(&self, line: &str, store: &mut Store) -> Result<(), String> {
        let json = parse_json_object(line)?;

        // Extract embedding if configured
        let embedding = if let Some(ref emb_field) = self.config.embedding_field {
            json.get(emb_field).and_then(|v| {
                if let JsonValue::Array(arr) = v {
                    let floats: Option<Vec<f32>> = arr
                        .iter()
                        .map(|v| match v {
                            JsonValue::Number(n) => Some(*n as f32),
                            _ => None,
                        })
                        .collect();
                    floats
                } else {
                    None
                }
            })
        } else {
            None
        };

        // Convert JSON to row data
        let mut named = HashMap::new();
        for (key, value) in &json {
            // Skip embedding field (handled separately)
            if self
                .config
                .embedding_field
                .as_ref()
                .map(|f| f == key)
                .unwrap_or(false)
            {
                continue;
            }

            named.insert(key.clone(), json_to_value(value));
        }

        // Generate IDs
        let entity_id = store.next_entity_id();
        let row_id = entity_id.0;

        // Create entity
        let entity = if let Some(emb) = embedding {
            // Entity with embedding - store as vector

            UnifiedEntity::new(
                entity_id,
                EntityKind::Vector {
                    collection: self.config.collection.clone(),
                },
                EntityData::Vector(VectorData {
                    dense: emb,
                    sparse: None,
                    content: Some(line.to_string()),
                }),
            )
        } else {
            // Plain row entity
            let row_data = RowData {
                columns: Vec::new(),
                named: Some(named),
                schema: None,
            };
            UnifiedEntity::new(
                entity_id,
                EntityKind::TableRow {
                    table: Arc::from(self.config.collection.as_str()),
                    row_id,
                },
                EntityData::Row(row_data),
            )
        };

        store
            .insert(&self.config.collection, entity)
            .map_err(|e| format!("Insert failed: {:?}", e))?;

        Ok(())
    }
}

/// JSONL import error
#[derive(Debug)]
pub enum JsonlError {
    Io(String),
    Parse(String),
}

impl std::fmt::Display for JsonlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JsonlError::Io(s) => write!(f, "I/O error: {}", s),
            JsonlError::Parse(s) => write!(f, "Parse error: {}", s),
        }
    }
}

impl std::error::Error for JsonlError {}

// ============================================================================
// Simple JSON Parser (no external dependencies)
// ============================================================================

/// Simple JSON value representation
#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsonValue>),
    Object(HashMap<String, JsonValue>),
}

/// Parse a JSON object from a string
pub fn parse_json_object(s: &str) -> Result<HashMap<String, JsonValue>, String> {
    let mut chars = s.trim().chars().peekable();

    // Expect opening brace
    match chars.next() {
        Some('{') => {}
        _ => return Err("Expected '{'".to_string()),
    }

    skip_whitespace(&mut chars);

    // Empty object
    if chars.peek() == Some(&'}') {
        chars.next();
        return Ok(HashMap::new());
    }

    let mut result = HashMap::new();

    loop {
        skip_whitespace(&mut chars);

        // Parse key
        let key = parse_string(&mut chars)?;

        skip_whitespace(&mut chars);

        // Expect colon
        match chars.next() {
            Some(':') => {}
            _ => return Err("Expected ':'".to_string()),
        }

        skip_whitespace(&mut chars);

        // Parse value
        let value = parse_value(&mut chars)?;

        result.insert(key, value);

        skip_whitespace(&mut chars);

        // Check for comma or closing brace
        match chars.next() {
            Some(',') => continue,
            Some('}') => break,
            _ => return Err("Expected ',' or '}'".to_string()),
        }
    }

    Ok(result)
}

fn parse_value(chars: &mut std::iter::Peekable<std::str::Chars>) -> Result<JsonValue, String> {
    skip_whitespace(chars);

    match chars.peek() {
        Some('"') => Ok(JsonValue::String(parse_string(chars)?)),
        Some('[') => parse_array(chars),
        Some('{') => {
            // Collect the object as a string and parse it
            let mut depth = 0;
            let mut obj_str = String::new();
            for c in chars.by_ref() {
                obj_str.push(c);
                if c == '{' {
                    depth += 1;
                }
                if c == '}' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
            }
            parse_json_object(&obj_str).map(JsonValue::Object)
        }
        Some('t') | Some('f') => parse_bool(chars),
        Some('n') => parse_null(chars),
        Some(c) if c.is_ascii_digit() || *c == '-' => parse_number(chars),
        _ => Err("Unexpected character".to_string()),
    }
}

fn parse_string(chars: &mut std::iter::Peekable<std::str::Chars>) -> Result<String, String> {
    // Expect opening quote
    match chars.next() {
        Some('"') => {}
        _ => return Err("Expected '\"'".to_string()),
    }

    let mut result = String::new();
    let mut escaped = false;

    loop {
        match chars.next() {
            Some('\\') if !escaped => {
                escaped = true;
            }
            Some('"') if !escaped => {
                break;
            }
            Some(c) => {
                if escaped {
                    match c {
                        'n' => result.push('\n'),
                        'r' => result.push('\r'),
                        't' => result.push('\t'),
                        '\\' => result.push('\\'),
                        '"' => result.push('"'),
                        _ => {
                            result.push('\\');
                            result.push(c);
                        }
                    }
                    escaped = false;
                } else {
                    result.push(c);
                }
            }
            None => return Err("Unterminated string".to_string()),
        }
    }

    Ok(result)
}

fn parse_number(chars: &mut std::iter::Peekable<std::str::Chars>) -> Result<JsonValue, String> {
    let mut num_str = String::new();

    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e' || c == 'E' {
            num_str.push(c);
            chars.next();
        } else {
            break;
        }
    }

    num_str
        .parse::<f64>()
        .map(JsonValue::Number)
        .map_err(|_| "Invalid number".to_string())
}

fn parse_array(chars: &mut std::iter::Peekable<std::str::Chars>) -> Result<JsonValue, String> {
    // Expect opening bracket
    match chars.next() {
        Some('[') => {}
        _ => return Err("Expected '['".to_string()),
    }

    skip_whitespace(chars);

    // Empty array
    if chars.peek() == Some(&']') {
        chars.next();
        return Ok(JsonValue::Array(Vec::new()));
    }

    let mut result = Vec::new();

    loop {
        skip_whitespace(chars);
        result.push(parse_value(chars)?);
        skip_whitespace(chars);

        match chars.next() {
            Some(',') => continue,
            Some(']') => break,
            _ => return Err("Expected ',' or ']'".to_string()),
        }
    }

    Ok(JsonValue::Array(result))
}

fn parse_bool(chars: &mut std::iter::Peekable<std::str::Chars>) -> Result<JsonValue, String> {
    let mut word = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_alphabetic() {
            word.push(c);
            chars.next();
        } else {
            break;
        }
    }

    match word.as_str() {
        "true" => Ok(JsonValue::Bool(true)),
        "false" => Ok(JsonValue::Bool(false)),
        _ => Err(format!("Invalid boolean: {}", word)),
    }
}

fn parse_null(chars: &mut std::iter::Peekable<std::str::Chars>) -> Result<JsonValue, String> {
    let mut word = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_alphabetic() {
            word.push(c);
            chars.next();
        } else {
            break;
        }
    }

    if word == "null" {
        Ok(JsonValue::Null)
    } else {
        Err(format!("Invalid null: {}", word))
    }
}

fn skip_whitespace(chars: &mut std::iter::Peekable<std::str::Chars>) {
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else {
            break;
        }
    }
}

/// Convert JsonValue to storage Value
fn json_to_value(jv: &JsonValue) -> Value {
    match jv {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(b) => Value::Boolean(*b),
        JsonValue::Number(n) => {
            if n.fract() == 0.0 && *n >= i64::MIN as f64 && *n <= i64::MAX as f64 {
                Value::Integer(*n as i64)
            } else {
                Value::Float(*n)
            }
        }
        JsonValue::String(s) => Value::text(s.clone()),
        JsonValue::Array(arr) => Value::text(format!(
            "[{}]",
            arr.iter()
                .map(|v| value_to_string(&json_to_value(v)))
                .collect::<Vec<_>>()
                .join(",")
        )),
        JsonValue::Object(_) => Value::text("[object]".to_string()),
    }
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::UnsignedInteger(u) => u.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Text(s) => s.to_string(),
        Value::Blob(b) => format!("<{} bytes>", b.len()),
        _ => "?".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_object() {
        let json = r#"{"name": "Alice", "age": 30}"#;
        let parsed = parse_json_object(json).unwrap();

        assert_eq!(
            parsed.get("name"),
            Some(&JsonValue::String("Alice".to_string()))
        );
        assert!(
            matches!(parsed.get("age"), Some(JsonValue::Number(n)) if (*n - 30.0).abs() < 0.01)
        );
    }

    #[test]
    fn test_parse_with_array() {
        let json = r#"{"embedding": [0.1, 0.2, 0.3]}"#;
        let parsed = parse_json_object(json).unwrap();

        if let Some(JsonValue::Array(arr)) = parsed.get("embedding") {
            assert_eq!(arr.len(), 3);
        } else {
            panic!("Expected array");
        }
    }

    #[test]
    fn test_parse_nested() {
        let json = r#"{"user": {"name": "Bob"}}"#;
        let parsed = parse_json_object(json).unwrap();

        if let Some(JsonValue::Object(obj)) = parsed.get("user") {
            assert_eq!(obj.get("name"), Some(&JsonValue::String("Bob".to_string())));
        } else {
            panic!("Expected nested object");
        }
    }

    #[test]
    fn test_parse_escape_sequences() {
        let json = r#"{"text": "Hello\nWorld"}"#;
        let parsed = parse_json_object(json).unwrap();

        assert_eq!(
            parsed.get("text"),
            Some(&JsonValue::String("Hello\nWorld".to_string()))
        );
    }

    #[test]
    fn test_json_to_value() {
        assert_eq!(json_to_value(&JsonValue::Null), Value::Null);
        assert_eq!(json_to_value(&JsonValue::Bool(true)), Value::Boolean(true));
        assert_eq!(json_to_value(&JsonValue::Number(42.0)), Value::Integer(42));
        assert_eq!(json_to_value(&JsonValue::Number(2.5)), Value::Float(2.5));
        assert_eq!(
            json_to_value(&JsonValue::String("test".to_string())),
            Value::text("test".to_string())
        );
    }
}
