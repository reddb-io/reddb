use reddb::api::RedDBOptions;
use reddb::RedDBRuntime;
use std::env;

fn main() {
    let path = env::args()
        .nth(1)
        .unwrap_or_else(|| "/home/cyber/Work/FF/crawlex/data/medicamentos-brasil.rdb".to_string());
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
        .unwrap_or_else(|err| panic!("open {path}: {err}"));

    let stmts = [
        // Relational core: normalized catalog facts.
        "CREATE TABLE IF NOT EXISTS meds_products (id TEXT PRIMARY KEY, canonical_name TEXT, brand_name TEXT, normalized_name TEXT, active_ingredient_id TEXT, lab_id TEXT, reference_product_id TEXT, therapeutic_class TEXT, category TEXT, subcategory TEXT, concentration TEXT, dosage_form TEXT, package_desc TEXT, regulatory_status TEXT, anvisa_registry TEXT, ean TEXT, source_primary TEXT, first_seen_unix INT, updated_at_unix INT)",
        "CREATE TABLE IF NOT EXISTS meds_active_ingredients (id TEXT PRIMARY KEY, name TEXT, normalized_name TEXT, synonyms_json TEXT, class TEXT, source_url TEXT, updated_at_unix INT)",
        "CREATE TABLE IF NOT EXISTS meds_labs (id TEXT PRIMARY KEY, name TEXT, normalized_name TEXT, cnpj TEXT, country TEXT, source_url TEXT, updated_at_unix INT)",
        "CREATE TABLE IF NOT EXISTS meds_pharmacies (id TEXT PRIMARY KEY, name TEXT, domain TEXT, marketplace TEXT, source_url TEXT, updated_at_unix INT)",
        "CREATE TABLE IF NOT EXISTS meds_categories (id TEXT PRIMARY KEY, name TEXT, parent_id TEXT, source_url TEXT, source_site TEXT, updated_at_unix INT)",
        "CREATE TABLE IF NOT EXISTS meds_product_links (id TEXT PRIMARY KEY, product_id TEXT, source_site TEXT, url TEXT, title TEXT, canonical_url TEXT, status INT, discovered_at_unix INT, last_seen_unix INT)",
        "CREATE TABLE IF NOT EXISTS meds_offers (id TEXT PRIMARY KEY, product_id TEXT, pharmacy_id TEXT, source_site TEXT, url TEXT, title TEXT, price_cents INT, list_price_cents INT, discount_pct REAL, currency TEXT, in_stock BOOL, shipping_json TEXT, seller_name TEXT, captured_at_unix INT)",
        "CREATE TABLE IF NOT EXISTS meds_reference_map (id TEXT PRIMARY KEY, product_id TEXT, reference_product_id TEXT, relation TEXT, evidence_url TEXT, evidence_text TEXT, source_site TEXT, updated_at_unix INT)",
        "CREATE TABLE IF NOT EXISTS meds_crawl_sources (id TEXT PRIMARY KEY, source_site TEXT, seed_url TEXT, role TEXT, priority_rank INT, enabled BOOL, last_crawled_unix INT)",
        // Documents: raw/unstructured extracted payloads.
        "CREATE DOCUMENT meds_product_docs",
        "CREATE DOCUMENT meds_offer_docs",
        "CREATE DOCUMENT meds_page_extracts",
        "CREATE DOCUMENT meds_raw_pages",
        "CREATE DOCUMENT meds_parse_findings",
        "CREATE DOCUMENT meds_source_taxonomies",
        // Queues: crawling and normalization workstreams.
        "CREATE QUEUE IF NOT EXISTS meds_url_frontier PRIORITY MAX_SIZE 1000000",
        "CREATE QUEUE IF NOT EXISTS meds_parse_jobs PRIORITY MAX_SIZE 1000000",
        "CREATE QUEUE IF NOT EXISTS meds_price_refresh_jobs PRIORITY MAX_SIZE 1000000",
        // Time-series: price history + crawl telemetry.
        "CREATE TIMESERIES meds_price_history RETENTION 730 d",
        "CREATE TIMESERIES meds_crawl_telemetry RETENTION 90 d",
        // Analytics descriptors/materialized intent.
        "CREATE METRIC meds.price.min TYPE gauge ROLE operational",
        "CREATE METRIC meds.price.available_offers TYPE counter ROLE operational",
        "CREATE METRIC meds.crawl.pages TYPE counter ROLE operational",
    ];

    for stmt in stmts {
        match rt.execute_query(stmt) {
            Ok(_) => println!("ok: {stmt}"),
            Err(err) => {
                let msg = err.to_string();
                if msg.contains("already exists") || msg.contains("Already exists") {
                    println!("exists: {stmt}");
                } else {
                    eprintln!("error: {stmt}\n  {msg}");
                    std::process::exit(1);
                }
            }
        }
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let seeds = [
        (
            "consultaremedios",
            "https://consultaremedios.com.br/",
            "catalog",
            100,
        ),
        (
            "raiadrogasil",
            "https://www.drogaraia.com.br/",
            "pharmacy",
            90,
        ),
        (
            "drogariasaopaulo",
            "https://www.drogariasaopaulo.com.br/",
            "pharmacy",
            90,
        ),
        (
            "drogariapacheco",
            "https://www.drogariapacheco.com.br/",
            "pharmacy",
            90,
        ),
        ("bifarma", "https://www.bifarma.com.br/", "pharmacy", 80),
    ];
    for (site, url, role, priority) in seeds {
        let id = format!("seed:{site}");
        let sql = format!(
            "INSERT INTO meds_crawl_sources (id, source_site, seed_url, role, priority_rank, enabled, last_crawled_unix) VALUES ('{}', '{}', '{}', '{}', {}, true, 0)",
            esc(&id), esc(site), esc(url), esc(role), priority
        );
        match rt.execute_query(&sql) {
            Ok(_) => println!("seed: {site} -> {url}"),
            Err(err) => {
                let msg = err.to_string();
                if msg.contains("duplicate") || msg.contains("already") || msg.contains("primary") {
                    println!("seed exists: {site}");
                } else {
                    eprintln!("seed error {site}: {msg}");
                }
            }
        }
        let q = format!(
            "QUEUE PUSH meds_url_frontier {{source_site: '{}', url: '{}', role: '{}', discovered_at_unix: {}}} PRIORITY {}",
            esc(site), esc(url), esc(role), now, priority
        );
        if let Err(err) = rt.execute_query(&q) {
            eprintln!("queue seed warning {site}: {err}");
        }
    }

    rt.checkpoint().ok();
    println!("database ready: {path}");
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "''")
}
