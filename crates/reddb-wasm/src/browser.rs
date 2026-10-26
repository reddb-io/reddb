use std::io::{self, SeekFrom};

use js_sys::{Array, Error, Uint8Array};
use reddb_file::{EmbeddedRdbFile, VfsFile};
use wasm_bindgen::prelude::*;

const MAX_EXACT_OFFSET: u64 = (1u64 << 53) - 1;

/// Contract shared by the JS worker and foreign-language browser adapters.
#[wasm_bindgen(js_name = runtimeInfo)]
pub fn runtime_info() -> Result<JsValue, JsValue> {
    let info = reddb_engine::json!({
        "abiVersion": 1,
        "valueEncoding": "reddb-json-v1",
        "capabilities": reddb_engine::json!({
            "queryValidation": true,
            "scalarQueries": true,
            "storedQueries": true,
            "storedQueryMode": "read-write-tables",
            "sqlWrites": true,
            "transactions": true,
            "sync": false,
        })
    });
    js_sys::JSON::parse(&info.to_string_compact())
}

#[wasm_bindgen]
extern "C" {
    pub type SyncFile;

    #[wasm_bindgen(method, catch, js_name = readAt)]
    fn read_at(this: &SyncFile, buffer: &mut [u8], offset: f64) -> Result<u32, JsValue>;
    #[wasm_bindgen(method, catch, js_name = writeAt)]
    fn write_at(this: &SyncFile, buffer: &[u8], offset: f64) -> Result<u32, JsValue>;
    #[wasm_bindgen(method, catch, js_name = getSize)]
    fn get_size(this: &SyncFile) -> Result<f64, JsValue>;
    #[wasm_bindgen(method, catch)]
    fn truncate(this: &SyncFile, size: f64) -> Result<(), JsValue>;
    #[wasm_bindgen(method, catch)]
    fn flush(this: &SyncFile) -> Result<(), JsValue>;
}

pub(crate) struct BrowserFile {
    handle: SyncFile,
    position: u64,
}

impl BrowserFile {
    pub(crate) fn new(handle: SyncFile) -> Self {
        Self {
            handle,
            position: 0,
        }
    }
}

fn io_error(error: JsValue) -> io::Error {
    let message = error
        .dyn_ref::<Error>()
        .map(|error| String::from(error.message()))
        .or_else(|| error.as_string())
        .unwrap_or_else(|| "browser file I/O failed".into());
    io::Error::other(message)
}

fn js_error(error: impl std::fmt::Display) -> JsValue {
    Error::new(&error.to_string()).into()
}

fn exact_offset(offset: u64) -> io::Result<f64> {
    if offset > MAX_EXACT_OFFSET {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file offset exceeds JavaScript's exact integer range",
        ));
    }
    Ok(offset as f64)
}

fn exact_integer(value: f64) -> io::Result<u64> {
    if !value.is_finite() || value < 0.0 || value.fract() != 0.0 || value > MAX_EXACT_OFFSET as f64
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "browser value must be an exact nonnegative integer",
        ));
    }
    // The checks above reject fractions and values outside the exact JS range.
    #[allow(clippy::cast_possible_truncation)]
    let integer = value as u64;
    Ok(integer)
}

impl VfsFile for BrowserFile {
    fn try_clone(&self) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "OPFS handles have one owner",
        ))
    }

    fn file_len(&self) -> io::Result<u64> {
        exact_integer(self.handle.get_size().map_err(io_error)?)
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.handle.truncate(exact_offset(len)?).map_err(io_error)
    }

    fn write_all(&mut self, mut buffer: &[u8]) -> io::Result<()> {
        while !buffer.is_empty() {
            let count = self
                .handle
                .write_at(buffer, exact_offset(self.position)?)
                .map_err(io_error)? as usize;
            if count == 0 || count > buffer.len() {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "invalid OPFS write length",
                ));
            }
            self.position = self
                .position
                .checked_add(count as u64)
                .ok_or(io::ErrorKind::InvalidInput)?;
            buffer = &buffer[count..];
        }
        Ok(())
    }

    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self
            .handle
            .read_at(buffer, exact_offset(self.position)?)
            .map_err(io_error)? as usize;
        if count > buffer.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid OPFS read length",
            ));
        }
        self.position = self
            .position
            .checked_add(count as u64)
            .ok_or(io::ErrorKind::InvalidInput)?;
        Ok(count)
    }

    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let position = match pos {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::Current(offset) => self.position.checked_add_signed(offset),
            SeekFrom::End(offset) => self.file_len()?.checked_add_signed(offset),
        }
        .ok_or(io::ErrorKind::InvalidInput)?;
        exact_offset(position)?;
        self.position = position;
        Ok(position)
    }

    fn sync_all(&self) -> io::Result<()> {
        self.handle.flush().map_err(io_error)
    }
}

/// Canonical `.rdb` artifact I/O. The worker owns and closes the underlying
/// exclusive access handle; freeing this wrapper never unlocks it early.
#[wasm_bindgen]
pub struct BrowserArtifact {
    file: EmbeddedRdbFile<BrowserFile>,
}

/// Local SQL over the same canonical snapshot/WAL publication and recovery path.
/// The worker retains the exclusive OPFS handle until this wrapper is freed.
#[wasm_bindgen]
pub struct BrowserDatabase {
    database: reddb_engine::query::local_database::LocalDatabase<BrowserFile>,
}

fn local_query_error(error: reddb_engine::query::local_read::LocalReadError) -> JsValue {
    let result = Error::new(&error.to_string());
    match js_sys::Reflect::set(&result, &"code".into(), &error.code().into()) {
        Ok(_) => result.into(),
        Err(error) => error,
    }
}

#[wasm_bindgen]
impl BrowserDatabase {
    pub fn open(handle: SyncFile, name: &str) -> Result<BrowserDatabase, JsValue> {
        let database = reddb_engine::query::local_database::LocalDatabase::open(
            BrowserFile::new(handle),
            name,
        )
        .map_err(local_query_error)?;
        Ok(Self { database })
    }

    pub fn create(handle: SyncFile, name: &str, now_ms: f64) -> Result<BrowserDatabase, JsValue> {
        let database = reddb_engine::query::local_database::LocalDatabase::create(
            BrowserFile::new(handle),
            name,
            exact_integer(now_ms).map_err(js_error)?,
        )
        .map_err(local_query_error)?;
        Ok(Self { database })
    }

    pub fn query(&mut self, sql: &str, now_ms: f64) -> Result<JsValue, JsValue> {
        let result = self
            .database
            .query(sql, exact_integer(now_ms).map_err(js_error)?)
            .map_err(local_query_error)?;
        js_sys::JSON::parse(&result.to_json().to_string_compact())
    }
}

#[wasm_bindgen]
impl BrowserArtifact {
    pub fn open(handle: SyncFile, name: &str) -> Result<BrowserArtifact, JsValue> {
        let file = EmbeddedRdbFile::open(BrowserFile::new(handle), name).map_err(js_error)?;
        Ok(Self { file })
    }

    pub fn create(
        handle: SyncFile,
        name: &str,
        created_at_unix_ms: f64,
    ) -> Result<BrowserArtifact, JsValue> {
        let created_at_unix_ms = exact_integer(created_at_unix_ms).map_err(js_error)?;
        let file = EmbeddedRdbFile::create(
            BrowserFile::new(handle),
            name,
            &[],
            u128::from(created_at_unix_ms),
        )
        .map_err(js_error)?;
        Ok(Self { file })
    }

    pub fn append(&mut self, payloads: Array) -> Result<(), JsValue> {
        let payloads: Vec<Vec<u8>> = payloads
            .iter()
            .map(|payload| {
                payload
                    .dyn_into::<Uint8Array>()
                    .map(|bytes| bytes.to_vec())
                    .map_err(|_| js_error("WAL payloads must be Uint8Arrays"))
            })
            .collect::<Result<_, _>>()?;
        self.file.append_wal_payloads(&payloads).map_err(js_error)?;
        Ok(())
    }

    #[wasm_bindgen(js_name = readWal)]
    pub fn read_wal(&mut self) -> Result<Array, JsValue> {
        let payloads = self.file.read_wal_payloads().map_err(js_error)?;
        Ok(payloads
            .iter()
            .map(|bytes| JsValue::from(Uint8Array::from(bytes.as_slice())))
            .collect())
    }

    pub fn checkpoint(&mut self, snapshot: &[u8], min_wal_bytes: u32) -> Result<(), JsValue> {
        self.file
            .write_snapshot(snapshot, u64::from(min_wal_bytes))
            .map_err(js_error)?;
        Ok(())
    }

    #[wasm_bindgen(js_name = readSnapshot)]
    pub fn read_snapshot(&mut self) -> Result<Option<Vec<u8>>, JsValue> {
        self.file.read_snapshot().map_err(js_error)
    }

    /// String-valued counters preserve all u64 bits across JavaScript.
    pub fn generation(&mut self) -> Result<String, JsValue> {
        Ok(self
            .file
            .state()
            .map_err(js_error)?
            .selected_superblock
            .generation
            .to_string())
    }
}

#[wasm_bindgen(js_name = validateQuery)]
pub fn validate_query(sql: &str) -> Result<(), JsValue> {
    reddb_rql::parse_frontend(sql).map(|_| ()).map_err(js_error)
}

/// Experimental storage-free execution, using the native scalar evaluator.
#[wasm_bindgen(js_name = executeScalar)]
pub fn execute_scalar(sql: &str) -> Result<JsValue, JsValue> {
    use reddb_engine::query::scalar::{execute, ScalarError};

    let result = execute(sql).map_err(|failure| {
        let code = match failure {
            ScalarError::Parse(_) => "QUERY_PARSE_ERROR",
            ScalarError::Unsupported => "QUERY_UNSUPPORTED",
            ScalarError::Evaluation(_) => "QUERY_EVALUATION_ERROR",
        };
        let error = Error::new(&failure.to_string());
        // Own, extensible Error object; Reflect failure is still propagated.
        match js_sys::Reflect::set(&error, &"code".into(), &code.into()) {
            Ok(_) => error.into(),
            Err(error) => error,
        }
    })?;
    js_sys::JSON::parse(&result.to_json().to_string_compact())
}
