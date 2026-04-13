//! High-level Python API: `reddb.connect(uri) -> RedDb`.
//!
//! Mirrors the JS and Rust drivers. Same connection-string contract,
//! same method names, same error semantics. See `PLAN_DRIVERS.md`.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};

#[cfg(feature = "embedded")]
use crate::embedded::{EmbeddedRuntime, QueryRows, ScalarOut};

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
        format!("RedDbError(code={:?}, message={:?})", self.code, self.message)
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
    fn query<'py>(&self, py: Python<'py>, sql: &str) -> PyResult<Bound<'py, PyDict>> {
        self.ensure_open()?;
        match &self.backend {
            #[cfg(feature = "embedded")]
            Backend::Embedded(rt) => {
                let qr = rt.query(sql).map_err(|e| err("QUERY_ERROR", e))?;
                query_rows_to_pydict(py, qr)
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
                let affected = rt
                    .insert_object(collection, &fields)
                    .map_err(|e| err("QUERY_ERROR", e))?;
                let out = PyDict::new(py);
                out.set_item("affected", affected)?;
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
                for item in payloads.iter() {
                    let dict = item.downcast::<PyDict>().map_err(|_| {
                        err("INVALID_PARAMS", "bulk_insert payloads must be dicts")
                    })?;
                    let fields = pydict_to_fields(dict)?;
                    total += rt
                        .insert_object(collection, &fields)
                        .map_err(|e| err("QUERY_ERROR", e))?;
                }
                let out = PyDict::new(py);
                out.set_item("affected", total)?;
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
                let n = rt.delete(collection, id).map_err(|e| err("QUERY_ERROR", e))?;
                let out = PyDict::new(py);
                out.set_item("affected", n)?;
                Ok(out)
            }
        }
    }

    fn health<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        self.ensure_open()?;
        let out = PyDict::new(py);
        out.set_item("ok", true)?;
        out.set_item("version", env!("CARGO_PKG_VERSION"))?;
        Ok(out)
    }

    fn version<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        self.ensure_open()?;
        let out = PyDict::new(py);
        out.set_item("version", env!("CARGO_PKG_VERSION"))?;
        out.set_item("protocol", "1.0")?;
        Ok(out)
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
// connect()
// -----------------------------------------------------------------------

/// Open a connection to a RedDB instance.
///
/// Accepted URIs:
///   - `memory://`               ephemeral in-memory database
///   - `file:///absolute/path`   embedded engine on disk
///   - `grpc://host:port`        remote gRPC server  (not yet wired)
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
        return Err(err(
            "FEATURE_DISABLED",
            format!(
                "grpc:// is not yet supported by the high-level Python API. \
                 Use reddb.legacy_grpc_connect(addr) for now, or wait for \
                 PLAN_DRIVERS.md Phase 4.5. Got: {uri}"
            ),
        ));
    }
    Err(err(
        "UNSUPPORTED_SCHEME",
        format!("unsupported URI: {uri}. Expected file://, memory:// or grpc://"),
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
