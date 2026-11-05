//! Private local workspace service for managed desktop hosts. Never a user SQL
//! endpoint: the only operations are metadata batches and explicit Vault access.
use super::{vault::LocalVaultKey, LocalDatabase};
use reddb_file::vfs::StdFile;
use reddb_file::{EmbeddedRdbWriterLock, OpenMode, StdVfs, Vfs, VfsFile};
use reddb_types::json::Value as Json;
use reddb_types::Value;
use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use zeroize::Zeroizing;

type Result<T> = std::result::Result<T, String>;
const REQUEST_BYTES_MAX: usize = 2 * 1024 * 1024;

/// `workspace-store --path <private.rdb>` binds an ephemeral loopback port.
/// A single bounded JSON bootstrap line arrives on stdin. The host keeps stdin
/// open as a lifeline and reads the private port/token handshake from stdout.
pub fn run(arguments: &[String]) -> Result<()> {
    let [flag, path] = arguments else {
        return Err("expected --path <private.rdb>".into());
    };
    if flag != "--path" {
        return Err("expected --path <private.rdb>".into());
    }
    let mut bootstrap = Zeroizing::new(String::new());
    std::io::stdin()
        .lock()
        .take(8192)
        .read_line(&mut bootstrap)
        .map_err(error)?;
    if !bootstrap.ends_with('\n') {
        return Err("invalid bootstrap frame".into());
    }
    let frame: Json = reddb_types::serde_json::from_str(&bootstrap).map_err(error)?;
    drop(bootstrap);
    let password = match frame {
        Json::Object(mut object) => match object.remove("password") {
            Some(Json::String(password)) => Some(Zeroizing::new(password)),
            Some(Json::Null) | None => None,
            _ => return Err("invalid opening credential".into()),
        },
        _ => return Err("invalid bootstrap frame".into()),
    };
    let path = Path::new(path);
    let _owner = EmbeddedRdbWriterLock::acquire(path).map_err(error)?;
    let existed = path.exists();
    let mut mode = if existed {
        OpenMode::create_keep()
    } else {
        OpenMode::create_new()
    };
    mode.read = true;
    let file = StdVfs.open(path, mode).map_err(error)?;
    let database = if existed {
        LocalDatabase::open(file, path)
    } else {
        LocalDatabase::create(file, path, 0)
    }
    .map_err(error)?;
    let mut service = Service {
        database,
        key: None,
    };
    service.database.query("CREATE TABLE IF NOT EXISTS workspace_metadata (entry_key TEXT PRIMARY KEY, entry_value TEXT NOT NULL)", 0).map_err(error)?;
    if let Some(password) = &password {
        // A failed unlock leaves metadata accessible and never recreates a Vault.
        let create = !service.database.vault_initialized().map_err(error)?;
        service.key = service.database.vault_unlock(password, create).ok();
    }
    drop(password);
    let listener = TcpListener::bind("127.0.0.1:0").map_err(error)?;
    listener.set_nonblocking(true).map_err(error)?;
    let token = private_token()?;
    println!("{}", reddb_types::json!({ "protocol": 1, "port": listener.local_addr().map_err(error)?.port(), "token": token.clone() }).to_string_compact());
    std::io::stdout().flush().map_err(error)?;
    let alive = Arc::new(AtomicBool::new(true));
    let lifeline = alive.clone();
    std::thread::spawn(move || {
        let mut sink = [0u8; 1];
        while std::io::stdin()
            .read(&mut sink)
            .is_ok_and(|count| count > 0)
        {}
        lifeline.store(false, Ordering::Release);
    });
    while alive.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let request = read_request(&mut stream, &token);
                let (response, stop) = match request {
                    Ok(request) => {
                        let stop = request.get("method").and_then(Json::as_str) == Some("shutdown");
                        let result = service.execute(&request);
                        (
                            match result {
                                Ok(value) => reddb_types::json!({ "ok": true, "result": value }),
                                Err(message) => {
                                    reddb_types::json!({ "ok": false, "error": message })
                                }
                            },
                            stop,
                        )
                    }
                    Err(_) => {
                        let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        continue;
                    }
                };
                let body = response.to_string_compact();
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                if stop {
                    break;
                }
            }
            Err(failure) if failure.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(failure) => return Err(error(failure)),
        }
    }
    service.database.into_inner().sync_all().map_err(error)
}

fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn private_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    // The engine owns browser/native Vault entropy too; use the same provider.
    reddb_crypto::os_random::fill_bytes(&mut bytes).map_err(error)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn read_request(stream: &mut TcpStream, token: &str) -> Result<Json> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(error)?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(error)?;
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") && header.len() < 8192 {
        let mut byte = [0];
        stream.read_exact(&mut byte).map_err(error)?;
        header.push(byte[0]);
    }
    let header = std::str::from_utf8(&header).map_err(error)?;
    let mut lines = header.split("\r\n");
    if lines.next() != Some("POST /workspace HTTP/1.1") {
        return Err("invalid endpoint".into());
    }
    let mut length = None;
    let mut authenticated = false;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or("invalid header")?;
        let name = name.to_ascii_lowercase();
        let value = value.trim();
        match name.as_str() {
            "origin" | "transfer-encoding" => return Err("browser requests are refused".into()),
            "authorization" => {
                let expected = format!("Bearer {token}");
                authenticated = value.len() == expected.len()
                    && value
                        .bytes()
                        .zip(expected.bytes())
                        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
                        == 0;
            }
            "content-length" => {
                if length.is_some() {
                    return Err("duplicate length".into());
                }
                length = Some(value.parse::<usize>().map_err(error)?);
            }
            _ => {}
        }
    }
    let length = length
        .filter(|length| *length > 0 && *length <= REQUEST_BYTES_MAX)
        .ok_or("invalid request size")?;
    if !authenticated {
        return Err("unauthenticated".into());
    }
    let mut body = Zeroizing::new(vec![0; length]);
    stream.read_exact(&mut body).map_err(error)?;
    reddb_types::serde_json::from_slice(&body).map_err(error)
}

struct Service {
    database: LocalDatabase<StdFile>,
    key: Option<LocalVaultKey>,
}

impl Service {
    fn execute(&mut self, request: &Json) -> Result<Json> {
        let params = request.get("params").unwrap_or(&Json::Null);
        let name = || {
            params
                .get("name")
                .and_then(Json::as_str)
                .ok_or_else(|| "missing Vault key".to_string())
        };
        match request
            .get("method")
            .and_then(Json::as_str)
            .ok_or("missing method")?
        {
            "read" => {
                let result = self
                    .database
                    .query("SELECT entry_key, entry_value FROM workspace_metadata", 0)
                    .map_err(error)?;
                let mut object = reddb_types::json::Map::new();
                for row in result.rows {
                    let key = row[0].as_text().ok_or("invalid metadata key")?;
                    let value = row[1].as_text().ok_or("invalid metadata value")?;
                    object.insert(key.into(), Json::String(value.into()));
                }
                Ok(Json::Object(object))
            }
            "commit" => self.commit(params),
            "vaultStatus" => Ok(
                reddb_types::json!({ "initialized": self.database.vault_initialized().map_err(error)?, "locked": self.key.is_none() }),
            ),
            "vaultUnlock" => {
                self.key = None;
                let password = params
                    .get("password")
                    .and_then(Json::as_str)
                    .ok_or("missing opening credential")?;
                let create = params
                    .get("create")
                    .and_then(Json::as_bool)
                    .unwrap_or(false);
                self.key = Some(
                    self.database
                        .vault_unlock(password, create)
                        .map_err(error)?,
                );
                Ok(Json::Null)
            }
            "vaultLock" => {
                self.key = None;
                Ok(Json::Null)
            }
            "vaultKeys" => Ok(Json::Array(
                self.database
                    .vault_keys(self.key.as_ref().ok_or("VAULT_LOCKED")?)
                    .map_err(error)?
                    .into_iter()
                    .map(Json::String)
                    .collect(),
            )),
            "vaultReveal" => Ok(self
                .database
                .vault_reveal(self.key.as_ref().ok_or("VAULT_LOCKED")?, name()?)
                .map_err(error)?
                .map(Json::String)
                .unwrap_or(Json::Null)),
            "shutdown" => Ok(Json::Null),
            _ => Err("unsupported workspace operation".into()),
        }
    }

    fn commit(&mut self, params: &Json) -> Result<Json> {
        self.database.query("BEGIN", 0).map_err(error)?;
        let result = self.commit_inner(params);
        if result.is_err() {
            let _ = self.database.query("ROLLBACK", 0);
        }
        result
    }

    fn commit_inner(&mut self, params: &Json) -> Result<Json> {
        let metadata = params
            .get("metadata")
            .and_then(Json::as_object)
            .ok_or("missing metadata batch")?;
        let if_absent = params
            .get("ifAbsent")
            .and_then(Json::as_bool)
            .unwrap_or(false);
        for (key, value) in metadata {
            if key.len() > 2048 {
                return Err("metadata key too long".into());
            }
            if if_absent
                && !self
                    .database
                    .query_with_params(
                        "SELECT entry_key FROM workspace_metadata WHERE entry_key = $1",
                        &[Value::text(key.as_str())],
                        0,
                    )
                    .map_err(error)?
                    .rows
                    .is_empty()
            {
                continue;
            }
            self.database
                .query_with_params(
                    "DELETE FROM workspace_metadata WHERE entry_key = $1",
                    &[Value::text(key.as_str())],
                    0,
                )
                .map_err(error)?;
            if !matches!(value, Json::Null) {
                let value = value.as_str().ok_or("invalid metadata value")?;
                self.database
                    .query_with_params(
                        "INSERT INTO workspace_metadata VALUES ($1, $2)",
                        &[Value::text(key.as_str()), Value::text(value)],
                        0,
                    )
                    .map_err(error)?;
            }
        }
        if let Some(secrets) = params.get("secrets").and_then(Json::as_object) {
            for (name, value) in secrets {
                if if_absent
                    && self
                        .database
                        .vault_reveal(self.key.as_ref().ok_or("VAULT_LOCKED")?, name)
                        .map_err(error)?
                        .is_some()
                {
                    continue;
                }
                let value = if matches!(value, Json::Null) {
                    None
                } else {
                    Some(value.as_str().ok_or("invalid secret value")?)
                };
                self.database
                    .vault_put(self.key.as_ref().ok_or("VAULT_LOCKED")?, name, value)
                    .map_err(error)?;
            }
        }
        self.database
            .query_with_params(
                "DELETE FROM workspace_metadata WHERE entry_key = $1",
                &[Value::text("__workspace_migration_v1")],
                0,
            )
            .map_err(error)?;
        self.database
            .query_with_params(
                "INSERT INTO workspace_metadata VALUES ($1, $2)",
                &[
                    Value::text("__workspace_migration_v1"),
                    Value::text("complete"),
                ],
                0,
            )
            .map_err(error)?;
        self.database.query("COMMIT", 0).map_err(error)?;
        Ok(Json::Null)
    }
}
