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
    // Fast path: use pre-serialized JSON if available (move, no clone)
    if result.result.pre_serialized_json.is_some() {
        let count = result.result.stats.rows_scanned;
        return QueryReply {
            ok: true,
            mode: format!("{:?}", result.mode).to_lowercase(),
            statement: result.statement.to_string(),
            engine: result.engine.to_string(),
            columns: result.result.columns,
            record_count: count,
            result_json: result.result.pre_serialized_json.unwrap(),
        };
    }

    let records = crate::presentation::query_view::filter_query_records(
        &result.result.records,
        entity_types,
        capabilities,
    );
    QueryReply {
        ok: true,
        mode: format!("{:?}", result.mode).to_lowercase(),
        statement: result.statement.to_string(),
        engine: result.engine.to_string(),
        columns: result.result.columns.clone(),
        record_count: records.len() as u64,
        result_json: unified_result_json_string_with_records(
            &result.result,
            &records,
            entity_types,
            capabilities,
        ),
    }
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
        for (key, value) in &record.values {
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
