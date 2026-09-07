use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;
use std::time::Instant;
fn main() {
 let a: Vec<String> = std::env::args().collect();
 let count: usize = a[1].parse().unwrap();
 let primary = a[2] == "pk";
 let rt = RedDBRuntime::with_options(if a[3]=="memory" { RedDBOptions::in_memory() } else {RedDBOptions::persistent(&a[3])}).unwrap();
 rt.execute_query(if primary {"CREATE TABLE t (id TEXT PRIMARY KEY,payload TEXT)"} else {"CREATE TABLE t (id TEXT,payload TEXT)"}).unwrap();
 let payload = format!("{{\"type\":\"text\",\"text\":\"{}\"}}", "x".repeat(400));
 let mut samples=Vec::new();
 for i in 0..count {
  let start=Instant::now();
  let result = rt.execute_query_with_params("INSERT INTO t (id,payload) VALUES ($1,$2)", &[Value::text(format!("k{i}")),Value::text(payload.clone())]).unwrap();
  samples.push(start.elapsed().as_secs_f64()*1000000.0);
  assert_eq!(result.affected_rows,1);
 }
 let rows=rt.execute_query("SELECT id,payload FROM t").unwrap().result.records;
 let mut seen=std::collections::HashSet::new();
 for row in &rows {
  let Some(Value::Text(id))=row.get("id") else {panic!("id")};
  let idx: usize=id.strip_prefix('k').unwrap().parse().unwrap();
  assert!(idx<count); assert!(seen.insert(idx));
  assert_eq!(row.get("payload"),Some(&Value::text(payload.clone())));
 }
 assert_eq!(rows.len(),count);
 println!("{{\"count\":{count},\"pk\":{primary},\"valid\":true,\"samples_us\":{samples:?}}}");
}
