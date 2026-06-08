use reddb_client::{types::JsonValue, Reddb};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = "/tmp/crawlex-queue-shape.rdb";
    let _ = std::fs::remove_file(path);
    let url = format!("file://{path}");
    let db = Reddb::connect(&url).await?;
    let _ = db
        .query("CREATE TABLE IF NOT EXISTS crawlex_frontier_jobs")
        .await;
    let row = JsonValue::object([
        ("job_id", JsonValue::number(1.0)),
        ("payload", JsonValue::string("hello")),
        ("status", JsonValue::string("pending")),
        ("available_at_ms", JsonValue::number(0.0)),
        ("priority", JsonValue::number(0.0)),
    ]);
    let ins = db.insert("crawlex_frontier_jobs", &row).await?;
    println!("insert={ins:?}");
    for sql in [
        "SELECT * FROM crawlex_frontier_jobs",
        "SELECT id, job_id, payload, status FROM crawlex_frontier_jobs",
        "SELECT _id, job_id, payload, status FROM crawlex_frontier_jobs",
        "SELECT rid, job_id, payload, status FROM crawlex_frontier_jobs",
    ] {
        println!("SQL: {sql}");
        match db.query(sql).await {
            Ok(q) => println!("rows={:?}", q.rows),
            Err(e) => println!("err={e}"),
        }
    }
    Ok(())
}
