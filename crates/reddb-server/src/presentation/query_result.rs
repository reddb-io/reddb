use crate::grpc::proto::QueryReply;
use crate::json::{Map, Value as JsonValue};
use crate::runtime::RuntimeQueryResult;
use crate::storage::query::unified::{UnifiedRecord, UnifiedResult};
use crate::storage::schema::Value;
use reddb_wire::redwire::{
    build_dispatch_reply_frame, build_error_frame_lossy, Frame, MessageKind,
};

pub(crate) fn json(
    result: &RuntimeQueryResult,
    entity_types: &Option<Vec<String>>,
    capabilities: &Option<Vec<String>>,
) -> JsonValue {
    super::query_result_json::runtime_query_json(result, entity_types, capabilities)
}

pub(crate) fn proto_reply(
    result: &RuntimeQueryResult,
    entity_types: &Option<Vec<String>>,
    capabilities: &Option<Vec<String>>,
) -> QueryReply {
    let mode = super::query_result_json::query_mode_name(result.mode).to_string();
    if result.statement == "ask" {
        let result_json = ask_json(&result.result)
            .unwrap_or_else(|| empty_ask_json())
            .to_string_compact();
        return QueryReply {
            ok: true,
            mode,
            statement: result.statement.to_string(),
            engine: result.engine.to_string(),
            columns: result.result.columns.clone(),
            record_count: 1,
            result_json,
            affected_rows: result.affected_rows,
        };
    }

    if let Some(pre_serialized_json) = result.result.pre_serialized_json.as_ref() {
        return QueryReply {
            ok: true,
            mode,
            statement: result.statement.to_string(),
            engine: result.engine.to_string(),
            columns: result.result.columns.clone(),
            record_count: result.result.stats.rows_scanned,
            result_json: pre_serialized_json.clone(),
            affected_rows: result.affected_rows,
        };
    }

    let records =
        super::query_view::filter_query_records(&result.result.records, entity_types, capabilities);
    let result_json = flat_result_json(&result.result, &records, entity_types, capabilities);
    QueryReply {
        ok: true,
        mode,
        statement: result.statement.to_string(),
        engine: result.engine.to_string(),
        columns: result.result.columns.clone(),
        record_count: records.len() as u64,
        result_json: result_json.to_string_compact(),
        affected_rows: result.affected_rows,
    }
}

pub(crate) fn summary(result: &RuntimeQueryResult) -> JsonValue {
    if let Some(ask) = ask_json(&result.result).filter(|_| result.statement == "ask") {
        return ask;
    }

    let columns = result
        .result
        .records
        .first()
        .map(|record| {
            let mut names = record
                .column_names()
                .into_iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>();
            names.sort();
            names.into_iter().map(JsonValue::String).collect()
        })
        .unwrap_or_default();
    let rows = result.result.records.iter().map(flat_record_json).collect();

    JsonValue::Object(
        [
            (
                "statement".to_string(),
                JsonValue::String(result.statement_type.to_string()),
            ),
            (
                "affected".to_string(),
                JsonValue::Number(result.affected_rows as f64),
            ),
            ("columns".to_string(), JsonValue::Array(columns)),
            ("rows".to_string(), JsonValue::Array(rows)),
        ]
        .into_iter()
        .collect(),
    )
}

pub(crate) fn wire_frame(
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

pub(crate) fn error_json(message: &str) -> JsonValue {
    JsonValue::Object(
        [
            ("error".to_string(), JsonValue::String(message.to_string())),
            ("ok".to_string(), JsonValue::Bool(false)),
        ]
        .into_iter()
        .collect(),
    )
}

pub(crate) fn error_proto(message: &str) -> tonic::Status {
    tonic::Status::internal(message.to_string())
}

pub(crate) fn ask_result_from_unified_result(
    result: &UnifiedResult,
) -> Option<crate::runtime::ai::ask_response_envelope::AskResult> {
    let row = result.records.first()?;
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

fn flat_result_json(
    result: &UnifiedResult,
    records: &[UnifiedRecord],
    entity_types: &Option<Vec<String>>,
    capabilities: &Option<Vec<String>>,
) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "columns".to_string(),
        JsonValue::Array(
            result
                .columns
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object.insert(
        "record_count".to_string(),
        JsonValue::Number(records.len() as f64),
    );
    object.insert(
        "selection".to_string(),
        super::query_view::search_selection_json(entity_types, capabilities),
    );
    object.insert(
        "records".to_string(),
        JsonValue::Array(records.iter().map(flat_record_json).collect()),
    );
    JsonValue::Object(object)
}

fn flat_record_json(record: &UnifiedRecord) -> JsonValue {
    JsonValue::Object(
        record
            .iter_fields()
            .map(|(name, value)| {
                (
                    name.to_string(),
                    crate::presentation::entity_json::storage_value_to_json(value),
                )
            })
            .collect(),
    )
}

fn ask_json(result: &UnifiedResult) -> Option<JsonValue> {
    ask_result_from_unified_result(result)
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
    use super::{error_json, error_proto, json, proto_reply, summary, wire_frame};
    use crate::json::Value as JsonValue;
    use crate::runtime::RuntimeQueryResult;
    use crate::storage::query::modes::QueryMode;
    use crate::storage::query::unified::{UnifiedRecord, UnifiedResult};
    use crate::storage::schema::Value;
    use reddb_wire::redwire::MessageKind;

    fn fixture(mode: QueryMode, statement: &'static str, affected_rows: u64) -> RuntimeQueryResult {
        let mut result = UnifiedResult::with_columns(vec!["id".into(), "name".into()]);
        let mut record = UnifiedRecord::new();
        record.set("id", Value::Integer(7));
        record.set("name", Value::text("Ada"));
        result.push(record);
        RuntimeQueryResult {
            query: "SELECT id, name FROM people".into(),
            mode,
            statement,
            engine: "fixture",
            result,
            affected_rows,
            statement_type: statement,
            bookmark: None,
            notice: None,
        }
    }

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
            let result = fixture(mode, "select", 0);
            assert_eq!(json(&result, &None, &None)["mode"].as_str(), Some(expected));
            assert_eq!(proto_reply(&result, &None, &None).mode, expected);

            let frame = wire_frame(17, Ok(&result));
            let payload: JsonValue = crate::json::from_slice(&frame.payload).unwrap();
            assert_eq!(payload["mode"].as_str(), Some(expected));
        }
    }

    #[test]
    fn every_statement_type_has_a_golden_in_every_encoding() {
        let statements = [
            "select", "insert", "update", "delete", "create", "drop", "alter",
        ];
        let mut actual = Vec::new();

        for (index, statement) in statements.into_iter().enumerate() {
            let result = fixture(QueryMode::Sql, statement, index as u64);
            let rendered_json = json(&result, &None, &None);
            let rendered_proto = proto_reply(&result, &None, &None);
            let rendered_summary = summary(&result);
            let rendered_wire = wire_frame(17, Ok(&result));
            let wire_json: JsonValue = crate::json::from_slice(&rendered_wire.payload).unwrap();

            actual.push(format!(
                "{statement}:json={}/{};proto={}/{};summary={}/{};wire={}/{}",
                rendered_json["statement"].as_str().unwrap(),
                rendered_json["affected_rows"].as_u64().unwrap_or(0),
                rendered_proto.statement,
                rendered_proto.affected_rows,
                rendered_summary["statement"].as_str().unwrap(),
                rendered_summary["affected"].as_u64().unwrap(),
                wire_json["statement"].as_str().unwrap(),
                wire_json["affected_rows"].as_u64().unwrap_or(0),
            ));
        }

        assert_eq!(
            actual.join("\n"),
            "select:json=select/0;proto=select/0;summary=select/0;wire=select/0\n\
             insert:json=insert/1;proto=insert/1;summary=insert/1;wire=insert/1\n\
             update:json=update/2;proto=update/2;summary=update/2;wire=update/2\n\
             delete:json=delete/3;proto=delete/3;summary=delete/3;wire=delete/3\n\
             create:json=create/4;proto=create/4;summary=create/4;wire=create/4\n\
             drop:json=drop/5;proto=drop/5;summary=drop/5;wire=drop/5\n\
             alter:json=alter/6;proto=alter/6;summary=alter/6;wire=alter/6"
        );
    }

    #[test]
    fn errors_have_a_golden_in_every_encoding() {
        assert_eq!(
            error_json("boom").to_string_compact(),
            r#"{"error":"boom","ok":false}"#
        );

        let status = error_proto("boom");
        assert_eq!(status.code(), tonic::Code::Internal);
        assert_eq!(status.message(), "boom");

        let frame = wire_frame(23, Err("boom".to_string()));
        assert_eq!(frame.kind, MessageKind::Error);
        assert_eq!(frame.correlation_id, 23);
        assert_eq!(frame.payload, b"boom");
    }

    #[test]
    fn every_transport_encoding_preserves_the_same_row() {
        let result = fixture(QueryMode::Sql, "select", 0);
        let canonical = json(&result, &None, &None);
        let proto = proto_reply(&result, &None, &None);
        let proto_json: JsonValue = crate::json::from_str(&proto.result_json).unwrap();
        let summary_json = summary(&result);
        let wire = wire_frame(41, Ok(&result));

        let canonical_row = &canonical["result"]["records"].as_array().unwrap()[0];
        let proto_row = &proto_json["records"].as_array().unwrap()[0];
        let summary_row = &summary_json["rows"].as_array().unwrap()[0];
        assert_eq!(canonical_row["values"]["name"].as_str(), Some("Ada"));
        assert_eq!(proto_row["name"].as_str(), Some("Ada"));
        assert_eq!(summary_row["name"].as_str(), Some("Ada"));
        assert_eq!(wire.payload, canonical.to_string_compact().as_bytes());
    }
}
