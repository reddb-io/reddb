//! `McpAskTool` — pure descriptor + arg parser for exposing
//! `ASK '...'` as an MCP tool (issue #409, PRD #391).
//!
//! Deep module: no I/O, no transport. The MCP server wiring owns
//! tool-registration plumbing, the per-call dispatch into
//! `execute_ask`, and any progressive-response framing. This module
//! owns "what's the tool's JSON-Schema?" and "given an MCP `arguments`
//! payload, are the inputs well-formed, and what did the caller
//! ask for?".
//!
//! Two halves:
//!
//! 1. [`descriptor`] returns the full tool record — `name`,
//!    `description`, `inputSchema` (JSON Schema draft-07 compatible).
//!    Pinned by tests so the wire shape can't silently drift when a
//!    field is renamed.
//! 2. [`parse`] takes the MCP `arguments` object and returns either a
//!    fully-typed [`AskInvocation`] or a typed [`ParseError`] naming
//!    the offending field and the contract that failed. Mapping
//!    errors → MCP error frames is the wiring layer's job.
//!
//! ## Why a deep module
//!
//! MCP clients call this tool with arbitrary JSON — wrong types,
//! out-of-range numbers, both `cache` and `nocache` set, missing
//! `question`. Drifting the validation rules between the SQL surface
//! (`ASK '...' LIMIT N STRICT OFF`) and the MCP surface would let a
//! tool call set a value that the SQL parser rejects, surfacing
//! confusing errors deep in the pipeline. Pinning the parser here
//! keeps the two surfaces aligned by construction: every option this
//! module accepts MUST also be a recognised SQL clause; tests pin
//! that 1:1 mapping.
//!
//! ## Option parity with SQL
//!
//! `ASK '...'` SQL options accepted, per #395/#396/#398/#400/#401/#403:
//!
//! | SQL                       | MCP option key   | Type           |
//! |---------------------------|------------------|----------------|
//! | `STRICT ON|OFF`           | `strict`         | `bool`         |
//! | `USING <provider>`        | `using`          | `string`       |
//! | `MODEL <name>`            | `model`          | `string`       |
//! | `LIMIT <n>`               | `limit`          | `u32` 1..=200  |
//! | `MIN_SCORE <f>`           | `min_score`      | `f64` 0..=1    |
//! | `DEPTH <n>`               | `depth`          | `u32` 0..=10   |
//! | `TEMPERATURE <f>`         | `temperature`    | `f64` 0..=2    |
//! | `SEED <n>`                | `seed`           | `u64`          |
//! | `CACHE TTL '5m'`          | `cache.ttl`      | duration str   |
//! | `NOCACHE`                 | `nocache`        | `bool` (true)  |
//!
//! `cache` and `nocache` are mutually exclusive (a single ASK call
//! either caches with a TTL or skips the cache; both is nonsensical
//! and the SQL parser already rejects it, so the MCP parser must too).

use crate::serde_json::{Map, Value};

/// MCP tool name. Pinned — clients hard-code this.
pub const TOOL_NAME: &str = "reddb.ask";

/// JSON-Schema draft used in `inputSchema.$schema`. Draft-07 is what
/// the MCP spec assumes and what every MCP client validates against.
pub const SCHEMA_DRAFT: &str = "http://json-schema.org/draft-07/schema#";

/// Caller-facing limits. Pinned at module level so a future change is
/// visible in one place and asserted by tests.
pub const LIMIT_MIN: u32 = 1;
pub const LIMIT_MAX: u32 = 200;
pub const LIMIT_DEFAULT: u32 = 20;
pub const DEPTH_MIN: u32 = 0;
pub const DEPTH_MAX: u32 = 10;
pub const DEPTH_DEFAULT: u32 = 2;
pub const MIN_SCORE_MIN: f64 = 0.0;
pub const MIN_SCORE_MAX: f64 = 1.0;
pub const TEMPERATURE_MIN: f64 = 0.0;
pub const TEMPERATURE_MAX: f64 = 2.0;

/// One MCP tool call's parsed, validated arguments. Every field bar
/// `question` is optional — the engine applies defaults downstream
/// (DeterminismDecider, RrfFuser, etc.), and `None` here means "no
/// override, use the engine default" rather than "use zero".
#[derive(Debug, Clone, PartialEq)]
pub struct AskInvocation {
    pub question: String,
    pub strict: Option<bool>,
    pub using: Option<String>,
    pub model: Option<String>,
    pub limit: Option<u32>,
    pub min_score: Option<f64>,
    pub depth: Option<u32>,
    pub temperature: Option<f64>,
    pub seed: Option<u64>,
    pub cache_ttl: Option<String>,
    pub nocache: Option<bool>,
}

/// Typed parse failures. Each variant names the offending JSON path
/// (e.g. `options.limit`) so the wiring layer can echo it back in
/// the MCP error frame without rebuilding the string.
#[derive(Debug, Clone, PartialEq)]
pub enum ParseError {
    /// `arguments` is not a JSON object.
    NotAnObject,
    /// `question` missing or empty after trim.
    MissingQuestion,
    /// `question` is present but not a string.
    QuestionWrongType,
    /// An option carries the wrong JSON type.
    WrongType {
        path: String,
        expected: &'static str,
    },
    /// A numeric option is outside its declared range.
    OutOfRange { path: String, detail: String },
    /// Both `cache` and `nocache` were set.
    CacheAndNocache,
    /// An unknown key was sent under `options`. Strict to catch
    /// typos like `tempurature` early.
    UnknownOption { path: String },
}

/// Build the MCP tool descriptor. Stable JSON; safe to cache.
///
/// Shape:
/// ```json
/// {
///   "name": "reddb.ask",
///   "description": "...",
///   "inputSchema": { "$schema": "...", "type": "object", ... }
/// }
/// ```
pub fn descriptor() -> Value {
    let mut top = Map::new();
    top.insert("name".into(), Value::String(TOOL_NAME.into()));
    top.insert("description".into(), Value::String(description_text()));
    top.insert("inputSchema".into(), input_schema());
    Value::Object(top)
}

fn description_text() -> String {
    // Pinned by `description_emphasises_grounding`. Touching it must
    // also touch that test so the user-visible promise stays loud.
    "Grounded question-answering against the RedDB engine. \
     Runs `ASK '<question>'` with hybrid retrieval (BM25 + vector + graph), \
     returns an answer with inline `[^N]` citations and a `sources_flat` list \
     of URNs backing each citation. Validation is strict by default — answers \
     that cite out-of-range sources are retried once, then rejected. Honour \
     the citations: every factual claim in `answer` is grounded in \
     `sources_flat[N-1]`."
        .to_string()
}

fn input_schema() -> Value {
    let mut schema = Map::new();
    schema.insert("$schema".into(), Value::String(SCHEMA_DRAFT.into()));
    schema.insert("type".into(), Value::String("object".into()));
    schema.insert("additionalProperties".into(), Value::Bool(false));
    schema.insert(
        "required".into(),
        Value::Array(vec![Value::String("question".into())]),
    );

    let mut props = Map::new();
    props.insert(
        "question".into(),
        prop_with(&[
            ("type", Value::String("string".into())),
            ("minLength", Value::Number(1.0)),
            (
                "description",
                Value::String("The natural-language question to ground.".into()),
            ),
        ]),
    );
    props.insert("options".into(), options_schema());
    schema.insert("properties".into(), Value::Object(props));
    Value::Object(schema)
}

fn options_schema() -> Value {
    let mut s = Map::new();
    s.insert("type".into(), Value::String("object".into()));
    s.insert("additionalProperties".into(), Value::Bool(false));
    s.insert(
        "description".into(),
        Value::String(
            "Per-call overrides mirroring `ASK '...'` SQL clauses. All fields optional.".into(),
        ),
    );

    let mut p = Map::new();
    p.insert(
        "strict".into(),
        prop_with(&[
            ("type", Value::String("boolean".into())),
            (
                "description",
                Value::String(
                    "If false, retry-on-citation-mismatch is disabled and warnings are surfaced instead of errors.".into(),
                ),
            ),
        ]),
    );
    p.insert(
        "using".into(),
        string_prop("Provider token override (e.g. \"openai\", \"anthropic\")."),
    );
    p.insert("model".into(), string_prop("Specific model id to invoke."));
    p.insert(
        "limit".into(),
        ranged_int(
            LIMIT_MIN,
            LIMIT_MAX,
            LIMIT_DEFAULT,
            "Total source budget after RRF fusion.",
        ),
    );
    p.insert(
        "min_score".into(),
        ranged_num(
            MIN_SCORE_MIN,
            MIN_SCORE_MAX,
            "Per-bucket score floor applied before RRF.",
        ),
    );
    p.insert(
        "depth".into(),
        ranged_int(
            DEPTH_MIN,
            DEPTH_MAX,
            DEPTH_DEFAULT,
            "Graph traversal depth for the graph bucket.",
        ),
    );
    p.insert(
        "temperature".into(),
        ranged_num(
            TEMPERATURE_MIN,
            TEMPERATURE_MAX,
            "Sampling temperature. Default 0 for determinism.",
        ),
    );
    p.insert(
        "seed".into(),
        prop_with(&[
            ("type", Value::String("integer".into())),
            ("minimum", Value::Number(0.0)),
            (
                "description",
                Value::String(
                    "Per-call seed override. Default is derived from question + sources fingerprint.".into(),
                ),
            ),
        ]),
    );

    let mut cache_obj = Map::new();
    cache_obj.insert("type".into(), Value::String("object".into()));
    cache_obj.insert("additionalProperties".into(), Value::Bool(false));
    cache_obj.insert(
        "required".into(),
        Value::Array(vec![Value::String("ttl".into())]),
    );
    let mut cache_props = Map::new();
    cache_props.insert(
        "ttl".into(),
        prop_with(&[
            ("type", Value::String("string".into())),
            ("minLength", Value::Number(1.0)),
            (
                "description",
                Value::String("TTL string accepted by the parser, e.g. \"5m\", \"1h\".".into()),
            ),
        ]),
    );
    cache_obj.insert("properties".into(), Value::Object(cache_props));
    p.insert("cache".into(), Value::Object(cache_obj));

    p.insert(
        "nocache".into(),
        prop_with(&[
            ("type", Value::String("boolean".into())),
            (
                "description",
                Value::String(
                    "If true, bypasses the answer cache for this call. Mutually exclusive with `cache`.".into(),
                ),
            ),
        ]),
    );

    s.insert("properties".into(), Value::Object(p));
    Value::Object(s)
}

fn prop_with(entries: &[(&str, Value)]) -> Value {
    let mut m = Map::new();
    for (k, v) in entries {
        m.insert((*k).to_string(), v.clone());
    }
    Value::Object(m)
}

fn string_prop(desc: &str) -> Value {
    prop_with(&[
        ("type", Value::String("string".into())),
        ("minLength", Value::Number(1.0)),
        ("description", Value::String(desc.into())),
    ])
}

fn ranged_int(min: u32, max: u32, default: u32, desc: &str) -> Value {
    prop_with(&[
        ("type", Value::String("integer".into())),
        ("minimum", Value::Number(min as f64)),
        ("maximum", Value::Number(max as f64)),
        ("default", Value::Number(default as f64)),
        ("description", Value::String(desc.into())),
    ])
}

fn ranged_num(min: f64, max: f64, desc: &str) -> Value {
    prop_with(&[
        ("type", Value::String("number".into())),
        ("minimum", Value::Number(min)),
        ("maximum", Value::Number(max)),
        ("description", Value::String(desc.into())),
    ])
}

/// Validate and convert MCP `arguments` into a typed
/// [`AskInvocation`]. The function is total: every input either
/// becomes an `AskInvocation` or a typed `ParseError`. No panics, no
/// silent coercion, no defaulting (defaulting is the engine's job).
pub fn parse(args: &Value) -> Result<AskInvocation, ParseError> {
    let obj = match args {
        Value::Object(m) => m,
        _ => return Err(ParseError::NotAnObject),
    };

    let question = match obj.get("question") {
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Err(ParseError::MissingQuestion);
            }
            // Preserve original (untrimmed) so downstream pipelines
            // see exactly what the caller sent. Trim is a non-empty
            // check, not a normalization.
            s.clone()
        }
        Some(_) => return Err(ParseError::QuestionWrongType),
        None => return Err(ParseError::MissingQuestion),
    };

    let mut inv = AskInvocation {
        question,
        strict: None,
        using: None,
        model: None,
        limit: None,
        min_score: None,
        depth: None,
        temperature: None,
        seed: None,
        cache_ttl: None,
        nocache: None,
    };

    for (key, _) in obj.iter() {
        if key != "question" && key != "options" {
            return Err(ParseError::UnknownOption { path: key.clone() });
        }
    }

    if let Some(opts_v) = obj.get("options") {
        let opts = match opts_v {
            Value::Object(m) => m,
            _ => {
                return Err(ParseError::WrongType {
                    path: "options".into(),
                    expected: "object",
                });
            }
        };
        parse_options(opts, &mut inv)?;
    }

    if inv.cache_ttl.is_some() && matches!(inv.nocache, Some(true)) {
        return Err(ParseError::CacheAndNocache);
    }

    Ok(inv)
}

fn parse_options(opts: &Map<String, Value>, inv: &mut AskInvocation) -> Result<(), ParseError> {
    for (key, val) in opts.iter() {
        match key.as_str() {
            "strict" => inv.strict = Some(expect_bool(val, "options.strict")?),
            "using" => inv.using = Some(expect_nonempty_string(val, "options.using")?),
            "model" => inv.model = Some(expect_nonempty_string(val, "options.model")?),
            "limit" => {
                let n = expect_u32(val, "options.limit")?;
                if !(LIMIT_MIN..=LIMIT_MAX).contains(&n) {
                    return Err(ParseError::OutOfRange {
                        path: "options.limit".into(),
                        detail: format!("must be in {}..={}", LIMIT_MIN, LIMIT_MAX),
                    });
                }
                inv.limit = Some(n);
            }
            "min_score" => {
                let f = expect_f64(val, "options.min_score")?;
                if !(MIN_SCORE_MIN..=MIN_SCORE_MAX).contains(&f) {
                    return Err(ParseError::OutOfRange {
                        path: "options.min_score".into(),
                        detail: format!("must be in {}..={}", MIN_SCORE_MIN, MIN_SCORE_MAX),
                    });
                }
                inv.min_score = Some(f);
            }
            "depth" => {
                let n = expect_u32(val, "options.depth")?;
                if !(DEPTH_MIN..=DEPTH_MAX).contains(&n) {
                    return Err(ParseError::OutOfRange {
                        path: "options.depth".into(),
                        detail: format!("must be in {}..={}", DEPTH_MIN, DEPTH_MAX),
                    });
                }
                inv.depth = Some(n);
            }
            "temperature" => {
                let f = expect_f64(val, "options.temperature")?;
                if !(TEMPERATURE_MIN..=TEMPERATURE_MAX).contains(&f) {
                    return Err(ParseError::OutOfRange {
                        path: "options.temperature".into(),
                        detail: format!("must be in {}..={}", TEMPERATURE_MIN, TEMPERATURE_MAX),
                    });
                }
                inv.temperature = Some(f);
            }
            "seed" => inv.seed = Some(expect_u64(val, "options.seed")?),
            "cache" => {
                let m = match val {
                    Value::Object(m) => m,
                    _ => {
                        return Err(ParseError::WrongType {
                            path: "options.cache".into(),
                            expected: "object",
                        });
                    }
                };
                let ttl = match m.get("ttl") {
                    Some(v) => expect_nonempty_string(v, "options.cache.ttl")?,
                    None => {
                        return Err(ParseError::WrongType {
                            path: "options.cache.ttl".into(),
                            expected: "string",
                        });
                    }
                };
                for (k, _) in m.iter() {
                    if k != "ttl" {
                        return Err(ParseError::UnknownOption {
                            path: format!("options.cache.{}", k),
                        });
                    }
                }
                inv.cache_ttl = Some(ttl);
            }
            "nocache" => inv.nocache = Some(expect_bool(val, "options.nocache")?),
            other => {
                return Err(ParseError::UnknownOption {
                    path: format!("options.{}", other),
                });
            }
        }
    }
    Ok(())
}

fn expect_bool(v: &Value, path: &str) -> Result<bool, ParseError> {
    match v {
        Value::Bool(b) => Ok(*b),
        _ => Err(ParseError::WrongType {
            path: path.into(),
            expected: "boolean",
        }),
    }
}

fn expect_nonempty_string(v: &Value, path: &str) -> Result<String, ParseError> {
    match v {
        Value::String(s) if !s.is_empty() => Ok(s.clone()),
        Value::String(_) => Err(ParseError::OutOfRange {
            path: path.into(),
            detail: "must be a non-empty string".into(),
        }),
        _ => Err(ParseError::WrongType {
            path: path.into(),
            expected: "string",
        }),
    }
}

fn expect_u32(v: &Value, path: &str) -> Result<u32, ParseError> {
    let n = expect_integer(v, path)?;
    if n < 0 || n > u32::MAX as i128 {
        return Err(ParseError::OutOfRange {
            path: path.into(),
            detail: format!("must fit in u32 (0..={})", u32::MAX),
        });
    }
    Ok(n as u32)
}

fn expect_u64(v: &Value, path: &str) -> Result<u64, ParseError> {
    let n = expect_integer(v, path)?;
    if n < 0 || n > u64::MAX as i128 {
        return Err(ParseError::OutOfRange {
            path: path.into(),
            detail: format!("must fit in u64 (0..={})", u64::MAX),
        });
    }
    Ok(n as u64)
}

fn expect_integer(v: &Value, path: &str) -> Result<i128, ParseError> {
    match v {
        Value::Number(n) => {
            if !n.is_finite() || n.fract() != 0.0 {
                return Err(ParseError::WrongType {
                    path: path.into(),
                    expected: "integer",
                });
            }
            Ok(*n as i128)
        }
        _ => Err(ParseError::WrongType {
            path: path.into(),
            expected: "integer",
        }),
    }
}

fn expect_f64(v: &Value, path: &str) -> Result<f64, ParseError> {
    match v {
        Value::Number(n) if n.is_finite() => Ok(*n),
        Value::Number(_) => Err(ParseError::WrongType {
            path: path.into(),
            expected: "finite number",
        }),
        _ => Err(ParseError::WrongType {
            path: path.into(),
            expected: "number",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(s: &str) -> Value {
        crate::utils::json::parse_json(s)
            .expect("valid test json")
            .into()
    }

    // ---- descriptor ----

    #[test]
    fn descriptor_top_level_keys_pinned() {
        let d = descriptor();
        let obj = d.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(|s| s.as_str()).collect();
        keys.sort();
        assert_eq!(keys, vec!["description", "inputSchema", "name"]);
    }

    #[test]
    fn descriptor_name_is_reddb_ask() {
        assert_eq!(
            descriptor().get("name").and_then(|v| v.as_str()),
            Some("reddb.ask")
        );
        assert_eq!(TOOL_NAME, "reddb.ask");
    }

    #[test]
    fn description_emphasises_grounding() {
        let desc = descriptor()
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap()
            .to_string();
        assert!(desc.contains("citation"), "description: {desc}");
        assert!(desc.contains("sources_flat"), "description: {desc}");
        assert!(desc.contains("URN"), "description: {desc}");
    }

    #[test]
    fn input_schema_requires_question_only() {
        let schema = descriptor().get("inputSchema").cloned().unwrap();
        let req = schema.get("required").cloned().unwrap();
        let arr = req.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].as_str(), Some("question"));
    }

    #[test]
    fn input_schema_rejects_additional_properties() {
        let schema = descriptor().get("inputSchema").cloned().unwrap();
        assert_eq!(
            schema.get("additionalProperties").and_then(|v| v.as_bool()),
            Some(false)
        );
        let opts = schema
            .get("properties")
            .and_then(|p| p.get("options"))
            .cloned()
            .unwrap();
        assert_eq!(
            opts.get("additionalProperties").and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn input_schema_options_keys_match_parser() {
        let schema = descriptor().get("inputSchema").cloned().unwrap();
        let opts = schema
            .get("properties")
            .and_then(|p| p.get("options"))
            .and_then(|o| o.get("properties"))
            .cloned()
            .unwrap();
        let mut keys: Vec<&str> = opts
            .as_object()
            .unwrap()
            .keys()
            .map(|s| s.as_str())
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "cache",
                "depth",
                "limit",
                "min_score",
                "model",
                "nocache",
                "seed",
                "strict",
                "temperature",
                "using",
            ]
        );
    }

    #[test]
    fn input_schema_ranges_pinned() {
        let schema = descriptor().get("inputSchema").cloned().unwrap();
        let opts = schema
            .get("properties")
            .and_then(|p| p.get("options"))
            .and_then(|o| o.get("properties"))
            .cloned()
            .unwrap();
        let limit = opts.get("limit").cloned().unwrap();
        assert_eq!(limit.get("minimum").and_then(|v| v.as_f64()), Some(1.0));
        assert_eq!(limit.get("maximum").and_then(|v| v.as_f64()), Some(200.0));
        assert_eq!(limit.get("default").and_then(|v| v.as_f64()), Some(20.0));
        let depth = opts.get("depth").cloned().unwrap();
        assert_eq!(depth.get("minimum").and_then(|v| v.as_f64()), Some(0.0));
        assert_eq!(depth.get("maximum").and_then(|v| v.as_f64()), Some(10.0));
        let temp = opts.get("temperature").cloned().unwrap();
        assert_eq!(temp.get("maximum").and_then(|v| v.as_f64()), Some(2.0));
    }

    #[test]
    fn descriptor_is_deterministic() {
        let a = descriptor().to_string_compact();
        let b = descriptor().to_string_compact();
        assert_eq!(a, b);
    }

    // ---- parse: happy path ----

    #[test]
    fn parse_minimal_question_only() {
        let inv = parse(&json(r#"{"question":"hi"}"#)).unwrap();
        assert_eq!(inv.question, "hi");
        assert!(inv.strict.is_none() && inv.limit.is_none() && inv.cache_ttl.is_none());
    }

    #[test]
    fn parse_full_options() {
        let v = json(
            r#"{
              "question": "What is the cap of X?",
              "options": {
                "strict": false,
                "using": "openai",
                "model": "gpt-4o-mini",
                "limit": 50,
                "min_score": 0.7,
                "depth": 2,
                "temperature": 0,
                "seed": 42,
                "cache": {"ttl": "5m"}
              }
            }"#,
        );
        let inv = parse(&v).unwrap();
        assert_eq!(inv.strict, Some(false));
        assert_eq!(inv.using.as_deref(), Some("openai"));
        assert_eq!(inv.model.as_deref(), Some("gpt-4o-mini"));
        assert_eq!(inv.limit, Some(50));
        assert_eq!(inv.min_score, Some(0.7));
        assert_eq!(inv.depth, Some(2));
        assert_eq!(inv.temperature, Some(0.0));
        assert_eq!(inv.seed, Some(42));
        assert_eq!(inv.cache_ttl.as_deref(), Some("5m"));
        assert!(inv.nocache.is_none());
    }

    #[test]
    fn parse_nocache_alone() {
        let inv = parse(&json(r#"{"question":"q","options":{"nocache":true}}"#)).unwrap();
        assert_eq!(inv.nocache, Some(true));
        assert!(inv.cache_ttl.is_none());
    }

    #[test]
    fn parse_preserves_untrimmed_question() {
        // Question is non-empty after trim; original (with whitespace) preserved.
        let inv = parse(&json(r#"{"question":"  hi  "}"#)).unwrap();
        assert_eq!(inv.question, "  hi  ");
    }

    #[test]
    fn parse_seed_zero_preserved() {
        // Guard against `unwrap_or(0)` regressions — same property
        // #400/#403 pin elsewhere.
        let inv = parse(&json(r#"{"question":"q","options":{"seed":0}}"#)).unwrap();
        assert_eq!(inv.seed, Some(0));
    }

    #[test]
    fn parse_temperature_zero_preserved() {
        let inv = parse(&json(r#"{"question":"q","options":{"temperature":0}}"#)).unwrap();
        assert_eq!(inv.temperature, Some(0.0));
    }

    // ---- parse: error paths ----

    #[test]
    fn parse_rejects_non_object_args() {
        let err = parse(&json("[]")).unwrap_err();
        assert_eq!(err, ParseError::NotAnObject);
    }

    #[test]
    fn parse_rejects_missing_question() {
        assert_eq!(parse(&json("{}")).unwrap_err(), ParseError::MissingQuestion);
    }

    #[test]
    fn parse_rejects_empty_question() {
        assert_eq!(
            parse(&json(r#"{"question":"   "}"#)).unwrap_err(),
            ParseError::MissingQuestion
        );
    }

    #[test]
    fn parse_rejects_non_string_question() {
        assert_eq!(
            parse(&json(r#"{"question":42}"#)).unwrap_err(),
            ParseError::QuestionWrongType
        );
    }

    #[test]
    fn parse_rejects_unknown_top_level_key() {
        let err = parse(&json(r#"{"question":"q","extra":1}"#)).unwrap_err();
        assert_eq!(
            err,
            ParseError::UnknownOption {
                path: "extra".into()
            }
        );
    }

    #[test]
    fn parse_rejects_unknown_option_key() {
        let err = parse(&json(r#"{"question":"q","options":{"tempurature":0}}"#)).unwrap_err();
        assert_eq!(
            err,
            ParseError::UnknownOption {
                path: "options.tempurature".into()
            }
        );
    }

    #[test]
    fn parse_rejects_options_not_object() {
        let err = parse(&json(r#"{"question":"q","options":"strict"}"#)).unwrap_err();
        match err {
            ParseError::WrongType { path, expected } => {
                assert_eq!(path, "options");
                assert_eq!(expected, "object");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parse_rejects_limit_out_of_range_high() {
        let err = parse(&json(r#"{"question":"q","options":{"limit":201}}"#)).unwrap_err();
        match err {
            ParseError::OutOfRange { path, .. } => assert_eq!(path, "options.limit"),
            _ => panic!("wrong variant: {err:?}"),
        }
    }

    #[test]
    fn parse_rejects_limit_zero() {
        let err = parse(&json(r#"{"question":"q","options":{"limit":0}}"#)).unwrap_err();
        match err {
            ParseError::OutOfRange { path, .. } => assert_eq!(path, "options.limit"),
            _ => panic!("wrong variant: {err:?}"),
        }
    }

    #[test]
    fn parse_rejects_min_score_above_one() {
        let err = parse(&json(r#"{"question":"q","options":{"min_score":1.5}}"#)).unwrap_err();
        match err {
            ParseError::OutOfRange { path, .. } => assert_eq!(path, "options.min_score"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parse_rejects_temperature_negative() {
        let err = parse(&json(r#"{"question":"q","options":{"temperature":-0.1}}"#)).unwrap_err();
        match err {
            ParseError::OutOfRange { path, .. } => assert_eq!(path, "options.temperature"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parse_rejects_non_integer_seed() {
        let err = parse(&json(r#"{"question":"q","options":{"seed":1.5}}"#)).unwrap_err();
        match err {
            ParseError::WrongType { path, expected } => {
                assert_eq!(path, "options.seed");
                assert_eq!(expected, "integer");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parse_rejects_negative_seed() {
        let err = parse(&json(r#"{"question":"q","options":{"seed":-1}}"#)).unwrap_err();
        match err {
            ParseError::OutOfRange { path, .. } => assert_eq!(path, "options.seed"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parse_rejects_cache_without_ttl() {
        let err = parse(&json(r#"{"question":"q","options":{"cache":{}}}"#)).unwrap_err();
        match err {
            ParseError::WrongType { path, expected } => {
                assert_eq!(path, "options.cache.ttl");
                assert_eq!(expected, "string");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parse_rejects_cache_extra_key() {
        let err = parse(&json(
            r#"{"question":"q","options":{"cache":{"ttl":"5m","mode":"sliding"}}}"#,
        ))
        .unwrap_err();
        match err {
            ParseError::UnknownOption { path } => assert_eq!(path, "options.cache.mode"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parse_rejects_cache_and_nocache_together() {
        let err = parse(&json(
            r#"{"question":"q","options":{"cache":{"ttl":"5m"},"nocache":true}}"#,
        ))
        .unwrap_err();
        assert_eq!(err, ParseError::CacheAndNocache);
    }

    #[test]
    fn parse_allows_nocache_false_with_cache() {
        // `nocache: false` is the implicit default — pairing it with
        // `cache` is benign, not a conflict.
        let inv = parse(&json(
            r#"{"question":"q","options":{"cache":{"ttl":"5m"},"nocache":false}}"#,
        ))
        .unwrap();
        assert_eq!(inv.cache_ttl.as_deref(), Some("5m"));
        assert_eq!(inv.nocache, Some(false));
    }

    #[test]
    fn parse_rejects_empty_using_string() {
        let err = parse(&json(r#"{"question":"q","options":{"using":""}}"#)).unwrap_err();
        match err {
            ParseError::OutOfRange { path, .. } => assert_eq!(path, "options.using"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parse_rejects_using_wrong_type() {
        let err = parse(&json(r#"{"question":"q","options":{"using":1}}"#)).unwrap_err();
        match err {
            ParseError::WrongType { path, expected } => {
                assert_eq!(path, "options.using");
                assert_eq!(expected, "string");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parse_is_deterministic() {
        let v = json(r#"{"question":"q","options":{"strict":true,"limit":10,"seed":7}}"#);
        assert_eq!(parse(&v).unwrap(), parse(&v).unwrap());
    }
}
