use reddb::api::RedDBOptions;
use reddb::RedDBRuntime;
use std::env;

fn main() {
    let path = env::args()
        .nth(1)
        .unwrap_or_else(|| "/home/cyber/Work/FF/crawlex/data/medicamentos-brasil.rdb".to_string());
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
        .unwrap_or_else(|err| panic!("open {path}: {err}"));
    for sql in [
        "SELECT source_site, seed_url, role, priority_rank FROM meds_crawl_sources ORDER BY priority_rank DESC",
        "SELECT count(*) AS n FROM meds_crawl_sources",
        "SELECT count(*) AS n FROM meds_products",
        "SELECT count(*) AS n FROM meds_offers",
    ] {
        println!("-- {sql}");
        match rt.execute_query(sql) {
            Ok(qr) => println!("{qr:#?}"),
            Err(err) => println!("ERROR: {err}"),
        }
    }
}
