//! One renderer per target encoding for a `RuntimeQueryResult`.
//!
//! The transports do not share a single reply shape — each shape is a
//! documented driver contract — so presentation owns one renderer per
//! *target encoding* and every transport delegates to exactly one of
//! them:
//!
//! | target encoding    | renderer                              | consumer                               |
//! |--------------------|---------------------------------------|----------------------------------------|
//! | canonical envelope | [`json`], [`envelope_frame`]          | HTTP, RedWire `QueryWithParams` (0x28) |
//! | gRPC reply         | [`proto_reply`]                       | `grpc::scan_json::query_reply`         |
//! | tagged JSON-RPC    | [`tagged_summary`], [`tagged_record`] | stdio drivers                          |
//! | RedWire summary    | [`summary_frame`]                     | RedWire `Query` (0x01)                 |
//!
//! The tagged JSON-RPC encoding is lossless: values JSON cannot carry
//! exactly are wrapped in a single-key envelope (`{"$ts":…}`,
//! `{"$bytes":…}`, `{"$uuid":…}`, `{"$float":"NaN"}`, `{"$int":…}`,
//! `{"$uint":…}`, `{"$decimal":…}`) that every driver decodes. The gRPC
//! reply flattens to plain JSON scalars instead. Neither may drift into
//! the other.

use std::fmt::Write;

use crate::grpc::proto::QueryReply;
use crate::json::{Map, Value as JsonValue};
use crate::runtime::RuntimeQueryResult;
use crate::storage::query::unified::{UnifiedRecord, UnifiedResult};
use reddb_types::encoding::base64_encode;
use reddb_types::Value;
use reddb_wire::redwire::operations::encode_query_result_summary_payload;
use reddb_wire::redwire::{
    build_dispatch_reply_frame, build_error_frame_lossy, Frame, MessageKind,
};

// ---------------------------------------------------------------------------
// Target: canonical JSON envelope
// ---------------------------------------------------------------------------

pub(crate) fn json(
    result: &RuntimeQueryResult,
    entity_types: &Option<Vec<String>>,
    capabilities: &Option<Vec<String>>,
) -> JsonValue {
    super::query_result_json::runtime_query_json(result, entity_types, capabilities)
}

/// RedWire `QueryWithParams` (0x28) carries the full canonical envelope.
pub(crate) fn envelope_frame(
    correlation_id: u64,
    outcome: Result<&RuntimeQueryResult, String>,
) -> Frame {
    match outcome {
        Ok(result) => build_dispatch_reply_frame(
            correlation_id,
            MessageKind::Result,
            json(result, &None, &None).to_string_compact().into_bytes(),
        ),
        Err(message) => build_error_frame_lossy(correlation_id, &message),
    }
}

// ---------------------------------------------------------------------------
// Target: RedWire summary payload
// ---------------------------------------------------------------------------

/// RedWire `Query` (0x01) answers with the pinned summary payload —
/// statement type plus affected rows — not the full result envelope.
/// `QueryWithParams` (0x28) is the frame that carries records back;
/// clients pick the frame by the reply shape they want. `affected` is
/// always present, including when it is 0.
pub(crate) fn summary_frame(
    correlation_id: u64,
    outcome: Result<&RuntimeQueryResult, String>,
) -> Frame {
    match outcome {
        Ok(result) => build_dispatch_reply_frame(
            correlation_id,
            MessageKind::Result,
            encode_query_result_summary_payload(result.statement_type, result.affected_rows),
        ),
        Err(message) => build_error_frame_lossy(correlation_id, &message),
    }
}

// ---------------------------------------------------------------------------
// Target: gRPC `QueryReply`
// ---------------------------------------------------------------------------

/// Takes the result by value so the pre-serialized JSON fast path can
/// move the string instead of cloning it on every scan reply.
pub(crate) fn proto_reply(
    result: RuntimeQueryResult,
    entity_types: &Option<Vec<String>>,
    capabilities: &Option<Vec<String>>,
) -> QueryReply {
    let RuntimeQueryResult {
        mode,
        statement,
        engine,
        result,
        affected_rows,
        ..
    } = result;
    let mode = super::query_result_json::query_mode_name(mode).to_string();

    if statement == "ask" {
        return QueryReply {
            ok: true,
            mode,
            statement: statement.to_string(),
            engine: engine.to_string(),
            columns: result.columns,
            record_count: 1,
            result_json: ask_json(&result.records)
                .unwrap_or_else(empty_ask_json)
                .to_string_compact(),
            affected_rows,
        };
    }

    // Fast path: use pre-serialized JSON if available (move, no clone).
    if let Some(pre_serialized_json) = result.pre_serialized_json {
        let count = result.stats.rows_scanned;
        return QueryReply {
            ok: true,
            mode,
            statement: statement.to_string(),
            engine: engine.to_string(),
            columns: result.columns,
            record_count: count,
            result_json: pre_serialized_json,
            affected_rows,
        };
    }

    let records =
        super::query_view::filter_query_records(&result.records, entity_types, capabilities);
    QueryReply {
        ok: true,
        mode,
        statement: statement.to_string(),
        engine: engine.to_string(),
        columns: result.columns.clone(),
        record_count: records.len() as u64,
        result_json: flat_result_json_string(&result, &records, entity_types, capabilities),
        affected_rows,
    }
}

/// Writes the gRPC `result_json` straight into a string buffer: no
/// intermediate `JsonValue` tree, and `record.iter_fields()` order is
/// preserved (the canonical `Map` is a `BTreeMap` and would re-sort).
#[allow(deprecated)]
fn flat_result_json_string(
    result: &UnifiedResult,
    records: &[UnifiedRecord],
    entity_types: &Option<Vec<String>>,
    capabilities: &Option<Vec<String>>,
) -> String {
    let selection_scope = if entity_types.is_none() && capabilities.is_none() {
        "any"
    } else {
        "filtered"
    };

    // Estimate capacity: ~200 bytes per record for typical user data.
    let mut buf = String::with_capacity(128 + records.len() * 200);

    buf.push_str("{\"columns\":[");
    for (i, col) in result.columns.iter().enumerate() {
        if i > 0 {
            buf.push(',');
        }
        write_json_string(&mut buf, col);
    }
    buf.push_str("],\"record_count\":");
    let _ = write!(buf, "{}", records.len());
    buf.push_str(",\"selection\":{\"scope\":\"");
    buf.push_str(selection_scope);
    buf.push_str("\"},\"records\":[");

    for (ri, record) in records.iter().enumerate() {
        if ri > 0 {
            buf.push(',');
        }
        buf.push('{');
        let mut first = true;
        for (key, value) in record.iter_fields() {
            if !first {
                buf.push(',');
            }
            first = false;
            write_json_string(&mut buf, key);
            buf.push(':');
            write_value_json(&mut buf, value);
        }
        buf.push('}');
    }

    buf.push_str("]}");
    buf
}

/// Write a JSON-escaped string (with quotes) to a buffer.
///
/// **Deprecation note (ADR 0010 / issue #177):** the canonical JSON
/// string encoder is `crate::serde_json::Value::escape_string`
/// (used internally by `to_string_compact`). This local fast-path
/// is correct after F-01 hotfix #181 but is not the canonical owner
/// of the serialization boundary; new gRPC reply assembly should
/// route caller-influenced strings through the canonical encoder
/// (or, on the audit boundary, through `AuditFieldEscaper`). Kept
/// here pending a follow-up retirement slice — the gRPC scan path
/// has hot-loop performance characteristics that need a benchmark
/// before retirement.
#[deprecated(
    note = "Use crate::serde_json::Value::to_string_compact for boundary emission; see ADR 0010 / issue #177"
)]
#[inline]
fn write_json_string(buf: &mut String, s: &str) {
    buf.push('"');
    for ch in s.chars() {
        match ch {
            '"' => buf.push_str("\\\""),
            '\\' => buf.push_str("\\\\"),
            '\n' => buf.push_str("\\n"),
            '\r' => buf.push_str("\\r"),
            '\t' => buf.push_str("\\t"),
            c if c < '\x20' => {
                let _ = write!(buf, "\\u{:04x}", c as u32);
            }
            c => buf.push(c),
        }
    }
    buf.push('"');
}

/// Write a storage `Value` as flat JSON to a buffer (no intermediate
/// `JsonValue`). This is the gRPC encoding, not the tagged one.
#[allow(deprecated)]
#[inline]
fn write_value_json(buf: &mut String, value: &Value) {
    match value {
        Value::Null => buf.push_str("null"),
        Value::Boolean(b) => buf.push_str(if *b { "true" } else { "false" }),
        Value::Integer(n) => {
            let _ = write!(buf, "{n}");
        }
        Value::UnsignedInteger(n) => {
            let _ = write!(buf, "{n}");
        }
        Value::Float(f) => {
            if f.is_finite() {
                let _ = write!(buf, "{f}");
            } else {
                buf.push_str("null");
            }
        }
        Value::Text(s) => write_json_string(buf, s),
        Value::Timestamp(t) => {
            let _ = write!(buf, "{t}");
        }
        Value::Duration(d) => {
            let _ = write!(buf, "{d}");
        }
        Value::Blob(bytes) => {
            buf.push('"');
            buf.push_str(&hex::encode(bytes));
            buf.push('"');
        }
        Value::Json(bytes) => {
            // A document body may be the native binary container (PRD-1398);
            // decode it to JSON for the gRPC wire.
            match crate::document_body::decode_container_to_json(bytes)
                .or_else(|| crate::json::from_slice::<JsonValue>(bytes).ok())
            {
                Some(json) => buf.push_str(&json.to_string_compact()),
                None => buf.push_str("null"),
            }
        }
        _ => buf.push_str("null"),
    }
}

// ---------------------------------------------------------------------------
// Target: tagged JSON-RPC (stdio)
// ---------------------------------------------------------------------------

/// The stdio JSON-RPC result envelope: `statement`, `affected`, sorted
/// `columns`, and `rows` of tagged records. ASK results replace it with
/// the shared ASK envelope.
pub(crate) fn tagged_summary(result: &RuntimeQueryResult) -> JsonValue {
    if result.statement == "ask" {
        if let Some(ask) = ask_json(&result.result.records) {
            return ask;
        }
    }

    let mut envelope = Map::new();
    envelope.insert(
        "statement".to_string(),
        JsonValue::String(result.statement_type.to_string()),
    );
    envelope.insert(
        "affected".to_string(),
        JsonValue::Number(result.affected_rows as f64),
    );

    let mut columns = Vec::new();
    if let Some(first) = result.result.records.first() {
        let mut keys: Vec<String> = first
            .column_names()
            .into_iter()
            .map(|key| key.to_string())
            .collect();
        keys.sort();
        columns = keys.into_iter().map(JsonValue::String).collect();
    }
    envelope.insert("columns".to_string(), JsonValue::Array(columns));
    envelope.insert(
        "rows".to_string(),
        JsonValue::Array(result.result.records.iter().map(tagged_record).collect()),
    );

    JsonValue::Object(envelope)
}

pub(crate) fn tagged_record(record: &UnifiedRecord) -> JsonValue {
    // iter_fields merges the columnar fast-path + HashMap so scan
    // rows (columnar only) contribute their values.
    let mut entries: Vec<(&str, &Value)> =
        record.iter_fields().map(|(k, v)| (k.as_ref(), v)).collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));

    let mut map = Map::new();
    for (key, value) in entries {
        map.insert(key.to_string(), tagged_value(value));
    }
    JsonValue::Object(map)
}

pub(crate) fn tagged_value(value: &Value) -> JsonValue {
    match value {
        Value::Null => JsonValue::Null,
        Value::Boolean(b) => JsonValue::Bool(*b),
        Value::Integer(n) => exact_i64_to_json(*n),
        Value::UnsignedInteger(n) => exact_u64_to_json(*n),
        Value::Float(n) if n.is_finite() => JsonValue::Number(*n),
        Value::Float(n) => {
            let token = if n.is_nan() {
                "NaN"
            } else if n.is_sign_positive() {
                "Infinity"
            } else {
                "-Infinity"
            };
            single_key_object("$float", JsonValue::String(token.to_string()))
        }
        Value::BigInt(n) => exact_i64_to_json(*n),
        Value::TimestampMs(n) | Value::Duration(n) => exact_i64_to_json(*n),
        Value::Decimal(n) => exact_decimal_to_json(Value::Decimal(*n).display_string()),
        Value::DecimalText(n) => exact_decimal_to_json(n.clone()),
        Value::Timestamp(n) => single_key_object("$ts", JsonValue::String(n.to_string())),
        Value::Password(_) | Value::Secret(_) => JsonValue::String("***".to_string()),
        Value::Text(s) => JsonValue::String(s.to_string()),
        Value::Blob(bytes) => single_key_object("$bytes", JsonValue::String(base64_encode(bytes))),
        Value::Json(bytes) => super::entity_json::storage_json_bytes_to_json(bytes),
        Value::Uuid(bytes) => single_key_object("$uuid", JsonValue::String(format_uuid(bytes))),
        Value::Email(s) | Value::Url(s) | Value::NodeRef(s) | Value::EdgeRef(s) => {
            JsonValue::String(s.clone())
        }
        other => JsonValue::String(format!("{other}")),
    }
}

pub(crate) fn single_key_object(key: &str, value: JsonValue) -> JsonValue {
    JsonValue::Object([(key.to_string(), value)].into_iter().collect())
}

const MAX_JSON_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

fn exact_i64_to_json(value: i64) -> JsonValue {
    if (-MAX_JSON_SAFE_INTEGER..=MAX_JSON_SAFE_INTEGER).contains(&value) {
        JsonValue::Integer(value)
    } else {
        single_key_object("$int", JsonValue::String(value.to_string()))
    }
}

fn exact_u64_to_json(value: u64) -> JsonValue {
    if value <= MAX_JSON_SAFE_INTEGER as u64 {
        JsonValue::Integer(value as i64)
    } else {
        single_key_object("$uint", JsonValue::String(value.to_string()))
    }
}

fn exact_decimal_to_json(value: String) -> JsonValue {
    single_key_object("$decimal", JsonValue::String(value))
}

/// Drivers decode `$uuid` as the dashed 8-4-4-4-12 form.
fn format_uuid(bytes: &[u8; 16]) -> String {
    let hex = hex::encode(bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

// ---------------------------------------------------------------------------
// ASK extraction, shared by every target
// ---------------------------------------------------------------------------

pub(crate) fn ask_result_from_unified_result(
    result: &UnifiedResult,
) -> Option<crate::runtime::ai::ask_response_envelope::AskResult> {
    ask_result_from_records(&result.records)
}

fn ask_result_from_records(
    records: &[UnifiedRecord],
) -> Option<crate::runtime::ai::ask_response_envelope::AskResult> {
    let row = records.first()?;
    let answer = text_field(row, "answer")?;
    let sources = json_field(row, "sources_flat").unwrap_or(JsonValue::Array(Vec::new()));
    let citations = json_field(row, "citations").unwrap_or(JsonValue::Array(Vec::new()));
    let validation = json_field(row, "validation").unwrap_or(JsonValue::Object(Map::new()));
    Some(crate::runtime::ai::ask_response_envelope::AskResult {
        answer,
        sources_flat: ask_sources(&sources),
        citations: ask_citations(&citations),
        validation: ask_validation(&validation),
        cache_hit: bool_field(row, "cache_hit").unwrap_or(false),
        provider: text_field(row, "provider").unwrap_or_default(),
        model: text_field(row, "model").unwrap_or_default(),
        prompt_tokens: u32_field(row, "prompt_tokens").unwrap_or(0),
        completion_tokens: u32_field(row, "completion_tokens").unwrap_or(0),
        cost_usd: f64_field(row, "cost_usd").unwrap_or(0.0),
        effective_mode: match text_field(row, "mode").as_deref() {
            Some("lenient") => crate::runtime::ai::ask_response_envelope::Mode::Lenient,
            _ => crate::runtime::ai::ask_response_envelope::Mode::Strict,
        },
        retry_count: u32_field(row, "retry_count").unwrap_or(0),
    })
}

pub(crate) fn ask_answer_tokens_from_unified_result(result: &UnifiedResult) -> Option<Vec<String>> {
    let value = json_field(result.records.first()?, "answer_tokens")?;
    let tokens = value
        .as_array()?
        .iter()
        .filter_map(|token| token.as_str().map(ToString::to_string))
        .collect::<Vec<_>>();
    (!tokens.is_empty()).then_some(tokens)
}

fn ask_json(records: &[UnifiedRecord]) -> Option<JsonValue> {
    ask_result_from_records(records)
        .map(|ask| crate::runtime::ai::ask_response_envelope::build(&ask))
}

fn empty_ask_json() -> JsonValue {
    crate::runtime::ai::ask_response_envelope::build(
        &crate::runtime::ai::ask_response_envelope::AskResult {
            answer: String::new(),
            sources_flat: Vec::new(),
            citations: Vec::new(),
            validation: crate::runtime::ai::ask_response_envelope::Validation {
                ok: true,
                warnings: Vec::new(),
                errors: Vec::new(),
            },
            cache_hit: false,
            provider: String::new(),
            model: String::new(),
            prompt_tokens: 0,
            completion_tokens: 0,
            cost_usd: 0.0,
            effective_mode: crate::runtime::ai::ask_response_envelope::Mode::Strict,
            retry_count: 0,
        },
    )
}

fn field<'a>(record: &'a UnifiedRecord, name: &str) -> Option<&'a Value> {
    record
        .iter_fields()
        .find_map(|(key, value)| (key.as_ref() == name).then_some(value))
}

fn text_field(record: &UnifiedRecord, name: &str) -> Option<String> {
    match field(record, name)? {
        Value::Text(value) => Some(value.to_string()),
        Value::Email(value) | Value::Url(value) | Value::NodeRef(value) | Value::EdgeRef(value) => {
            Some(value.clone())
        }
        value => Some(value.to_string()),
    }
}

fn bool_field(record: &UnifiedRecord, name: &str) -> Option<bool> {
    match field(record, name)? {
        Value::Boolean(value) => Some(*value),
        _ => None,
    }
}

fn u32_field(record: &UnifiedRecord, name: &str) -> Option<u32> {
    match field(record, name)? {
        Value::Integer(value)
        | Value::BigInt(value)
        | Value::TimestampMs(value)
        | Value::Timestamp(value)
        | Value::Duration(value)
        | Value::Decimal(value) => (*value >= 0).then_some((*value).min(u32::MAX as i64) as u32),
        Value::UnsignedInteger(value) => Some((*value).min(u32::MAX as u64) as u32),
        Value::Float(value) => (*value >= 0.0).then_some((*value).min(u32::MAX as f64) as u32),
        _ => None,
    }
}

fn f64_field(record: &UnifiedRecord, name: &str) -> Option<f64> {
    match field(record, name)? {
        Value::Integer(value)
        | Value::BigInt(value)
        | Value::TimestampMs(value)
        | Value::Timestamp(value)
        | Value::Duration(value)
        | Value::Decimal(value) => Some(*value as f64),
        Value::UnsignedInteger(value) => Some(*value as f64),
        Value::Float(value) => Some(*value),
        _ => None,
    }
}

fn json_field(record: &UnifiedRecord, name: &str) -> Option<JsonValue> {
    match field(record, name)? {
        Value::Json(bytes) => crate::document_body::decode_container_to_json(bytes)
            .or_else(|| crate::json::from_slice(bytes).ok()),
        Value::Text(text) => crate::json::from_str(text).ok(),
        _ => None,
    }
}

fn ask_sources(value: &JsonValue) -> Vec<crate::runtime::ai::ask_response_envelope::SourceRow> {
    value
        .as_array()
        .unwrap_or(&[])
        .iter()
        .filter_map(|source| {
            Some(crate::runtime::ai::ask_response_envelope::SourceRow {
                urn: source.get("urn")?.as_str()?.to_string(),
                payload: source
                    .get("payload")
                    .and_then(JsonValue::as_str)
                    .map(ToString::to_string)
                    .unwrap_or_else(|| source.to_string_compact()),
            })
        })
        .collect()
}

fn ask_citations(value: &JsonValue) -> Vec<crate::runtime::ai::ask_response_envelope::Citation> {
    value
        .as_array()
        .unwrap_or(&[])
        .iter()
        .filter_map(|citation| {
            Some(crate::runtime::ai::ask_response_envelope::Citation {
                marker: citation.get("marker")?.as_u64()?.min(u32::MAX as u64) as u32,
                urn: citation.get("urn")?.as_str()?.to_string(),
            })
        })
        .collect()
}

fn ask_validation(value: &JsonValue) -> crate::runtime::ai::ask_response_envelope::Validation {
    crate::runtime::ai::ask_response_envelope::Validation {
        ok: value.get("ok").and_then(JsonValue::as_bool).unwrap_or(true),
        warnings: validation_items(value, "warnings")
            .into_iter()
            .map(
                |(kind, detail)| crate::runtime::ai::ask_response_envelope::ValidationWarning {
                    kind,
                    detail,
                },
            )
            .collect(),
        errors: validation_items(value, "errors")
            .into_iter()
            .map(
                |(kind, detail)| crate::runtime::ai::ask_response_envelope::ValidationError {
                    kind,
                    detail,
                },
            )
            .collect(),
    }
}

fn validation_items(value: &JsonValue, name: &str) -> Vec<(String, String)> {
    value
        .get(name)
        .and_then(JsonValue::as_array)
        .unwrap_or(&[])
        .iter()
        .filter_map(|item| {
            Some((
                item.get("kind")?.as_str()?.to_string(),
                item.get("detail")
                    .and_then(JsonValue::as_str)
                    .unwrap_or_default()
                    .to_string(),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{envelope_frame, json, proto_reply, summary_frame, tagged_summary};
    use crate::json::Value as JsonValue;
    use crate::runtime::RuntimeQueryResult;
    use crate::storage::query::modes::QueryMode;
    use crate::storage::query::unified::{UnifiedRecord, UnifiedResult};
    use reddb_types::Value;
    use reddb_wire::redwire::MessageKind;

    const UUID_BYTES: [u8; 16] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];

    const EXOTIC_COLUMNS: [&str; 7] = ["id", "name", "ts", "blob", "uuid", "ratio", "big"];

    fn result_with(columns: &[&str], values: Vec<Value>) -> UnifiedResult {
        let mut result =
            UnifiedResult::with_columns(columns.iter().map(|name| (*name).to_string()).collect());
        // A schema-backed record so `iter_fields()` yields schema order;
        // `UnifiedRecord::new()` would spill every field into the overflow
        // `HashMap` and make the gRPC byte golden order-dependent.
        let schema: Arc<Vec<Arc<str>>> = Arc::new(columns.iter().map(|n| Arc::from(*n)).collect());
        result.push(UnifiedRecord::with_schema(schema, values));
        result
    }

    fn runtime_result(
        statement: &'static str,
        affected_rows: u64,
        result: UnifiedResult,
    ) -> RuntimeQueryResult {
        RuntimeQueryResult {
            query: "SELECT * FROM people".into(),
            mode: QueryMode::Sql,
            statement,
            engine: "fixture",
            result,
            affected_rows,
            statement_type: statement,
            bookmark: None,
            notice: None,
        }
    }

    /// A row exercising the `Value` variants the tagged encoding must not
    /// lose: `Timestamp`, `Blob`, `Uuid`, non-finite `Float`, and an
    /// integer past the JSON-safe range.
    fn exotic_fixture(statement: &'static str, affected_rows: u64) -> RuntimeQueryResult {
        runtime_result(
            statement,
            affected_rows,
            result_with(
                &EXOTIC_COLUMNS,
                vec![
                    Value::Integer(7),
                    Value::text("Ada \"L\"\n"),
                    Value::Timestamp(1_700_000_000),
                    Value::Blob(vec![0xde, 0xad, 0xbe, 0xef]),
                    Value::Uuid(UUID_BYTES),
                    Value::Float(f64::NAN),
                    Value::BigInt(9_007_199_254_740_993),
                ],
            ),
        )
    }

    /// Same shape without the non-finite float, for the canonical
    /// envelope: `NaN` is not parseable JSON, so envelope round-trips
    /// use this fixture.
    fn finite_fixture(statement: &'static str, affected_rows: u64) -> RuntimeQueryResult {
        runtime_result(
            statement,
            affected_rows,
            result_with(&["id", "name"], vec![Value::Integer(7), Value::text("Ada")]),
        )
    }

    fn non_finite_fixture() -> RuntimeQueryResult {
        runtime_result(
            "select",
            0,
            result_with(
                &["nan", "positive_infinity", "negative_infinity"],
                vec![
                    Value::Float(f64::NAN),
                    Value::Float(f64::INFINITY),
                    Value::Float(f64::NEG_INFINITY),
                ],
            ),
        )
    }

    fn ask_fixture() -> RuntimeQueryResult {
        let result = result_with(
            &["answer", "provider", "model", "citations"],
            vec![
                Value::text("Deploy failed [^1]."),
                Value::text("acme"),
                Value::text("m-1"),
                Value::Json(br#"[{"marker":1,"urn":"urn:reddb:row:d:1"}]"#.to_vec()),
            ],
        );
        RuntimeQueryResult {
            query: "ASK 'why?'".into(),
            mode: QueryMode::Sql,
            statement: "ask",
            engine: "runtime-ai",
            result,
            affected_rows: 0,
            statement_type: "select",
            bookmark: None,
            notice: None,
        }
    }

    /// Byte golden for the tagged JSON-RPC target. These are the exact
    /// bytes the python-asyncio, Java and .NET `ValueCodec` decoders read:
    /// `$ts`, `$bytes` (base64), `$uuid` (dashed), `$float`, `$int`.
    #[test]
    fn tagged_jsonrpc_target_golden_bytes() {
        let rendered = tagged_summary(&exotic_fixture("select", 0)).to_string_compact();
        assert_eq!(
            rendered,
            concat!(
                r#"{"affected":0,"#,
                r#""columns":["big","blob","id","name","ratio","ts","uuid"],"#,
                r#""rows":[{"big":{"$int":"9007199254740993"},"#,
                r#""blob":{"$bytes":"3q2+7w=="},"id":7,"name":"Ada \"L\"\n","#,
                r#""ratio":{"$float":"NaN"},"ts":{"$ts":"1700000000"},"#,
                r#""uuid":{"$uuid":"00112233-4455-6677-8899-aabbccddeeff"}}],"#,
                r#""statement":"select"}"#
            )
        );
    }

    /// A DML statement that affected nothing still reports `affected`.
    #[test]
    fn tagged_jsonrpc_zero_affected_golden_bytes() {
        let mut result = exotic_fixture("delete", 0);
        result.result.records.clear();
        assert_eq!(
            tagged_summary(&result).to_string_compact(),
            r#"{"affected":0,"columns":[],"rows":[],"statement":"delete"}"#
        );
    }

    #[test]
    fn tagged_jsonrpc_ask_target_is_the_shared_envelope() {
        let rendered = tagged_summary(&ask_fixture()).to_string_compact();
        assert!(
            rendered.contains(r#""answer":"Deploy failed [^1].""#),
            "ASK envelope, got {rendered}"
        );
        assert!(
            rendered.contains(r#""citations":[{"marker":1,"urn":"urn:reddb:row:d:1"}]"#),
            "citations survive the tagged target, got {rendered}"
        );
        assert!(
            !rendered.contains(r#""rows""#),
            "ASK must not be row-wrapped, got {rendered}"
        );
    }

    /// HTTP and RedWire QueryWithParams share this canonical envelope.
    /// Non-finite floats use the drivers' lossless tagged representation,
    /// keeping the emitted bytes valid JSON and parseable by our own parser.
    #[test]
    fn canonical_envelope_non_finite_float_golden_round_trips() {
        let result = non_finite_fixture();
        let envelope = json(&result, &None, &None);
        let records = envelope["result"]["records"]
            .as_array()
            .expect("canonical query envelope records must be an array");
        let values = &records[0]["values"];
        assert_eq!(
            values.to_string_compact(),
            concat!(
                r#"{"nan":{"$float":"NaN"},"negative_infinity":{"$float":"-Infinity"},"#,
                r#""positive_infinity":{"$float":"Infinity"}}"#
            )
        );

        let rendered = envelope.to_string_compact();
        let parsed: JsonValue = crate::json::from_slice(rendered.as_bytes())
            .expect("canonical query envelope must parse after encoding");
        assert_eq!(parsed.to_string_compact(), rendered);
        let parsed_records = parsed["result"]["records"]
            .as_array()
            .expect("parsed canonical query envelope records must be an array");
        assert_eq!(&parsed_records[0]["values"], values);

        let frame = envelope_frame(29, Ok(&result));
        assert_eq!(frame.payload, rendered.as_bytes());
        let parsed_frame: JsonValue = crate::json::from_slice(&frame.payload)
            .expect("RedWire canonical query envelope must parse after encoding");
        assert_eq!(parsed_frame, parsed);
    }

    /// gRPC has a distinct plain-JSON contract, which represents all
    /// non-finite floats as null rather than adopting canonical tags.
    #[test]
    fn grpc_non_finite_float_golden_uses_null() {
        let reply = proto_reply(non_finite_fixture(), &None, &None);
        assert_eq!(
            reply.result_json,
            concat!(
                r#"{"columns":["nan","positive_infinity","negative_infinity"],"#,
                r#""record_count":1,"selection":{"scope":"any"},"records":[{"#,
                r#""nan":null,"positive_infinity":null,"negative_infinity":null}]}"#
            )
        );
        let _: JsonValue = crate::json::from_slice(reply.result_json.as_bytes())
            .expect("gRPC result_json must remain parseable JSON");
    }

    /// Byte golden for the gRPC target: top-level key order stays
    /// `columns`, `record_count`, `selection`, `records`; per-record key
    /// order stays `iter_fields()` order rather than sorted; `selection`
    /// stays `{"scope":…}`; values are flat, not tagged.
    #[test]
    fn grpc_result_json_target_golden_bytes() {
        let reply = proto_reply(exotic_fixture("select", 0), &None, &None);
        assert_eq!(
            reply.result_json,
            concat!(
                r#"{"columns":["id","name","ts","blob","uuid","ratio","big"],"#,
                r#""record_count":1,"selection":{"scope":"any"},"#,
                r#""records":[{"id":7,"name":"Ada \"L\"\n","ts":1700000000,"#,
                r#""blob":"deadbeef","uuid":null,"ratio":null,"big":null}]}"#
            )
        );
        assert_eq!(reply.mode, "sql");
        assert_eq!(reply.affected_rows, 0);
        assert_eq!(reply.record_count, 1);
    }

    #[test]
    fn grpc_result_json_filtered_selection_keeps_scope_only_shape() {
        let reply = proto_reply(
            exotic_fixture("select", 0),
            &Some(vec!["row".to_string()]),
            &None,
        );
        assert!(
            reply
                .result_json
                .contains(r#""selection":{"scope":"filtered"},"#),
            "got {}",
            reply.result_json
        );
    }

    #[test]
    fn grpc_ask_target_renders_the_shared_envelope() {
        let reply = proto_reply(ask_fixture(), &None, &None);
        assert_eq!(reply.record_count, 1);
        assert!(
            reply
                .result_json
                .contains(r#""answer":"Deploy failed [^1].""#),
            "got {}",
            reply.result_json
        );
        assert!(
            !reply.result_json.contains(r#""records""#),
            "ASK must not be row-wrapped, got {}",
            reply.result_json
        );
    }

    /// The pre-serialized scan fast path hands its string straight to the
    /// reply — no re-render, no clone of the record tree.
    #[test]
    fn grpc_pre_serialized_scan_fast_path_is_passed_through() {
        let mut result = exotic_fixture("select", 0);
        result.result.pre_serialized_json = Some(r#"{"records":["fast path"]}"#.to_string());
        result.result.stats.rows_scanned = 41;
        let reply = proto_reply(result, &None, &None);
        assert_eq!(reply.result_json, r#"{"records":["fast path"]}"#);
        assert_eq!(reply.record_count, 41);
    }

    /// Byte golden for the RedWire `Query` (0x01) summary payload: the
    /// `{"statement","affected"}` top-level shape driver helpers read.
    #[test]
    fn redwire_summary_target_golden_bytes() {
        let result = exotic_fixture("insert", 3);
        let frame = summary_frame(11, Ok(&result));
        assert_eq!(frame.kind, MessageKind::Result);
        assert_eq!(frame.correlation_id, 11);
        assert_eq!(
            String::from_utf8(frame.payload).unwrap(),
            r#"{"affected":3,"ok":true,"statement":"insert"}"#
        );

        let zero = exotic_fixture("delete", 0);
        assert_eq!(
            String::from_utf8(summary_frame(12, Ok(&zero)).payload).unwrap(),
            r#"{"affected":0,"ok":true,"statement":"delete"}"#,
            "affected must be present when it is 0"
        );
    }

    #[test]
    fn redwire_frames_carry_two_distinct_target_encodings() {
        let result = finite_fixture("select", 0);
        let summary = summary_frame(1, Ok(&result));
        let envelope = envelope_frame(2, Ok(&result));
        assert_ne!(summary.payload, envelope.payload);

        let summary_json: JsonValue = crate::json::from_slice(&summary.payload).unwrap();
        assert!(
            summary_json.get("result").is_none(),
            "Query (0x01) must not carry the full result envelope"
        );
        assert!(summary_json.get("affected").is_some());

        let envelope_json: JsonValue = crate::json::from_slice(&envelope.payload).unwrap();
        assert!(
            envelope_json.get("result").is_some(),
            "QueryWithParams (0x28) carries the full result envelope"
        );
    }

    #[test]
    fn error_outcomes_render_an_error_frame_on_both_query_frames() {
        for frame in [
            summary_frame(23, Err("boom".to_string())),
            envelope_frame(23, Err("boom".to_string())),
        ] {
            assert_eq!(frame.kind, MessageKind::Error);
            assert_eq!(frame.correlation_id, 23);
            assert_eq!(frame.payload, b"boom");
        }
    }

    /// A new `QueryMode` variant fails compilation in `query_mode_name`;
    /// this pins the rendered string per encoding.
    #[test]
    fn every_query_mode_has_one_golden_name_for_every_encoding() {
        let modes = [
            (QueryMode::Sql, "sql"),
            (QueryMode::Gremlin, "gremlin"),
            (QueryMode::Cypher, "cypher"),
            (QueryMode::Sparql, "sparql"),
            (QueryMode::Path, "path"),
            (QueryMode::Natural, "natural"),
            (QueryMode::Unknown, "unknown"),
        ];

        for (mode, expected) in modes {
            let mut result = finite_fixture("select", 0);
            result.mode = mode;
            assert_eq!(json(&result, &None, &None)["mode"].as_str(), Some(expected));

            let frame = envelope_frame(17, Ok(&result));
            let payload: JsonValue = crate::json::from_slice(&frame.payload).unwrap();
            assert_eq!(payload["mode"].as_str(), Some(expected));

            assert_eq!(proto_reply(result, &None, &None).mode, expected);
        }
    }

    /// Every statement type renders through every target, each pinning
    /// the statement name and affected count in its own shape.
    #[test]
    fn every_statement_type_has_a_golden_in_every_encoding() {
        let statements = [
            "select", "insert", "update", "delete", "create", "drop", "alter",
        ];

        for (index, statement) in statements.into_iter().enumerate() {
            let affected = index as u64;
            let result = exotic_fixture(statement, affected);

            assert_eq!(
                String::from_utf8(summary_frame(3, Ok(&result)).payload).unwrap(),
                format!(r#"{{"affected":{affected},"ok":true,"statement":"{statement}"}}"#)
            );

            let tagged = tagged_summary(&result).to_string_compact();
            assert!(
                tagged.starts_with(&format!(r#"{{"affected":{affected},"#))
                    && tagged.ends_with(&format!(r#""statement":"{statement}"}}"#)),
                "tagged summary for {statement}, got {tagged}"
            );

            assert_eq!(
                json(&finite_fixture(statement, affected), &None, &None)["statement"].as_str(),
                Some(statement)
            );

            let proto = proto_reply(result, &None, &None);
            assert_eq!(proto.statement, statement);
            assert_eq!(proto.affected_rows, affected);
        }
    }
}
