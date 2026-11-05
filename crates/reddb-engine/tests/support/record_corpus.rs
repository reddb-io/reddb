//! Deterministic entity shapes for the original native materialization oracle.
use reddb_engine::storage::entity::*;
use reddb_types::Value;
use std::{collections::HashMap, sync::Arc};

pub fn entities() -> Vec<UnifiedEntity> {
    let mut entities = crate::codec_corpus::entities();
    let mut add = |row: RowData| {
        let mut entity = UnifiedEntity::table_row(
            EntityId::new(2000 + entities.len() as u64),
            "record_rows",
            7,
            vec![],
        );
        entity.data = EntityData::Row(row);
        entities.push(entity);
    };
    add(RowData::with_names(
        vec![Value::text("theme"), Value::text("dark")],
        vec!["key".into(), "value".into()],
    ));
    let mut row = RowData::new(vec![
        Value::Integer(42),
        Value::UnsignedInteger(1700000000123),
        Value::UnsignedInteger(1700000000456),
        Value::text("org-a"),
    ]);
    row.schema = Some(Arc::new(vec![
        "answer".into(),
        "created_at".into(),
        "updated_at".into(),
        "tenant_id".into(),
    ]));
    add(row);
    let mut body = vec![];
    reddb_types::document_body_codec::encode(
        &[
            ("answer", &Value::Integer(42)),
            ("nested", &Value::Json(b"{\"n\":7}".to_vec())),
        ],
        &mut body,
    )
    .expect("record document");
    add(RowData::with_names(
        vec![Value::Json(body), Value::Integer(999)],
        vec!["body".into(), "answer".into()],
    ));
    add(RowData::with_names(
        vec![Value::Json(b"{\"answer\":7}".to_vec())],
        vec!["body".into()],
    ));
    add(RowData::new(vec![]));
    entities.push(UnifiedEntity::graph_edge(
        EntityId::new(3000),
        "numeric_endpoints",
        "9007199254740993",
        "42",
        0.5,
        HashMap::new(),
    ));
    for (index, entity) in entities.iter_mut().enumerate() {
        entity.created_at = 123456;
        entity.updated_at = 123499;
        entity.sequence_id = index as u64 + 99;
        entity.set_logical_id(EntityId::new(9007199254740993 + index as u64));
        if let EntityData::TimeSeries(ts) = &mut entity.data {
            ts.tags
                .insert("typed".into(), "\u{1}[1,true,null,{\"n\":7}]".into());
            ts.tags.insert("plain".into(), "123".into());
            ts.tags.insert("invalid".into(), "\u{1}broken".into());
        }
    }
    entities
}

pub fn columns() -> Vec<String> {
    [
        "answer",
        "title",
        "body",
        "c2",
        "key",
        "value",
        "metric",
        "timestamp",
        "tags",
        "max",
        "rid",
        "created_at",
        "updated_at",
        "tenant_id",
        "absent",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}
