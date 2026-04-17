use reddb::api::{DurabilityMode, RedDBOptions};
use reddb::application::{
    CreateRowInput, CreateRowsBatchInput, EntityUseCases, ExecuteQueryInput, QueryUseCases,
};
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;

fn main() {
    let mut opts = RedDBOptions::in_memory();
    opts.durability_mode = DurabilityMode::Async;
    let rt = RedDBRuntime::with_options(opts).expect("rt");
    let uc_e = EntityUseCases::new(&rt);
    let uc_q = QueryUseCases::new(&rt);

    uc_q
        .execute(ExecuteQueryInput {
            query: "CREATE TABLE users (id INT, city TEXT, age INT)".into(),
        })
        .expect("create");

    // First half: bulk insert in batches (mimics reddb_binary::insert_bulk)
    let mut batch_rows = Vec::new();
    for i in 0..100u64 {
        let city = if i % 2 == 0 { "NYC" } else { "LA" };
        batch_rows.push(CreateRowInput {
            collection: "users".into(),
            fields: vec![
                ("id".into(), Value::Integer(i as i64)),
                ("city".into(), Value::Text(city.into())),
                ("age".into(), Value::Integer(20 + (i % 40) as i64)),
            ],
            metadata: vec![],
            node_links: vec![],
            vector_links: vec![],
        });
    }
    uc_e.create_rows_batch(CreateRowsBatchInput {
        collection: "users".into(),
        rows: batch_rows,
    })
    .expect("bulk1");

    uc_q
        .execute(ExecuteQueryInput {
            query: "CREATE INDEX idx_city ON users (city) USING HASH".into(),
        })
        .expect("create_idx");

    // Second half: "insert_one" via create_rows_batch with exactly 1 row
    // (mimics reddb_binary::insert_one which wraps to insert_bulk([record]))
    let mut out_count = 0usize;
    for i in 100u64..105u64 {
        let city = if i % 2 == 0 { "NYC" } else { "LA" };
        let out = uc_e.create_rows_batch(CreateRowsBatchInput {
            collection: "users".into(),
            rows: vec![CreateRowInput {
                collection: "users".into(),
                fields: vec![
                    ("id".into(), Value::Integer(i as i64)),
                    ("city".into(), Value::Text(city.into())),
                    ("age".into(), Value::Integer(20 + (i % 40) as i64)),
                ],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            }],
        })
        .expect("insert2");
        let ids: Vec<_> = out.iter().map(|o| o.id.raw()).collect();
        let fetched: Vec<_> = out.iter().map(|o| o.entity.is_some()).collect();
        let direct: Vec<_> = out
            .iter()
            .map(|o| rt.db().store().get("users", o.id).is_some())
            .collect();
        println!("row i={i} → ids={ids:?} output.entity.is_some={fetched:?} store.get={direct:?}");
        out_count += out.len();
    }
    println!("total outputs for 5 single-row batches: {out_count}");

    // Enumerate ALL ids actually in users collection
    let mgr = rt.db().store().get_collection("users").unwrap();
    let all = mgr.query_all(|_| true);
    let ids_in_manager: Vec<u64> = all.iter().map(|e| e.id.raw()).collect();
    println!("users manager has {} entities, ids={:?}", all.len(), ids_in_manager);

    let r_total = uc_q
        .execute(ExecuteQueryInput {
            query: "SELECT COUNT(*) AS n FROM users".into(),
        })
        .expect("q_total");
    println!("COUNT total = {:?}", r_total.result.records);

    let r_nyc = uc_q
        .execute(ExecuteQueryInput {
            query: "SELECT COUNT(*) AS n FROM users WHERE city = 'NYC'".into(),
        })
        .expect("q_nyc");
    println!("COUNT NYC = {:?}", r_nyc.result.records);
    println!("(expected 200 total, 100 NYC)");
}
