use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};
use reddb_types::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const TEST: &str = "acknowledged_cross_model_writes_survive_one_process_death";
const CHILD_PATH: &str = "REDDB_CROSS_MODEL_CRASH_PATH";

fn options(path: &Path) -> RedDBOptions {
    RedDBOptions::persistent(path)
        .with_memory_budget(64 * 1024 * 1024)
        .with_storage_profile(StorageDeployPreset::Serverless.selection())
        .expect("paged profile")
}

fn execute(runtime: &RedDBRuntime, sql: &str) -> reddb_server::runtime::RuntimeQueryResult {
    runtime
        .execute_query(sql)
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}

fn seed(path: &Path) {
    let runtime = RedDBRuntime::with_options(options(path)).expect("seed runtime");
    for sql in [
        "SET CONFIG runtime.mvcc.autovacuum.enabled = false",
        "CREATE TABLE apocalypse_rows (id INT, org_id TEXT, value INT)",
        "CREATE UNIQUE INDEX apocalypse_id ON apocalypse_rows (id) USING HASH",
        "CREATE INDEX apocalypse_org ON apocalypse_rows (org_id) USING BTREE",
        "INSERT INTO apocalypse_rows (id,org_id,value) VALUES (1,'org-a',10)",
        "CREATE DOCUMENT apocalypse_docs",
        "CREATE KV apocalypse_kv",
        "CREATE GRAPH apocalypse_graph",
        "CREATE TIMESERIES apocalypse_metrics RETENTION 7 d",
        "CREATE COLLECTION apocalypse_vectors KIND vector.turbo DIM 2 METRIC cosine",
        "CREATE HLL apocalypse_visitors",
        "CREATE SKETCH apocalypse_frequency",
        "CREATE FILTER apocalypse_seen",
    ] {
        execute(&runtime, sql);
    }
    runtime.checkpoint().expect("baseline checkpoint");
    runtime.graceful_shutdown(false).expect("baseline shutdown");
}

fn child(path: &Path) {
    let runtime = RedDBRuntime::with_options(options(path)).expect("writer runtime");
    for sql in [
        "UPDATE apocalypse_rows SET org_id = 'org-b', value = 20 WHERE id = 1",
        r#"INSERT INTO apocalypse_docs DOCUMENT VALUES ({"name":"survivor","generation":2})"#,
        "INSERT INTO apocalypse_kv KV (key, value) VALUES ('generation', 2)",
        "INSERT INTO apocalypse_metrics (metric, value, tags, timestamp) VALUES ('cpu.usage', 42.0, '{\"host\":\"srv-a\"}', 100)",
        "INSERT INTO apocalypse_vectors VECTOR (embedding, content) VALUES ([1,0], 'survivor-vector')",
        "HLL ADD apocalypse_visitors 'alice' 'bob' 'carol'",
        "SKETCH ADD apocalypse_frequency 'red' 3",
        "FILTER ADD apocalypse_seen 'alice'",
    ] {
        execute(&runtime, sql);
    }
    let alice = execute(
        &runtime,
        "INSERT INTO apocalypse_graph NODE (label, node_type, generation) VALUES ('alice','User',2) RETURNING *",
    );
    let bob = execute(
        &runtime,
        "INSERT INTO apocalypse_graph NODE (label, node_type, generation) VALUES ('bob','User',2) RETURNING *",
    );
    let rid = |result: &reddb_server::runtime::RuntimeQueryResult| match result.result.records[0]
        .get("rid")
    {
        Some(Value::UnsignedInteger(value)) => *value,
        value => panic!("graph rid: {value:?}"),
    };
    execute(
        &runtime,
        &format!(
            "INSERT INTO apocalypse_graph EDGE (label, from, to, generation) VALUES ('knows',{},{},2)",
            rid(&alice),
            rid(&bob)
        ),
    );
    std::fs::write(path.with_extension("ready"), b"all writes acknowledged")
        .expect("publish child readiness");
    loop {
        std::thread::park();
    }
}

fn spawn_child(path: &Path) -> Child {
    Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", TEST, "--nocapture"])
        .env(CHILD_PATH, path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("writer child")
}

fn kill_after_ack(path: &Path) {
    let ready = path.with_extension("ready");
    let mut process = spawn_child(path);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready.exists() {
        assert!(
            process.try_wait().expect("writer status").is_none(),
            "writer exited before acknowledging every model"
        );
        if Instant::now() >= deadline {
            process.kill().expect("stop stuck writer");
            let output = process.wait_with_output().expect("writer output");
            panic!(
                "writer did not acknowledge within 30 seconds: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    process.kill().expect("kill acknowledged writer");
    assert!(
        !process.wait().expect("reap writer").success(),
        "writer must die without shutdown"
    );
}

fn only_value(runtime: &RedDBRuntime, sql: &str, field: &str) -> Value {
    let result = execute(runtime, sql);
    assert_eq!(result.result.records.len(), 1, "{sql}");
    result.result.records[0]
        .get(field)
        .unwrap_or_else(|| panic!("{sql}: missing {field}"))
        .clone()
}

fn verify(runtime: &RedDBRuntime) {
    assert_eq!(
        only_value(
            runtime,
            "SELECT value FROM apocalypse_rows WHERE id = 1",
            "value"
        ),
        Value::Integer(20)
    );
    assert_eq!(
        only_value(
            runtime,
            "SELECT value FROM apocalypse_rows WHERE org_id = 'org-b'",
            "value"
        ),
        Value::Integer(20)
    );
    assert!(execute(
        runtime,
        "SELECT id FROM apocalypse_rows WHERE org_id = 'org-a'"
    )
    .result
    .records
    .is_empty());
    assert_eq!(
        only_value(
            runtime,
            "SELECT generation FROM apocalypse_docs WHERE name = 'survivor'",
            "generation"
        ),
        Value::Integer(2)
    );
    assert_eq!(
        only_value(
            runtime,
            "SELECT value FROM apocalypse_kv WHERE key = 'generation'",
            "value"
        ),
        Value::Integer(2)
    );
    assert_eq!(
        execute(
            runtime,
            "SELECT label FROM apocalypse_graph WHERE generation = 2 ORDER BY label"
        )
        .result
        .records
        .len(),
        3,
        "two nodes and their edge must recover together"
    );
    assert_eq!(
        only_value(
            runtime,
            "SELECT value FROM apocalypse_metrics WHERE metric = 'cpu.usage'",
            "value"
        ),
        Value::Float(42.0)
    );
    let vector = execute(
        runtime,
        "VECTOR SEARCH apocalypse_vectors SIMILAR TO [1,0] MODE APPROXIMATE LIMIT 1",
    );
    assert_eq!(
        vector.result.records[0].get("content"),
        Some(&Value::text("survivor-vector"))
    );
    assert_eq!(
        only_value(runtime, "HLL COUNT apocalypse_visitors", "count").display_string(),
        "3"
    );
    assert_eq!(
        only_value(
            runtime,
            "SKETCH COUNT apocalypse_frequency 'red'",
            "estimate"
        )
        .display_string(),
        "3"
    );
    assert_eq!(
        only_value(runtime, "FILTER CHECK apocalypse_seen 'alice'", "exists"),
        Value::Boolean(true)
    );
}

#[test]
fn acknowledged_cross_model_writes_survive_one_process_death() {
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        child(Path::new(&path));
    }

    let directory = tempfile::tempdir().expect("isolated cross-model database");
    let path: PathBuf = directory.path().join("cross-model.rdb");
    seed(&path);
    kill_after_ack(&path);

    let snapshot = reddb_file::support_dir_for(&path).join("snapshots/apocalypse_vectors.tv");
    std::fs::create_dir_all(snapshot.parent().expect("snapshot parent"))
        .expect("legacy snapshot directory");
    std::fs::write(&snapshot, b"corrupt legacy vector.turbo snapshot")
        .expect("corrupt derived snapshot");
    let corrupt_snapshot = std::fs::read(&snapshot).expect("snapshot evidence");

    for reopen in 0..3 {
        let runtime = RedDBRuntime::with_options(options(&path))
            .unwrap_or_else(|error| panic!("reopen {reopen}: {error}"));
        verify(&runtime);
        runtime.checkpoint().expect("recovered checkpoint");
        assert_eq!(
            std::fs::read(&snapshot).expect("derived snapshot evidence"),
            corrupt_snapshot,
            "authoritative recovery must not rewrite the corrupt derived artifact"
        );
    }
}
