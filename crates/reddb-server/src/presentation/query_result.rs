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
        let statements = ["select", "insert", "update", "delete", "create", "drop", "alter"];
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
        assert_eq!(error_json("boom").to_string_compact(), r#"{"error":"boom","ok":false}"#);

        let status = error_proto("boom");
        assert_eq!(status.code(), tonic::Code::Internal);
        assert_eq!(status.message(), "boom");

        let frame = wire_frame(23, Err("boom"));
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
        let wire_json: JsonValue = crate::json::from_slice(&wire.payload).unwrap();

        assert_eq!(canonical["result"]["records"][0]["values"]["name"], "Ada");
        assert_eq!(proto_json["records"][0]["name"], "Ada");
        assert_eq!(summary_json["rows"][0]["name"], "Ada");
        assert_eq!(wire_json, canonical);
    }
}
