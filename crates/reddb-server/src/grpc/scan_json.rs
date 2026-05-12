//! `write_json_string` is deprecated for new boundary emission
//! (see ADR 0010 / issue #177), but the existing scan-fast-path
//! helpers in this file route through it internally. Allow the
//! internal recursion without warnings; the lint surfaces only
//! at out-of-file callers.
#![allow(deprecated)]

use super::*;

pub(crate) fn scan_reply(page: ScanPage) -> ScanReply {
    ScanReply {
        collection: page.collection,
        total: page.total as u64,
        next_offset: page.next.map(|cursor| cursor.offset as u64),
        items: page.items.into_iter().map(scan_entity).collect(),
    }
}

pub(crate) fn scan_entity(entity: UnifiedEntity) -> ScanEntity {
    ScanEntity {
        id: entity.id.raw(),
        kind: entity.kind.storage_type().to_string(),
        collection: entity.kind.collection().to_string(),
        json: crate::presentation::entity_json::compact_entity_json_string(&entity),
    }
}

pub(crate) fn query_reply(
    result: RuntimeQueryResult,
    entity_types: &Option<Vec<String>>,
    capabilities: &Option<Vec<String>>,
) -> QueryReply {
    let RuntimeQueryResult {
        mode,
        statement,
        engine,
        result,
        ..
    } = result;

    if statement == "ask" {
        let ask_json = ask_query_result_json(&result).unwrap_or_else(|| {
            let ask = crate::runtime::ai::ask_response_envelope::AskResult {
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
            };
            crate::runtime::ai::ask_response_envelope::build(&ask)
        });
        return QueryReply {
            ok: true,
            mode: format!("{mode:?}").to_lowercase(),
            statement: statement.to_string(),
            engine: engine.to_string(),
            columns: result.columns,
            record_count: 1,
            result_json: ask_json.to_string_compact(),
        };
    }

    // Fast path: use pre-serialized JSON if available (move, no clone)
    if let Some(pre_serialized_json) = result.pre_serialized_json {
        let count = result.stats.rows_scanned;
        return QueryReply {
            ok: true,
            mode: format!("{mode:?}").to_lowercase(),
            statement: statement.to_string(),
            engine: engine.to_string(),
            columns: result.columns,
            record_count: count,
            result_json: pre_serialized_json,
        };
    }

    let records = crate::presentation::query_view::filter_query_records(
        &result.records,
        entity_types,
        capabilities,
    );
    QueryReply {
        ok: true,
        mode: format!("{mode:?}").to_lowercase(),
        statement: statement.to_string(),
        engine: engine.to_string(),
        columns: result.columns.clone(),
        record_count: records.len() as u64,
        result_json: unified_result_json_string_with_records(
            &result,
            &records,
            entity_types,
            capabilities,
        ),
    }
}

fn ask_query_result_json(
    result: &crate::storage::query::unified::UnifiedResult,
) -> Option<crate::json::Value> {
    let row = result.records.first()?;
    let answer = text_field(row, "answer")?;
    let provider = text_field(row, "provider").unwrap_or_default();
    let model = text_field(row, "model").unwrap_or_default();
    let sources_flat_json =
        json_field(row, "sources_flat").unwrap_or(crate::json::Value::Array(Vec::new()));
    let citations_json =
        json_field(row, "citations").unwrap_or(crate::json::Value::Array(Vec::new()));
    let validation_json = json_field(row, "validation")
        .unwrap_or_else(|| crate::json::Value::Object(crate::json::Map::new()));

    let effective_mode = match text_field(row, "mode").as_deref() {
        Some("lenient") => crate::runtime::ai::ask_response_envelope::Mode::Lenient,
        _ => crate::runtime::ai::ask_response_envelope::Mode::Strict,
    };

    let ask = crate::runtime::ai::ask_response_envelope::AskResult {
        answer,
        sources_flat: ask_sources_flat(&sources_flat_json),
        citations: ask_citations(&citations_json),
        validation: ask_validation(&validation_json),
        cache_hit: bool_field(row, "cache_hit").unwrap_or(false),
        provider,
        model,
        prompt_tokens: u32_field(row, "prompt_tokens").unwrap_or(0),
        completion_tokens: u32_field(row, "completion_tokens").unwrap_or(0),
        cost_usd: f64_field(row, "cost_usd").unwrap_or(0.0),
        effective_mode,
        retry_count: u32_field(row, "retry_count").unwrap_or(0),
    };

    Some(crate::runtime::ai::ask_response_envelope::build(&ask))
}

fn record_field<'a>(
    record: &'a crate::storage::query::unified::UnifiedRecord,
    key: &str,
) -> Option<&'a crate::storage::schema::Value> {
    record.iter_fields().find_map(|(name, value)| {
        let name: &str = name;
        (name == key).then_some(value)
    })
}

fn text_field(record: &crate::storage::query::unified::UnifiedRecord, key: &str) -> Option<String> {
    match record_field(record, key)? {
        crate::storage::schema::Value::Text(s) => Some(s.to_string()),
        crate::storage::schema::Value::Email(s)
        | crate::storage::schema::Value::Url(s)
        | crate::storage::schema::Value::NodeRef(s)
        | crate::storage::schema::Value::EdgeRef(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

fn bool_field(record: &crate::storage::query::unified::UnifiedRecord, key: &str) -> Option<bool> {
    match record_field(record, key)? {
        crate::storage::schema::Value::Boolean(value) => Some(*value),
        _ => None,
    }
}

fn u32_field(record: &crate::storage::query::unified::UnifiedRecord, key: &str) -> Option<u32> {
    match record_field(record, key)? {
        crate::storage::schema::Value::Integer(n) => {
            (*n >= 0).then_some((*n).min(u32::MAX as i64) as u32)
        }
        crate::storage::schema::Value::UnsignedInteger(n) => Some((*n).min(u32::MAX as u64) as u32),
        crate::storage::schema::Value::BigInt(n)
        | crate::storage::schema::Value::TimestampMs(n)
        | crate::storage::schema::Value::Timestamp(n)
        | crate::storage::schema::Value::Duration(n)
        | crate::storage::schema::Value::Decimal(n) => {
            (*n >= 0).then_some((*n).min(u32::MAX as i64) as u32)
        }
        crate::storage::schema::Value::Float(n) => {
            (*n >= 0.0).then_some((*n).min(u32::MAX as f64) as u32)
        }
        _ => None,
    }
}

fn f64_field(record: &crate::storage::query::unified::UnifiedRecord, key: &str) -> Option<f64> {
    match record_field(record, key)? {
        crate::storage::schema::Value::Integer(n) => Some(*n as f64),
        crate::storage::schema::Value::UnsignedInteger(n) => Some(*n as f64),
        crate::storage::schema::Value::BigInt(n)
        | crate::storage::schema::Value::TimestampMs(n)
        | crate::storage::schema::Value::Timestamp(n)
        | crate::storage::schema::Value::Duration(n)
        | crate::storage::schema::Value::Decimal(n) => Some(*n as f64),
        crate::storage::schema::Value::Float(n) => Some(*n),
        _ => None,
    }
}

fn json_field(
    record: &crate::storage::query::unified::UnifiedRecord,
    key: &str,
) -> Option<crate::json::Value> {
    match record_field(record, key)? {
        crate::storage::schema::Value::Json(bytes) => crate::json::from_slice(bytes).ok(),
        crate::storage::schema::Value::Text(text) => crate::json::from_str(text).ok(),
        _ => None,
    }
}

fn ask_sources_flat(
    value: &crate::json::Value,
) -> Vec<crate::runtime::ai::ask_response_envelope::SourceRow> {
    value
        .as_array()
        .unwrap_or(&[])
        .iter()
        .filter_map(|source| {
            let urn = source
                .get("urn")
                .and_then(crate::json::Value::as_str)?
                .to_string();
            let payload = source
                .get("payload")
                .and_then(crate::json::Value::as_str)
                .map(ToString::to_string)
                .unwrap_or_else(|| source.to_string_compact());
            Some(crate::runtime::ai::ask_response_envelope::SourceRow { urn, payload })
        })
        .collect()
}

fn ask_citations(
    value: &crate::json::Value,
) -> Vec<crate::runtime::ai::ask_response_envelope::Citation> {
    value
        .as_array()
        .unwrap_or(&[])
        .iter()
        .filter_map(|citation| {
            let marker = citation
                .get("marker")
                .and_then(crate::json::Value::as_u64)?;
            let urn = citation
                .get("urn")
                .and_then(crate::json::Value::as_str)?
                .to_string();
            Some(crate::runtime::ai::ask_response_envelope::Citation {
                marker: marker.min(u32::MAX as u64) as u32,
                urn,
            })
        })
        .collect()
}

fn ask_validation(
    value: &crate::json::Value,
) -> crate::runtime::ai::ask_response_envelope::Validation {
    crate::runtime::ai::ask_response_envelope::Validation {
        ok: value
            .get("ok")
            .and_then(crate::json::Value::as_bool)
            .unwrap_or(true),
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

fn validation_items(value: &crate::json::Value, key: &str) -> Vec<(String, String)> {
    value
        .get(key)
        .and_then(crate::json::Value::as_array)
        .unwrap_or(&[])
        .iter()
        .filter_map(|item| {
            Some((
                item.get("kind")
                    .and_then(crate::json::Value::as_str)?
                    .to_string(),
                item.get("detail")
                    .and_then(crate::json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            ))
        })
        .collect()
}

pub(crate) fn unified_result_json_string_with_records(
    result: &crate::storage::query::unified::UnifiedResult,
    records: &[crate::storage::query::unified::UnifiedRecord],
    entity_types: &Option<Vec<String>>,
    capabilities: &Option<Vec<String>>,
) -> String {
    // Fast path: write JSON directly to string buffer (no intermediate JsonValue tree)
    use crate::storage::schema::Value;
    use std::fmt::Write;

    let selection_scope = if entity_types.is_none() && capabilities.is_none() {
        "any"
    } else {
        "filtered"
    };

    // Estimate capacity: ~200 bytes per record for typical user data
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
pub fn write_json_string(buf: &mut String, s: &str) {
    buf.push('"');
    for ch in s.chars() {
        match ch {
            '"' => buf.push_str("\\\""),
            '\\' => buf.push_str("\\\\"),
            '\n' => buf.push_str("\\n"),
            '\r' => buf.push_str("\\r"),
            '\t' => buf.push_str("\\t"),
            c if c < '\x20' => {
                let _ = std::fmt::Write::write_fmt(buf, format_args!("\\u{:04x}", c as u32));
            }
            c => buf.push(c),
        }
    }
    buf.push('"');
}

/// Write a storage Value as JSON to a buffer (no intermediate JsonValue).
#[inline]
pub fn write_value_json(buf: &mut String, value: &crate::storage::schema::Value) {
    use crate::storage::schema::Value;
    match value {
        Value::Null => buf.push_str("null"),
        Value::Boolean(b) => buf.push_str(if *b { "true" } else { "false" }),
        Value::Integer(n) => {
            let _ = std::fmt::Write::write_fmt(buf, format_args!("{n}"));
        }
        Value::UnsignedInteger(n) => {
            let _ = std::fmt::Write::write_fmt(buf, format_args!("{n}"));
        }
        Value::Float(f) => {
            if f.is_finite() {
                let _ = std::fmt::Write::write_fmt(buf, format_args!("{f}"));
            } else {
                buf.push_str("null");
            }
        }
        Value::Text(s) => write_json_string(buf, s),
        Value::Timestamp(t) => {
            let _ = std::fmt::Write::write_fmt(buf, format_args!("{t}"));
        }
        Value::Duration(d) => {
            let _ = std::fmt::Write::write_fmt(buf, format_args!("{d}"));
        }
        Value::Blob(bytes) => {
            buf.push('"');
            buf.push_str(&hex::encode(bytes));
            buf.push('"');
        }
        _ => buf.push_str("null"),
    }
}

pub(crate) fn grpc_parse_query_filters(
    request: &QueryRequest,
) -> Result<(Option<Vec<String>>, Option<Vec<String>>), Status> {
    crate::application::query_payload::normalize_search_selection(
        &request.entity_types,
        &request.capabilities,
    )
    .map_err(|err| Status::invalid_argument(err.to_string()))
}
