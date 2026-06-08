use reddb::api::RedDBOptions;
use reddb::schema::Value;
use reddb::RedDBRuntime;
use std::env;

fn main() {
    let path = env::args()
        .nth(1)
        .unwrap_or_else(|| "/home/cyber/Work/FF/crawlex/data/medicamentos-brasil.rdb".to_string());
    let rt =
        RedDBRuntime::with_options(RedDBOptions::persistent(path.clone())).expect("open database");
    println!("database={path}");

    print_query(&rt, "counts", "SELECT COUNT(*) AS n FROM meds_products");
    print_query(&rt, "counts", "SELECT COUNT(*) AS n FROM meds_labs");
    print_query(&rt, "counts", "SELECT COUNT(*) AS n FROM meds_pharmacies");
    print_query(&rt, "counts", "SELECT COUNT(*) AS n FROM meds_offers");
    print_query(
        &rt,
        "counts",
        "SELECT COUNT(*) AS n FROM crawlex_raw_response_rows",
    );
    print_query(
        &rt,
        "counts",
        "SELECT COUNT(*) AS n FROM crawlex_rendered_page_rows",
    );
    print_query(
        &rt,
        "counts",
        "SELECT COUNT(*) AS n FROM crawlex_tech_fingerprints",
    );
    print_query(
        &rt,
        "counts",
        "SELECT COUNT(*) AS n FROM crawlex_challenges",
    );

    print_query(
        &rt,
        "products_sample",
        "SELECT id, canonical_name, brand_name, ean, source_primary FROM meds_products LIMIT 8",
    );
    print_query(
        &rt,
        "offers_sample",
        "SELECT product_id, source_site, price_cents, list_price_cents, seller_name FROM meds_offers LIMIT 8",
    );
    print_query(
        &rt,
        "labs_sample",
        "SELECT id, name, country, source_url FROM meds_labs LIMIT 8",
    );
    print_query(
        &rt,
        "crawl_pages_sample",
        "SELECT url, status, body_bytes, inline_body_bytes, truncated FROM crawlex_raw_response_rows LIMIT 8",
    );
}

fn print_query(rt: &RedDBRuntime, label: &str, sql: &str) {
    println!("\n[{label}] {sql}");
    let result = match rt.execute_query(sql) {
        Ok(result) => result,
        Err(err) => {
            println!("ERROR {err}");
            return;
        }
    };
    println!("columns={:?}", result.result.columns);
    for (idx, record) in result.result.records.iter().enumerate() {
        print!("row[{idx}]");
        if let Some(map) = record.overflow() {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            for key in keys {
                let val = map.get(key).unwrap();
                print!(" {key}={}", short_value(val));
            }
        } else {
            for (col, val) in result
                .result
                .columns
                .iter()
                .zip(record.schema_values().iter())
            {
                print!(" {col}={}", short_value(val));
            }
        }
        println!();
    }
}

fn short_value(value: &Value) -> String {
    let raw = match value {
        Value::Text(s) => s.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::UnsignedInteger(i) => i.to_string(),
        Value::Float(f) => format!("{f:.2}"),
        Value::Boolean(b) => b.to_string(),
        Value::Null => "null".to_string(),
        other => format!("{other:?}"),
    };
    let compact = raw.replace('\n', " ").replace('\r', " ");
    if compact.chars().count() > 120 {
        format!("{}...", compact.chars().take(120).collect::<String>())
    } else {
        compact
    }
}
