/// Schema-driven flag parser.
///
/// Given a list of `FlagSchema` descriptors and a token stream from the
/// lexer, validates flags, coerces values to the declared types, applies
/// defaults, and collects all errors so they can be reported at once.
use super::error::{suggest, ParseError};
use super::token::Token;
use super::types::{FlagSchema, FlagValue, ValueType};
use std::collections::HashMap;

/// Result of parsing tokens against a flag schema.
pub struct SchemaResult {
    pub flags: HashMap<String, FlagValue>,
    pub positionals: Vec<String>,
    pub errors: Vec<ParseError>,
}

/// Schema-driven flag parser.
pub struct SchemaParser {
    flags: Vec<FlagSchema>,
}

impl SchemaParser {
    pub fn new(flags: Vec<FlagSchema>) -> Self {
        Self { flags }
    }

    /// Parse tokens against the schema.
    ///
    /// Returns parsed flags (with defaults applied), remaining positionals,
    /// and any validation errors. Errors are collected rather than thrown so
    /// the caller can display all of them at once.
    pub fn parse(&self, tokens: &[Token]) -> SchemaResult {
        // Build lookup maps keyed by long name and short char.
        let mut long_map: HashMap<&str, &FlagSchema> = HashMap::new();
        let mut short_map: HashMap<char, &FlagSchema> = HashMap::new();

        for schema in &self.flags {
            long_map.insert(&schema.long, schema);
            if let Some(ch) = schema.short {
                short_map.insert(ch, schema);
            }
        }

        // Collect all long names for suggestion generation.
        let all_longs: Vec<&str> = self.flags.iter().map(|f| f.long.as_str()).collect();

        // Initialize flags map with defaults from schema.
        let mut flags: HashMap<String, FlagValue> = HashMap::new();
        for schema in &self.flags {
            if let Some(ref default) = schema.default {
                if let Ok(val) = coerce_value(default, schema.value_type) {
                    flags.insert(schema.long.clone(), val);
                }
            }
        }

        let mut positionals: Vec<String> = Vec::new();
        let mut errors: Vec<ParseError> = Vec::new();
        let mut i = 0;

        while i < tokens.len() {
            match &tokens[i] {
                Token::LongFlag { name, value } => {
                    // Handle --no-<name> boolean negation.
                    if let Some(base) = name.strip_prefix("no-") {
                        if let Some(schema) = long_map.get(base) {
                            if schema.value_type == ValueType::Bool {
                                flags.insert(base.to_string(), FlagValue::Bool(false));
                                i += 1;
                                continue;
                            }
                        }
                    }

                    match long_map.get(name.as_str()) {
                        None => {
                            let suggestions = suggest(name, &all_longs, 3)
                                .into_iter()
                                .map(|s| format!("--{}", s))
                                .collect();
                            errors.push(ParseError::UnknownFlag {
                                flag: format!("--{}", name),
                                suggestions,
                            });
                        }
                        Some(schema) => {
                            self.process_flag(
                                schema,
                                value,
                                tokens,
                                &mut i,
                                &mut flags,
                                &mut errors,
                            );
                        }
                    }
                    i += 1;
                }

                Token::ShortFlag { name, value } => {
                    match short_map.get(name) {
                        None => {
                            // Build suggestions from short flags that exist.
                            let all_shorts: Vec<&str> = self
                                .flags
                                .iter()
                                .filter_map(|f| f.short.as_ref())
                                .map(|_| "")
                                .collect();
                            let _ = all_shorts; // short flags are single chars, suggestion not very useful
                            errors.push(ParseError::UnknownFlag {
                                flag: format!("-{}", name),
                                suggestions: vec![],
                            });
                        }
                        Some(schema) => {
                            let str_value = value.as_ref().map(|v| v.to_string());
                            self.process_flag(
                                schema,
                                &str_value,
                                tokens,
                                &mut i,
                                &mut flags,
                                &mut errors,
                            );
                        }
                    }
                    i += 1;
                }

                Token::ShortCluster(chars) => {
                    let last_idx = chars.len() - 1;
                    for (ci, &ch) in chars.iter().enumerate() {
                        match short_map.get(&ch) {
                            None => {
                                errors.push(ParseError::UnknownFlag {
                                    flag: format!("-{}", ch),
                                    suggestions: vec![],
                                });
                            }
                            Some(schema) => {
                                if ci < last_idx {
                                    // All chars except the last must be boolean.
                                    if schema.expects_value {
                                        errors.push(ParseError::InvalidValue {
                      flag: format!("-{}", ch),
                      value: String::new(),
                      expected_type: format_type(schema.value_type),
                      reason: "flag requires a value and cannot appear in a cluster".to_string(),
                    });
                                    } else {
                                        store_flag(&mut flags, schema, FlagValue::Bool(true));
                                    }
                                } else {
                                    // Last char: may consume next positional if it expects a value.
                                    if schema.expects_value {
                                        if i + 1 < tokens.len() {
                                            if let Token::Positional(ref val) = tokens[i + 1] {
                                                match coerce_value(val, schema.value_type) {
                                                    Ok(fv) => {
                                                        store_flag(&mut flags, schema, fv);
                                                    }
                                                    Err(reason) => {
                                                        errors.push(ParseError::InvalidValue {
                                                            flag: format!("-{}", ch),
                                                            value: val.clone(),
                                                            expected_type: format_type(
                                                                schema.value_type,
                                                            ),
                                                            reason,
                                                        });
                                                    }
                                                }
                                                i += 1; // consume the peeked positional
                                            } else {
                                                errors.push(ParseError::MissingFlagValue {
                                                    flag: format!("-{}", ch),
                                                    expected_type: format_type(schema.value_type),
                                                });
                                            }
                                        } else {
                                            errors.push(ParseError::MissingFlagValue {
                                                flag: format!("-{}", ch),
                                                expected_type: format_type(schema.value_type),
                                            });
                                        }
                                    } else {
                                        store_flag(&mut flags, schema, FlagValue::Bool(true));
                                    }
                                }
                            }
                        }
                    }
                    i += 1;
                }

                Token::Positional(s) => {
                    positionals.push(s.clone());
                    i += 1;
                }

                Token::EndOfOptions => {
                    // Everything after becomes positional.
                    i += 1;
                    while i < tokens.len() {
                        match &tokens[i] {
                            Token::Positional(s) => positionals.push(s.clone()),
                            // After EndOfOptions the tokenizer already wraps everything as Positional,
                            // but handle other variants defensively.
                            Token::LongFlag { name, value } => {
                                let mut repr = format!("--{}", name);
                                if let Some(v) = value {
                                    repr.push('=');
                                    repr.push_str(v);
                                }
                                positionals.push(repr);
                            }
                            Token::ShortFlag { name, value } => {
                                let mut repr = format!("-{}", name);
                                if let Some(v) = value {
                                    repr.push('=');
                                    repr.push_str(v);
                                }
                                positionals.push(repr);
                            }
                            Token::ShortCluster(chars) => {
                                let s: String = chars.iter().collect();
                                positionals.push(format!("-{}", s));
                            }
                            Token::EndOfOptions => {
                                positionals.push("--".to_string());
                            }
                        }
                        i += 1;
                    }
                }
            }
        }

        // Post-parse validation: required flags.
        for schema in &self.flags {
            if schema.required && !flags.contains_key(&schema.long) {
                errors.push(ParseError::MissingRequired {
                    flag: format!("--{}", schema.long),
                });
            }
        }

        // Post-parse validation: choices.
        for schema in &self.flags {
            if let Some(ref choices) = schema.choices {
                if let Some(val) = flags.get(&schema.long) {
                    let s = val.as_str_value();
                    if !choices.contains(&s) {
                        errors.push(ParseError::InvalidChoice {
                            flag: format!("--{}", schema.long),
                            value: s,
                            allowed: choices.clone(),
                        });
                    }
                }
            }
        }

        SchemaResult {
            flags,
            positionals,
            errors,
        }
    }

    /// Process a single flag (long or short) that matched a schema entry.
    /// Handles value consumption, peek-ahead, and boolean rejection.
    fn process_flag(
        &self,
        schema: &FlagSchema,
        value: &Option<String>,
        tokens: &[Token],
        i: &mut usize,
        flags: &mut HashMap<String, FlagValue>,
        errors: &mut Vec<ParseError>,
    ) {
        let flag_display = if let Some(ch) = schema.short {
            if schema.long.is_empty() {
                format!("-{}", ch)
            } else {
                format!("--{}", schema.long)
            }
        } else {
            format!("--{}", schema.long)
        };

        if schema.expects_value {
            match value {
                Some(raw) => match coerce_value(raw, schema.value_type) {
                    Ok(fv) => {
                        store_flag(flags, schema, fv);
                    }
                    Err(reason) => {
                        errors.push(ParseError::InvalidValue {
                            flag: flag_display,
                            value: raw.clone(),
                            expected_type: format_type(schema.value_type),
                            reason,
                        });
                    }
                },
                None => {
                    // Peek next token for the value.
                    if *i + 1 < tokens.len() {
                        if let Token::Positional(ref val) = tokens[*i + 1] {
                            match coerce_value(val, schema.value_type) {
                                Ok(fv) => {
                                    store_flag(flags, schema, fv);
                                }
                                Err(reason) => {
                                    errors.push(ParseError::InvalidValue {
                                        flag: flag_display,
                                        value: val.clone(),
                                        expected_type: format_type(schema.value_type),
                                        reason,
                                    });
                                }
                            }
                            *i += 1; // consume the peeked token
                            return;
                        }
                    }
                    errors.push(ParseError::MissingFlagValue {
                        flag: flag_display,
                        expected_type: format_type(schema.value_type),
                    });
                }
            }
        } else {
            // Boolean flag -- must not have an inline value.
            if let Some(inline_value) = value {
                errors.push(ParseError::InvalidValue {
                    flag: flag_display,
                    value: inline_value.clone(),
                    expected_type: "bool".to_string(),
                    reason: "boolean flags do not accept a value".to_string(),
                });
            } else {
                store_flag(flags, schema, FlagValue::Bool(true));
            }
        }
    }
}

/// Store a flag value, handling repeated-flag semantics.
fn store_flag(flags: &mut HashMap<String, FlagValue>, schema: &FlagSchema, value: FlagValue) {
    if schema.value_type == ValueType::Count {
        let current = flags.get(&schema.long).and_then(|v| {
            if let FlagValue::Count(n) = v {
                Some(*n)
            } else {
                None
            }
        });
        flags.insert(
            schema.long.clone(),
            FlagValue::Count(current.unwrap_or(0) + 1),
        );
    } else {
        // Last value wins for all other types.
        flags.insert(schema.long.clone(), value);
    }
}

/// Coerce a raw string to the declared value type.
fn coerce_value(raw: &str, value_type: ValueType) -> Result<FlagValue, String> {
    match value_type {
        ValueType::String => Ok(FlagValue::Str(raw.to_string())),
        ValueType::Bool => match raw.to_lowercase().as_str() {
            "true" | "yes" | "1" | "on" => Ok(FlagValue::Bool(true)),
            "false" | "no" | "0" | "off" => Ok(FlagValue::Bool(false)),
            _ => Err(format!("expected boolean, got '{}'", raw)),
        },
        ValueType::Integer => raw
            .parse::<i64>()
            .map(FlagValue::Int)
            .map_err(|_| format!("expected integer, got '{}'", raw)),
        ValueType::Float => raw
            .parse::<f64>()
            .map(FlagValue::Float)
            .map_err(|_| format!("expected number, got '{}'", raw)),
        ValueType::Count => Ok(FlagValue::Count(1)),
    }
}

/// Human-readable name for a value type (used in error messages).
fn format_type(vt: ValueType) -> String {
    match vt {
        ValueType::String => "string".to_string(),
        ValueType::Bool => "bool".to_string(),
        ValueType::Integer => "integer".to_string(),
        ValueType::Float => "float".to_string(),
        ValueType::Count => "count".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::token::tokenize;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|a| a.to_string()).collect()
    }

    fn make_parser(schemas: Vec<FlagSchema>) -> SchemaParser {
        SchemaParser::new(schemas)
    }

    #[test]
    fn test_parse_long_flag_bool() {
        let parser = make_parser(vec![FlagSchema::boolean("verbose").with_short('v')]);
        let tokens = tokenize(&args(&["--verbose"]));
        let result = parser.parse(&tokens);

        assert!(result.errors.is_empty());
        assert_eq!(result.flags.get("verbose"), Some(&FlagValue::Bool(true)));
    }

    #[test]
    fn test_parse_long_flag_with_value() {
        let parser = make_parser(vec![FlagSchema::new("output").with_short('o')]);
        let tokens = tokenize(&args(&["--output", "json"]));
        let result = parser.parse(&tokens);

        assert!(result.errors.is_empty());
        assert_eq!(
            result.flags.get("output"),
            Some(&FlagValue::Str("json".to_string()))
        );
        assert!(result.positionals.is_empty());
    }

    #[test]
    fn test_parse_long_flag_equals() {
        let parser = make_parser(vec![FlagSchema::new("output")]);
        let tokens = tokenize(&args(&["--output=json"]));
        let result = parser.parse(&tokens);

        assert!(result.errors.is_empty());
        assert_eq!(
            result.flags.get("output"),
            Some(&FlagValue::Str("json".to_string()))
        );
    }

    #[test]
    fn test_parse_short_flag() {
        let parser = make_parser(vec![FlagSchema::boolean("verbose").with_short('v')]);
        let tokens = tokenize(&args(&["-v"]));
        let result = parser.parse(&tokens);

        assert!(result.errors.is_empty());
        assert_eq!(result.flags.get("verbose"), Some(&FlagValue::Bool(true)));
    }

    #[test]
    fn test_parse_short_cluster() {
        let parser = make_parser(vec![
            FlagSchema::boolean("verbose").with_short('v'),
            FlagSchema::boolean("all").with_short('a'),
            FlagSchema::boolean("force").with_short('f'),
        ]);
        let tokens = tokenize(&args(&["-vaf"]));
        let result = parser.parse(&tokens);

        assert!(result.errors.is_empty());
        assert_eq!(result.flags.get("verbose"), Some(&FlagValue::Bool(true)));
        assert_eq!(result.flags.get("all"), Some(&FlagValue::Bool(true)));
        assert_eq!(result.flags.get("force"), Some(&FlagValue::Bool(true)));
    }

    #[test]
    fn test_parse_short_cluster_last_takes_value() {
        let parser = make_parser(vec![
            FlagSchema::boolean("verbose").with_short('v'),
            FlagSchema::new("output").with_short('o'),
        ]);
        let tokens = tokenize(&args(&["-vo", "json"]));
        let result = parser.parse(&tokens);

        assert!(result.errors.is_empty());
        assert_eq!(result.flags.get("verbose"), Some(&FlagValue::Bool(true)));
        assert_eq!(
            result.flags.get("output"),
            Some(&FlagValue::Str("json".to_string()))
        );
        assert!(result.positionals.is_empty());
    }

    #[test]
    fn test_parse_negation() {
        let parser = make_parser(vec![FlagSchema::boolean("verbose").with_short('v')]);
        let tokens = tokenize(&args(&["--no-verbose"]));
        let result = parser.parse(&tokens);

        assert!(result.errors.is_empty());
        assert_eq!(result.flags.get("verbose"), Some(&FlagValue::Bool(false)));
    }

    #[test]
    fn test_parse_unknown_flag_suggests() {
        let parser = make_parser(vec![
            FlagSchema::boolean("verbose"),
            FlagSchema::new("output"),
        ]);
        let tokens = tokenize(&args(&["--verbos"]));
        let result = parser.parse(&tokens);

        assert_eq!(result.errors.len(), 1);
        if let ParseError::UnknownFlag {
            ref flag,
            ref suggestions,
        } = result.errors[0]
        {
            assert_eq!(flag, "--verbos");
            assert!(suggestions.contains(&"--verbose".to_string()));
        } else {
            panic!("expected UnknownFlag error");
        }
    }

    #[test]
    fn test_parse_missing_value() {
        let parser = make_parser(vec![FlagSchema::new("output")]);
        let tokens = tokenize(&args(&["--output"]));
        let result = parser.parse(&tokens);

        assert_eq!(result.errors.len(), 1);
        assert!(matches!(
          &result.errors[0],
          ParseError::MissingFlagValue { flag, .. } if flag == "--output"
        ));
    }

    #[test]
    fn test_parse_invalid_type() {
        let parser = make_parser(vec![{
            let mut s = FlagSchema::new("threads");
            s.value_type = ValueType::Integer;
            s
        }]);
        let tokens = tokenize(&args(&["--threads=abc"]));
        let result = parser.parse(&tokens);

        assert_eq!(result.errors.len(), 1);
        assert!(matches!(
          &result.errors[0],
          ParseError::InvalidValue { flag, value, .. } if flag == "--threads" && value == "abc"
        ));
    }

    #[test]
    fn test_parse_invalid_choice() {
        let parser = make_parser(vec![
            FlagSchema::new("output").with_choices(&["text", "json", "yaml"])
        ]);
        let tokens = tokenize(&args(&["--output=xml"]));
        let result = parser.parse(&tokens);

        assert_eq!(result.errors.len(), 1);
        assert!(matches!(
          &result.errors[0],
          ParseError::InvalidChoice { flag, value, .. } if flag == "--output" && value == "xml"
        ));
    }

    #[test]
    fn test_parse_required_missing() {
        let parser = make_parser(vec![FlagSchema::new("target").required()]);
        let tokens = tokenize(&args(&[]));
        let result = parser.parse(&tokens);

        assert_eq!(result.errors.len(), 1);
        assert!(matches!(
          &result.errors[0],
          ParseError::MissingRequired { flag } if flag == "--target"
        ));
    }

    #[test]
    fn test_parse_defaults_applied() {
        let parser = make_parser(vec![
            FlagSchema::new("output").with_default("text"),
            FlagSchema::boolean("verbose"),
        ]);
        let tokens = tokenize(&args(&[]));
        let result = parser.parse(&tokens);

        assert!(result.errors.is_empty());
        assert_eq!(
            result.flags.get("output"),
            Some(&FlagValue::Str("text".to_string()))
        );
        // Boolean without default should not be in the map.
        assert!(!result.flags.contains_key("verbose"));
    }

    #[test]
    fn test_parse_end_of_options() {
        let parser = make_parser(vec![FlagSchema::boolean("verbose").with_short('v')]);
        let tokens = tokenize(&args(&["-v", "--", "--not-a-flag", "target"]));
        let result = parser.parse(&tokens);

        assert!(result.errors.is_empty());
        assert_eq!(result.flags.get("verbose"), Some(&FlagValue::Bool(true)));
        assert_eq!(result.positionals, vec!["--not-a-flag", "target"]);
    }

    #[test]
    fn test_parse_positionals_preserved() {
        let parser = make_parser(vec![FlagSchema::boolean("verbose").with_short('v')]);
        let tokens = tokenize(&args(&["server", "192.168.1.1", "-v", "extra"]));
        let result = parser.parse(&tokens);

        assert!(result.errors.is_empty());
        assert_eq!(result.flags.get("verbose"), Some(&FlagValue::Bool(true)));
        assert_eq!(result.positionals, vec!["server", "192.168.1.1", "extra"]);
    }

    #[test]
    fn test_parse_mixed_realistic() {
        // red serve --path /data --bind 0.0.0.0:6380 --role primary -vo json --no-color
        let parser = make_parser(vec![
            FlagSchema::new("path").with_short('p'),
            FlagSchema::new("bind").with_short('b'),
            FlagSchema::new("role").with_short('r'),
            FlagSchema::boolean("verbose").with_short('v'),
            FlagSchema::new("output")
                .with_short('o')
                .with_choices(&["text", "json", "yaml"]),
            FlagSchema::boolean("no-color"),
        ]);
        let tokens = tokenize(&args(&[
            "server",
            "--path",
            "/data",
            "--bind",
            "0.0.0.0:6380",
            "--role",
            "primary",
            "-vo",
            "json",
            "--no-color",
        ]));
        let result = parser.parse(&tokens);

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.positionals, vec!["server"]);
        assert_eq!(
            result.flags.get("path"),
            Some(&FlagValue::Str("/data".to_string()))
        );
        assert_eq!(
            result.flags.get("bind"),
            Some(&FlagValue::Str("0.0.0.0:6380".to_string()))
        );
        assert_eq!(
            result.flags.get("role"),
            Some(&FlagValue::Str("primary".to_string()))
        );
        assert_eq!(result.flags.get("verbose"), Some(&FlagValue::Bool(true)));
        assert_eq!(
            result.flags.get("output"),
            Some(&FlagValue::Str("json".to_string()))
        );
        assert_eq!(result.flags.get("no-color"), Some(&FlagValue::Bool(true)));
    }
}
