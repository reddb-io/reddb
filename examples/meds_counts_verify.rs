use reddb::api::RedDBOptions;
use reddb::RedDBRuntime;
use std::env;

fn main() {
    let path = env::args()
        .nth(1)
        .unwrap_or_else(|| "/home/cyber/Work/FF/crawlex/data/medicamentos-brasil.rdb".to_string());
    let rt =
        RedDBRuntime::with_options(RedDBOptions::persistent(path.clone())).expect("open database");
    println!("database: {path}");
    for sql in [
        "SELECT COUNT(*) AS n FROM meds_crawl_sources",
        "SELECT source_site, seed_url, role, priority_rank FROM meds_crawl_sources ORDER BY priority_rank DESC",
        "SELECT COUNT(*) AS n FROM crawlex_raw_response_rows",
        "SELECT url, status, body_bytes, inline_body_bytes, truncated FROM crawlex_raw_response_rows LIMIT 20",
        "SELECT COUNT(*) AS n FROM crawlex_rendered_page_rows",
        "SELECT url, status, bytes, html_bytes, inline_html_bytes, truncated FROM crawlex_rendered_page_rows LIMIT 20",
        "SELECT COUNT(*) AS n FROM crawlex_tech_fingerprints",
        "SELECT COUNT(*) AS n FROM crawlex_challenges",
        "SELECT COUNT(*) AS n FROM meds_products",
        "SELECT COUNT(*) AS n FROM meds_offers",
    ] {
        println!("\n-- {sql}");
        match rt.execute_query(sql) {
            Ok(value) => println!("{value:#?}"),
            Err(err) => println!("ERROR: {err:?}"),
        }
    }
}
