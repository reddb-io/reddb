use reddb::api::RedDBOptions;
use reddb::application::{
    CreateRowInput, CreateRowsBatchInput, EntityUseCases, ExecuteQueryInput, QueryUseCases,
};
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;

fn main() {
    let opts = RedDBOptions::in_memory();
    let rt = RedDBRuntime::with_options(opts).expect("rt");
    let uc_e = EntityUseCases::new(&rt);
    let uc_q = QueryUseCases::new(&rt);

    uc_q
        .execute(ExecuteQueryInput {
            query: "CREATE TABLE t (id INT, name TEXT)".into(),
        })
        .expect("create");

    // Test A: 100-row batch
    let rows: Vec<_> = (0..100u64)
        .map(|i| CreateRowInput {
            collection: "t".into(),
            fields: vec![
                ("id".into(), Value::Integer(i as i64)),
                ("name".into(), Value::Text(format!("A{i}"))),
            ],
            metadata: vec![],
            node_links: vec![],
            vector_links: vec![],
        })
        .collect();
    let r = uc_e
        .create_rows_batch(CreateRowsBatchInput {
            collection: "t".into(),
            rows,
        })
        .expect("a");
    let found_a = r.iter().filter(|o| o.entity.is_some()).count();
    println!("A: 100-row batch -> {} entities returned, {} have entity populated", r.len(), found_a);

    // CREATE INDEX between A and B
    uc_q
        .execute(ExecuteQueryInput {
            query: "CREATE INDEX idx_name ON t (name) USING HASH".into(),
        })
        .expect("idx");

    // Test B: create_rows_batch with 1-row
    for i in 10..13u64 {
        let r = uc_e
            .create_rows_batch(CreateRowsBatchInput {
                collection: "t".into(),
                rows: vec![CreateRowInput {
                    collection: "t".into(),
                    fields: vec![
                        ("id".into(), Value::Integer(i as i64)),
                        ("name".into(), Value::Text(format!("B{i}"))),
                    ],
                    metadata: vec![],
                    node_links: vec![],
                    vector_links: vec![],
                }],
            })
            .expect("b");
        println!(
            "B: create_rows_batch(1 row) id={:?} entity.is_some={} store.get={}",
            r[0].id.raw(),
            r[0].entity.is_some(),
            rt.db().store().get("t", r[0].id).is_some()
        );
    }

    // Test C: create_rows_batch with 3 rows
    let rows: Vec<_> = (20..23u64)
        .map(|i| CreateRowInput {
            collection: "t".into(),
            fields: vec![
                ("id".into(), Value::Integer(i as i64)),
                ("name".into(), Value::Text(format!("C{i}"))),
            ],
            metadata: vec![],
            node_links: vec![],
            vector_links: vec![],
        })
        .collect();
    let r = uc_e
        .create_rows_batch(CreateRowsBatchInput {
            collection: "t".into(),
            rows,
        })
        .expect("c");
    for o in &r {
        println!(
            "C: create_rows_batch(3 rows) id={:?} entity.is_some={} store.get={}",
            o.id.raw(),
            o.entity.is_some(),
            rt.db().store().get("t", o.id).is_some()
        );
    }

    let mgr = rt.db().store().get_collection("t").unwrap();
    println!("manager count = {}", mgr.query_all(|_| true).len());
    println!("expected 9 (3 A + 3 B + 3 C)");
}
