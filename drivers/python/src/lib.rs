use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use tokio::sync::Mutex;

pub mod proto {
    tonic::include_proto!("reddb.v1");
}

use proto::red_db_client::RedDbClient;
use proto::*;

/// Shared tokio runtime for all connections
fn get_runtime() -> &'static tokio::runtime::Runtime {
    use std::sync::OnceLock;
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
            .expect("Failed to create tokio runtime")
    })
}

type Client = RedDbClient<tonic::transport::Channel>;

/// Native RedDB connection — compiled Rust gRPC client (like psycopg2 for PostgreSQL).
#[pyclass]
struct Connection {
    client: Arc<Mutex<Client>>,
}

#[pymethods]
impl Connection {
    /// Execute a SQL query and return the result as a JSON string.
    fn query(&self, sql: &str) -> PyResult<String> {
        let client = self.client.clone();
        let sql = sql.to_string();
        get_runtime().block_on(async {
            let mut c = client.lock().await;
            let reply = c
                .query(QueryRequest {
                    query: sql,
                    ..Default::default()
                })
                .await
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            let r = reply.into_inner();
            Ok(r.result_json)
        })
    }

    /// Execute a SQL query, return (result_json, record_count).
    fn execute(&self, sql: &str) -> PyResult<(String, u64)> {
        let client = self.client.clone();
        let sql = sql.to_string();
        get_runtime().block_on(async {
            let mut c = client.lock().await;
            let reply = c
                .query(QueryRequest {
                    query: sql,
                    ..Default::default()
                })
                .await
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            let r = reply.into_inner();
            Ok((r.result_json, r.record_count))
        })
    }

    /// Bulk insert rows as JSON payloads.
    fn bulk_insert(&self, collection: &str, payloads: Vec<String>) -> PyResult<u64> {
        let client = self.client.clone();
        let collection = collection.to_string();
        get_runtime().block_on(async {
            let mut c = client.lock().await;
            let reply = c
                .bulk_create_rows(JsonBulkCreateRequest {
                    collection,
                    payload_json: payloads,
                })
                .await
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            Ok(reply.into_inner().count)
        })
    }

    /// Create a single row.
    fn create_row(&self, collection: &str, payload_json: &str) -> PyResult<u64> {
        let client = self.client.clone();
        let collection = collection.to_string();
        let payload = payload_json.to_string();
        get_runtime().block_on(async {
            let mut c = client.lock().await;
            let reply = c
                .create_row(JsonCreateRequest {
                    collection,
                    payload_json: payload,
                })
                .await
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            Ok(reply.into_inner().id)
        })
    }

    /// Execute multiple queries in a single gRPC round-trip.
    /// Returns a list of JSON result strings.
    fn batch_query(&self, queries: Vec<String>) -> PyResult<Vec<String>> {
        let client = self.client.clone();
        get_runtime().block_on(async {
            let mut c = client.lock().await;
            let reply = c
                .batch_query(BatchQueryRequest { queries })
                .await
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            Ok(reply
                .into_inner()
                .results
                .into_iter()
                .map(|r| r.result_json)
                .collect())
        })
    }

    /// Close the connection.
    fn close(&self) -> PyResult<()> {
        Ok(())
    }
}

/// Connect to a RedDB server. Returns a native Connection object.
///
/// Usage:
///   import reddb_python
///   conn = reddb_python.connect("127.0.0.1:50051")
///   result = conn.query("SELECT * FROM users WHERE _entity_id = 1")
///   conn.bulk_insert("users", ['{"fields":{"name":"Alice"}}'])
#[pyfunction]
fn connect(addr: &str) -> PyResult<Connection> {
    let addr = if addr.starts_with("http") {
        addr.to_string()
    } else {
        format!("http://{addr}")
    };
    let client = get_runtime().block_on(async {
        let channel = tonic::transport::Endpoint::from_shared(addr)
            .map_err(|e| PyRuntimeError::new_err(format!("invalid addr: {e}")))?
            .connect()
            .await
            .map_err(|e| PyRuntimeError::new_err(format!("connect failed: {e}")))?;
        Ok::<_, PyErr>(
            RedDbClient::new(channel)
                .max_decoding_message_size(256 * 1024 * 1024)
                .max_encoding_message_size(256 * 1024 * 1024),
        )
    })?;
    Ok(Connection {
        client: Arc::new(Mutex::new(client)),
    })
}

// ═══════════════════════════════════════════════════════════════
// Wire Protocol Connection — raw TCP, zero overhead
// ═══════════════════════════════════════════════════════════════

const WIRE_MSG_QUERY: u8 = 0x01;
const WIRE_MSG_RESULT: u8 = 0x02;
const WIRE_MSG_ERROR: u8 = 0x03;
const WIRE_MSG_BULK_INSERT: u8 = 0x04;
const WIRE_MSG_BULK_OK: u8 = 0x05;

/// Abstraction over plain TCP and TLS streams.
enum TlsOrPlain {
    Plain(TcpStream),
    Tls(rustls::StreamOwned<rustls::ClientConnection, TcpStream>),
}

impl Read for TlsOrPlain {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            TlsOrPlain::Plain(s) => s.read(buf),
            TlsOrPlain::Tls(s) => s.read(buf),
        }
    }
}

impl Write for TlsOrPlain {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            TlsOrPlain::Plain(s) => s.write(buf),
            TlsOrPlain::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            TlsOrPlain::Plain(s) => s.flush(),
            TlsOrPlain::Tls(s) => s.flush(),
        }
    }
}

/// Wire protocol connection — raw TCP binary, zero JSON/protobuf overhead.
/// Supports both plaintext and TLS-encrypted connections.
#[pyclass]
struct WireConnection {
    stream: std::sync::Mutex<TlsOrPlain>,
}

impl WireConnection {
    fn send_and_recv(
        stream: &mut TlsOrPlain,
        msg_type: u8,
        payload: &[u8],
    ) -> Result<(u8, Vec<u8>), String> {
        // Send: [total_len:u32 LE][msg_type:u8][payload...]
        let total_len = (1 + payload.len()) as u32;
        stream
            .write_all(&total_len.to_le_bytes())
            .map_err(|e| e.to_string())?;
        stream.write_all(&[msg_type]).map_err(|e| e.to_string())?;
        stream.write_all(payload).map_err(|e| e.to_string())?;
        stream.flush().map_err(|e| e.to_string())?;

        // Recv: [total_len:u32 LE][msg_type:u8][payload...]
        let mut header = [0u8; 5];
        stream.read_exact(&mut header).map_err(|e| e.to_string())?;
        let resp_len = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
        let resp_type = header[4];
        let payload_len = resp_len.saturating_sub(1);
        let mut resp_payload = vec![0u8; payload_len];
        if payload_len > 0 {
            stream
                .read_exact(&mut resp_payload)
                .map_err(|e| e.to_string())?;
        }
        Ok((resp_type, resp_payload))
    }
}

#[pymethods]
impl WireConnection {
    /// Execute a SQL query. Returns the result JSON string.
    fn query(&self, sql: &str) -> PyResult<String> {
        let mut stream = self
            .stream
            .lock()
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        let (resp_type, payload) = Self::send_and_recv(&mut stream, WIRE_MSG_QUERY, sql.as_bytes())
            .map_err(|e| PyRuntimeError::new_err(e))?;

        if resp_type == WIRE_MSG_ERROR {
            let msg = String::from_utf8_lossy(&payload);
            return Err(PyRuntimeError::new_err(msg.to_string()));
        }

        // Result is JSON string (wire protocol sends pre-serialized JSON)
        Ok(String::from_utf8_lossy(&payload).to_string())
    }

    /// Execute a SQL query, return raw bytes (for benchmarking — skip Python dict creation).
    fn query_raw(&self, sql: &str) -> PyResult<usize> {
        let mut stream = self
            .stream
            .lock()
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        let (resp_type, payload) = Self::send_and_recv(&mut stream, WIRE_MSG_QUERY, sql.as_bytes())
            .map_err(|e| PyRuntimeError::new_err(e))?;
        if resp_type == WIRE_MSG_ERROR {
            return Err(PyRuntimeError::new_err(
                String::from_utf8_lossy(&payload).to_string(),
            ));
        }
        // Return number of bytes received (skip decoding for max throughput measurement)
        Ok(payload.len())
    }

    /// Execute a SQL query with BINARY result encoding (zero JSON).
    /// Returns number of bytes received (for benchmarking).
    fn query_binary_raw(&self, sql: &str) -> PyResult<usize> {
        let mut stream = self
            .stream
            .lock()
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        let (resp_type, payload) = Self::send_and_recv(&mut stream, 0x07, sql.as_bytes())
            .map_err(|e| PyRuntimeError::new_err(e))?;
        if resp_type == 0x03 {
            return Err(PyRuntimeError::new_err(
                String::from_utf8_lossy(&payload).to_string(),
            ));
        }
        Ok(payload.len())
    }

    /// Bulk insert rows.
    fn bulk_insert(&self, collection: &str, payloads: Vec<String>) -> PyResult<u64> {
        let mut buf = Vec::with_capacity(collection.len() + 6 + payloads.len() * 256);
        // [coll_len:u16][coll_bytes][n:u32][json_len:u32 + json_bytes]...
        buf.extend_from_slice(&(collection.len() as u16).to_le_bytes());
        buf.extend_from_slice(collection.as_bytes());
        buf.extend_from_slice(&(payloads.len() as u32).to_le_bytes());
        for p in &payloads {
            buf.extend_from_slice(&(p.len() as u32).to_le_bytes());
            buf.extend_from_slice(p.as_bytes());
        }

        let mut stream = self
            .stream
            .lock()
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        let (resp_type, payload) = Self::send_and_recv(&mut stream, WIRE_MSG_BULK_INSERT, &buf)
            .map_err(|e| PyRuntimeError::new_err(e))?;

        if resp_type == WIRE_MSG_ERROR {
            return Err(PyRuntimeError::new_err(
                String::from_utf8_lossy(&payload).to_string(),
            ));
        }
        if payload.len() >= 8 {
            Ok(u64::from_le_bytes(payload[0..8].try_into().unwrap()))
        } else {
            Ok(0)
        }
    }

    /// Binary bulk insert — sends typed values directly, zero JSON parsing on server.
    /// columns: list of column names
    /// rows: list of tuples/lists of values (str, int, float, bool, None)
    fn bulk_insert_binary(
        &self,
        collection: &str,
        columns: Vec<String>,
        rows: Vec<Vec<PyObject>>,
    ) -> PyResult<u64> {
        Python::with_gil(|py| {
            let ncols = columns.len();
            let nrows = rows.len();
            let mut buf =
                Vec::with_capacity(collection.len() + 6 + ncols * 20 + nrows * ncols * 12);

            // [coll_len:u16][coll][ncols:u16][col_names...][nrows:u32]
            buf.extend_from_slice(&(collection.len() as u16).to_le_bytes());
            buf.extend_from_slice(collection.as_bytes());
            buf.extend_from_slice(&(ncols as u16).to_le_bytes());
            for col in &columns {
                buf.extend_from_slice(&(col.len() as u16).to_le_bytes());
                buf.extend_from_slice(col.as_bytes());
            }
            buf.extend_from_slice(&(nrows as u32).to_le_bytes());

            for row in &rows {
                for val in row {
                    if val.is_none(py) {
                        buf.push(0); // NULL
                    } else if let Ok(v) = val.extract::<bool>(py) {
                        buf.push(4);
                        buf.push(v as u8);
                    } else if let Ok(v) = val.extract::<i64>(py) {
                        buf.push(1);
                        buf.extend_from_slice(&v.to_le_bytes());
                    } else if let Ok(v) = val.extract::<f64>(py) {
                        buf.push(2);
                        buf.extend_from_slice(&v.to_le_bytes());
                    } else if let Ok(v) = val.extract::<String>(py) {
                        buf.push(3);
                        buf.extend_from_slice(&(v.len() as u32).to_le_bytes());
                        buf.extend_from_slice(v.as_bytes());
                    } else {
                        buf.push(0);
                    }
                }
            }

            let mut stream = self
                .stream
                .lock()
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
            let (resp_type, payload) = Self::send_and_recv(&mut stream, 0x06, &buf)
                .map_err(|e| PyRuntimeError::new_err(e))?;

            if resp_type == 0x03 {
                return Err(PyRuntimeError::new_err(
                    String::from_utf8_lossy(&payload).to_string(),
                ));
            }
            if payload.len() >= 8 {
                Ok(u64::from_le_bytes(payload[0..8].try_into().unwrap()))
            } else {
                Ok(0)
            }
        })
    }

    fn close(&self) -> PyResult<()> {
        Ok(())
    }
}

fn decode_wire_result_to_py(py: Python<'_>, data: &[u8]) -> PyResult<PyObject> {
    use pyo3::types::{PyDict, PyList};

    if data.len() < 2 {
        return Ok(PyList::empty(py).into());
    }

    let mut pos = 0;

    // ncols
    let ncols = u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap()) as usize;
    pos += 2;

    // Column names
    let mut col_names = Vec::with_capacity(ncols);
    for _ in 0..ncols {
        let name_len = u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap()) as usize;
        pos += 2;
        let name = String::from_utf8_lossy(&data[pos..pos + name_len]).to_string();
        pos += name_len;
        col_names.push(name);
    }

    // nrows
    let nrows = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
    pos += 4;

    // Rows
    let rows = PyList::empty(py);
    for _ in 0..nrows {
        let dict = PyDict::new(py);
        for col in &col_names {
            if pos >= data.len() {
                break;
            }
            let tag = data[pos];
            pos += 1;
            match tag {
                0 => {
                    dict.set_item(col, py.None())?;
                }
                1 => {
                    // i64
                    let v = i64::from_le_bytes(data[pos..pos + 8].try_into().unwrap_or([0; 8]));
                    pos += 8;
                    dict.set_item(col, v)?;
                }
                2 => {
                    // f64
                    let v = f64::from_le_bytes(data[pos..pos + 8].try_into().unwrap_or([0; 8]));
                    pos += 8;
                    dict.set_item(col, v)?;
                }
                3 => {
                    // text
                    let len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap_or([0; 4]))
                        as usize;
                    pos += 4;
                    let s = String::from_utf8_lossy(&data[pos..pos + len]);
                    pos += len;
                    dict.set_item(col, s.as_ref())?;
                }
                4 => {
                    // bool
                    let v = data[pos] != 0;
                    pos += 1;
                    dict.set_item(col, v)?;
                }
                5 => {
                    // u64
                    let v = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap_or([0; 8]));
                    pos += 8;
                    dict.set_item(col, v)?;
                }
                _ => {
                    dict.set_item(col, py.None())?;
                }
            }
        }
        rows.append(dict)?;
    }

    Ok(rows.into())
}

/// Connect to RedDB wire protocol (raw TCP). Ultra-fast, zero overhead.
///
/// Usage:
///   conn = reddb_python.wire_connect("127.0.0.1:5050")
///   rows = conn.query("SELECT * FROM users WHERE _entity_id = 1")
#[pyfunction]
fn wire_connect(addr: &str) -> PyResult<WireConnection> {
    let stream = TcpStream::connect(addr)
        .map_err(|e| PyRuntimeError::new_err(format!("wire connect failed: {e}")))?;
    stream
        .set_nodelay(true)
        .map_err(|e| PyRuntimeError::new_err(format!("set_nodelay: {e}")))?;
    Ok(WireConnection {
        stream: std::sync::Mutex::new(TlsOrPlain::Plain(stream)),
    })
}

/// Connect to RedDB wire protocol with TLS encryption.
///
/// Usage:
///   conn = reddb_python.wire_connect_tls("127.0.0.1:50053")
///   conn = reddb_python.wire_connect_tls("127.0.0.1:50053", ca_cert="/path/to/cert.pem")
#[pyfunction]
#[pyo3(signature = (addr, ca_cert=None, accept_invalid_certs=false))]
fn wire_connect_tls(
    addr: &str,
    ca_cert: Option<&str>,
    accept_invalid_certs: bool,
) -> PyResult<WireConnection> {
    use rustls::ClientConfig;
    use std::io::BufReader;

    let mut root_store = rustls::RootCertStore::empty();

    // Add system root certs
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    // Add custom CA cert if provided
    if let Some(ca_path) = ca_cert {
        let ca_pem = std::fs::read(ca_path)
            .map_err(|e| PyRuntimeError::new_err(format!("read CA cert: {e}")))?;
        let certs = rustls_pemfile::certs(&mut BufReader::new(&ca_pem[..]))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| PyRuntimeError::new_err(format!("parse CA cert: {e}")))?;
        for cert in certs {
            root_store
                .add(cert)
                .map_err(|e| PyRuntimeError::new_err(format!("add CA cert: {e}")))?;
        }
    }

    let config = if accept_invalid_certs {
        // Dev mode: accept self-signed certs
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(std::sync::Arc::new(DangerousVerifier))
            .with_no_client_auth()
    } else {
        ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth()
    };

    let connector = std::sync::Arc::new(config);

    // Parse host:port
    let (host, _port) = addr
        .rsplit_once(':')
        .ok_or_else(|| PyRuntimeError::new_err("addr must be host:port"))?;
    let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|e| PyRuntimeError::new_err(format!("invalid server name: {e}")))?;

    let tcp =
        TcpStream::connect(addr).map_err(|e| PyRuntimeError::new_err(format!("connect: {e}")))?;
    tcp.set_nodelay(true)
        .map_err(|e| PyRuntimeError::new_err(format!("nodelay: {e}")))?;

    let conn = rustls::ClientConnection::new(connector, server_name)
        .map_err(|e| PyRuntimeError::new_err(format!("TLS init: {e}")))?;
    let tls_stream = rustls::StreamOwned::new(conn, tcp);

    Ok(WireConnection {
        stream: std::sync::Mutex::new(TlsOrPlain::Tls(tls_stream)),
    })
}

/// Dangerous certificate verifier that accepts any cert (dev mode only).
#[derive(Debug)]
struct DangerousVerifier;

impl rustls::client::danger::ServerCertVerifier for DangerousVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Python module
#[pymodule]
fn reddb_python(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(connect, m)?)?;
    m.add_function(wrap_pyfunction!(wire_connect, m)?)?;
    m.add_function(wrap_pyfunction!(wire_connect_tls, m)?)?;
    m.add_class::<Connection>()?;
    m.add_class::<WireConnection>()?;
    Ok(())
}
