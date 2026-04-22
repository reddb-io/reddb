use std::time::{Duration, Instant};

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
