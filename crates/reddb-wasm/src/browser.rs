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
            "queryParameters": true,
            "biometricParameters": true,
            "databaseBackups": true,
            "databaseInspection": true,
            "localVault": true,
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

/// Opaque, zeroizing key owned by the initialized Worker session, not JS storage.
#[wasm_bindgen]
pub struct BrowserVaultKey {
    key: reddb_engine::query::local_database::vault::LocalVaultKey,
}

fn local_query_error(error: reddb_engine::query::local_read::LocalReadError) -> JsValue {
    coded_error(error.code(), error)
}

fn coded_error(code: &str, message: impl std::fmt::Display) -> JsValue {
    let result = Error::new(&message.to_string());
    match js_sys::Reflect::set(&result, &"code".into(), &code.into()) {
        Ok(_) => result.into(),
        Err(error) => error,
    }
}

fn query_parameter(value: JsValue) -> Result<reddb_types::Value, JsValue> {
    use reddb_types::Value;
    let invalid = || {
        coded_error(
            "INVALID_ARGUMENT",
            "Parameters require a supported scalar or typed RedDB value envelope",
        )
    };
    if let Some(value) = typed_query_parameter(&value)? {
        return Ok(value);
    }
    if value.is_null() {
        return Ok(Value::Null);
    }
    if let Some(value) = value.as_bool() {
        return Ok(Value::Boolean(value));
    }
    if let Some(value) = value.as_string() {
        return Ok(Value::text(value));
    }
    if let Some(value) = value.as_f64() {
        if !value.is_finite() {
            return Err(invalid());
        }
        return if value.fract() == 0.0 {
            if value.abs() > MAX_EXACT_OFFSET as f64 {
                return Err(invalid());
            }
            // Finite, integral and bounded to JS's safe range, which fits i64.
            #[allow(clippy::cast_possible_truncation)]
            let integer = value as i64;
            Ok(Value::Integer(integer))
        } else {
            Ok(Value::Float(value))
        };
    }
    if value.is_bigint() {
        let integer: String = value
            .unchecked_into::<js_sys::BigInt>()
            .to_string(10)
            .map_err(JsValue::from)?
            .into();
        if let Ok(value) = integer.parse::<i64>() {
            return Ok(Value::Integer(value));
        }
        return integer
            .parse::<u64>()
            .map(Value::UnsignedInteger)
            .map_err(|_| invalid());
    }
    Err(invalid())
}

fn typed_query_parameter(value: &JsValue) -> Result<Option<reddb_types::Value>, JsValue> {
    use reddb_types::{BiometricFormat, BiometricModality, BiometricSample, Value};

    if !value.is_object() || Array::is_array(value) {
        return Ok(None);
    }
    if let Some(envelope) = object_property(value, "$biometric")? {
        let modality = string_property(&envelope, "modality")?;
        let format_name = string_property(&envelope, "format")?;
        let data = decode_hex(&string_property(&envelope, "data")?)?;
        let format = BiometricFormat::from_standard_name(&format_name)
            .ok_or_else(|| coded_error("INVALID_ARGUMENT", "Unknown biometric format"))?;
        let expected = match modality.to_ascii_lowercase().as_str() {
            "fingerprint" => BiometricModality::Fingerprint,
            "face" => BiometricModality::Face,
            "iris" => BiometricModality::Iris,
            _ => {
                return Err(coded_error(
                    "INVALID_ARGUMENT",
                    "Unknown biometric modality",
                ))
            }
        };
        if format.modality() != expected {
            return Err(coded_error(
                "INVALID_ARGUMENT",
                "Biometric format does not match modality",
            ));
        }
        let sample = BiometricSample::with_format(format, data)
            .map_err(|error| coded_error("INVALID_ARGUMENT", error))?;
        return Ok(Some(match expected {
            BiometricModality::Fingerprint => Value::Fingerprint(sample),
            BiometricModality::Face => Value::Face(sample),
            BiometricModality::Iris => Value::Iris(sample),
        }));
    }
    if let Some(envelope) = object_property(value, "$feature_vector")? {
        let space_id = decode_space_id(&string_property(&envelope, "space_id")?)?;
        let raw_values = js_sys::Reflect::get(&envelope, &JsValue::from_str("values"))?;
        if !Array::is_array(&raw_values) {
            return Err(coded_error(
                "INVALID_ARGUMENT",
                "Feature vector values must be an array",
            ));
        }
        let values = Array::from(&raw_values)
            .iter()
            .map(|value| {
                let number = value
                    .as_f64()
                    .filter(|number| number.is_finite())
                    .ok_or_else(|| {
                        coded_error(
                            "INVALID_ARGUMENT",
                            "Feature coordinates must be finite numbers",
                        )
                    })?;
                #[allow(clippy::cast_possible_truncation)]
                let coordinate = number as f32;
                Ok(coordinate)
            })
            .collect::<Result<Vec<_>, JsValue>>()?;
        let feature = reddb_types::FeatureVector::new(space_id, values)
            .map_err(|error| coded_error("INVALID_ARGUMENT", error))?;
        return Ok(Some(Value::FeatureVector(feature)));
    }
    if let Some(envelope) = object_property(value, "$bit_signature")? {
        let space_id = decode_space_id(&string_property(&envelope, "space_id")?)?;
        let bits = decode_hex(&string_property(&envelope, "bits")?)?;
        let mask = optional_property(&envelope, "mask")?
            .map(|value| {
                value
                    .as_string()
                    .ok_or_else(|| {
                        coded_error(
                            "INVALID_ARGUMENT",
                            "Bit signature mask must be hexadecimal text",
                        )
                    })
                    .and_then(|value| decode_hex(&value))
            })
            .transpose()?;
        let signature = reddb_types::BitSignature::new(space_id, bits, mask)
            .map_err(|error| coded_error("INVALID_ARGUMENT", error))?;
        return Ok(Some(Value::BitSignature(signature)));
    }
    if let Some(envelope) = object_property(value, "$landmark_set")? {
        let keys = js_sys::Object::keys(&js_sys::Object::from(envelope.clone()));
        if keys.length() != 4
            || !js_sys::Reflect::has(&envelope, &JsValue::from_str("space_id"))?
            || !js_sys::Reflect::has(&envelope, &JsValue::from_str("dimensions"))?
            || !js_sys::Reflect::has(&envelope, &JsValue::from_str("pairs"))?
            || !js_sys::Reflect::has(&envelope, &JsValue::from_str("midline"))?
        {
            return Err(coded_error(
                "INVALID_ARGUMENT",
                "$landmark_set must contain space_id, dimensions, pairs, and midline",
            ));
        }
        let space_id = decode_space_id(&string_property(&envelope, "space_id")?)?;
        let dimensions = js_sys::Reflect::get(&envelope, &JsValue::from_str("dimensions"))?
            .as_f64()
            .filter(|value| *value == 2.0 || *value == 3.0)
            .map(|value| value as u8)
            .ok_or_else(|| coded_error("INVALID_ARGUMENT", "Landmark dimensions must be 2 or 3"))?;
        let parse_point = |value: JsValue| {
            if !Array::is_array(&value) {
                return Err(coded_error(
                    "INVALID_ARGUMENT",
                    "Landmark point must be an array",
                ));
            }
            let values = Array::from(&value);
            if values.length() != u32::from(dimensions) {
                return Err(coded_error(
                    "INVALID_ARGUMENT",
                    "Landmark coordinate count differs from dimensions",
                ));
            }
            let mut point = [0.0_f32; 3];
            for (index, coordinate) in values.iter().enumerate() {
                let number = coordinate
                    .as_f64()
                    .filter(|number| number.is_finite())
                    .ok_or_else(|| {
                        coded_error("INVALID_ARGUMENT", "Landmark coordinates must be finite")
                    })?;
                #[allow(clippy::cast_possible_truncation)]
                let stored = number as f32;
                if !stored.is_finite() {
                    return Err(coded_error(
                        "INVALID_ARGUMENT",
                        "Landmark coordinates must fit f32",
                    ));
                }
                point[index] = stored;
            }
            Ok(point)
        };
        let pair_values = js_sys::Reflect::get(&envelope, &JsValue::from_str("pairs"))?;
        let midline_values = js_sys::Reflect::get(&envelope, &JsValue::from_str("midline"))?;
        if !Array::is_array(&pair_values) || !Array::is_array(&midline_values) {
            return Err(coded_error(
                "INVALID_ARGUMENT",
                "Landmark pairs and midline must be arrays",
            ));
        }
        let pair_values = Array::from(&pair_values);
        let midline_values = Array::from(&midline_values);
        let point_count = usize::try_from(pair_values.length())
            .ok()
            .and_then(|count| count.checked_mul(2))
            .and_then(|count| count.checked_add(midline_values.length() as usize))
            .ok_or_else(|| coded_error("INVALID_ARGUMENT", "Too many landmark points"))?;
        if point_count > reddb_types::MAX_LANDMARK_POINTS {
            return Err(coded_error("INVALID_ARGUMENT", "Too many landmark points"));
        }
        let mut pairs = Vec::with_capacity(pair_values.length() as usize);
        for pair in pair_values.iter() {
            if !pair.is_object()
                || js_sys::Object::keys(&js_sys::Object::from(pair.clone())).length() != 2
                || !js_sys::Reflect::has(&pair, &JsValue::from_str("left"))?
                || !js_sys::Reflect::has(&pair, &JsValue::from_str("right"))?
            {
                return Err(coded_error(
                    "INVALID_ARGUMENT",
                    "Landmark pair must contain left and right",
                ));
            }
            pairs.push((
                parse_point(js_sys::Reflect::get(&pair, &JsValue::from_str("left"))?)?,
                parse_point(js_sys::Reflect::get(&pair, &JsValue::from_str("right"))?)?,
            ));
        }
        let midline = midline_values
            .iter()
            .map(parse_point)
            .collect::<Result<Vec<_>, _>>()?;
        let landmarks = reddb_types::LandmarkSet::new(space_id, dimensions, pairs, midline)
            .map_err(|error| coded_error("INVALID_ARGUMENT", error))?;
        return Ok(Some(Value::LandmarkSet(landmarks)));
    }
    Ok(None)
}

fn object_property(object: &JsValue, name: &str) -> Result<Option<JsValue>, JsValue> {
    let Some(value) = optional_property(object, name)? else {
        return Ok(None);
    };
    if !value.is_object() || value.is_null() {
        return Err(coded_error(
            "INVALID_ARGUMENT",
            format!("Typed parameter '{name}' must contain an object"),
        ));
    }
    Ok(Some(value))
}

fn optional_property(object: &JsValue, name: &str) -> Result<Option<JsValue>, JsValue> {
    let key = JsValue::from_str(name);
    if !js_sys::Reflect::has(object, &key)? {
        return Ok(None);
    }
    js_sys::Reflect::get(object, &key).map(Some)
}

fn string_property(object: &JsValue, name: &str) -> Result<String, JsValue> {
    js_sys::Reflect::get(object, &JsValue::from_str(name))?
        .as_string()
        .ok_or_else(|| {
            coded_error(
                "INVALID_ARGUMENT",
                format!("Typed parameter field '{name}' must be text"),
            )
        })
}

fn decode_space_id(value: &str) -> Result<[u8; 16], JsValue> {
    decode_hex(value)?
        .try_into()
        .map_err(|_| coded_error("INVALID_ARGUMENT", "Feature space_id must contain 16 bytes"))
}

fn decode_hex(value: &str) -> Result<Vec<u8>, JsValue> {
    if !value.len().is_multiple_of(2) {
        return Err(coded_error(
            "INVALID_ARGUMENT",
            "Hexadecimal text must contain an even number of digits",
        ));
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digit = |byte| match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                b'A'..=b'F' => Some(byte - b'A' + 10),
                _ => None,
            };
            let high = digit(pair[0]).ok_or_else(|| {
                coded_error(
                    "INVALID_ARGUMENT",
                    "Typed parameter contains invalid hexadecimal text",
                )
            })?;
            let low = digit(pair[1]).ok_or_else(|| {
                coded_error(
                    "INVALID_ARGUMENT",
                    "Typed parameter contains invalid hexadecimal text",
                )
            })?;
            Ok((high << 4) | low)
        })
        .collect()
}

#[wasm_bindgen]
impl BrowserDatabase {
    #[wasm_bindgen(js_name = vaultInitialized)]
    pub fn vault_initialized(&self) -> Result<bool, JsValue> {
        self.database.vault_initialized().map_err(local_query_error)
    }

    #[wasm_bindgen(js_name = vaultUnlock)]
    pub fn vault_unlock(
        &mut self,
        password: &str,
        create: bool,
    ) -> Result<BrowserVaultKey, JsValue> {
        self.database
            .vault_unlock(password, create)
            .map(|key| BrowserVaultKey { key })
            .map_err(local_query_error)
    }

    #[wasm_bindgen(js_name = vaultKeys)]
    pub fn vault_keys(&self, key: &BrowserVaultKey) -> Result<Array, JsValue> {
        Ok(self
            .database
            .vault_keys(&key.key)
            .map_err(local_query_error)?
            .iter()
            .map(|key| JsValue::from_str(key))
            .collect())
    }

    #[wasm_bindgen(js_name = vaultReveal)]
    pub fn vault_reveal(
        &self,
        key: &BrowserVaultKey,
        name: &str,
    ) -> Result<Option<String>, JsValue> {
        self.database
            .vault_reveal(&key.key, name)
            .map_err(local_query_error)
    }

    #[wasm_bindgen(js_name = vaultPut)]
    pub fn vault_put(
        &mut self,
        key: &BrowserVaultKey,
        name: &str,
        value: Option<String>,
    ) -> Result<(), JsValue> {
        self.database
            .vault_put(&key.key, name, value.as_deref())
            .map_err(local_query_error)
    }

    pub fn catalog(&self) -> Result<JsValue, JsValue> {
        let result = self.database.catalog().map_err(local_query_error)?;
        js_sys::JSON::parse(&result.to_json().to_string_compact())
    }

    #[wasm_bindgen(js_name = readQuery)]
    pub fn read_query(&self, sql: &str, params: Array, scopes: Array) -> Result<JsValue, JsValue> {
        if params.length() as usize > reddb_engine::query::local_database::MAX_QUERY_PARAMETERS
            || scopes.length() > 64
        {
            return Err(coded_error(
                "INVALID_ARGUMENT",
                "Inspection argument limit exceeded",
            ));
        }
        let values = params
            .iter()
            .map(query_parameter)
            .collect::<Result<Vec<_>, _>>()?;
        let scopes = scopes
            .iter()
            .map(|scope| {
                let text = |key: &str| -> Result<String, JsValue> {
                    js_sys::Reflect::get(&scope, &JsValue::from_str(key))?
                        .as_string()
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| {
                            coded_error(
                                "INVALID_ARGUMENT",
                                "Expected an inspection table and field",
                            )
                        })
                };
                Ok(reddb_engine::query::local_read::ReadScope {
                    table: text("table")?,
                    field: text("field")?,
                    value: query_parameter(js_sys::Reflect::get(
                        &scope,
                        &JsValue::from_str("value"),
                    )?)?,
                })
            })
            .collect::<Result<Vec<_>, JsValue>>()?;
        let result = self
            .database
            .read_query(sql, &values, &scopes)
            .map_err(local_query_error)?;
        js_sys::JSON::parse(&result.to_json().to_string_compact())
    }

    #[wasm_bindgen(js_name = checkHealthy)]
    pub fn check_healthy(&self) -> Result<(), JsValue> {
        self.database.check_healthy().map_err(local_query_error)
    }

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

    #[wasm_bindgen(js_name = queryWithParams)]
    pub fn query_with_params(
        &mut self,
        sql: &str,
        params: Array,
        now_ms: f64,
    ) -> Result<JsValue, JsValue> {
        if params.length() as usize > reddb_engine::query::local_database::MAX_QUERY_PARAMETERS {
            return Err(coded_error(
                "INVALID_ARGUMENT",
                "At most 65536 positional parameters are supported",
            ));
        }
        let values = params
            .iter()
            .map(query_parameter)
            .collect::<Result<Vec<_>, _>>()?;
        let result = self
            .database
            .query_with_params(sql, &values, exact_integer(now_ms).map_err(js_error)?)
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
