//! RedDB Type System
//!
//! Defines the core data types supported by RedDB, including:
//! - Primitive types (Integer, Float, Text, Blob, Boolean)
//! - Network types (IpAddr, MacAddr)
//! - Temporal types (Timestamp, Duration)
//! - Vector types (for similarity search)
//!
//! All types support efficient binary serialization for storage.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Type identifier for column definitions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum DataType {
    /// Signed 64-bit integer
    Integer = 1,
    /// Unsigned 64-bit integer
    UnsignedInteger = 2,
    /// 64-bit floating point
    Float = 3,
    /// Variable-length UTF-8 text
    Text = 4,
    /// Variable-length binary data
    Blob = 5,
    /// Boolean (true/false)
    Boolean = 6,
    /// Unix timestamp (seconds since epoch)
    Timestamp = 7,
    /// Duration in milliseconds
    Duration = 8,
    /// IPv4 or IPv6 address
    IpAddr = 9,
    /// MAC address (6 bytes)
    MacAddr = 10,
    /// Fixed-dimension float vector (for similarity search)
    Vector = 11,
    /// Nullable wrapper (stores inner type in high nibble)
    Nullable = 12,
    /// JSON-like structured data
    Json = 13,
    /// UUID (16 bytes)
    Uuid = 14,
    /// Reference to a graph node (for unified queries)
    NodeRef = 15,
    /// Reference to a graph edge
    EdgeRef = 16,
    /// Reference to a vector in vector store
    VectorRef = 17,
    /// Reference to a table row (table_id, row_id)
    RowRef = 18,
}

impl DataType {
    /// Serialize type to byte
    pub fn to_byte(&self) -> u8 {
        *self as u8
    }

    /// Deserialize type from byte
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            1 => Some(DataType::Integer),
            2 => Some(DataType::UnsignedInteger),
            3 => Some(DataType::Float),
            4 => Some(DataType::Text),
            5 => Some(DataType::Blob),
            6 => Some(DataType::Boolean),
            7 => Some(DataType::Timestamp),
            8 => Some(DataType::Duration),
            9 => Some(DataType::IpAddr),
            10 => Some(DataType::MacAddr),
            11 => Some(DataType::Vector),
            12 => Some(DataType::Nullable),
            13 => Some(DataType::Json),
            14 => Some(DataType::Uuid),
            15 => Some(DataType::NodeRef),
            16 => Some(DataType::EdgeRef),
            17 => Some(DataType::VectorRef),
            18 => Some(DataType::RowRef),
            _ => None,
        }
    }

    /// Returns the fixed size in bytes if known, None for variable-length types
    pub fn fixed_size(&self) -> Option<usize> {
        match self {
            DataType::Integer => Some(8),
            DataType::UnsignedInteger => Some(8),
            DataType::Float => Some(8),
            DataType::Boolean => Some(1),
            DataType::Timestamp => Some(8),
            DataType::Duration => Some(8),
            DataType::MacAddr => Some(6),
            DataType::Uuid => Some(16),
            // Variable-length types
            DataType::Text => None,
            DataType::Blob => None,
            DataType::IpAddr => None, // 4 or 16 bytes
            DataType::Vector => None, // depends on dimensions
            DataType::Nullable => None,
            DataType::Json => None,
            // Cross-references (variable-length IDs)
            DataType::NodeRef => None,
            DataType::EdgeRef => None,
            DataType::VectorRef => Some(8), // u64 vector ID
            DataType::RowRef => None,       // table_id (varint) + row_id (u64)
        }
    }

    /// Check if this type supports indexing
    pub fn is_indexable(&self) -> bool {
        matches!(
            self,
            DataType::Integer
                | DataType::UnsignedInteger
                | DataType::Float
                | DataType::Text
                | DataType::Timestamp
                | DataType::IpAddr
                | DataType::Uuid
                | DataType::NodeRef
                | DataType::EdgeRef
                | DataType::VectorRef
                | DataType::RowRef
        )
    }

    /// Check if this type supports ordering
    pub fn is_orderable(&self) -> bool {
        matches!(
            self,
            DataType::Integer
                | DataType::UnsignedInteger
                | DataType::Float
                | DataType::Text
                | DataType::Timestamp
                | DataType::Duration
        )
    }
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DataType::Integer => write!(f, "INTEGER"),
            DataType::UnsignedInteger => write!(f, "UNSIGNED INTEGER"),
            DataType::Float => write!(f, "FLOAT"),
            DataType::Text => write!(f, "TEXT"),
            DataType::Blob => write!(f, "BLOB"),
            DataType::Boolean => write!(f, "BOOLEAN"),
            DataType::Timestamp => write!(f, "TIMESTAMP"),
            DataType::Duration => write!(f, "DURATION"),
            DataType::IpAddr => write!(f, "IPADDR"),
            DataType::MacAddr => write!(f, "MACADDR"),
            DataType::Vector => write!(f, "VECTOR"),
            DataType::Nullable => write!(f, "NULLABLE"),
            DataType::Json => write!(f, "JSON"),
            DataType::Uuid => write!(f, "UUID"),
            DataType::NodeRef => write!(f, "NODEREF"),
            DataType::EdgeRef => write!(f, "EDGEREF"),
            DataType::VectorRef => write!(f, "VECTORREF"),
            DataType::RowRef => write!(f, "ROWREF"),
        }
    }
}

/// A typed value that can be stored in RedDB
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Null value
    Null,
    /// Signed 64-bit integer
    Integer(i64),
    /// Unsigned 64-bit integer
    UnsignedInteger(u64),
    /// 64-bit floating point
    Float(f64),
    /// UTF-8 text
    Text(String),
    /// Binary data
    Blob(Vec<u8>),
    /// Boolean
    Boolean(bool),
    /// Unix timestamp (seconds)
    Timestamp(i64),
    /// Duration in milliseconds
    Duration(i64),
    /// IP address (v4 or v6)
    IpAddr(IpAddr),
    /// MAC address
    MacAddr([u8; 6]),
    /// Float vector for similarity search
    Vector(Vec<f32>),
    /// JSON-like structured data (stored as bytes)
    Json(Vec<u8>),
    /// UUID
    Uuid([u8; 16]),
    /// Graph node reference (node ID string)
    NodeRef(String),
    /// Graph edge reference (edge ID string)
    EdgeRef(String),
    /// Vector store reference (collection, vector ID)
    VectorRef(String, u64),
    /// Table row reference (table name, row ID)
    RowRef(String, u64),
}

impl Value {
    /// Get the data type of this value
    pub fn data_type(&self) -> DataType {
        match self {
            Value::Null => DataType::Nullable,
            Value::Integer(_) => DataType::Integer,
            Value::UnsignedInteger(_) => DataType::UnsignedInteger,
            Value::Float(_) => DataType::Float,
            Value::Text(_) => DataType::Text,
            Value::Blob(_) => DataType::Blob,
            Value::Boolean(_) => DataType::Boolean,
            Value::Timestamp(_) => DataType::Timestamp,
            Value::Duration(_) => DataType::Duration,
            Value::IpAddr(_) => DataType::IpAddr,
            Value::MacAddr(_) => DataType::MacAddr,
            Value::Vector(_) => DataType::Vector,
            Value::Json(_) => DataType::Json,
            Value::Uuid(_) => DataType::Uuid,
            Value::NodeRef(_) => DataType::NodeRef,
            Value::EdgeRef(_) => DataType::EdgeRef,
            Value::VectorRef(_, _) => DataType::VectorRef,
            Value::RowRef(_, _) => DataType::RowRef,
        }
    }

    /// Check if value is null
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Serialize value to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        match self {
            Value::Null => {
                buf.push(0); // Null marker
            }
            Value::Integer(v) => {
                buf.push(DataType::Integer.to_byte());
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Value::UnsignedInteger(v) => {
                buf.push(DataType::UnsignedInteger.to_byte());
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Value::Float(v) => {
                buf.push(DataType::Float.to_byte());
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Value::Text(s) => {
                buf.push(DataType::Text.to_byte());
                let bytes = s.as_bytes();
                // Varint length encoding
                write_varint(&mut buf, bytes.len() as u64);
                buf.extend_from_slice(bytes);
            }
            Value::Blob(data) => {
                buf.push(DataType::Blob.to_byte());
                write_varint(&mut buf, data.len() as u64);
                buf.extend_from_slice(data);
            }
            Value::Boolean(v) => {
                buf.push(DataType::Boolean.to_byte());
                buf.push(if *v { 1 } else { 0 });
            }
            Value::Timestamp(v) => {
                buf.push(DataType::Timestamp.to_byte());
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Value::Duration(v) => {
                buf.push(DataType::Duration.to_byte());
                buf.extend_from_slice(&v.to_le_bytes());
            }
            Value::IpAddr(addr) => {
                buf.push(DataType::IpAddr.to_byte());
                match addr {
                    IpAddr::V4(v4) => {
                        buf.push(4); // IPv4 marker
                        buf.extend_from_slice(&v4.octets());
                    }
                    IpAddr::V6(v6) => {
                        buf.push(6); // IPv6 marker
                        buf.extend_from_slice(&v6.octets());
                    }
                }
            }
            Value::MacAddr(mac) => {
                buf.push(DataType::MacAddr.to_byte());
                buf.extend_from_slice(mac);
            }
            Value::Vector(vec) => {
                buf.push(DataType::Vector.to_byte());
                write_varint(&mut buf, vec.len() as u64);
                for v in vec {
                    buf.extend_from_slice(&v.to_le_bytes());
                }
            }
            Value::Json(data) => {
                buf.push(DataType::Json.to_byte());
                write_varint(&mut buf, data.len() as u64);
                buf.extend_from_slice(data);
            }
            Value::Uuid(uuid) => {
                buf.push(DataType::Uuid.to_byte());
                buf.extend_from_slice(uuid);
            }
            Value::NodeRef(node_id) => {
                buf.push(DataType::NodeRef.to_byte());
                let bytes = node_id.as_bytes();
                write_varint(&mut buf, bytes.len() as u64);
                buf.extend_from_slice(bytes);
            }
            Value::EdgeRef(edge_id) => {
                buf.push(DataType::EdgeRef.to_byte());
                let bytes = edge_id.as_bytes();
                write_varint(&mut buf, bytes.len() as u64);
                buf.extend_from_slice(bytes);
            }
            Value::VectorRef(collection, vector_id) => {
                buf.push(DataType::VectorRef.to_byte());
                let coll_bytes = collection.as_bytes();
                write_varint(&mut buf, coll_bytes.len() as u64);
                buf.extend_from_slice(coll_bytes);
                buf.extend_from_slice(&vector_id.to_le_bytes());
            }
            Value::RowRef(table, row_id) => {
                buf.push(DataType::RowRef.to_byte());
                let table_bytes = table.as_bytes();
                write_varint(&mut buf, table_bytes.len() as u64);
                buf.extend_from_slice(table_bytes);
                buf.extend_from_slice(&row_id.to_le_bytes());
            }
        }

        buf
    }

    /// Deserialize value from bytes
    pub fn from_bytes(data: &[u8]) -> Result<(Self, usize), ValueError> {
        if data.is_empty() {
            return Err(ValueError::EmptyData);
        }

        let type_byte = data[0];
        let mut offset = 1;

        // Null marker
        if type_byte == 0 {
            return Ok((Value::Null, 1));
        }

        let data_type = DataType::from_byte(type_byte).ok_or(ValueError::InvalidType(type_byte))?;

        let value = match data_type {
            DataType::Integer => {
                if data.len() < offset + 8 {
                    return Err(ValueError::TruncatedData);
                }
                let v = i64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                offset += 8;
                Value::Integer(v)
            }
            DataType::UnsignedInteger => {
                if data.len() < offset + 8 {
                    return Err(ValueError::TruncatedData);
                }
                let v = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                offset += 8;
                Value::UnsignedInteger(v)
            }
            DataType::Float => {
                if data.len() < offset + 8 {
                    return Err(ValueError::TruncatedData);
                }
                let v = f64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                offset += 8;
                Value::Float(v)
            }
            DataType::Text => {
                let (len, varint_size) = read_varint(&data[offset..])?;
                offset += varint_size;
                if data.len() < offset + len as usize {
                    return Err(ValueError::TruncatedData);
                }
                let s = String::from_utf8(data[offset..offset + len as usize].to_vec())
                    .map_err(|_| ValueError::InvalidUtf8)?;
                offset += len as usize;
                Value::Text(s)
            }
            DataType::Blob => {
                let (len, varint_size) = read_varint(&data[offset..])?;
                offset += varint_size;
                if data.len() < offset + len as usize {
                    return Err(ValueError::TruncatedData);
                }
                let blob = data[offset..offset + len as usize].to_vec();
                offset += len as usize;
                Value::Blob(blob)
            }
            DataType::Boolean => {
                if data.len() < offset + 1 {
                    return Err(ValueError::TruncatedData);
                }
                let v = data[offset] != 0;
                offset += 1;
                Value::Boolean(v)
            }
            DataType::Timestamp => {
                if data.len() < offset + 8 {
                    return Err(ValueError::TruncatedData);
                }
                let v = i64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                offset += 8;
                Value::Timestamp(v)
            }
            DataType::Duration => {
                if data.len() < offset + 8 {
                    return Err(ValueError::TruncatedData);
                }
                let v = i64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                offset += 8;
                Value::Duration(v)
            }
            DataType::IpAddr => {
                if data.len() < offset + 1 {
                    return Err(ValueError::TruncatedData);
                }
                let version = data[offset];
                offset += 1;
                match version {
                    4 => {
                        if data.len() < offset + 4 {
                            return Err(ValueError::TruncatedData);
                        }
                        let octets: [u8; 4] = data[offset..offset + 4].try_into().unwrap();
                        offset += 4;
                        Value::IpAddr(IpAddr::V4(Ipv4Addr::from(octets)))
                    }
                    6 => {
                        if data.len() < offset + 16 {
                            return Err(ValueError::TruncatedData);
                        }
                        let octets: [u8; 16] = data[offset..offset + 16].try_into().unwrap();
                        offset += 16;
                        Value::IpAddr(IpAddr::V6(Ipv6Addr::from(octets)))
                    }
                    _ => return Err(ValueError::InvalidIpVersion(version)),
                }
            }
            DataType::MacAddr => {
                if data.len() < offset + 6 {
                    return Err(ValueError::TruncatedData);
                }
                let mac: [u8; 6] = data[offset..offset + 6].try_into().unwrap();
                offset += 6;
                Value::MacAddr(mac)
            }
            DataType::Vector => {
                let (len, varint_size) = read_varint(&data[offset..])?;
                offset += varint_size;
                let float_count = len as usize;
                if data.len() < offset + float_count * 4 {
                    return Err(ValueError::TruncatedData);
                }
                let mut vec = Vec::with_capacity(float_count);
                for _ in 0..float_count {
                    let v = f32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                    offset += 4;
                    vec.push(v);
                }
                Value::Vector(vec)
            }
            DataType::Json => {
                let (len, varint_size) = read_varint(&data[offset..])?;
                offset += varint_size;
                if data.len() < offset + len as usize {
                    return Err(ValueError::TruncatedData);
                }
                let json = data[offset..offset + len as usize].to_vec();
                offset += len as usize;
                Value::Json(json)
            }
            DataType::Uuid => {
                if data.len() < offset + 16 {
                    return Err(ValueError::TruncatedData);
                }
                let uuid: [u8; 16] = data[offset..offset + 16].try_into().unwrap();
                offset += 16;
                Value::Uuid(uuid)
            }
            DataType::NodeRef => {
                let (len, len_bytes) = read_varint(&data[offset..])?;
                offset += len_bytes;
                if data.len() < offset + len as usize {
                    return Err(ValueError::TruncatedData);
                }
                let node_id =
                    String::from_utf8_lossy(&data[offset..offset + len as usize]).to_string();
                offset += len as usize;
                Value::NodeRef(node_id)
            }
            DataType::EdgeRef => {
                let (len, len_bytes) = read_varint(&data[offset..])?;
                offset += len_bytes;
                if data.len() < offset + len as usize {
                    return Err(ValueError::TruncatedData);
                }
                let edge_id =
                    String::from_utf8_lossy(&data[offset..offset + len as usize]).to_string();
                offset += len as usize;
                Value::EdgeRef(edge_id)
            }
            DataType::VectorRef => {
                let (len, len_bytes) = read_varint(&data[offset..])?;
                offset += len_bytes;
                if data.len() < offset + len as usize + 8 {
                    return Err(ValueError::TruncatedData);
                }
                let collection =
                    String::from_utf8_lossy(&data[offset..offset + len as usize]).to_string();
                offset += len as usize;
                let vector_id = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                offset += 8;
                Value::VectorRef(collection, vector_id)
            }
            DataType::RowRef => {
                let (len, len_bytes) = read_varint(&data[offset..])?;
                offset += len_bytes;
                if data.len() < offset + len as usize + 8 {
                    return Err(ValueError::TruncatedData);
                }
                let table =
                    String::from_utf8_lossy(&data[offset..offset + len as usize]).to_string();
                offset += len as usize;
                let row_id = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                offset += 8;
                Value::RowRef(table, row_id)
            }
            DataType::Nullable => {
                // Nullable without inner type means null
                Value::Null
            }
        };

        Ok((value, offset))
    }

    /// Try to convert to i64
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Value::Integer(v) => Some(*v),
            Value::UnsignedInteger(v) => {
                if *v <= i64::MAX as u64 {
                    Some(*v as i64)
                } else {
                    None
                }
            }
            Value::Timestamp(v) => Some(*v),
            Value::Duration(v) => Some(*v),
            _ => None,
        }
    }

    /// Try to convert to f64
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(v) => Some(*v),
            Value::Integer(v) => Some(*v as f64),
            Value::UnsignedInteger(v) => Some(*v as f64),
            _ => None,
        }
    }

    /// Try to convert to string reference
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    /// Try to convert to bool
    pub fn as_boolean(&self) -> Option<bool> {
        match self {
            Value::Boolean(v) => Some(*v),
            _ => None,
        }
    }

    /// Try to convert to IpAddr
    pub fn as_ip_addr(&self) -> Option<IpAddr> {
        match self {
            Value::IpAddr(addr) => Some(*addr),
            _ => None,
        }
    }

    /// Try to convert to vector reference
    pub fn as_vector(&self) -> Option<&[f32]> {
        match self {
            Value::Vector(v) => Some(v),
            _ => None,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => write!(f, "NULL"),
            Value::Integer(v) => write!(f, "{}", v),
            Value::UnsignedInteger(v) => write!(f, "{}", v),
            Value::Float(v) => write!(f, "{}", v),
            Value::Text(s) => write!(f, "'{}'", s),
            Value::Blob(b) => write!(f, "<blob {} bytes>", b.len()),
            Value::Boolean(v) => write!(f, "{}", v),
            Value::Timestamp(v) => write!(f, "ts:{}", v),
            Value::Duration(v) => write!(f, "{}ms", v),
            Value::IpAddr(addr) => write!(f, "{}", addr),
            Value::MacAddr(mac) => write!(
                f,
                "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
            ),
            Value::Vector(v) => write!(f, "<vector dim={}>", v.len()),
            Value::Json(j) => write!(f, "<json {} bytes>", j.len()),
            Value::Uuid(u) => {
                write!(
                    f,
                    "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                    u[0], u[1], u[2], u[3], u[4], u[5], u[6], u[7],
                    u[8], u[9], u[10], u[11], u[12], u[13], u[14], u[15]
                )
            }
            Value::NodeRef(id) => write!(f, "node:{}", id),
            Value::EdgeRef(id) => write!(f, "edge:{}", id),
            Value::VectorRef(coll, id) => write!(f, "vector:{}:{}", coll, id),
            Value::RowRef(table, id) => write!(f, "row:{}:{}", table, id),
        }
    }
}

/// Errors that can occur during value operations
#[derive(Debug, Clone, PartialEq)]
pub enum ValueError {
    /// No data to parse
    EmptyData,
    /// Invalid type byte
    InvalidType(u8),
    /// Data was truncated
    TruncatedData,
    /// Invalid UTF-8 in text
    InvalidUtf8,
    /// Invalid IP version byte
    InvalidIpVersion(u8),
    /// Varint overflow
    VarintOverflow,
    /// Type mismatch
    TypeMismatch { expected: DataType, found: DataType },
}

impl fmt::Display for ValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValueError::EmptyData => write!(f, "empty data"),
            ValueError::InvalidType(t) => write!(f, "invalid type byte: {}", t),
            ValueError::TruncatedData => write!(f, "truncated data"),
            ValueError::InvalidUtf8 => write!(f, "invalid UTF-8"),
            ValueError::InvalidIpVersion(v) => write!(f, "invalid IP version: {}", v),
            ValueError::VarintOverflow => write!(f, "varint overflow"),
            ValueError::TypeMismatch { expected, found } => {
                write!(f, "type mismatch: expected {}, found {}", expected, found)
            }
        }
    }
}

impl std::error::Error for ValueError {}

/// Write a variable-length integer (LEB128 encoding)
fn write_varint(buf: &mut Vec<u8>, mut value: u64) {
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if value == 0 {
            break;
        }
    }
}

/// Read a variable-length integer (LEB128 encoding)
fn read_varint(data: &[u8]) -> Result<(u64, usize), ValueError> {
    let mut result: u64 = 0;
    let mut shift = 0;
    let mut offset = 0;

    loop {
        if offset >= data.len() {
            return Err(ValueError::TruncatedData);
        }
        let byte = data[offset];
        offset += 1;

        if shift >= 64 {
            return Err(ValueError::VarintOverflow);
        }

        result |= ((byte & 0x7F) as u64) << shift;
        shift += 7;

        if byte & 0x80 == 0 {
            break;
        }
    }

    Ok((result, offset))
}

/// A row of values (tuple)
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    values: Vec<Value>,
}

impl Row {
    /// Create a new row from values
    pub fn new(values: Vec<Value>) -> Self {
        Self { values }
    }

    /// Get value at index
    pub fn get(&self, index: usize) -> Option<&Value> {
        self.values.get(index)
    }

    /// Get number of columns
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Check if row is empty
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Iterate over values
    pub fn iter(&self) -> impl Iterator<Item = &Value> {
        self.values.iter()
    }

    /// Get slice of all values
    pub fn values(&self) -> &[Value] {
        &self.values
    }

    /// Convert to owned values
    pub fn into_values(self) -> Vec<Value> {
        self.values
    }

    /// Serialize row to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Number of columns
        write_varint(&mut buf, self.values.len() as u64);

        // Each value
        for value in &self.values {
            let value_bytes = value.to_bytes();
            buf.extend_from_slice(&value_bytes);
        }

        buf
    }

    /// Deserialize row from bytes
    pub fn from_bytes(data: &[u8]) -> Result<(Self, usize), ValueError> {
        if data.is_empty() {
            return Err(ValueError::EmptyData);
        }

        let (column_count, mut offset) = read_varint(data)?;
        let mut values = Vec::with_capacity(column_count as usize);

        for _ in 0..column_count {
            let (value, size) = Value::from_bytes(&data[offset..])?;
            offset += size;
            values.push(value);
        }

        Ok((Row { values }, offset))
    }
}

impl From<Vec<Value>> for Row {
    fn from(values: Vec<Value>) -> Self {
        Row::new(values)
    }
}

impl IntoIterator for Row {
    type Item = Value;
    type IntoIter = std::vec::IntoIter<Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.values.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_datatype_roundtrip() {
        let types = [
            DataType::Integer,
            DataType::UnsignedInteger,
            DataType::Float,
            DataType::Text,
            DataType::Blob,
            DataType::Boolean,
            DataType::Timestamp,
            DataType::Duration,
            DataType::IpAddr,
            DataType::MacAddr,
            DataType::Vector,
            DataType::Json,
            DataType::Uuid,
        ];

        for dt in types {
            let byte = dt.to_byte();
            let recovered = DataType::from_byte(byte).unwrap();
            assert_eq!(dt, recovered);
        }
    }

    #[test]
    fn test_value_integer() {
        let value = Value::Integer(-12345);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
    }

    #[test]
    fn test_value_text() {
        let value = Value::Text("Hello, RedDB!".to_string());
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
    }

    #[test]
    fn test_value_ipaddr_v4() {
        let value = Value::IpAddr(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)));
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
    }

    #[test]
    fn test_value_ipaddr_v6() {
        let value = Value::IpAddr(IpAddr::V6(Ipv6Addr::LOCALHOST));
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
    }

    #[test]
    fn test_value_vector() {
        let value = Value::Vector(vec![1.0, 2.0, 3.0, 4.5]);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
    }

    #[test]
    fn test_value_mac_addr() {
        let value = Value::MacAddr([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
    }

    #[test]
    fn test_value_uuid() {
        let uuid = [
            0x55, 0x04, 0x43, 0x01, 0x8f, 0x3b, 0x4a, 0x12, 0x9c, 0x5d, 0x6e, 0x7f, 0x80, 0x91,
            0xa2, 0xb3,
        ];
        let value = Value::Uuid(uuid);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
    }

    #[test]
    fn test_value_null() {
        let value = Value::Null;
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
    }

    #[test]
    fn test_value_blob() {
        let value = Value::Blob(vec![0x00, 0x01, 0x02, 0x03, 0xFF]);
        let bytes = value.to_bytes();
        let (recovered, size) = Value::from_bytes(&bytes).unwrap();
        assert_eq!(value, recovered);
        assert_eq!(size, bytes.len());
    }

    #[test]
    fn test_row_roundtrip() {
        let row = Row::new(vec![
            Value::Integer(42),
            Value::Text("example.com".to_string()),
            Value::IpAddr(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
            Value::Boolean(true),
            Value::Null,
        ]);

        let bytes = row.to_bytes();
        let (recovered, size) = Row::from_bytes(&bytes).unwrap();

        assert_eq!(row, recovered);
        assert_eq!(size, bytes.len());
    }

    #[test]
    fn test_varint_encoding() {
        let test_cases = [0u64, 1, 127, 128, 255, 256, 16383, 16384, u64::MAX];

        for &value in &test_cases {
            let mut buf = Vec::new();
            write_varint(&mut buf, value);
            let (recovered, _) = read_varint(&buf).unwrap();
            assert_eq!(value, recovered, "Failed for value {}", value);
        }
    }

    #[test]
    fn test_value_display() {
        assert_eq!(format!("{}", Value::Null), "NULL");
        assert_eq!(format!("{}", Value::Integer(42)), "42");
        assert_eq!(format!("{}", Value::Boolean(true)), "true");
        assert_eq!(format!("{}", Value::Text("hello".to_string())), "'hello'");
    }

    #[test]
    fn test_datatype_properties() {
        assert_eq!(DataType::Integer.fixed_size(), Some(8));
        assert_eq!(DataType::Text.fixed_size(), None);
        assert!(DataType::Integer.is_indexable());
        assert!(DataType::Text.is_indexable());
        assert!(!DataType::Blob.is_indexable());
        assert!(DataType::Integer.is_orderable());
        assert!(!DataType::Boolean.is_orderable());
    }
}
