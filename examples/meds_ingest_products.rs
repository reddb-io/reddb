use reddb::api::RedDBOptions;
use reddb::RedDBRuntime;
use serde_json::Value;
use std::{env, fs};

fn main() {
    let db_path = env::args()
        .nth(1)
        .unwrap_or_else(|| "/home/cyber/Work/FF/crawlex/data/medicamentos-brasil.rdb".to_string());
    let json_path = env::args().nth(2).unwrap_or_else(|| {
        "/home/cyber/Work/FF/crawlex/data/meds_extracted_products.json".to_string()
    });
    let products: Vec<Value> =
        serde_json::from_str(&fs::read_to_string(&json_path).expect("read products json"))
            .expect("parse products json");
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(db_path.clone()))
        .expect("open database");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    ensure_pharmacy(
        &rt,
        "consultaremedios-marketplace",
        "Consulta Remédios Marketplace",
        "consultaremedios.com.br",
        "https://consultaremedios.com.br/",
        now,
    );

    let mut inserted_products = 0;
    let mut inserted_labs = 0;
    let mut inserted_offers = 0;
    for p in products {
        let source_site = str_field(&p, "source_site");
        let sku = str_field(&p, "sku");
        let name = str_field(&p, "name");
        let brand = str_field(&p, "brand");
        let brand_url = str_field(&p, "brand_url");
        let seller = str_field(&p, "seller");
        let min_price = f64_field(&p, "min_price");
        let max_price = f64_field(&p, "max_price");
        let index = i64_field(&p, "index");

        let source = if source_site == "unknown" || source_site.is_empty() {
            "consultaremedios"
        } else {
            &source_site
        };
        let product_id = format!("{source}:{sku}");
        let lab_id = if brand.trim().is_empty() {
            String::new()
        } else {
            format!("brand:{}", slug(&brand))
        };
        if !lab_id.is_empty() {
            let sql = format!(
                "INSERT INTO meds_labs (id, name, normalized_name, cnpj, country, source_url, updated_at_unix) VALUES ('{}', '{}', '{}', '', 'BR', '{}', {})",
                esc(&lab_id), esc(&brand), esc(&norm(&brand)), esc(&brand_url), now
            );
            if exec_insert(&rt, &sql, "lab") {
                inserted_labs += 1;
            }
        }
        let product_sql = format!(
            "INSERT INTO meds_products (id, canonical_name, brand_name, normalized_name, active_ingredient_id, lab_id, reference_product_id, therapeutic_class, category, subcategory, concentration, dosage_form, package_desc, regulatory_status, anvisa_registry, ean, source_primary, first_seen_unix, updated_at_unix) VALUES ('{}', '{}', '{}', '{}', '', '{}', '', '', '', '', '', '', '', '', '', '{}', '{}', {}, {})",
            esc(&product_id), esc(&name), esc(&brand), esc(&norm(&name)), esc(&lab_id), esc(&sku), esc(source), now, now
        );
        if exec_insert(&rt, &product_sql, "product") {
            inserted_products += 1;
        }
        if let Some(min_price) = min_price {
            let offer_id = format!("offer:{source}:{sku}:{index}");
            let price_cents = (min_price * 100.0).round() as i64;
            let list_price_cents = (max_price.unwrap_or(min_price) * 100.0).round() as i64;
            let discount = if list_price_cents > price_cents && list_price_cents > 0 {
                ((list_price_cents - price_cents) as f64 * 100.0) / list_price_cents as f64
            } else {
                0.0
            };
            let offer_sql = format!(
                "INSERT INTO meds_offers (id, product_id, pharmacy_id, source_site, url, title, price_cents, list_price_cents, discount_pct, currency, in_stock, shipping_json, seller_name, captured_at_unix) VALUES ('{}', '{}', 'consultaremedios-marketplace', '{}', '{}', '{}', {}, {}, {}, 'BRL', true, '{{}}', '{}', {})",
                esc(&offer_id), esc(&product_id), esc(source), esc("https://consultaremedios.com.br/"), esc(&name), price_cents, list_price_cents, discount, esc(&seller), now
            );
            if exec_insert(&rt, &offer_sql, "offer") {
                inserted_offers += 1;
            }
        }
    }
    rt.checkpoint().ok();
    println!("ingested products={inserted_products} labs={inserted_labs} offers={inserted_offers} from {json_path} into {db_path}");
}

fn ensure_pharmacy(rt: &RedDBRuntime, id: &str, name: &str, domain: &str, url: &str, now: i64) {
    let sql = format!(
        "INSERT INTO meds_pharmacies (id, name, domain, marketplace, source_url, updated_at_unix) VALUES ('{}', '{}', '{}', 'true', '{}', {})",
        esc(id), esc(name), esc(domain), esc(url), now
    );
    exec_insert(rt, &sql, "pharmacy");
}

fn exec_insert(rt: &RedDBRuntime, sql: &str, label: &str) -> bool {
    match rt.execute_query(sql) {
        Ok(_) => true,
        Err(err) => {
            let msg = err.to_string().to_lowercase();
            if msg.contains("duplicate") || msg.contains("already") || msg.contains("primary") {
                false
            } else {
                eprintln!("{label} insert warning: {err}\n  sql={sql}");
                false
            }
        }
    }
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "''")
}
fn norm(s: &str) -> String {
    s.trim().to_lowercase()
}

fn str_field(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn f64_field(value: &Value, field: &str) -> Option<f64> {
    value.get(field).and_then(Value::as_f64)
}

fn i64_field(value: &Value, field: &str) -> i64 {
    value.get(field).and_then(Value::as_i64).unwrap_or_default()
}

fn slug(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}
