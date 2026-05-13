//! High-level Python API: `reddb.connect(uri) -> RedDb`.
//!
//! Mirrors the JS and Rust drivers. Same connection-string contract,
//! same method names, same error semantics. See `PLAN_DRIVERS.md`.

use std::sync::{Arc, Mutex};

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyDict, PyList, PyTuple};

#[cfg(feature = "embedded")]
use crate::embedded::{EmbeddedRuntime, ParamValue, QueryRows, ScalarOut};
#[cfg(feature = "embedded")]
use reddb::runtime::RedDBRuntime;
#[cfg(feature = "embedded")]
use reddb::storage::cache::{BlobCachePolicy, BlobCachePut};
#[cfg(feature = "embedded")]
use reddb::storage::cache::blob::CachePresence;

use reddb::client::RedDBClient;

// -----------------------------------------------------------------------
// Error type
// -----------------------------------------------------------------------

/// Stable error class raised by the high-level driver. The `code`
/// attribute matches the JSON-RPC error codes used by `red rpc --stdio`.
#[pyclass(extends=pyo3::exceptions::PyException)]
pub struct RedDbError {
    #[pyo3(get)]
    pub code: String,
    #[pyo3(get)]
    pub message: String,
}

#[pymethods]
impl RedDbError {
    #[new]
    fn new(code: String, message: String) -> Self {
        Self { code, message }
    }

    fn __str__(&self) -> String {
        format!("[{}] {}", self.code, self.message)
    }

    fn __repr__(&self) -> String {
        format!(
            "RedDbError(code={:?}, message={:?})",
            self.code, self.message
        )
    }
}

fn err(code: &str, msg: impl Into<String>) -> PyErr {
    let msg = msg.into();
    PyErr::new::<PyValueError, _>(format!("[{code}] {msg}"))
}

// -----------------------------------------------------------------------
// Backend dispatch
// -----------------------------------------------------------------------

enum Backend {
    #[cfg(feature = "embedded")]
    Embedded(EmbeddedRuntime),
    Grpc(Mutex<RedDBClient>),
}

#[pyclass]
pub struct RedDb {
    backend: Backend,
    closed: bool,
}

#[pymethods]
impl RedDb {
    /// Run a SQL query and return a result dict:
    /// `{"statement": str, "affected": int, "columns": [str], "rows": [dict]}`.
    ///
    /// Positional `$N` bind parameters can be passed either variadically
    /// (`db.query("SELECT * FROM t WHERE id = $1", 42)`) or via the
    /// `params=` keyword (`db.query("...", params=[42, "x"])`). When both
    /// forms are supplied, the keyword form wins.
    #[pyo3(signature = (sql, *args, params=None))]
    fn query<'py>(
        &self,
        py: Python<'py>,
        sql: &str,
        args: &Bound<'py, PyTuple>,
        params: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Bound<'py, PyDict>> {
        self.ensure_open()?;

        let binds = collect_params(args, params)?;

        match &self.backend {
            #[cfg(feature = "embedded")]
            Backend::Embedded(rt) => {
                let qr = if binds.is_empty() {
                    rt.query(sql).map_err(|e| err("QUERY_ERROR", e))?
                } else {
                    let values: Vec<ParamValue> = binds
                        .iter()
                        .map(|b| py_to_param_value(b))
                        .collect::<PyResult<Vec<_>>>()?;
                    rt.query_with_params(sql, &values)
                        .map_err(|e| err("QUERY_ERROR", e))?
                };
                query_rows_to_pydict(py, qr)
            }
            Backend::Grpc(client) => {
                if !binds.is_empty() {
                    return Err(err(
                        "PARAMS_UNSUPPORTED",
                        "parameterized queries over grpc:// are not supported yet \
                         (the gRPC server does not advertise FEATURE_PARAMS). \
                         Use file:// or memory:// for now.",
                    ));
                }
                let json_str = crate::get_runtime()
                    .block_on(async {
                        let mut guard = client.lock().expect("client poisoned");
                        guard.query(sql).await
                    })
                    .map_err(|e| err("QUERY_ERROR", e.to_string()))?;
                grpc_query_json_to_pydict(py, &json_str)
            }
        }
    }

    /// Insert one row. `payload` must be a `dict[str, scalar]`.
    fn insert<'py>(
        &self,
        py: Python<'py>,
        collection: &str,
        payload: &Bound<'py, PyDict>,
    ) -> PyResult<Bound<'py, PyDict>> {
        self.ensure_open()?;
        match &self.backend {
            #[cfg(feature = "embedded")]
            Backend::Embedded(rt) => {
                let fields = pydict_to_fields(payload)?;
                let result = rt
                    .insert_object(collection, &fields)
                    .map_err(|e| err("QUERY_ERROR", e))?;
                let out = PyDict::new(py);
                out.set_item("affected", result.affected)?;
                if let Some(id) = result.id {
                    out.set_item("id", id)?;
                }
                Ok(out)
            }
            Backend::Grpc(client) => {
                let json_payload = pydict_to_json_str(payload)?;
                let reply = crate::get_runtime()
                    .block_on(async {
                        let mut guard = client.lock().expect("client poisoned");
                        guard.create_row_entity(collection, &json_payload).await
                    })
                    .map_err(|e| err("QUERY_ERROR", e.to_string()))?;
                let out = PyDict::new(py);
                out.set_item("affected", 1u64)?;
                out.set_item("id", reply.id.to_string())?;
                Ok(out)
            }
        }
    }

    /// Insert many rows in one call. `payloads` is a list of dicts.
    fn bulk_insert<'py>(
        &self,
        py: Python<'py>,
        collection: &str,
        payloads: &Bound<'py, PyList>,
    ) -> PyResult<Bound<'py, PyDict>> {
        self.ensure_open()?;
        match &self.backend {
            #[cfg(feature = "embedded")]
            Backend::Embedded(rt) => {
                let mut total: u64 = 0;
                let mut ids = Vec::with_capacity(payloads.len());
                for item in payloads.iter() {
                    let dict = item
                        .downcast::<PyDict>()
                        .map_err(|_| err("INVALID_PARAMS", "bulk_insert payloads must be dicts"))?;
                    let fields = pydict_to_fields(dict)?;
                    let result = rt
                        .insert_object(collection, &fields)
                        .map_err(|e| err("QUERY_ERROR", e))?;
                    total += result.affected;
                    if let Some(id) = result.id {
                        ids.push(id);
                    }
                }
                let out = PyDict::new(py);
                out.set_item("affected", total)?;
                out.set_item("ids", ids)?;
                Ok(out)
            }
            Backend::Grpc(client) => {
                let mut encoded = Vec::with_capacity(payloads.len());
                for item in payloads.iter() {
                    let dict = item
                        .downcast::<PyDict>()
                        .map_err(|_| err("INVALID_PARAMS", "bulk_insert payloads must be dicts"))?;
                    encoded.push(pydict_to_json_str(dict)?);
                }
                let reply = crate::get_runtime()
                    .block_on(async {
                        let mut guard = client.lock().expect("client poisoned");
                        guard.bulk_create_rows(collection, encoded).await
                    })
                    .map_err(|e| err("QUERY_ERROR", e.to_string()))?;
                let out = PyDict::new(py);
                out.set_item("affected", reply.count)?;
                let ids: Vec<String> = reply.ids.into_iter().map(|id| id.to_string()).collect();
                out.set_item("ids", ids)?;
                Ok(out)
            }
        }
    }

    /// Delete an entity by id. Returns `{"affected": int}`.
    fn delete<'py>(
        &self,
        py: Python<'py>,
        collection: &str,
        id: &str,
    ) -> PyResult<Bound<'py, PyDict>> {
        self.ensure_open()?;
        match &self.backend {
            #[cfg(feature = "embedded")]
            Backend::Embedded(rt) => {
                let n = rt
                    .delete(collection, id)
                    .map_err(|e| err("QUERY_ERROR", e))?;
                let out = PyDict::new(py);
                out.set_item("affected", n)?;
                Ok(out)
            }
            Backend::Grpc(client) => {
                let id = id
                    .parse::<u64>()
                    .map_err(|_| err("INVALID_PARAMS", "id must be a numeric string"))?;
                crate::get_runtime()
                    .block_on(async {
                        let mut guard = client.lock().expect("client poisoned");
                        guard.delete_entity(collection, id).await
                    })
                    .map_err(|e| err("QUERY_ERROR", e.to_string()))?;
                let out = PyDict::new(py);
                out.set_item("affected", 1u64)?;
                Ok(out)
            }
        }
    }

    fn health<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        self.ensure_open()?;
        let out = PyDict::new(py);
        match &self.backend {
            #[cfg(feature = "embedded")]
            Backend::Embedded(_) => {
                out.set_item("ok", true)?;
                out.set_item("version", env!("CARGO_PKG_VERSION"))?;
            }
            Backend::Grpc(client) => {
                let health = crate::get_runtime()
                    .block_on(async {
                        let mut guard = client.lock().expect("client poisoned");
                        guard.health_status().await
                    })
                    .map_err(|e| err("QUERY_ERROR", e.to_string()))?;
                out.set_item("ok", health.healthy)?;
                out.set_item("state", health.state)?;
                out.set_item("checked_at_unix_ms", health.checked_at_unix_ms)?;
                out.set_item("version", env!("CARGO_PKG_VERSION"))?;
            }
        }
        Ok(out)
    }

    fn version<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        self.ensure_open()?;
        let out = PyDict::new(py);
        out.set_item("version", env!("CARGO_PKG_VERSION"))?;
        out.set_item("protocol", "1.0")?;
        Ok(out)
    }

    /// Return the cache client for this connection.
    #[getter]
    fn cache(&self) -> PyResult<CacheClient> {
        self.ensure_open()?;
        match &self.backend {
            #[cfg(feature = "embedded")]
            Backend::Embedded(rt) => Ok(CacheClient {
                backend: CacheBackend::Embedded(rt.clone_runtime()),
            }),
            Backend::Grpc(_) => Ok(CacheClient {
                backend: CacheBackend::Grpc,
            }),
        }
    }

    fn close(&mut self) -> PyResult<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        match &self.backend {
            #[cfg(feature = "embedded")]
            Backend::Embedded(rt) => {
                rt.checkpoint().map_err(|e| err("IO_ERROR", e))?;
            }
            Backend::Grpc(_) => {
                // tonic channel closes when the client is dropped.
            }
        }
        Ok(())
    }

    fn __enter__<'py>(slf: PyRef<'py, Self>) -> PyRef<'py, Self> {
        slf
    }

    fn __exit__(
        &mut self,
        _exc_type: &Bound<'_, PyAny>,
        _exc_val: &Bound<'_, PyAny>,
        _exc_tb: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        self.close()
    }
}

impl RedDb {
    fn ensure_open(&self) -> PyResult<()> {
        if self.closed {
            return Err(err(
                "CLIENT_CLOSED",
                "operation on a closed RedDb connection",
            ));
        }
        Ok(())
    }
}

// -----------------------------------------------------------------------
// CacheClient
// -----------------------------------------------------------------------

enum CacheBackend {
    #[cfg(feature = "embedded")]
    Embedded(Arc<RedDBRuntime>),
    Grpc,
}

/// Cache client — exposes `cache.{get,put,exists,invalidate,
/// invalidate_prefix,invalidate_tags,flush_namespace}`.
///
/// Obtain via `db.cache`.
#[pyclass]
pub struct CacheClient {
    backend: CacheBackend,
}

#[pymethods]
impl CacheClient {
    /// Fetch a cached value. Returns bytes on hit, None on miss.
    fn get<'py>(&self, py: Python<'py>, namespace: &str, key: &str) -> PyResult<Option<Bound<'py, PyBytes>>> {
        match &self.backend {
            #[cfg(feature = "embedded")]
            CacheBackend::Embedded(rt) => {
                match rt.result_blob_cache().get(namespace, key) {
                    Some(hit) => Ok(Some(PyBytes::new(py, hit.value()))),
                    None => Ok(None),
                }
            }
            CacheBackend::Grpc => Err(err("NOT_SUPPORTED", "cache not available over gRPC transport; use the HTTP transport")),
        }
    }

    /// Store a value in the cache.
    #[pyo3(signature = (namespace, key, value, *, ttl_ms=None, tags=None))]
    fn put(
        &self,
        namespace: &str,
        key: &str,
        value: &[u8],
        ttl_ms: Option<u64>,
        tags: Option<Vec<String>>,
    ) -> PyResult<()> {
        match &self.backend {
            #[cfg(feature = "embedded")]
            CacheBackend::Embedded(rt) => {
                let policy = match ttl_ms {
                    Some(ms) => BlobCachePolicy::default().ttl_ms(ms),
                    None => BlobCachePolicy::default(),
                };
                let mut put = BlobCachePut::new(value.to_vec()).with_policy(policy);
                if let Some(t) = tags {
                    put = put.with_tags(t);
                }
                rt.result_blob_cache()
                    .put(namespace, key, put)
                    .map_err(|e| err("CACHE_ERROR", format!("{e:?}")))?;
                Ok(())
            }
            CacheBackend::Grpc => Err(err("NOT_SUPPORTED", "cache not available over gRPC transport")),
        }
    }

    /// Check whether a key is present. Returns 'present', 'absent', or 'maybe'.
    fn exists(&self, namespace: &str, key: &str) -> PyResult<&'static str> {
        match &self.backend {
            #[cfg(feature = "embedded")]
            CacheBackend::Embedded(rt) => {
                let status = match rt.result_blob_cache().exists(namespace, key) {
                    CachePresence::Present => "present",
                    CachePresence::Absent => "absent",
                    CachePresence::MaybePresent => "maybe",
                };
                Ok(status)
            }
            CacheBackend::Grpc => Err(err("NOT_SUPPORTED", "cache not available over gRPC transport")),
        }
    }

    /// Remove a single entry. Returns number of entries removed.
    fn invalidate(&self, namespace: &str, key: &str) -> PyResult<usize> {
        match &self.backend {
            #[cfg(feature = "embedded")]
            CacheBackend::Embedded(rt) => {
                Ok(rt.result_blob_cache().invalidate_key(namespace, key))
            }
            CacheBackend::Grpc => Err(err("NOT_SUPPORTED", "cache not available over gRPC transport")),
        }
    }

    /// Remove all entries whose key starts with `prefix`. Returns count removed.
    fn invalidate_prefix(&self, namespace: &str, prefix: &str) -> PyResult<usize> {
        match &self.backend {
            #[cfg(feature = "embedded")]
            CacheBackend::Embedded(rt) => {
                Ok(rt.result_blob_cache().invalidate_prefix(namespace, prefix))
            }
            CacheBackend::Grpc => Err(err("NOT_SUPPORTED", "cache not available over gRPC transport")),
        }
    }

    /// Remove all entries tagged with any of the given tags. Returns count removed.
    fn invalidate_tags(&self, namespace: &str, tags: Vec<String>) -> PyResult<usize> {
        match &self.backend {
            #[cfg(feature = "embedded")]
            CacheBackend::Embedded(rt) => {
                let tag_refs: Vec<&str> = tags.iter().map(|s| s.as_str()).collect();
                Ok(rt.result_blob_cache().invalidate_tags(namespace, &tag_refs))
            }
            CacheBackend::Grpc => Err(err("NOT_SUPPORTED", "cache not available over gRPC transport")),
        }
    }

    /// Remove all entries in a namespace.
    fn flush_namespace(&self, namespace: &str) -> PyResult<()> {
        match &self.backend {
            #[cfg(feature = "embedded")]
            CacheBackend::Embedded(rt) => {
                rt.result_blob_cache().invalidate_namespace(namespace);
                Ok(())
            }
            CacheBackend::Grpc => Err(err("NOT_SUPPORTED", "cache not available over gRPC transport")),
        }
    }
}

// -----------------------------------------------------------------------
// connect()
// -----------------------------------------------------------------------

/// Open a connection to a RedDB instance.
///
/// Accepted URIs:
///   - `memory://`               ephemeral in-memory database
///   - `file:///absolute/path`   embedded engine on disk
///   - `grpc://host:port`        remote gRPC server (tonic backend)
#[pyfunction]
pub fn connect(uri: &str) -> PyResult<RedDb> {
    if uri.is_empty() {
        return Err(err("INVALID_URI", "empty connection string"));
    }
    if uri == "memory://" || uri == "memory:" {
        #[cfg(feature = "embedded")]
        {
            let rt = EmbeddedRuntime::in_memory().map_err(|e| err("IO_ERROR", e))?;
            return Ok(RedDb {
                backend: Backend::Embedded(rt),
                closed: false,
            });
        }
        #[cfg(not(feature = "embedded"))]
        return Err(err("FEATURE_DISABLED", "embedded backend not compiled in"));
    }
    if let Some(rest) = uri.strip_prefix("file://") {
        if rest.is_empty() {
            return Err(err("INVALID_URI", "file:// URI is missing a path"));
        }
        #[cfg(feature = "embedded")]
        {
            let rt = EmbeddedRuntime::open(rest.into()).map_err(|e| err("IO_ERROR", e))?;
            return Ok(RedDb {
                backend: Backend::Embedded(rt),
                closed: false,
            });
        }
        #[cfg(not(feature = "embedded"))]
        return Err(err("FEATURE_DISABLED", "embedded backend not compiled in"));
    }
    if uri.starts_with("grpc://") {
        let addr = uri.strip_prefix("grpc://").unwrap_or(uri).to_string();
        let endpoint = format!("http://{addr}");
        let client = crate::get_runtime()
            .block_on(RedDBClient::connect(&endpoint, None))
            .map_err(|e| err("IO_ERROR", format!("grpc connect failed: {e}")))?;
        return Ok(RedDb {
            backend: Backend::Grpc(Mutex::new(client)),
            closed: false,
        });
    }
    Err(err(
        "UNSUPPORTED_SCHEME",
        format!("unsupported URI: {uri}. Expected file://, memory:// or grpc://"),
    ))
}

// -----------------------------------------------------------------------
// Parameterized queries — Python -> SchemaValue
// -----------------------------------------------------------------------

/// Resolve the effective bind list from positional `*args` and the
/// `params=` keyword. The keyword form wins when both are present —
/// callers typically pick one or the other.
fn collect_params<'py>(
    args: &Bound<'py, PyTuple>,
    params_kw: Option<&Bound<'py, PyAny>>,
) -> PyResult<Vec<Bound<'py, PyAny>>> {
    if let Some(kw) = params_kw {
        if kw.is_none() {
            return collect_args(args);
        }
        let list = kw
            .downcast::<PyList>()
            .map_err(|_| err("INVALID_PARAMS", "params= must be a list"))?;
        let mut out = Vec::with_capacity(list.len());
        for item in list.iter() {
            out.push(item);
        }
        return Ok(out);
    }
    collect_args(args)
}

fn collect_args<'py>(args: &Bound<'py, PyTuple>) -> PyResult<Vec<Bound<'py, PyAny>>> {
    let mut out = Vec::with_capacity(args.len());
    for item in args.iter() {
        out.push(item);
    }
    Ok(out)
}

/// Convert a single Python value into a `SchemaValue` for `$N` binding.
///
/// Mapping (mirrors the issue's contract):
///   None                 -> Null
///   bool                 -> Boolean
///   int                  -> Integer        (i64; UnsignedInteger above i64::MAX)
///   float                -> Float          (f64)
///   str                  -> Text
///   bytes / bytearray    -> Blob
///   list[float|int]      -> Vector         (downcast to f32)
///   datetime.datetime    -> Timestamp      (seconds since epoch)
///   uuid.UUID            -> Uuid           (16 raw bytes)
///   dict                 -> Json           (canonical JSON bytes)
#[cfg(feature = "embedded")]
fn py_to_param_value(value: &Bound<'_, PyAny>) -> PyResult<ParamValue> {
    use pyo3::types::{PyByteArray, PyDict as PyDictT, PyFloat, PyList as PyListT};
    use reddb::storage::schema::Value as SV;

    if value.is_none() {
        return Ok(SV::Null);
    }

    // bool MUST be checked before int — `bool` is an `int` subclass in
    // Python and `extract::<i64>(True) == Ok(1)`.
    let type_name = value
        .get_type()
        .name()
        .ok()
        .map(|n| n.to_string())
        .unwrap_or_default();
    if type_name == "bool" {
        if let Ok(b) = value.extract::<bool>() {
            return Ok(SV::Boolean(b));
        }
    }

    if let Ok(b) = value.downcast::<PyBytes>() {
        return Ok(SV::Blob(b.as_bytes().to_vec()));
    }
    if let Ok(ba) = value.downcast::<PyByteArray>() {
        let bytes = unsafe { ba.as_bytes() }.to_vec();
        return Ok(SV::Blob(bytes));
    }

    // int — try i64 first, then fall back to u64 for values above i64::MAX.
    // Floats also extract as i64 in pyo3 when the fractional part is zero,
    // so guard by `PyFloat` first.
    if value.downcast::<PyFloat>().is_err() {
        if let Ok(i) = value.extract::<i64>() {
            return Ok(SV::Integer(i));
        }
        if let Ok(u) = value.extract::<u64>() {
            return Ok(SV::UnsignedInteger(u));
        }
    }
    if let Ok(f) = value.extract::<f64>() {
        return Ok(SV::Float(f));
    }

    if let Ok(s) = value.extract::<String>() {
        return Ok(SV::Text(std::sync::Arc::from(s.as_str())));
    }

    // list[float|int] -> Vector
    if let Ok(list) = value.downcast::<PyListT>() {
        let mut floats: Vec<f32> = Vec::with_capacity(list.len());
        let mut all_numeric = true;
        for item in list.iter() {
            if let Ok(f) = item.extract::<f64>() {
                floats.push(f as f32);
            } else if let Ok(i) = item.extract::<i64>() {
                floats.push(i as f32);
            } else {
                all_numeric = false;
                break;
            }
        }
        if all_numeric {
            return Ok(SV::Vector(floats));
        }
        return Err(err(
            "INVALID_PARAMS",
            "list params must contain only numbers (for Vector binding)",
        ));
    }

    // datetime.datetime -> Timestamp(seconds). Detected by duck typing:
    // requires `.timestamp()` returning a float AND a `.year` attribute,
    // so arbitrary objects with a stray `timestamp()` method don't hijack
    // the path.
    if value.getattr("year").is_ok() {
        if let Ok(ts) = value.call_method0("timestamp") {
            if let Ok(secs) = ts.extract::<f64>() {
                return Ok(SV::Timestamp(secs as i64));
            }
        }
    }

    // uuid.UUID -> Uuid([u8;16]) via the `.bytes` attribute.
    if let Ok(bytes_attr) = value.getattr("bytes") {
        if let Ok(b) = bytes_attr.downcast::<PyBytes>() {
            let raw = b.as_bytes();
            if raw.len() == 16 {
                let mut arr = [0u8; 16];
                arr.copy_from_slice(raw);
                return Ok(SV::Uuid(arr));
            }
        }
    }

    if let Ok(dict) = value.downcast::<PyDictT>() {
        let json_str = pydict_to_json_str(dict)?;
        return Ok(SV::Json(json_str.into_bytes()));
    }

    Err(err(
        "INVALID_PARAMS",
        format!(
            "unsupported parameter type: {} (expected None, bool, int, float, str, \
             bytes, list[number], dict, datetime.datetime, or uuid.UUID)",
            type_name,
        ),
    ))
}

// -----------------------------------------------------------------------
// Conversions: Python <-> embedded backend types
// -----------------------------------------------------------------------

#[cfg(feature = "embedded")]
fn pydict_to_fields(payload: &Bound<'_, PyDict>) -> PyResult<Vec<(String, ScalarOut)>> {
    let mut out = Vec::with_capacity(payload.len());
    for (k, v) in payload.iter() {
        let key: String = k
            .extract()
            .map_err(|_| err("INVALID_PARAMS", "field keys must be strings"))?;
        out.push((key, py_to_scalar(&v)?));
    }
    Ok(out)
}

#[cfg(feature = "embedded")]
fn py_to_scalar(value: &Bound<'_, PyAny>) -> PyResult<ScalarOut> {
    if value.is_none() {
        return Ok(ScalarOut::Null);
    }
    if let Ok(b) = value.extract::<bool>() {
        return Ok(ScalarOut::Bool(b));
    }
    if let Ok(i) = value.extract::<i64>() {
        return Ok(ScalarOut::Int(i));
    }
    if let Ok(f) = value.extract::<f64>() {
        return Ok(ScalarOut::Float(f));
    }
    if let Ok(s) = value.extract::<String>() {
        return Ok(ScalarOut::Text(s));
    }
    Err(err(
        "INVALID_PARAMS",
        "field values must be None, bool, int, float or str",
    ))
}

#[cfg(feature = "embedded")]
fn query_rows_to_pydict<'py>(py: Python<'py>, qr: QueryRows) -> PyResult<Bound<'py, PyDict>> {
    let out = PyDict::new(py);
    out.set_item("statement", qr.statement)?;
    out.set_item("affected", qr.affected)?;
    out.set_item("columns", PyList::new(py, qr.columns)?)?;
    let rows = PyList::empty(py);
    for row in qr.rows {
        let dict = PyDict::new(py);
        for (col, val) in row {
            dict.set_item(col, scalar_to_py(py, val))?;
        }
        rows.append(dict)?;
    }
    out.set_item("rows", rows)?;
    Ok(out)
}

#[cfg(feature = "embedded")]
fn scalar_to_py(py: Python<'_>, v: ScalarOut) -> PyObject {
    use pyo3::IntoPyObject;
    match v {
        ScalarOut::Null => py.None(),
        ScalarOut::Bool(b) => b.into_pyobject(py).unwrap().to_owned().into_any().unbind(),
        ScalarOut::Int(n) => n.into_pyobject(py).unwrap().into_any().unbind(),
        ScalarOut::Float(n) => n.into_pyobject(py).unwrap().into_any().unbind(),
        ScalarOut::Text(s) => s.into_pyobject(py).unwrap().into_any().unbind(),
    }
}

// -----------------------------------------------------------------------
// gRPC helpers — minimal JSON builder / parser so we don't force a
// serde_json dep on downstream code.
// -----------------------------------------------------------------------

/// Encode a `dict[str, scalar]` into a compact JSON object literal.
fn pydict_to_json_str(payload: &Bound<'_, PyDict>) -> PyResult<String> {
    let mut out = String::new();
    out.push('{');
    let mut first = true;
    for (k, v) in payload.iter() {
        if !first {
            out.push(',');
        }
        first = false;
        let key: String = k
            .extract()
            .map_err(|_| err("INVALID_PARAMS", "field keys must be strings"))?;
        out.push('"');
        out.push_str(&json_escape(&key));
        out.push_str("\":");
        if v.is_none() {
            out.push_str("null");
        } else if let Ok(b) = v.extract::<bool>() {
            out.push_str(if b { "true" } else { "false" });
        } else if let Ok(i) = v.extract::<i64>() {
            out.push_str(&i.to_string());
        } else if let Ok(f) = v.extract::<f64>() {
            out.push_str(&f.to_string());
        } else if let Ok(s) = v.extract::<String>() {
            out.push('"');
            out.push_str(&json_escape(&s));
            out.push('"');
        } else {
            return Err(err(
                "INVALID_PARAMS",
                "field values must be None, bool, int, float or str",
            ));
        }
    }
    out.push('}');
    Ok(out)
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Parse the server's `result_json` (from `QueryReply`) into the same
/// dict shape the embedded backend returns. We rely on the fact that
/// the server produces standard JSON; parse it with `serde_json` (already
/// a direct dep of this crate via the gRPC stack) and rebuild a PyDict.
fn grpc_query_json_to_pydict<'py>(py: Python<'py>, json_str: &str) -> PyResult<Bound<'py, PyDict>> {
    let parsed: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|e| err("INTERNAL_ERROR", format!("bad server JSON: {e}")))?;

    let out = PyDict::new(py);

    // Rows (the server uses various keys for the row array in different
    // versions — try the canonical `rows` first, then `records`.)
    let rows_value = parsed
        .get("rows")
        .or_else(|| parsed.get("records"))
        .cloned()
        .unwrap_or(serde_json::Value::Array(Vec::new()));
    let rows_py = PyList::empty(py);
    if let serde_json::Value::Array(rows) = rows_value {
        for row in rows {
            let dict = PyDict::new(py);
            if let serde_json::Value::Object(map) = row {
                for (k, v) in map {
                    dict.set_item(k, json_value_to_py(py, &v)?)?;
                }
            }
            rows_py.append(dict)?;
        }
    }
    out.set_item("rows", rows_py)?;

    let columns_py = PyList::empty(py);
    if let Some(serde_json::Value::Array(cols)) = parsed.get("columns") {
        for col in cols {
            if let Some(s) = col.as_str() {
                columns_py.append(s)?;
            }
        }
    }
    out.set_item("columns", columns_py)?;

    let affected = parsed.get("affected").and_then(|v| v.as_u64()).unwrap_or(0);
    out.set_item("affected", affected)?;

    let statement = parsed
        .get("statement")
        .and_then(|v| v.as_str())
        .unwrap_or("select")
        .to_string();
    out.set_item("statement", statement)?;

    Ok(out)
}

fn json_value_to_py<'py>(py: Python<'py>, v: &serde_json::Value) -> PyResult<Bound<'py, PyAny>> {
    use pyo3::IntoPyObject;
    Ok(match v {
        serde_json::Value::Null => py.None().into_bound(py),
        serde_json::Value::Bool(b) => (*b).into_pyobject(py).unwrap().to_owned().into_any(),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.into_pyobject(py).unwrap().into_any()
            } else if let Some(u) = n.as_u64() {
                u.into_pyobject(py).unwrap().into_any()
            } else {
                n.as_f64()
                    .unwrap_or(0.0)
                    .into_pyobject(py)
                    .unwrap()
                    .into_any()
            }
        }
        serde_json::Value::String(s) => s.as_str().into_pyobject(py).unwrap().into_any(),
        // For complex values, fall back to the JSON text representation.
        other => other.to_string().into_pyobject(py).unwrap().into_any(),
    })
}
