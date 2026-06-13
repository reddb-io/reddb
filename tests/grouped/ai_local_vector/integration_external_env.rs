use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reddb::client::RedDBClient;

fn env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn required_env(key: &str) -> String {
    env(key).unwrap_or_else(|| panic!("missing required env var: {key}"))
}

fn test_profile() -> String {
    env("REDDB_TEST_PROFILE").unwrap_or_else(|| "replica".to_string())
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime should build")
}

fn unique_table(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();
    format!("{}_{}_{}", prefix, std::process::id(), nanos)
}

fn sql_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn http_call(
    method: &str,
    url: &str,
    payload: Option<&str>,
) -> Result<(u16, String), Box<dyn std::error::Error>> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .build()
        .into();
    let response = match method {
        "GET" => agent.get(url).call(),
        "POST" => {
            let body = payload.unwrap_or("{}").to_string();
            agent
                .post(url)
                .header("content-type", "application/json")
                .send(body)
        }
        other => panic!("unsupported method: {other}"),
    };

    match response {
        Ok(mut resp) => {
            let status = resp.status().as_u16();
            let body = resp.body_mut().read_to_string()?;
            Ok((status, body))
        }
        Err(err) => Err(Box::new(err)),
    }
}

fn wait_for_replica_visibility(
    replica_addr: &str,
    query: &str,
    expected: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let outcome = runtime().block_on(async {
            let mut client = RedDBClient::connect(replica_addr, None).await?;
            client.query(query).await
        });
        if let Ok(body) = outcome {
            if body.contains(expected) {
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(format!("replica never returned expected value: {expected}").into())
}

fn grpc_endpoint(addr: &str) -> String {
    if addr.starts_with("http://") || addr.starts_with("https://") {
        addr.to_string()
    } else {
        format!("http://{addr}")
    }
}

fn wait_for_primary_query(
    primary_addr: &str,
    query: &str,
    expected: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        let outcome = runtime().block_on(async {
            let mut client = RedDBClient::connect(primary_addr, None).await?;
            client.query(query).await
        });
        if let Ok(body) = outcome {
            if body.contains(expected) {
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(format!("primary never returned expected value: {expected}").into())
}

#[test]
#[ignore = "requires a running docker/example environment"]
fn external_primary_health_and_query_round_trip() {
    let primary_addr = required_env("REDDB_TEST_PRIMARY_GRPC_ADDR");

    runtime().block_on(async {
        let mut client = RedDBClient::connect(&primary_addr, None)
            .await
            .expect("primary grpc connection should succeed");

        let health = client.health().await.expect("health should succeed");
        assert!(
            health.contains("healthy: true"),
            "unexpected health: {health}"
        );

        let created = client
            .create_row(
                "external_smoke_users",
                r#"{"fields":{"name":"external-alice","tier":"gold","age":31}}"#,
            )
            .await
            .expect("create_row should succeed");
        assert!(
            created.contains("id:"),
            "unexpected create_row response: {created}"
        );

        let query = client
            .query("SELECT * FROM external_smoke_users ORDER BY name")
            .await
            .expect("query should succeed");
        assert!(
            query.contains("external-alice"),
            "round-trip query should contain inserted row: {query}"
        );
    });
}

#[test]
#[ignore = "requires a running docker/example environment"]
fn external_replica_read_only_and_catches_up() {
    let Some(replica_addr) = env("REDDB_TEST_REPLICA_GRPC_ADDR") else {
        eprintln!("skipping replica-specific check: REDDB_TEST_REPLICA_GRPC_ADDR not set");
        return;
    };
    let primary_addr = required_env("REDDB_TEST_PRIMARY_GRPC_ADDR");

    runtime().block_on(async {
        let mut primary = RedDBClient::connect(&primary_addr, None)
            .await
            .expect("primary grpc connection should succeed");
        let seed = primary
            .create_row(
                "external_replica_users",
                r#"{"fields":{"name":"external-replica-user","role":"reader"}}"#,
            )
            .await
            .expect("primary write should succeed");
        assert!(seed.contains("id:"), "unexpected seed response: {seed}");
    });

    wait_for_replica_visibility(
        &replica_addr,
        "SELECT * FROM external_replica_users ORDER BY name",
        "external-replica-user",
    )
    .expect("replica should catch up");

    runtime().block_on(async {
        let mut replica = RedDBClient::connect(&replica_addr, None)
            .await
            .expect("replica grpc connection should succeed");

        let status = replica
            .replication_status()
            .await
            .expect("replication status should succeed");
        assert!(
            status.contains("replica"),
            "unexpected replica status: {status}"
        );

        let write_error = replica
            .create_row(
                "external_replica_users",
                r#"{"fields":{"name":"should-fail"}}"#,
            )
            .await
            .expect_err("replica write should fail");
        let rendered = write_error.to_string();
        assert!(
            rendered.contains("read-only")
                || rendered.contains("read only")
                || rendered.contains("permission")
                || rendered.contains("FAILED_PRECONDITION")
                || rendered.contains("PERMISSION_DENIED"),
            "unexpected replica write error: {rendered}"
        );
    });
}

#[test]
#[ignore = "requires full profile plus AI env configured inside the docker stack"]
fn external_ask_on_two_replicas_forwards_audit_and_cost_to_primary() {
    if test_profile() != "full" {
        eprintln!("skipping ASK cluster check: REDDB_TEST_PROFILE must be full");
        return;
    }

    let primary_addr = required_env("REDDB_TEST_PRIMARY_GRPC_ADDR");
    let replica_addr = required_env("REDDB_TEST_REPLICA_GRPC_ADDR");
    let secondary_replica_addr = required_env("REDDB_TEST_SECONDARY_REPLICA_GRPC_ADDR");
    let question_one = format!("replica audit sync {}", std::process::id());
    let question_two = format!("replica cost sync {}", std::process::id());
    let table = unique_table("external_ask_context");
    let context = format!(
        "deploy failed because checkout timed out {}",
        std::process::id()
    );

    runtime().block_on(async {
        let mut primary = RedDBClient::connect(&primary_addr, None)
            .await
            .expect("primary grpc connection should succeed");
        primary
            .query("SET CONFIG ask.daily_cost_cap_usd = 0.003000")
            .await
            .expect("set primary ASK daily cap");
        let create = format!("CREATE TABLE {table} (body TEXT)");
        primary
            .query(&create)
            .await
            .expect("create ASK context table");
        let insert = format!(
            "INSERT INTO {table} (body) VALUES ({})",
            sql_string(&context)
        );
        primary.query(&insert).await.expect("seed ASK context");
    });

    let select = format!("SELECT * FROM {table}");
    wait_for_replica_visibility(&replica_addr, &select, &context)
        .expect("replica-1 should catch up");
    wait_for_replica_visibility(&secondary_replica_addr, &select, &context)
        .expect("replica-2 should catch up");

    runtime().block_on(async {
        let mut replica =
            reddb::grpc::proto::red_db_client::RedDbClient::connect(grpc_endpoint(&replica_addr))
                .await
                .expect("replica-1 grpc connection should succeed");
        let reply = replica
            .ask(reddb::grpc::proto::AskRequest {
                question: question_one.clone(),
                provider: Some("openai".to_string()),
                model: Some("mock-chat".to_string()),
                depth: Some(1),
                limit: Some(1),
                min_score: None,
                collection: Some(table.clone()),
                temperature: Some(0.0),
                seed: Some(1),
                strict: Some(false),
            })
            .await
            .expect("ASK on replica-1 should succeed before cap is exhausted")
            .into_inner();
        assert_eq!(reply.answer, "mock response");
    });

    wait_for_primary_query(&primary_addr, "SELECT * FROM red_ask_audit", &question_one)
        .expect("primary audit row should be visible before the first ASK is considered complete");

    runtime().block_on(async {
        let mut replica = reddb::grpc::proto::red_db_client::RedDbClient::connect(grpc_endpoint(
            &secondary_replica_addr,
        ))
        .await
        .expect("replica-2 grpc connection should succeed");
        let err = replica
            .ask(reddb::grpc::proto::AskRequest {
                question: question_two,
                provider: Some("openai".to_string()),
                model: Some("mock-chat".to_string()),
                depth: Some(1),
                limit: Some(1),
                min_score: None,
                collection: Some(table),
                temperature: Some(0.0),
                seed: Some(2),
                strict: Some(false),
            })
            .await
            .expect_err("primary daily cost cap should be shared across replicas");
        let rendered = err.to_string();
        assert!(
            rendered.contains("RESOURCE_EXHAUSTED")
                || rendered.contains("daily_cost_cap_usd")
                || rendered.contains("quota"),
            "unexpected ASK over-cap error: {rendered}"
        );
    });
}

#[test]
#[ignore = "requires a fresh full profile and intentionally stops the primary container"]
fn external_ask_on_replica_primary_down_returns_503() {
    if test_profile() != "full" {
        eprintln!("skipping primary-down ASK check: REDDB_TEST_PROFILE must be full");
        return;
    }
    if env("REDDB_TEST_ASK_PRIMARY_DOWN_ENABLED").as_deref() != Some("1") {
        eprintln!("skipping primary-down ASK check: set REDDB_TEST_ASK_PRIMARY_DOWN_ENABLED=1");
        return;
    }

    let primary_addr = required_env("REDDB_TEST_PRIMARY_GRPC_ADDR");
    let replica_http = required_env("REDDB_TEST_REPLICA_HTTP_URL");
    let replica_addr = required_env("REDDB_TEST_REPLICA_GRPC_ADDR");
    let table = unique_table("external_ask_primary_down_context");
    let context = format!("primary down context is replicated {}", std::process::id());

    runtime().block_on(async {
        let mut primary = RedDBClient::connect(&primary_addr, None)
            .await
            .expect("primary grpc connection should succeed");
        let create = format!("CREATE TABLE {table} (body TEXT)");
        primary
            .query(&create)
            .await
            .expect("create primary-down ASK context table");
        let insert = format!(
            "INSERT INTO {table} (body) VALUES ({})",
            sql_string(&context)
        );
        primary
            .query(&insert)
            .await
            .expect("seed primary-down ASK context");
    });

    let select = format!("SELECT * FROM {table}");
    wait_for_replica_visibility(&replica_addr, &select, &context)
        .expect("replica should catch up before primary is stopped");

    let stop_status = std::process::Command::new("docker")
        .args([
            "compose",
            "-f",
            "testdata/compose/full.yml",
            "stop",
            "primary",
        ])
        .status()
        .expect("docker compose stop primary should run");
    assert!(stop_status.success(), "docker compose stop primary failed");

    let body = format!(
        "{{\"query\":\"ASK 'issue410 primary down' COLLECTION {} USING openai MODEL 'mock-chat' STRICT OFF LIMIT 1\"}}",
        table
    );
    let (status, response) = http_call("POST", &format!("{replica_http}/query"), Some(&body))
        .expect("replica HTTP ASK should respond");
    assert_eq!(status, 503, "unexpected primary-down ASK body: {response}");
    assert!(
        response.contains("ask_primary_sync_unavailable"),
        "unexpected primary-down ASK body: {response}"
    );
}

#[test]
#[ignore = "requires a running docker/example environment"]
fn external_http_control_plane_matches_profile() {
    let profile = test_profile();
    let primary_http = required_env("REDDB_TEST_PRIMARY_HTTP_URL");

    let (status, body) = http_call("GET", &format!("{primary_http}/health"), None)
        .expect("health endpoint should respond");
    assert_eq!(status, 200, "unexpected /health response: {body}");
    assert!(body.contains("healthy"), "unexpected /health body: {body}");

    let (status, body) = http_call("GET", &format!("{primary_http}/ready/query"), None)
        .expect("ready/query endpoint should respond");
    assert_eq!(status, 200, "unexpected /ready/query response: {body}");

    match profile.as_str() {
        "replica" | "full" | "remote" => {
            let (status, body) =
                http_call("GET", &format!("{primary_http}/replication/status"), None)
                    .expect("replication status should respond");
            assert_eq!(status, 200, "unexpected replication response: {body}");
            assert!(
                body.contains("primary"),
                "unexpected replication body: {body}"
            );
        }
        _ => {}
    }

    match profile.as_str() {
        "remote" | "backup" | "pitr" | "serverless" => {
            let (status, body) = http_call("GET", &format!("{primary_http}/backup/status"), None)
                .expect("backup status should respond");
            assert_eq!(status, 200, "unexpected backup status: {body}");
            assert!(
                body.contains("\"ok\":true"),
                "unexpected backup body: {body}"
            );
        }
        _ => {}
    }

    match profile.as_str() {
        "backup" | "pitr" => {
            let (status, body) = http_call(
                "POST",
                &format!("{primary_http}/backup/trigger"),
                Some("{}"),
            )
            .expect("backup trigger should respond");
            assert_eq!(status, 200, "unexpected backup trigger: {body}");
            assert!(
                body.contains("\"ok\":true"),
                "unexpected trigger body: {body}"
            );

            let (status, body) = http_call(
                "GET",
                &format!("{primary_http}/recovery/restore-points"),
                None,
            )
            .expect("restore points should respond");
            assert_eq!(status, 200, "unexpected restore points: {body}");
            assert!(
                body.contains("restore_points"),
                "unexpected restore body: {body}"
            );
        }
        "serverless" => {
            let (status, body) =
                http_call("GET", &format!("{primary_http}/ready/serverless"), None)
                    .expect("serverless readiness should respond");
            assert!(
                status == 200 || status == 503,
                "unexpected serverless readiness status {status}: {body}"
            );

            let (status, body) = http_call(
                "POST",
                &format!("{primary_http}/serverless/attach"),
                Some("{}"),
            )
            .expect("serverless attach should respond");
            assert_eq!(status, 200, "unexpected serverless attach: {body}");

            let (status, body) = http_call(
                "POST",
                &format!("{primary_http}/serverless/warmup"),
                Some(r#"{"dry_run":true}"#),
            )
            .expect("serverless warmup should respond");
            assert!(
                status == 200 || status == 503,
                "unexpected serverless warmup status {status}: {body}"
            );
        }
        _ => {}
    }
}
