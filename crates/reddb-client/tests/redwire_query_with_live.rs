#![cfg(feature = "redwire")]

use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use reddb_client::redwire::{Auth, ConnectOptions, RedWireClient};
use reddb_client::{Value, ValueOut};

#[tokio::test]
async fn redwire_query_with_params_against_live_server() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("RED_SMOKE").as_deref() != Ok("1") {
        eprintln!("skipping live RedWire smoke; set RED_SMOKE=1 and RED_BIN=/path/to/red");
        return Ok(());
    }
    let bin = match std::env::var("RED_BIN") {
        Ok(path) if std::path::Path::new(&path).exists() => path,
        _ => {
            eprintln!("skipping live RedWire smoke; RED_BIN is unset or missing");
            return Ok(());
        }
    };

    let port = pick_free_port()?;
    // Held for the whole test: the TempDir guard removes the scratch DB
    // directory on drop (incl. panic), after the server is stopped below.
    let data_dir = tempfile::Builder::new()
        .prefix("reddb-test-rust-redwire-")
        .tempdir()?;
    let data_path = data_dir.path().join("data.db");

    let mut server = Command::new(&bin)
        .arg("server")
        .arg("--path")
        .arg(&data_path)
        .arg("--bind")
        .arg(format!("127.0.0.1:{port}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    let test_result = run_live_query_with(port).await;
    stop_server(&mut server);
    drop(data_dir);
    test_result
}

async fn run_live_query_with(port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = wait_for_client(port).await?;

    client
        .query("CREATE TABLE rust_params (id INT, name TEXT)")
        .await?;
    let inserted = client
        .query_with(
            "INSERT INTO rust_params (id, name) VALUES ($1, $2)",
            &[Value::Int64(42), Value::Text("Ada".into())],
        )
        .await?;
    assert_eq!(inserted.affected, 1);

    let selected = client
        .query_with(
            "SELECT name FROM rust_params WHERE id = $1",
            &[Value::Int64(42)],
        )
        .await?;
    assert!(
        selected.rows.iter().any(|row| {
            row.iter()
                .any(|(column, value)| column == "name" && value == &ValueOut::String("Ada".into()))
        }),
        "expected selected row to contain Ada, got {selected:?}"
    );

    let _ = client.close().await;
    Ok(())
}

async fn wait_for_client(port: u16) -> Result<RedWireClient, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let opts = ConnectOptions::new("127.0.0.1", port).with_auth(Auth::Anonymous);
    let mut last_error = None;

    while Instant::now() < deadline {
        match RedWireClient::connect(opts.clone()).await {
            Ok(mut client) => match client.ping().await {
                Ok(()) => return Ok(client),
                Err(err) => {
                    last_error = Some(err.to_string());
                    let _ = client.close().await;
                }
            },
            Err(err) => last_error = Some(err.to_string()),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    Err(format!(
        "server did not accept RedWire connections: {}",
        last_error.unwrap_or_else(|| "timed out".into())
    )
    .into())
}

fn pick_free_port() -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

fn stop_server(server: &mut Child) {
    let _ = server.kill();
    let _ = server.wait();
}
